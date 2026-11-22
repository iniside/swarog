//! `demoseed` — fills a running fleet with deterministic demo data (players, characters,
//! inventory, matches, friends, groups, cloud saves) through the gateway's public HTTP API, then
//! runs throwaway client scenarios over the session, party and destructive ops.
//!
//!   demoseed [--base-url <url>] [--players <n>] [--matches <n>] [--concurrency <n>]
//!            [--event-timeout <secs>] [--clients <n>]
//!
//! Every call names its `opscatalog` op; the run ends with a report of every call that
//! answered an unexpected status, every op of the catalog no call covered, and exits 1 on either.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinSet;

pub mod client;
pub mod expect;
pub mod ops;
mod readback;
pub mod report;
mod scenarios;

use client::{Caller, Gateway};
pub use report::{Outcome, Report, Summary, Tally};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_PLAYERS: usize = 30;
pub const MIN_PLAYERS: usize = 2;
pub const DEFAULT_MATCHES: usize = 100;
// Below the accounts module's 32 concurrent login slots, so a seed run never draws a 503 from them.
pub const DEFAULT_CONCURRENCY: usize = 16;
pub const DEFAULT_CLIENTS: usize = 8;
// How long an event-fed read (starter item, leaderboard, notifications) polls before it is an error.
pub const DEFAULT_EVENT_TIMEOUT: Duration = Duration::from_secs(10);
pub const SPLIT_ADMIN_URL: &str = "http://127.0.0.1:8085/admin";

const PASSWORD: &str = "demo-password-1";
pub(crate) const CLIENT_KEY: &str = "dev-key-client";
// `match.report` is absent from the dev-client key's policy; only the full dev-server key may call it.
pub(crate) const SERVER_KEY: &str = "dev-key-server";

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_ATTEMPTS: u32 = 20;
const MAX_PAGES: usize = 100;
const PAGE_LIMIT: i64 = 100;

const SEED: u64 = 0x5EED_DE30_0000_0001;
const STREAM_PLAYER: u64 = 1;
const STREAM_MATCH: u64 = 2;
const STREAM_FRIEND: u64 = 3;
const STREAM_GROUP: u64 = 4;
const STREAM_PENDING_FRIEND: u64 = 5;

// Friend requests the seed sends and never accepts, so the admin view shows pending edges.
pub const PENDING_FRIEND_REQUESTS: usize = 4;

const DISPLAY_NAMES: &[&str] = &[
    "Aldric", "Brynja", "Cassian", "Dagny", "Eirik", "Freya", "Gunnar", "Hilda", "Ivar",
    "Jorunn", "Kael", "Liv", "Magnus", "Nessa", "Orrin", "Petra", "Quill", "Ragna", "Soren",
    "Thyra", "Ulric", "Vala", "Wulf", "Xenia", "Yrsa", "Zoran", "Ansel", "Birgit", "Corvin",
    "Desna",
];
const CHARACTER_NAMES: &[&str] = &[
    "Ashblade", "Briarthorn", "Cinderfall", "Duskwhisper", "Emberlynn", "Frostmane",
    "Grimward", "Hollowstep", "Ironbark", "Jadefang", "Kestrel", "Lumenveil", "Mosshide",
    "Nightbloom", "Oakenshield", "Pyrewing",
];
const CLASSES: &[&str] = &["warrior", "mage", "rogue", "cleric", "ranger", "paladin"];
const ZONES: &[&str] = &["Emberfields", "Frostvale", "Sunken Keep", "Whispering Wood", "Ashen Pass"];
// (name, join policy, join requests left undecided). The undecided ones keep a pending row in
// the admin view; they draw from the group's stream after its members, so adding them left
// every earlier draw unchanged.
const GROUPS: &[(&str, &str, usize)] = &[
    ("Iron Vanguard", "open", 0),
    ("Silver Circle", "request", 0),
    ("Shadow Pact", "invite", 0),
    ("Dawn Riders", "open", 0),
    ("Ember Wardens", "request", 1),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub base_url: String,
    pub players: usize,
    pub matches: usize,
    pub concurrency: usize,
    pub event_timeout: Duration,
    pub clients: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Seed(Config),
}

pub const USAGE: &str = "\
demoseed — seed deterministic demo data into a running fleet through the gateway API

USAGE:
  demoseed [--base-url <url>] [--players <n>] [--matches <n>] [--concurrency <n>]
           [--event-timeout <secs>] [--clients <n>]

  --base-url <url>   gateway front door (default http://127.0.0.1:8080; split: http://127.0.0.1:8082)
  --players <n>      demo players to create (default 30, at least 2)
  --matches <n>      match reports to send (default 100)
  --concurrency <n>  requests in flight at once (default 16, at least 1)
  --event-timeout <secs>
                     how long an event-fed read polls before it is an error (default 10)
  --clients <n>      throwaway client scenarios to run after the seed (default 8, at least 1)

Requires a fleet started with the dev flags (devctl up sets APIKEYS_DEV_SEED, ACCOUNTS_DEV_AUTH,
INVENTORY_DEV_GRANT, WALLET_DEV_SEED). Re-running converges: only missing rows are created, and
every seeded kind is read back. Then each client scenario creates its own guest players and a
registered account, drives the session, party and destructive ops on them, and deletes them.
Exits 1 when any call
answered an unexpected status or an op of the catalog was never called.";

pub fn parse_args(args: &[String]) -> Result<Command> {
    let mut config = Config {
        base_url: DEFAULT_BASE_URL.to_string(),
        players: DEFAULT_PLAYERS,
        matches: DEFAULT_MATCHES,
        concurrency: DEFAULT_CONCURRENCY,
        event_timeout: DEFAULT_EVENT_TIMEOUT,
        clients: DEFAULT_CLIENTS,
    };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "--base-url" | "--players" | "--matches" | "--concurrency" | "--event-timeout" | "--clients" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("demoseed: {flag} requires a value\n\n{USAGE}"))?;
                match flag.as_str() {
                    "--base-url" => config.base_url = value.trim_end_matches('/').to_string(),
                    "--players" => config.players = parse_players(flag, value)?,
                    "--concurrency" => config.concurrency = parse_positive(flag, value)?,
                    "--clients" => config.clients = parse_positive(flag, value)?,
                    "--event-timeout" => {
                        config.event_timeout = Duration::from_secs(parse_positive(flag, value)? as u64)
                    }
                    _ => config.matches = parse_count(flag, value)?,
                }
            }
            other => bail!("demoseed: unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    if config.base_url.is_empty() {
        bail!("demoseed: --base-url must not be empty");
    }
    Ok(Command::Seed(config))
}

fn parse_count(flag: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| anyhow!("demoseed: {flag} expects a non-negative integer, got {value:?}"))
}

// Friendships, group members and public save reads each need a second player; with fewer the
// ops behind them are never called and the run could only end "not covered".
fn parse_players(flag: &str, value: &str) -> Result<usize> {
    match parse_count(flag, value)? {
        n if n >= MIN_PLAYERS => Ok(n),
        n => bail!(
            "demoseed: {flag} must be at least {MIN_PLAYERS} (friendships, groups and public saves need two players), got {n}"
        ),
    }
}

fn parse_positive(flag: &str, value: &str) -> Result<usize> {
    match value.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!("demoseed: {flag} expects a positive integer, got {value:?}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedPlan {
    pub players: Vec<PlayerPlan>,
    pub matches: Vec<MatchPlan>,
    pub friendships: Vec<FriendPlan>,
    /// Requested and never accepted; disjoint from `friendships` as unordered pairs, since a
    /// request against an existing reverse request accepts it.
    pub pending_friendships: Vec<FriendPlan>,
    pub groups: Vec<GroupPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerPlan {
    pub email: String,
    pub display_name: String,
    pub characters: Vec<CharacterPlan>,
    pub items: Vec<ItemTarget>,
    pub save: SavePlan,
    /// Readable by every player; `public_save_reader` names the one that reads it.
    pub public_save: SavePlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterPlan {
    pub name: String,
    pub class: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTarget {
    pub item_id: String,
    pub qty: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePlan {
    pub collection: String,
    pub key: String,
    pub value: String,
    pub acl: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchPlan {
    pub report_id: String,
    pub winner: usize,
    pub loser: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendPlan {
    pub requester: usize,
    pub target: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupPlan {
    pub name: String,
    pub join_policy: String,
    pub owner: usize,
    pub members: Vec<usize>,
    /// Players whose join request stays undecided; disjoint from the owner and the members.
    pub pending: Vec<usize>,
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn stream(kind: u64, index: usize) -> Self {
        let mut rng = SplitMix64(SEED ^ kind.wrapping_mul(0xA24B_AED4_963E_E407) ^ index as u64);
        rng.next_u64();
        rng
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }

    fn distinct(&mut self, pool: usize, count: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..pool).collect();
        let count = count.min(pool);
        for i in 0..count {
            let j = i + self.below(pool - i);
            indices.swap(i, j);
        }
        indices.truncate(count);
        indices
    }
}

pub fn plan(players: usize, matches: usize) -> SeedPlan {
    let players_plan = (0..players).map(plan_player).collect();
    let mut seed_plan = SeedPlan {
        players: players_plan,
        matches: Vec::new(),
        friendships: Vec::new(),
        pending_friendships: Vec::new(),
        groups: Vec::new(),
    };
    if players == 0 {
        return seed_plan;
    }
    if players >= 2 {
        seed_plan.matches = (0..matches)
            .map(|m| {
                let mut rng = SplitMix64::stream(STREAM_MATCH, m);
                let winner = rng.below(players);
                let mut loser = rng.below(players - 1);
                if loser >= winner {
                    loser += 1;
                }
                MatchPlan { report_id: format!("demo-match-{m:04}"), winner, loser }
            })
            .collect();
        let mut seen = BTreeSet::new();
        for requester in 0..players {
            let mut rng = SplitMix64::stream(STREAM_FRIEND, requester);
            let wanted = 1 + rng.below(2);
            for _ in 0..wanted {
                let mut target = rng.below(players - 1);
                if target >= requester {
                    target += 1;
                }
                if seen.insert((requester.min(target), requester.max(target))) {
                    seed_plan.friendships.push(FriendPlan { requester, target });
                }
            }
        }
        let mut rng = SplitMix64::stream(STREAM_PENDING_FRIEND, 0);
        for _ in 0..PENDING_FRIEND_REQUESTS * 16 {
            if seed_plan.pending_friendships.len() == PENDING_FRIEND_REQUESTS {
                break;
            }
            let requester = rng.below(players);
            let mut target = rng.below(players - 1);
            if target >= requester {
                target += 1;
            }
            if seen.insert((requester.min(target), requester.max(target))) {
                seed_plan.pending_friendships.push(FriendPlan { requester, target });
            }
        }
    }
    seed_plan.groups = GROUPS
        .iter()
        .enumerate()
        .map(|(g, (name, policy, undecided))| {
            let mut rng = SplitMix64::stream(STREAM_GROUP, g);
            let owner = (g * 7) % players;
            let others: Vec<usize> = (0..players).filter(|&p| p != owner).collect();
            let size = 3 + rng.below(5);
            let members: Vec<usize> = rng.distinct(others.len(), size).into_iter().map(|i| others[i]).collect();
            let rest: Vec<usize> = others.into_iter().filter(|p| !members.contains(p)).collect();
            let pending = rng.distinct(rest.len(), *undecided).into_iter().map(|i| rest[i]).collect();
            GroupPlan { name: name.to_string(), join_policy: policy.to_string(), owner, members, pending }
        })
        .collect();
    seed_plan
}

fn plan_player(index: usize) -> PlayerPlan {
    let mut rng = SplitMix64::stream(STREAM_PLAYER, index);
    let base_name = DISPLAY_NAMES[index % DISPLAY_NAMES.len()];
    let display_name = match index / DISPLAY_NAMES.len() {
        0 => base_name.to_string(),
        round => format!("{base_name}{}", round + 1),
    };
    let character_count = 1 + rng.below(3);
    let characters = rng
        .distinct(CHARACTER_NAMES.len(), character_count)
        .into_iter()
        .map(|c| CharacterPlan {
            name: CHARACTER_NAMES[c].to_string(),
            class: CLASSES[rng.below(CLASSES.len())].to_string(),
        })
        .collect();
    let mut items = vec![
        ItemTarget { item_id: "coin".to_string(), qty: rng.range(100, 5000) },
        ItemTarget { item_id: "health_potion".to_string(), qty: rng.range(1, 20) },
    ];
    if rng.below(2) == 0 {
        items.push(ItemTarget { item_id: "starter_sword".to_string(), qty: 1 });
    }
    let save_value = json!({
        "level": rng.range(1, 60),
        "zone": ZONES[rng.below(ZONES.len())],
        "playtime_secs": rng.range(600, 360_000),
    })
    .to_string();
    // Drawn last, so every earlier draw (and the pinned golden) is unchanged by it.
    let public_value = json!({"display_name": display_name, "favorite_zone": ZONES[rng.below(ZONES.len())]}).to_string();
    PlayerPlan {
        email: format!("demo-{index:02}@demo.local"),
        display_name,
        characters,
        items,
        save: SavePlan {
            collection: "saves".to_string(),
            key: "slot1".to_string(),
            value: save_value,
            acl: "private".to_string(),
        },
        public_save: SavePlan {
            collection: "saves".to_string(),
            key: "public".to_string(),
            value: public_value,
            acl: "public_read".to_string(),
        },
    }
}

/// The player that reads `owner`'s public save by `owner_id`: the next one round the ring, never
/// the owner itself. `None` with fewer than two players.
pub fn public_save_reader(owner: usize, players: usize) -> Option<usize> {
    (players >= 2).then(|| (owner + 1) % players)
}

/// The planned player with the most wins, the lowest index on a tie; `None` without matches.
pub fn top_winner(seed: &SeedPlan) -> Option<usize> {
    let mut wins = vec![0usize; seed.players.len()];
    for planned in &seed.matches {
        wins[planned.winner] += 1;
    }
    let (index, &most) = wins.iter().enumerate().max_by_key(|&(index, &w)| (w, std::cmp::Reverse(index)))?;
    (most > 0).then_some(index)
}

pub fn inventory_shortfall(target: i64, held: i64) -> i64 {
    (target - held).max(0)
}

pub fn missing_characters<'a>(planned: &'a [CharacterPlan], existing: &[String]) -> Vec<&'a CharacterPlan> {
    planned.iter().filter(|c| !existing.iter().any(|name| name == &c.name)).collect()
}

pub fn friend_needs_accept(state: &str) -> Result<bool> {
    match state {
        "pending" => Ok(true),
        "accepted" => Ok(false),
        other => bail!("friend request answered with unknown state {other:?}"),
    }
}

pub fn invite_needs_respond(my_state: &str) -> Result<bool> {
    match my_state {
        "invited" => Ok(true),
        "member" => Ok(false),
        other => bail!("invited player holds a group row in unexpected state {other:?}"),
    }
}

// Local shapes of the success bodies this seed reads (I4): only the fields it uses, so an
// added field is no error but a renamed or missing one is a shape error for that op.
#[derive(Deserialize)]
struct SessionBody {
    player_id: String,
    token: String,
}

#[derive(Deserialize)]
struct MeBody {
    handle: String,
}

#[derive(Clone, Deserialize)]
struct CharacterRow {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct HoldingRow {
    item_id: String,
    quantity: i64,
}

#[derive(Deserialize)]
struct FriendBody {
    edge_id: String,
    state: String,
}

#[derive(Clone, Deserialize)]
struct GroupRow {
    id: String,
    name: String,
    my_state: String,
}

#[derive(Deserialize)]
struct GroupPageBody {
    items: Vec<GroupRow>,
    next_cursor: String,
}

struct Session {
    player_id: String,
    token: String,
    handle: String,
}

type Sessions = Arc<Vec<Option<Session>>>;

/// Seeds the plan through the gateway and returns the report. The only error is a base URL that
/// does not answer the startup probe; every failed call after that is recorded in the report.
/// Phases run in order (players, per-player rows, matches and the leaderboard, friendships,
/// groups, inboxes and public saves), each one's items as parallel tasks; a later phase skips,
/// with a recorded cause, the items whose players failed. Every seeded kind is read back. Last,
/// `config.clients` throwaway client scenarios run as parallel tasks (`scenarios.rs`).
pub async fn run(config: &Config) -> Result<Report> {
    let gateway = Gateway::new(&config.base_url, config.concurrency)?;
    gateway.probe().await?;
    let seed = Arc::new(plan(config.players, config.matches));
    let timeout = config.event_timeout;

    let mut tasks = JoinSet::new();
    for index in 0..seed.players.len() {
        let gateway = gateway.scenario(format!("player {}", seed.players[index].email));
        let seed = Arc::clone(&seed);
        tasks.spawn(async move { (index, seed_player(&gateway, &seed.players[index]).await) });
    }
    let mut sessions: Vec<Option<Session>> = (0..seed.players.len()).map(|_| None).collect();
    for (index, session) in join_all(tasks).await {
        sessions[index] = session;
    }
    let sessions: Sessions = Arc::new(sessions);

    let mut tasks = JoinSet::new();
    for index in 0..seed.players.len() {
        let gateway = gateway.scenario(format!("player {}", seed.players[index].email));
        if !players_seeded(&gateway, &seed, &sessions, &[index], "characters, inventory, saves and wallet") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move {
            let (player, session) = (&seed.players[index], sessions[index].as_ref().expect("checked above"));
            let characters = seed_characters(&gateway, player, session).await;
            seed_inventory(&gateway, player, session).await;
            seed_save(&gateway, &player.save, session).await;
            let public_saved = seed_save(&gateway, &player.public_save, session).await.is_some();
            match characters {
                Some(characters) => readback::starter_items(&gateway, session, &characters, timeout).await,
                None => gateway.skip("starter item reads skipped: the character list failed"),
            }
            readback::wallet(&gateway, session).await;
            readback::own_saves(&gateway, player, session).await;
            (index, public_saved)
        });
    }
    let mut public_saved = vec![false; seed.players.len()];
    for (index, saved) in join_all(tasks).await {
        public_saved[index] = saved;
    }

    let mut tasks = JoinSet::new();
    for index in 0..seed.matches.len() {
        let planned = &seed.matches[index];
        let gateway = gateway.scenario(format!("match {}", planned.report_id));
        if !players_seeded(&gateway, &seed, &sessions, &[planned.winner, planned.loser], "the report") {
            tasks.spawn(async move { (index, false) });
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move { (index, seed_match(&gateway, &seed.matches[index], &sessions).await.is_some()) });
    }
    let reported: Vec<(usize, bool)> = join_all(tasks).await;
    let top = top_winner(&seed);
    if top.is_none() {
        // No match planned: one read still covers `topScores`.
        readback::leaderboard_shape(&gateway.scenario("leaderboard")).await;
    }
    if let Some(top) = top {
        let gateway = gateway.scenario("leaderboard");
        let lost: Vec<&str> = reported
            .iter()
            .filter(|&&(index, ok)| !ok && seed.matches[index].winner == top)
            .map(|&(index, _)| seed.matches[index].report_id.as_str())
            .collect();
        if !lost.is_empty() {
            gateway.skip(format!("leaderboard read skipped: the top winner's reports failed: {}", lost.join(", ")));
        } else if let Some(winner) = sessions[top].as_ref() {
            readback::leaderboard(&gateway, &winner.player_id, timeout).await;
        }
    }

    // Before any accept: each requester's `friend.accepted` count, so the inbox read counts
    // only the rows this run adds. `None` for a player owed none or whose read failed.
    let mut tasks = JoinSet::new();
    let requesters: BTreeSet<usize> = seed.friendships.iter().map(|f| f.requester).collect();
    for index in requesters {
        if sessions[index].is_none() {
            continue;
        }
        let gateway = gateway.scenario(format!("player {}", seed.players[index].email));
        let sessions = Arc::clone(&sessions);
        tasks.spawn(async move {
            let session = sessions[index].as_ref().expect("checked above");
            (index, readback::accepted_rows(&gateway, session).await)
        });
    }
    let mut baselines: Vec<Option<usize>> = vec![None; seed.players.len()];
    for (index, baseline) in join_all(tasks).await {
        baselines[index] = baseline;
    }

    let mut tasks = JoinSet::new();
    let planned_pairs =
        seed.friendships.iter().map(|f| (f, true)).chain(seed.pending_friendships.iter().map(|f| (f, false)));
    for (friendship, accept) in planned_pairs {
        let kind = if accept { "friendship" } else { "pending friendship" };
        let gateway = gateway.scenario(format!(
            "{kind} {} -> {}",
            seed.players[friendship.requester].email, seed.players[friendship.target].email
        ));
        if !players_seeded(&gateway, &seed, &sessions, &[friendship.requester, friendship.target], "the friendship") {
            continue;
        }
        let (friendship, sessions) = (friendship.clone(), Arc::clone(&sessions));
        tasks.spawn(async move {
            let requester = sessions[friendship.requester].as_ref().expect("checked above");
            let target = sessions[friendship.target].as_ref().expect("checked above");
            let accepted_now = seed_friendship(&gateway, requester, target, accept).await == Some(true);
            (friendship.requester, accepted_now)
        });
    }
    // Per player, the friendships this run accepted on its request: each owes it a fresh
    // `friend.accepted` inbox row.
    let mut accepted_now = vec![0usize; seed.players.len()];
    for (requester, accepted) in join_all(tasks).await {
        accepted_now[requester] += usize::from(accepted);
    }

    let mut tasks = JoinSet::new();
    for index in 0..seed.groups.len() {
        let group = &seed.groups[index];
        let gateway = gateway.scenario(format!("group {}", group.name));
        if !players_seeded(&gateway, &seed, &sessions, &[group.owner], "the group and its members") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move { seed_group(&gateway, &seed, &seed.groups[index], &sessions).await });
    }
    join_all(tasks).await;

    let mut tasks = JoinSet::new();
    for index in 0..seed.players.len() {
        let gateway = gateway.scenario(format!("player {}", seed.players[index].email));
        if !players_seeded(&gateway, &seed, &sessions, &[index], "the inbox and public save reads") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        let (fresh, public_saved) = (accepted_now[index], public_saved[index]);
        let (baseline, fresh) = match baselines[index] {
            Some(baseline) => (baseline, fresh),
            None if fresh > 0 => {
                gateway.skip("inbox row check skipped: the inbox read before the friendships failed");
                (0, 0)
            }
            None => (0, 0),
        };
        tasks.spawn(async move {
            let session = sessions[index].as_ref().expect("checked above");
            readback::inbox(&gateway, session, baseline, fresh, timeout).await;
            let Some(reader) = public_save_reader(index, seed.players.len()) else { return };
            match sessions[reader].as_ref() {
                Some(reader) if public_saved => {
                    readback::public_save(&gateway, &seed.players[index].public_save, session, reader).await;
                }
                Some(_) => gateway.skip("public save read skipped: the public save was not written"),
                None => gateway.skip(format!(
                    "public save read skipped: the account seed failed for the reader {}",
                    seed.players[reader].email
                )),
            }
        });
    }
    join_all(tasks).await;

    let nonce = run_nonce();
    let mut tasks = JoinSet::new();
    for index in 0..config.clients {
        let client = scenarios::Client { nonce: nonce.clone(), index, timeout };
        let gateway = gateway.scenario(client.name());
        tasks.spawn(async move { scenarios::run_client(&gateway, &client).await });
    }
    join_all(tasks).await;

    Ok(gateway.take_report())
}

/// Names this run's throwaway rows (emails, group names, report ids) apart from every earlier
/// run's: the wall clock in nanoseconds and the process id, in hex.
fn run_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or_default();
    format!("{nanos:x}{:x}", std::process::id())
}

/// Joins every task of a phase, so the next phase starts only after this one ended. A task that
/// panicked (a programming error such as a missing path param) panics the run.
async fn join_all<T: 'static>(mut tasks: JoinSet<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(tasks.len());
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(value) => out.push(value),
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(err) => panic!("demoseed: a seed task was cancelled: {err}"),
        }
    }
    out
}

/// True when every named player holds a session; otherwise records the skip of what on the
/// scenario, naming the players whose seed failed.
fn players_seeded(gateway: &Gateway, seed: &SeedPlan, sessions: &[Option<Session>], players: &[usize], what: &str) -> bool {
    let failed: Vec<&str> =
        players.iter().filter(|&&p| sessions[p].is_none()).map(|&p| seed.players[p].email.as_str()).collect();
    if failed.is_empty() {
        return true;
    }
    gateway.skip(format!("{what} skipped: the account seed failed for {}", failed.join(", ")));
    false
}

/// The value, or `None` after recording that the dependent step was skipped; the failure that
/// produced the `None` is already in the report.
fn or_skip<T>(gateway: &Gateway, value: Option<T>, skipped: &str) -> Option<T> {
    if value.is_none() {
        gateway.skip(skipped);
    }
    value
}

async fn seed_player(gateway: &Gateway, player: &PlayerPlan) -> Option<Session> {
    let register = json!({"email": player.email, "password": PASSWORD, "displayName": player.display_name});
    let registered =
        gateway.call(ops::ACCOUNTS_REGISTER, &[], Caller::Anonymous, Some(&register), expect::ACCOUNTS_REGISTER).await.ok();
    let (status, _) = or_skip(gateway, registered, "login and profile read skipped: the registration failed")?;
    gateway.tally(|s| s.players.record(Outcome::from_status(status)));
    let login = json!({"email": player.email, "password": PASSWORD});
    let session = gateway
        .call(ops::ACCOUNTS_LOGIN, &[], Caller::Anonymous, Some(&login), expect::ACCOUNTS_LOGIN)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<SessionBody>(ops::ACCOUNTS_LOGIN, body));
    let session = or_skip(gateway, session, "profile read skipped: the login failed")?;
    let (_, body) = gateway
        .call(ops::ACCOUNTS_ME, &[], Caller::Player(&session.token), None, expect::ACCOUNTS_ME)
        .await
        .ok()?;
    let me: MeBody = gateway.typed(ops::ACCOUNTS_ME, body)?;
    Some(Session { player_id: session.player_id, token: session.token, handle: me.handle })
}

/// Creates the planned characters the list does not show and returns every planned one that
/// exists afterwards (listed or created), for the starter item read.
async fn seed_characters(gateway: &Gateway, player: &PlayerPlan, session: &Session) -> Option<Vec<CharacterRow>> {
    let caller = Caller::Player(&session.token);
    let listed = gateway
        .call(ops::CHARACTERS_LIST, &[], caller, None, expect::CHARACTERS_LIST)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<CharacterRow>>(ops::CHARACTERS_LIST, body));
    let listed = or_skip(gateway, listed, "character creation skipped: the character list failed")?;
    let existing: Vec<String> = listed.iter().map(|c| c.name.clone()).collect();
    let missing = missing_characters(&player.characters, &existing);
    gateway.tally(|s| s.characters.existing += player.characters.len() - missing.len());
    let mut present: Vec<CharacterRow> =
        listed.into_iter().filter(|c| player.characters.iter().any(|p| p.name == c.name)).collect();
    for character in missing {
        let body = json!({"name": character.name, "class": character.class});
        let created = gateway.call(ops::CHARACTERS_CREATE, &[], caller, Some(&body), expect::CHARACTERS_CREATE).await.ok();
        if let Some((status, body)) = created {
            gateway.tally(|s| s.characters.record(Outcome::from_status(status)));
            present.extend(gateway.typed::<CharacterRow>(ops::CHARACTERS_CREATE, body));
        }
    }
    Some(present)
}

async fn seed_inventory(gateway: &Gateway, player: &PlayerPlan, session: &Session) -> Option<()> {
    let caller = Caller::Player(&session.token);
    let listed = gateway
        .call(ops::INVENTORY_LIST_MINE, &[], caller, None, expect::INVENTORY_LIST_MINE)
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<HoldingRow>>(ops::INVENTORY_LIST_MINE, body));
    let listed = or_skip(gateway, listed, "inventory grants skipped: the inventory list failed")?;
    let mut held: HashMap<String, i64> = HashMap::new();
    for holding in listed {
        *held.entry(holding.item_id).or_default() += holding.quantity;
    }
    for target in &player.items {
        let shortfall = inventory_shortfall(target.qty, held.get(&target.item_id).copied().unwrap_or(0));
        if shortfall == 0 {
            gateway.tally(|s| s.grants.existing += 1);
            continue;
        }
        let body = json!({"item_id": target.item_id, "qty": shortfall});
        if let Some((status, _)) =
            gateway.call(ops::INVENTORY_GRANT, &[], caller, Some(&body), expect::INVENTORY_GRANT).await.ok()
        {
            gateway.tally(|s| s.grants.record(Outcome::from_status(status)));
        }
    }
    Some(())
}

async fn seed_save(gateway: &Gateway, save: &SavePlan, session: &Session) -> Option<()> {
    let params = [("collection", save.collection.as_str()), ("key", save.key.as_str())];
    let body = json!({"value": save.value, "expected_version": 0, "acl": save.acl});
    let (status, _) = gateway
        .call(ops::STORAGE_PUT, &params, Caller::Player(&session.token), Some(&body), expect::STORAGE_PUT)
        .await
        .ok()?;
    gateway.tally(|s| s.saves.record(Outcome::from_status(status)));
    Some(())
}

async fn seed_match(gateway: &Gateway, planned: &MatchPlan, sessions: &[Option<Session>]) -> Option<()> {
    let winner = sessions[planned.winner].as_ref()?;
    let loser = sessions[planned.loser].as_ref()?;
    let body = json!({"ReportId": planned.report_id, "Winner": winner.player_id, "Loser": loser.player_id});
    let (status, _) = gateway.call(ops::MATCH_REPORT, &[], Caller::Server, Some(&body), expect::MATCH_REPORT).await.ok()?;
    gateway.tally(|s| s.matches.record(Outcome::from_status(status)));
    Some(())
}

/// Requests the friendship and, when `accept`, has the target accept it, then reads both
/// friend lists. `Some(true)` when this run accepted the edge, so the requester's inbox owes a
/// fresh `friend.accepted` row; a pending edge must show in the target's pending list first.
async fn seed_friendship(gateway: &Gateway, requester: &Session, target: &Session, accept: bool) -> Option<bool> {
    let body = json!({"target_handle": target.handle});
    let friend = gateway
        .call(ops::FRIENDS_REQUEST, &[], Caller::Player(&requester.token), Some(&body), expect::FRIENDS_REQUEST)
        .await
        .ok()
        .and_then(|(_, friend)| gateway.typed::<FriendBody>(ops::FRIENDS_REQUEST, friend));
    let friend = or_skip(gateway, friend, "friend accept skipped: the friend request failed")?;
    let pending = match friend_needs_accept(&friend.state) {
        Ok(pending) => pending,
        Err(err) => {
            gateway.shape_error(ops::FRIENDS_REQUEST, format!("{err:#}"));
            gateway.skip("friend accept skipped: the friend request answered an unknown state");
            return None;
        }
    };
    if pending {
        // A re-run finds an accepted edge, which no pending list shows, so only an edge this
        // run saw pending is looked for.
        let listed = readback::incoming_request(gateway, target, &friend.edge_id).await;
        or_skip(gateway, listed, "friend accept skipped: the pending request is not in the target's list")?;
    }
    if !accept {
        return Some(false);
    }
    let accepted_now = if pending {
        let id = [("id", friend.edge_id.as_str())];
        let (status, _) = gateway
            .call(ops::FRIENDS_ACCEPT, &id, Caller::Player(&target.token), None, expect::FRIENDS_ACCEPT)
            .await
            .ok()?;
        gateway.tally(|s| s.friendships.record(Outcome::from_status(status)));
        true
    } else {
        gateway.tally(|s| s.friendships.existing += 1);
        false
    };
    readback::friend_listed(gateway, requester, target).await;
    readback::friend_listed(gateway, target, requester).await;
    Some(accepted_now)
}

async fn seed_group(gateway: &Gateway, seed: &SeedPlan, group: &GroupPlan, sessions: &[Option<Session>]) -> Option<()> {
    let owner = sessions[group.owner].as_ref()?;
    let found = find_mine(gateway, owner, |row| row.name == group.name).await;
    let group_id = match found {
        Some(Some(row)) => {
            gateway.tally(|s| s.groups.existing += 1);
            row.id
        }
        Some(None) => {
            let body = json!({"name": group.name, "join_policy": group.join_policy});
            let created = gateway
                .call(ops::GROUPS_CREATE, &[], Caller::Player(&owner.token), Some(&body), expect::GROUPS_CREATE)
                .await
                .ok()
                .and_then(|(_, body)| gateway.typed::<GroupRow>(ops::GROUPS_CREATE, body));
            match created {
                Some(row) => {
                    gateway.tally(|s| s.groups.created += 1);
                    row.id
                }
                None => {
                    gateway.skip("memberships skipped: the group was not created");
                    return None;
                }
            }
        }
        None => {
            gateway.skip("group and memberships skipped: the owner's group list failed");
            return None;
        }
    };
    let mut joined = vec![owner];
    for &member in &group.members {
        let Some(session) = sessions[member].as_ref() else {
            gateway.skip(format!("membership skipped: the account seed failed for {}", seed.players[member].email));
            continue;
        };
        if let Some(outcome) = seed_membership(gateway, group, &group_id, owner, session).await {
            gateway.tally(|s| s.memberships.record(outcome));
            joined.push(session);
        }
    }
    let mut requested = Vec::new();
    for &asker in &group.pending {
        let Some(session) = sessions[asker].as_ref() else {
            gateway.skip(format!("join request skipped: the account seed failed for {}", seed.players[asker].email));
            continue;
        };
        if join(gateway, &group_id, session).await.is_some() {
            requested.push(session);
        }
    }
    readback::group_rows(gateway, &group_id, owner, &joined, &requested).await;
    Some(())
}

async fn seed_membership(
    gateway: &Gateway,
    group: &GroupPlan,
    group_id: &str,
    owner: &Session,
    member: &Session,
) -> Option<Outcome> {
    let id = [("id", group_id)];
    match group.join_policy.as_str() {
        "open" => join(gateway, group_id, member).await,
        "request" => {
            or_skip(gateway, join(gateway, group_id, member).await, "join decision skipped: the join request failed")?;
            let body = json!({"subject_id": member.player_id, "decision": "accept"});
            let (status, _) = gateway
                .call(ops::GROUPS_DECIDE, &id, Caller::Player(&owner.token), Some(&body), expect::GROUPS_DECIDE)
                .await
                .ok()?;
            Some(Outcome::from_status(status))
        }
        "invite" => {
            let body = json!({"target_handle": member.handle});
            let invited = gateway
                .call(ops::GROUPS_INVITE, &id, Caller::Player(&owner.token), Some(&body), expect::GROUPS_INVITE)
                .await
                .ok();
            or_skip(gateway, invited, "invite response skipped: the invite failed")?;
            let found = find_mine(gateway, member, |row| row.id == group_id).await;
            let Some(row) = or_skip(gateway, found, "invite response skipped: the invitee's group list failed")? else {
                gateway.shape_error(
                    ops::GROUPS_LIST_MINE,
                    format!("invitee {} holds no row for the group after the invite", member.handle),
                );
                gateway.skip("invite response skipped: the invitee holds no row for the group");
                return None;
            };
            match invite_needs_respond(&row.my_state) {
                Ok(true) => {
                    let body = json!({"decision": "accept"});
                    let (status, _) = gateway
                        .call(ops::GROUPS_RESPOND, &id, Caller::Player(&member.token), Some(&body), expect::GROUPS_RESPOND)
                        .await
                        .ok()?;
                    Some(Outcome::from_status(status))
                }
                Ok(false) => Some(Outcome::AlreadyExists),
                Err(err) => {
                    gateway.shape_error(ops::GROUPS_LIST_MINE, format!("{err:#}"));
                    gateway.skip("invite response skipped: the invitee's row is in an unknown state");
                    None
                }
            }
        }
        other => {
            gateway.shape_error(ops::GROUPS_CREATE, format!("plan names unknown join policy {other:?}"));
            None
        }
    }
}

async fn join(gateway: &Gateway, group_id: &str, member: &Session) -> Option<Outcome> {
    let (status, _) = gateway
        .call(ops::GROUPS_JOIN, &[("id", group_id)], Caller::Player(&member.token), Some(&json!({})), expect::GROUPS_JOIN)
        .await
        .ok()?;
    Some(Outcome::from_status(status))
}

/// Pages the caller's groups for the first row matching; the outer `None` means a page failed
/// (already recorded), the inner one that no row matched.
async fn find_mine(gateway: &Gateway, session: &Session, matches: impl Fn(&GroupRow) -> bool) -> Option<Option<GroupRow>> {
    let mut cursor = String::new();
    for _ in 0..MAX_PAGES {
        let body = json!({"cursor": cursor, "limit": PAGE_LIMIT});
        let (_, page) = gateway
            .call(ops::GROUPS_LIST_MINE, &[], Caller::Player(&session.token), Some(&body), expect::GROUPS_LIST_MINE)
            .await
            .ok()?;
        let page: GroupPageBody = gateway.typed(ops::GROUPS_LIST_MINE, page)?;
        if let Some(row) = page.items.iter().find(|row| matches(row)) {
            return Some(Some(row.clone()));
        }
        if page.next_cursor.is_empty() {
            return Some(None);
        }
        cursor = page.next_cursor;
    }
    gateway.shape_error(ops::GROUPS_LIST_MINE, format!("more than {MAX_PAGES} pages for one player"));
    None
}

#[cfg(test)]
mod tests;

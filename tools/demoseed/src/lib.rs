//! `demoseed` — fills a running fleet with deterministic demo data (players, characters,
//! inventory, matches, friends, groups, cloud saves) through the gateway's public HTTP API.
//!
//!   demoseed [--base-url <url>] [--players <n>] [--matches <n>] [--concurrency <n>]
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
pub mod ops;
pub mod report;

use client::{Caller, Gateway};
pub use report::{Outcome, Report, Summary, Tally};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_PLAYERS: usize = 30;
pub const DEFAULT_MATCHES: usize = 100;
// Below the accounts module's 32 concurrent login slots, so a seed run never draws a 503 from them.
pub const DEFAULT_CONCURRENCY: usize = 16;
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
const GROUPS: &[(&str, &str)] = &[
    ("Iron Vanguard", "open"),
    ("Silver Circle", "request"),
    ("Shadow Pact", "invite"),
    ("Dawn Riders", "open"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub base_url: String,
    pub players: usize,
    pub matches: usize,
    pub concurrency: usize,
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

  --base-url <url>   gateway front door (default http://127.0.0.1:8080; split: http://127.0.0.1:8082)
  --players <n>      demo players to create (default 30)
  --matches <n>      match reports to send (default 100)
  --concurrency <n>  requests in flight at once (default 16, at least 1)

Requires a fleet started with the dev flags (devctl up sets APIKEYS_DEV_SEED, ACCOUNTS_DEV_AUTH,
INVENTORY_DEV_GRANT). Re-running converges: only missing rows are created. Exits 1 when any call
answered an unexpected status or an op of the catalog was never called.";

pub fn parse_args(args: &[String]) -> Result<Command> {
    let mut config = Config {
        base_url: DEFAULT_BASE_URL.to_string(),
        players: DEFAULT_PLAYERS,
        matches: DEFAULT_MATCHES,
        concurrency: DEFAULT_CONCURRENCY,
    };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "--base-url" | "--players" | "--matches" | "--concurrency" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("demoseed: {flag} requires a value\n\n{USAGE}"))?;
                match flag.as_str() {
                    "--base-url" => config.base_url = value.trim_end_matches('/').to_string(),
                    "--players" => config.players = parse_count(flag, value)?,
                    "--concurrency" => config.concurrency = parse_positive(flag, value)?,
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
    pub groups: Vec<GroupPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerPlan {
    pub email: String,
    pub display_name: String,
    pub characters: Vec<CharacterPlan>,
    pub items: Vec<ItemTarget>,
    pub save: SavePlan,
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
    }
    seed_plan.groups = GROUPS
        .iter()
        .enumerate()
        .map(|(g, (name, policy))| {
            let mut rng = SplitMix64::stream(STREAM_GROUP, g);
            let owner = (g * 7) % players;
            let others: Vec<usize> = (0..players).filter(|&p| p != owner).collect();
            let size = 3 + rng.below(5);
            let members = rng.distinct(others.len(), size).into_iter().map(|i| others[i]).collect();
            GroupPlan { name: name.to_string(), join_policy: policy.to_string(), owner, members }
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
    PlayerPlan {
        email: format!("demo-{index:02}@demo.local"),
        display_name,
        characters,
        items,
        save: SavePlan { collection: "saves".to_string(), key: "slot1".to_string(), value: save_value },
    }
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

#[derive(Deserialize)]
struct CharacterRow {
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
/// Phases run in order (players, per-player rows, matches, friendships, groups), each one's items
/// as parallel tasks; a later phase skips, with a recorded cause, the items whose players failed.
pub async fn run(config: &Config) -> Result<Report> {
    let gateway = Gateway::new(&config.base_url, config.concurrency)?;
    gateway.probe().await?;
    let seed = Arc::new(plan(config.players, config.matches));

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
        if !players_seeded(&gateway, &seed, &sessions, &[index], "characters, inventory and save") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move {
            let (player, session) = (&seed.players[index], sessions[index].as_ref().expect("checked above"));
            seed_characters(&gateway, player, session).await;
            seed_inventory(&gateway, player, session).await;
            seed_save(&gateway, player, session).await;
        });
    }
    join_all(tasks).await;

    let mut tasks = JoinSet::new();
    for index in 0..seed.matches.len() {
        let planned = &seed.matches[index];
        let gateway = gateway.scenario(format!("match {}", planned.report_id));
        if !players_seeded(&gateway, &seed, &sessions, &[planned.winner, planned.loser], "the report") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move { seed_match(&gateway, &seed.matches[index], &sessions).await });
    }
    join_all(tasks).await;

    let mut tasks = JoinSet::new();
    for index in 0..seed.friendships.len() {
        let friendship = &seed.friendships[index];
        let gateway = gateway.scenario(format!(
            "friendship {} -> {}",
            seed.players[friendship.requester].email, seed.players[friendship.target].email
        ));
        if !players_seeded(&gateway, &seed, &sessions, &[friendship.requester, friendship.target], "the friendship") {
            continue;
        }
        let (seed, sessions) = (Arc::clone(&seed), Arc::clone(&sessions));
        tasks.spawn(async move {
            let friendship = &seed.friendships[index];
            let requester = sessions[friendship.requester].as_ref().expect("checked above");
            let target = sessions[friendship.target].as_ref().expect("checked above");
            seed_friendship(&gateway, requester, target).await;
        });
    }
    join_all(tasks).await;

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

    Ok(gateway.take_report())
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

/// A create or grant runs only after the list showed the row missing or the quantity short, so
/// a 409 there is a cap, ceiling or tombstone refusal, not "already exists", and is an error.
const CHARACTERS_CREATE_EXPECT: &[u16] = &[201];
const INVENTORY_GRANT_EXPECT: &[u16] = &[200];

async fn seed_player(gateway: &Gateway, player: &PlayerPlan) -> Option<Session> {
    let register = json!({"email": player.email, "password": PASSWORD, "displayName": player.display_name});
    let registered =
        gateway.call(ops::ACCOUNTS_REGISTER, &[], Caller::Anonymous, Some(&register), &[201, 409]).await.ok();
    let (status, _) = or_skip(gateway, registered, "login and profile read skipped: the registration failed")?;
    gateway.tally(|s| s.players.record(Outcome::from_status(status)));
    let login = json!({"email": player.email, "password": PASSWORD});
    let session = gateway
        .call(ops::ACCOUNTS_LOGIN, &[], Caller::Anonymous, Some(&login), &[200])
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<SessionBody>(ops::ACCOUNTS_LOGIN, body));
    let session = or_skip(gateway, session, "profile read skipped: the login failed")?;
    let (_, body) = gateway
        .call(ops::ACCOUNTS_ME, &[], Caller::Player(&session.token), None, &[200])
        .await
        .ok()?;
    let me: MeBody = gateway.typed(ops::ACCOUNTS_ME, body)?;
    Some(Session { player_id: session.player_id, token: session.token, handle: me.handle })
}

async fn seed_characters(gateway: &Gateway, player: &PlayerPlan, session: &Session) -> Option<()> {
    let caller = Caller::Player(&session.token);
    let listed = gateway
        .call(ops::CHARACTERS_LIST, &[], caller, None, &[200])
        .await
        .ok()
        .and_then(|(_, body)| gateway.typed::<Vec<CharacterRow>>(ops::CHARACTERS_LIST, body));
    let listed = or_skip(gateway, listed, "character creation skipped: the character list failed")?;
    let existing: Vec<String> = listed.into_iter().map(|c| c.name).collect();
    let missing = missing_characters(&player.characters, &existing);
    gateway.tally(|s| s.characters.existing += player.characters.len() - missing.len());
    for character in missing {
        let body = json!({"name": character.name, "class": character.class});
        if let Some((status, _)) =
            gateway.call(ops::CHARACTERS_CREATE, &[], caller, Some(&body), CHARACTERS_CREATE_EXPECT).await.ok()
        {
            gateway.tally(|s| s.characters.record(Outcome::from_status(status)));
        }
    }
    Some(())
}

async fn seed_inventory(gateway: &Gateway, player: &PlayerPlan, session: &Session) -> Option<()> {
    let caller = Caller::Player(&session.token);
    let listed = gateway
        .call(ops::INVENTORY_LIST_MINE, &[], caller, None, &[200])
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
            gateway.call(ops::INVENTORY_GRANT, &[], caller, Some(&body), INVENTORY_GRANT_EXPECT).await.ok()
        {
            gateway.tally(|s| s.grants.record(Outcome::from_status(status)));
        }
    }
    Some(())
}

async fn seed_save(gateway: &Gateway, player: &PlayerPlan, session: &Session) -> Option<()> {
    let params = [("collection", player.save.collection.as_str()), ("key", player.save.key.as_str())];
    let body = json!({"value": player.save.value, "expected_version": 0, "acl": "private"});
    let (status, _) = gateway
        .call(ops::STORAGE_PUT, &params, Caller::Player(&session.token), Some(&body), &[200, 409])
        .await
        .ok()?;
    gateway.tally(|s| s.saves.record(Outcome::from_status(status)));
    Some(())
}

async fn seed_match(gateway: &Gateway, planned: &MatchPlan, sessions: &[Option<Session>]) -> Option<()> {
    let winner = sessions[planned.winner].as_ref()?;
    let loser = sessions[planned.loser].as_ref()?;
    let body = json!({"ReportId": planned.report_id, "Winner": winner.player_id, "Loser": loser.player_id});
    let (status, _) = gateway.call(ops::MATCH_REPORT, &[], Caller::Server, Some(&body), &[202, 409]).await.ok()?;
    gateway.tally(|s| s.matches.record(Outcome::from_status(status)));
    Some(())
}

async fn seed_friendship(gateway: &Gateway, requester: &Session, target: &Session) -> Option<()> {
    let body = json!({"target_handle": target.handle});
    let friend = gateway
        .call(ops::FRIENDS_REQUEST, &[], Caller::Player(&requester.token), Some(&body), &[201])
        .await
        .ok()
        .and_then(|(_, friend)| gateway.typed::<FriendBody>(ops::FRIENDS_REQUEST, friend));
    let friend = or_skip(gateway, friend, "friend accept skipped: the friend request failed")?;
    match friend_needs_accept(&friend.state) {
        Ok(true) => {}
        Ok(false) => {
            gateway.tally(|s| s.friendships.existing += 1);
            return Some(());
        }
        Err(err) => {
            gateway.shape_error(ops::FRIENDS_REQUEST, format!("{err:#}"));
            gateway.skip("friend accept skipped: the friend request answered an unknown state");
            return None;
        }
    }
    let (status, _) = gateway
        .call(ops::FRIENDS_ACCEPT, &[("id", &friend.edge_id)], Caller::Player(&target.token), None, &[204, 409])
        .await
        .ok()?;
    gateway.tally(|s| s.friendships.record(Outcome::from_status(status)));
    Some(())
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
                .call(ops::GROUPS_CREATE, &[], Caller::Player(&owner.token), Some(&body), &[201])
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
    for &member in &group.members {
        let Some(session) = sessions[member].as_ref() else {
            gateway.skip(format!("membership skipped: the account seed failed for {}", seed.players[member].email));
            continue;
        };
        if let Some(outcome) = seed_membership(gateway, group, &group_id, owner, session).await {
            gateway.tally(|s| s.memberships.record(outcome));
        }
    }
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
                .call(ops::GROUPS_DECIDE, &id, Caller::Player(&owner.token), Some(&body), &[204, 409])
                .await
                .ok()?;
            Some(Outcome::from_status(status))
        }
        "invite" => {
            let body = json!({"target_handle": member.handle});
            let invited = gateway
                .call(ops::GROUPS_INVITE, &id, Caller::Player(&owner.token), Some(&body), &[201, 409])
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
                        .call(ops::GROUPS_RESPOND, &id, Caller::Player(&member.token), Some(&body), &[204, 409])
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
        .call(ops::GROUPS_JOIN, &[("id", group_id)], Caller::Player(&member.token), Some(&json!({})), &[200, 409])
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
            .call(ops::GROUPS_LIST_MINE, &[], Caller::Player(&session.token), Some(&body), &[200])
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

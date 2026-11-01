//! `demoseed` — fills a running fleet with deterministic demo data (players, characters,
//! inventory, matches, friends, groups, cloud saves) through the gateway's public HTTP API.
//!
//!   demoseed [--base-url <url>] [--players <n>] [--matches <n>]

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";
pub const DEFAULT_PLAYERS: usize = 30;
pub const DEFAULT_MATCHES: usize = 100;
pub const SPLIT_ADMIN_URL: &str = "http://127.0.0.1:8085/admin";

const PASSWORD: &str = "demo-password-1";
const CLIENT_KEY: &str = "dev-key-client";
// `match.report` is absent from the dev-client key's policy; only the full dev-server key may call it.
const SERVER_KEY: &str = "dev-key-server";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 20;
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Seed(Config),
}

pub const USAGE: &str = "\
demoseed — seed deterministic demo data into a running fleet through the gateway API

USAGE:
  demoseed [--base-url <url>] [--players <n>] [--matches <n>]

  --base-url <url>   gateway front door (default http://127.0.0.1:8080; split: http://127.0.0.1:8082)
  --players <n>      demo players to create (default 30)
  --matches <n>      match reports to send (default 100)

Requires a fleet started with the dev flags (devctl up sets APIKEYS_DEV_SEED, ACCOUNTS_DEV_AUTH,
INVENTORY_DEV_GRANT). Re-running converges: only missing rows are created.";

pub fn parse_args(args: &[String]) -> Result<Command> {
    let mut config = Config {
        base_url: DEFAULT_BASE_URL.to_string(),
        players: DEFAULT_PLAYERS,
        matches: DEFAULT_MATCHES,
    };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" | "help" => return Ok(Command::Help),
            "--base-url" | "--players" | "--matches" => {
                let value = iter
                    .next()
                    .ok_or_else(|| anyhow!("demoseed: {flag} requires a value\n\n{USAGE}"))?;
                match flag.as_str() {
                    "--base-url" => config.base_url = value.trim_end_matches('/').to_string(),
                    "--players" => config.players = parse_count(flag, value)?,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    AlreadyExists,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Retry,
    Fail(u16),
}

pub fn classify(status: u16) -> Result<Outcome, Rejection> {
    match status {
        200..=299 => Ok(Outcome::Created),
        409 => Ok(Outcome::AlreadyExists),
        429 => Err(Rejection::Retry),
        other => Err(Rejection::Fail(other)),
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub created: usize,
    pub existing: usize,
}

impl Tally {
    fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Created => self.created += 1,
            Outcome::AlreadyExists => self.existing += 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub players: Tally,
    pub characters: Tally,
    pub grants: Tally,
    pub saves: Tally,
    pub matches: Tally,
    pub friendships: Tally,
    pub groups: Tally,
    pub memberships: Tally,
}

impl Summary {
    pub fn render(&self, base_url: &str) -> String {
        let row = |kind: &str, tally: &Tally, created: &str, existing: &str| {
            format!("  {kind:<12} {:>4} {created:<10} {:>4} {existing}\n", tally.created, tally.existing)
        };
        let mut out = String::from("demoseed summary\n");
        out += &row("players", &self.players, "created", "already registered");
        out += &row("characters", &self.characters, "created", "already present");
        out += &row("inventory", &self.grants, "granted", "already at target");
        out += &row("saves", &self.saves, "created", "already saved");
        out += &row("matches", &self.matches, "sent", "bound to another result (409)");
        out += "  (an identical match re-send is a 202 no-op, so re-runs still count it as sent)\n";
        out += &row("friendships", &self.friendships, "accepted", "already friends");
        out += &row("groups", &self.groups, "created", "already present");
        out += &row("memberships", &self.memberships, "added", "already held");
        out += "\nAdmin panel (admin/admin):\n";
        out += &format!("  monolith  {base_url}/admin\n");
        out += &format!("  split     {SPLIT_ADMIN_URL}\n");
        out
    }
}

#[derive(Clone, Copy)]
enum Caller<'a> {
    Anonymous,
    Player(&'a str),
    Server,
}

struct Gateway {
    http: reqwest::Client,
    base: String,
}

impl Gateway {
    async fn call(&self, method: Method, path: &str, caller: Caller<'_>, body: Option<&Value>) -> Result<(Outcome, Value)> {
        let url = format!("{}{path}", self.base);
        for attempt in 0..MAX_ATTEMPTS {
            let key = match caller {
                Caller::Server => SERVER_KEY,
                Caller::Anonymous | Caller::Player(_) => CLIENT_KEY,
            };
            let mut request = self.http.request(method.clone(), &url).header("X-Api-Key", key);
            if let Caller::Player(token) = caller {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.with_context(|| {
                format!("{method} {url}: no response (is a fleet running at {}?)", self.base)
            })?;
            let status = response.status().as_u16();
            match classify(status) {
                Ok(Outcome::Created) => {
                    let text = response
                        .text()
                        .await
                        .with_context(|| format!("{method} {url}: read response body"))?;
                    let value = if text.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_str(&text)
                            .with_context(|| format!("{method} {url}: response is not JSON: {text}"))?
                    };
                    return Ok((Outcome::Created, value));
                }
                Ok(Outcome::AlreadyExists) => return Ok((Outcome::AlreadyExists, Value::Null)),
                Err(Rejection::Retry) => {
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt + 1).min(8))).await;
                }
                Err(Rejection::Fail(code)) => {
                    let text = response.text().await.unwrap_or_default();
                    bail!("{method} {url}: HTTP {code}: {}", text.chars().take(400).collect::<String>());
                }
            }
        }
        bail!("{method} {url}: still rate-limited (429) after {MAX_ATTEMPTS} attempts")
    }

    async fn read(&self, method: Method, path: &str, caller: Caller<'_>, body: Option<&Value>) -> Result<Value> {
        match self.call(method.clone(), path, caller, body).await? {
            (Outcome::Created, value) => Ok(value),
            (Outcome::AlreadyExists, _) => bail!("{method} {path}: unexpected 409 on a read"),
        }
    }
}

struct Session {
    player_id: String,
    token: String,
    handle: String,
}

fn str_field<'a>(value: &'a Value, field: &str, what: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{what}: response has no string field {field:?}: {value}"))
}

fn array<'a>(value: &'a Value, what: &str) -> Result<&'a Vec<Value>> {
    value.as_array().ok_or_else(|| anyhow!("{what}: expected a JSON array, got {value}"))
}

pub async fn run(config: &Config) -> Result<Summary> {
    let gateway = Gateway {
        http: reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("build HTTP client")?,
        base: config.base_url.clone(),
    };
    let seed = plan(config.players, config.matches);
    let mut summary = Summary::default();

    let mut sessions = Vec::with_capacity(seed.players.len());
    for player in &seed.players {
        sessions.push(seed_player(&gateway, player, &mut summary).await?);
    }
    for (player, session) in seed.players.iter().zip(&sessions) {
        seed_characters(&gateway, player, session, &mut summary).await?;
        seed_inventory(&gateway, player, session, &mut summary).await?;
        seed_save(&gateway, player, session, &mut summary).await?;
    }
    for planned in &seed.matches {
        let body = json!({
            "ReportId": planned.report_id,
            "Winner": sessions[planned.winner].player_id,
            "Loser": sessions[planned.loser].player_id,
        });
        let (outcome, _) = gateway.call(Method::POST, "/match/report", Caller::Server, Some(&body)).await?;
        summary.matches.record(outcome);
    }
    for friendship in &seed.friendships {
        seed_friendship(&gateway, &sessions[friendship.requester], &sessions[friendship.target], &mut summary).await?;
    }
    for group in &seed.groups {
        seed_group(&gateway, group, &sessions, &mut summary).await?;
    }
    Ok(summary)
}

async fn seed_player(gateway: &Gateway, player: &PlayerPlan, summary: &mut Summary) -> Result<Session> {
    let register = json!({"email": player.email, "password": PASSWORD, "displayName": player.display_name});
    let (outcome, _) = gateway
        .call(Method::POST, "/accounts/register", Caller::Anonymous, Some(&register))
        .await
        .with_context(|| format!("register {} (a 404 means the fleet runs without ACCOUNTS_DEV_AUTH)", player.email))?;
    summary.players.record(outcome);
    let login = json!({"email": player.email, "password": PASSWORD});
    let session = gateway.read(Method::POST, "/accounts/login", Caller::Anonymous, Some(&login)).await?;
    let token = str_field(&session, "token", "accounts.login")?.to_string();
    let player_id = str_field(&session, "player_id", "accounts.login")?.to_string();
    let me = gateway.read(Method::GET, "/accounts/me", Caller::Player(&token), None).await?;
    let handle = str_field(&me, "handle", "accounts.me")?.to_string();
    Ok(Session { player_id, token, handle })
}

async fn seed_characters(gateway: &Gateway, player: &PlayerPlan, session: &Session, summary: &mut Summary) -> Result<()> {
    let listed = gateway.read(Method::GET, "/characters", Caller::Player(&session.token), None).await?;
    let existing = array(&listed, "characters.list")?
        .iter()
        .map(|c| str_field(c, "name", "characters.list").map(str::to_string))
        .collect::<Result<Vec<_>>>()?;
    let missing = missing_characters(&player.characters, &existing);
    summary.characters.existing += player.characters.len() - missing.len();
    for character in missing {
        let body = json!({"name": character.name, "class": character.class});
        let (outcome, _) = gateway.call(Method::POST, "/characters", Caller::Player(&session.token), Some(&body)).await?;
        summary.characters.record(outcome);
    }
    Ok(())
}

async fn seed_inventory(gateway: &Gateway, player: &PlayerPlan, session: &Session, summary: &mut Summary) -> Result<()> {
    let listed = gateway.read(Method::GET, "/inventory/me", Caller::Player(&session.token), None).await?;
    let mut held: HashMap<String, i64> = HashMap::new();
    for holding in array(&listed, "inventory.listMine")? {
        let item_id = str_field(holding, "item_id", "inventory.listMine")?;
        let quantity = holding
            .get("quantity")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("inventory.listMine: holding has no integer quantity: {holding}"))?;
        *held.entry(item_id.to_string()).or_default() += quantity;
    }
    for target in &player.items {
        let shortfall = inventory_shortfall(target.qty, held.get(&target.item_id).copied().unwrap_or(0));
        if shortfall == 0 {
            summary.grants.existing += 1;
            continue;
        }
        let body = json!({"item_id": target.item_id, "qty": shortfall});
        let (outcome, _) = gateway
            .call(Method::POST, "/inventory/me/grant", Caller::Player(&session.token), Some(&body))
            .await
            .with_context(|| format!("grant {} (a 404 means the fleet runs without INVENTORY_DEV_GRANT)", target.item_id))?;
        summary.grants.record(outcome);
    }
    Ok(())
}

async fn seed_save(gateway: &Gateway, player: &PlayerPlan, session: &Session, summary: &mut Summary) -> Result<()> {
    let path = format!("/storage/{}/{}", player.save.collection, player.save.key);
    let body = json!({"value": player.save.value, "expected_version": 0, "acl": "private"});
    let (outcome, _) = gateway.call(Method::POST, &path, Caller::Player(&session.token), Some(&body)).await?;
    summary.saves.record(outcome);
    Ok(())
}

async fn seed_friendship(gateway: &Gateway, requester: &Session, target: &Session, summary: &mut Summary) -> Result<()> {
    let body = json!({"target_handle": target.handle});
    let (_, friend) = gateway
        .call(Method::POST, "/friends/requests", Caller::Player(&requester.token), Some(&body))
        .await?;
    let state = str_field(&friend, "state", "friends.request")?;
    if !friend_needs_accept(state)? {
        summary.friendships.existing += 1;
        return Ok(());
    }
    let edge_id = str_field(&friend, "edge_id", "friends.request")?;
    let (outcome, _) = gateway
        .call(Method::POST, &format!("/friends/requests/{edge_id}/accept"), Caller::Player(&target.token), None)
        .await?;
    summary.friendships.record(outcome);
    Ok(())
}

async fn seed_group(gateway: &Gateway, group: &GroupPlan, sessions: &[Session], summary: &mut Summary) -> Result<()> {
    let owner = &sessions[group.owner];
    let group_id = match find_group(gateway, owner, &group.name).await? {
        Some(id) => {
            summary.groups.existing += 1;
            id
        }
        None => {
            let body = json!({"name": group.name, "join_policy": group.join_policy});
            let created = gateway.read(Method::POST, "/groups", Caller::Player(&owner.token), Some(&body)).await?;
            summary.groups.created += 1;
            str_field(&created, "id", "groups.create")?.to_string()
        }
    };
    for &member in &group.members {
        let member = &sessions[member];
        let outcome = match group.join_policy.as_str() {
            "open" => join(gateway, &group_id, member).await?,
            "request" => {
                let joined = join(gateway, &group_id, member).await?;
                if joined == Outcome::Created {
                    let body = json!({"subject_id": member.player_id, "decision": "accept"});
                    gateway
                        .call(Method::POST, &format!("/groups/{group_id}/decide"), Caller::Player(&owner.token), Some(&body))
                        .await?;
                }
                joined
            }
            "invite" => {
                let body = json!({"target_handle": member.handle});
                let (invited, _) = gateway
                    .call(Method::POST, &format!("/groups/{group_id}/invites"), Caller::Player(&owner.token), Some(&body))
                    .await?;
                if invited == Outcome::Created {
                    let body = json!({"decision": "accept"});
                    gateway
                        .call(Method::POST, &format!("/groups/{group_id}/respond"), Caller::Player(&member.token), Some(&body))
                        .await?;
                }
                invited
            }
            other => bail!("group {:?}: unknown join policy {other:?}", group.name),
        };
        summary.memberships.record(outcome);
    }
    Ok(())
}

async fn join(gateway: &Gateway, group_id: &str, member: &Session) -> Result<Outcome> {
    let (outcome, _) = gateway
        .call(Method::POST, &format!("/groups/{group_id}/join"), Caller::Player(&member.token), Some(&json!({})))
        .await?;
    Ok(outcome)
}

async fn find_group(gateway: &Gateway, owner: &Session, name: &str) -> Result<Option<String>> {
    let mut cursor = String::new();
    for _ in 0..MAX_PAGES {
        let body = json!({"cursor": cursor, "limit": PAGE_LIMIT});
        let page = gateway.read(Method::POST, "/groups/list", Caller::Player(&owner.token), Some(&body)).await?;
        let items = page
            .get("items")
            .ok_or_else(|| anyhow!("groups.listMine: response has no items: {page}"))?;
        for item in array(items, "groups.listMine")? {
            if str_field(item, "name", "groups.listMine")? == name {
                return Ok(Some(str_field(item, "id", "groups.listMine")?.to_string()));
            }
        }
        cursor = str_field(&page, "next_cursor", "groups.listMine")?.to_string();
        if cursor.is_empty() {
            return Ok(None);
        }
    }
    bail!("groups.listMine: more than {MAX_PAGES} pages for one player")
}

#[cfg(test)]
mod tests;

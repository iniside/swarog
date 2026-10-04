//! party tests — shared fixtures for the sibling `service_tests`/`race_tests`/`sweep_tests`/
//! `purge_tests`, plus the DB-free tests (the `PARTY_LEASE_SECS` bounds, the conformance
//! probes, `sweep::Liveness` on a paused clock) and the module wiring tests.
//!
//! The suite drives the REAL `Service` over the local Postgres (`testdb::test_pool`, which
//! FAILS the run when the cluster is unreachable) with a fake `accountsapi::Directory` and a
//! recording `push::Sink`. That sink also PROBES: every push it receives is checked, on an
//! independent connection, against the committed database state, so every test that pushes
//! also proves the push followed the commit (`Fixture::run` asserts no violation).
//!
//! Concurrency is staged, never slept: a racer is a `Service` on its OWN pool whose
//! connections carry a unique `application_name`, so a test waits on `pg_stat_activity`
//! (`wait_event_type = 'Lock'`) until the racer is parked on the lock the test holds, then
//! releases it.

use super::*;

use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use accountsapi::PlayerSummary;
use opsapi::{Error, Identity, Status};
use partyapi::{InviteView, PartyView};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};

pub(crate) use testdb::test_pool;

/// Serializes every test that expires a lease or an invite, runs a whole sweep pass (which
/// visits every expired party in the shared database) or starts the module's sweep loop, so
/// no other test's expiry can be swept out from under it.
pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serializes the tests that read or write `PARTY_LEASE_SECS` (process-global).
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static SCHEMA_READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Migrates the durable plane and the party schema exactly once per test binary.
pub(crate) async fn ensure_schema(pool: &PgPool) {
    SCHEMA_READY
        .get_or_init(|| async {
            asyncevents::Plane::new(pool.clone(), testdb::dsn())
                .unwrap()
                .migrate()
                .await
                .unwrap();
            Party::new()
                .migrate(&Context::with_db(pool.clone()))
                .await
                .unwrap();
        })
        .await;
}

static SEQ: AtomicU64 = AtomicU64::new(1);

fn next_uuid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    format!(
        "{:08x}-{:04x}-4000-8000-{:012x}",
        nanos as u32,
        n as u16,
        (nanos >> 20) & 0xffff_ffff_ffff
    )
}

// ---------------------------------------------------------------------------------------
// Fake Directory
// ---------------------------------------------------------------------------------------

/// An in-memory `accountsapi::Directory` (never the accounts impl — fortress rule) with
/// switchable failures and call counters.
pub(crate) struct FakeDirectory {
    players: Mutex<HashMap<String, PlayerSummary>>,
    pub(crate) fail_lookup: AtomicBool,
    pub(crate) fail_handle: AtomicBool,
    pub(crate) by_id_calls: AtomicUsize,
    pub(crate) by_handle_calls: AtomicUsize,
}

impl FakeDirectory {
    pub(crate) fn new() -> FakeDirectory {
        FakeDirectory {
            players: Mutex::new(HashMap::new()),
            fail_lookup: AtomicBool::new(false),
            fail_handle: AtomicBool::new(false),
            by_id_calls: AtomicUsize::new(0),
            by_handle_calls: AtomicUsize::new(0),
        }
    }

    pub(crate) fn insert(&self, id: &str, handle: &str) {
        self.players.lock().unwrap().insert(
            id.to_string(),
            PlayerSummary {
                player_id: id.to_string(),
                display_name: handle.to_string(),
                handle: handle.to_string(),
                online_until: String::new(),
            },
        );
    }

    pub(crate) fn handle_of(&self, id: &str) -> String {
        self.players.lock().unwrap().get(id).unwrap().handle.clone()
    }

    pub(crate) fn by_id(&self) -> usize {
        self.by_id_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn by_handle(&self) -> usize {
        self.by_handle_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Directory for FakeDirectory {
    async fn players_by_id(&self, ids: Vec<String>) -> Result<Vec<PlayerSummary>, Error> {
        self.by_id_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_lookup.load(Ordering::SeqCst) {
            return Err(Error::internal("fake directory down"));
        }
        let players = self.players.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| {
                players
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(id))
                    .map(|(_, v)| v.clone())
            })
            .collect())
    }

    async fn find_by_handle(&self, handle: String) -> Result<Option<PlayerSummary>, Error> {
        self.by_handle_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_handle.load(Ordering::SeqCst) {
            return Err(Error::internal("fake directory down"));
        }
        let players = self.players.lock().unwrap();
        Ok(players
            .values()
            .find(|p| p.handle.eq_ignore_ascii_case(&handle))
            .cloned())
    }
}

// ---------------------------------------------------------------------------------------
// Recording + probing push sink
// ---------------------------------------------------------------------------------------

pub(crate) struct Sent {
    pub(crate) player: String,
    pub(crate) topic: String,
    pub(crate) payload: Vec<u8>,
}

/// A counting `push::Sink`. With a `probe` pool it also checks, at the moment of each push
/// and on a separate connection, that the pushed revision is already COMMITTED — a push sent
/// before its transaction committed reads the previous state and is recorded as a violation.
/// The probe blocks the pushing worker (`block_in_place`), so a probing sink needs a
/// multi-thread runtime.
pub(crate) struct RecordingSink {
    sent: Mutex<Vec<Sent>>,
    probe: Option<PgPool>,
    violations: Mutex<Vec<String>>,
}

impl RecordingSink {
    pub(crate) fn new(probe: Option<PgPool>) -> RecordingSink {
        RecordingSink {
            sent: Mutex::new(Vec::new()),
            probe,
            violations: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn total(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    pub(crate) fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }

    pub(crate) fn violations(&self) -> Vec<String> {
        self.violations.lock().unwrap().clone()
    }

    /// Every `party.state` push to `player`, in delivery order.
    pub(crate) fn states_to(&self, player: &str) -> Vec<PartyView> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.player == player && s.topic == partyapi::PUSH_STATE_TOPIC)
            .map(|s| serde_json::from_slice(&s.payload).unwrap())
            .collect()
    }

    pub(crate) fn invites_to(&self, player: &str) -> Vec<InviteView> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.player == player && s.topic == partyapi::PUSH_INVITE_TOPIC)
            .map(|s| serde_json::from_slice(&s.payload).unwrap())
            .collect()
    }
}

impl push::Sink for RecordingSink {
    fn send(
        &self,
        target: &push::Target,
        msg: &push::Message,
    ) -> Result<push::Delivered, push::Error> {
        let push::Target::Player(player) = target else {
            self.violations
                .lock()
                .unwrap()
                .push(format!("party pushed to a non-player target {target:?}"));
            return Ok(push::Delivered::Local(0));
        };
        self.sent.lock().unwrap().push(Sent {
            player: player.clone(),
            topic: msg.topic.clone(),
            payload: msg.payload.clone(),
        });
        if let Some(pool) = &self.probe {
            let verdict = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(committed_before_push(pool, player, msg))
            });
            if let Some(violation) = verdict {
                self.violations.lock().unwrap().push(violation);
            }
        }
        Ok(push::Delivered::Local(1))
    }
}

async fn committed_before_push(pool: &PgPool, player: &str, msg: &push::Message) -> Option<String> {
    match msg.topic.as_str() {
        t if t == partyapi::PUSH_STATE_TOPIC => {
            let view: PartyView = match serde_json::from_slice(&msg.payload) {
                Ok(v) => v,
                Err(e) => return Some(format!("undecodable state push: {e}")),
            };
            if view.party_id.is_empty() {
                let committed: Option<i64> = sqlx::query_scalar(
                    "SELECT revision FROM party.player_revisions WHERE player_id = $1::uuid",
                )
                .bind(player)
                .fetch_optional(pool)
                .await
                .unwrap();
                match committed {
                    Some(r) if r >= view.revision => None,
                    other => Some(format!(
                        "no-party push to {player} at revision {} but committed player revision is {other:?}",
                        view.revision
                    )),
                }
            } else {
                let committed: Option<i64> =
                    sqlx::query_scalar("SELECT revision FROM party.parties WHERE id = $1::uuid")
                        .bind(&view.party_id)
                        .fetch_optional(pool)
                        .await
                        .unwrap();
                match committed {
                    Some(r) if r < view.revision => Some(format!(
                        "state push of party {} at revision {} but committed revision is {r}",
                        view.party_id, view.revision
                    )),
                    _ => None,
                }
            }
        }
        t if t == partyapi::PUSH_INVITE_TOPIC => {
            let view: InviteView = match serde_json::from_slice(&msg.payload) {
                Ok(v) => v,
                Err(e) => return Some(format!("undecodable invite push: {e}")),
            };
            let found: Option<i32> =
                sqlx::query_scalar("SELECT 1 FROM party.invites WHERE id = $1::uuid")
                    .bind(&view.invite_id)
                    .fetch_optional(pool)
                    .await
                    .unwrap();
            found
                .is_none()
                .then(|| format!("invite push for {} before its row committed", view.invite_id))
        }
        other => Some(format!("unexpected push topic {other}")),
    }
}

// ---------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Fixture {
    pub(crate) pool: PgPool,
    pub(crate) dir: Arc<FakeDirectory>,
    pub(crate) sink: Arc<RecordingSink>,
    pub(crate) push: Arc<push::Push>,
    pub(crate) svc: Arc<Service>,
    created: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    /// A fixture whose sink probes the database — multi-thread runtime only.
    pub(crate) async fn new(pool: &PgPool) -> Fixture {
        Fixture::build(pool, true).await
    }

    /// A fixture whose sink only records — for a current-thread (paused-clock) test.
    pub(crate) async fn quiet(pool: &PgPool) -> Fixture {
        Fixture::build(pool, false).await
    }

    async fn build(pool: &PgPool, probe: bool) -> Fixture {
        ensure_schema(pool).await;
        let dir = Arc::new(FakeDirectory::new());
        let sink = Arc::new(RecordingSink::new(probe.then(|| pool.clone())));
        let push = Arc::new(push::Push::new());
        push.install(sink.clone());
        let svc = Arc::new(Service::new(pool.clone(), push.clone(), DEFAULT_LEASE_SECS));
        svc.directory
            .set(dir.clone() as Arc<dyn Directory>)
            .ok()
            .expect("directory set once");
        Fixture {
            pool: pool.clone(),
            dir,
            sink,
            push,
            svc,
            created: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A fresh player known to the fake directory, cleaned up when the test ends.
    pub(crate) fn player(&self) -> String {
        let id = next_uuid();
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        self.dir.insert(&id, &format!("T{n}#{:04}", n % 10_000));
        self.created.lock().unwrap().push(id.clone());
        id
    }

    pub(crate) fn players<const N: usize>(&self) -> [String; N] {
        std::array::from_fn(|_| self.player())
    }

    pub(crate) fn handle(&self, id: &str) -> String {
        self.dir.handle_of(id)
    }

    /// A `Service` over `pool` sharing this fixture's directory and sink.
    pub(crate) fn service_on(&self, pool: PgPool) -> Arc<Service> {
        let svc = Arc::new(Service::new(pool, self.push.clone(), DEFAULT_LEASE_SECS));
        svc.directory
            .set(self.dir.clone() as Arc<dyn Directory>)
            .ok()
            .expect("directory set once");
        svc
    }

    /// A racer on its own tagged pool of `max_conns` connections.
    pub(crate) fn racer(&self, max_conns: u32) -> Racer {
        self.racer_with(max_conns, None)
    }

    pub(crate) fn racer_with(&self, max_conns: u32, gate: Option<Arc<Gate>>) -> Racer {
        let app = format!("party-test-{}", next_uuid());
        let pool = tagged_pool(&app, max_conns, gate);
        Racer {
            svc: self.service_on(pool),
            app,
        }
    }

    /// Runs `body` on its own task and cleans up afterwards even when it panicked, so a red
    /// run never leaves a party behind; then asserts no push preceded its commit.
    pub(crate) async fn run<F, Fut>(self, body: F)
    where
        F: FnOnce(Fixture) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let outcome = tokio::spawn(body(self.clone())).await;
        self.cleanup().await;
        let violations = self.sink.violations();
        if let Err(e) = outcome {
            if !violations.is_empty() {
                eprintln!("pushes sent before their mutation committed: {violations:?}");
            }
            if e.is_panic() {
                std::panic::resume_unwind(e.into_panic());
            }
            panic!("party test task ended without completing: {e}");
        }
        assert!(
            violations.is_empty(),
            "a push was sent before its mutation committed: {violations:?}"
        );
    }

    async fn cleanup(&self) {
        let players = self.created.lock().unwrap().clone();
        if players.is_empty() {
            return;
        }
        let parties: Vec<String> = sqlx::query_scalar(
            "SELECT party_id::text FROM party.members WHERE player_id = ANY($1::uuid[]) \
             UNION SELECT id::text FROM party.parties WHERE leader_id = ANY($1::uuid[])",
        )
        .bind(&players)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let _ = sqlx::query(
            "DELETE FROM party.invites WHERE invitee_id = ANY($1::uuid[]) \
                OR inviter_id = ANY($1::uuid[]) OR party_id = ANY($2::uuid[])",
        )
        .bind(&players)
        .bind(&parties)
        .execute(&self.pool)
        .await;
        let _ = sqlx::query(
            "DELETE FROM party.members WHERE player_id = ANY($1::uuid[]) \
                OR party_id = ANY($2::uuid[])",
        )
        .bind(&players)
        .bind(&parties)
        .execute(&self.pool)
        .await;
        let _ = sqlx::query("DELETE FROM party.parties WHERE id = ANY($1::uuid[])")
            .bind(&parties)
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("DELETE FROM party.player_revisions WHERE player_id = ANY($1::uuid[])")
            .bind(&players)
            .execute(&self.pool)
            .await;
    }
}

/// A `Service` on its own pool; `app` is the `application_name` its connections report.
pub(crate) struct Racer {
    pub(crate) svc: Arc<Service>,
    pub(crate) app: String,
}

/// Pauses the FIRST idle-connection checkout of a pool: signals `reached`, then waits for `go`.
pub(crate) struct Gate {
    calls: AtomicUsize,
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) go: tokio::sync::Notify,
}

impl Gate {
    pub(crate) fn new() -> Arc<Gate> {
        Arc::new(Gate {
            calls: AtomicUsize::new(0),
            reached: tokio::sync::Notify::new(),
            go: tokio::sync::Notify::new(),
        })
    }
}

fn tagged_pool(app: &str, max_conns: u32, gate: Option<Arc<Gate>>) -> PgPool {
    let options = PgConnectOptions::from_str(&testdb::dsn())
        .expect("DATABASE_URL parses")
        .application_name(app);
    let mut pool_options = PgPoolOptions::new()
        .max_connections(max_conns)
        .acquire_timeout(Duration::from_secs(60));
    if let Some(gate) = gate {
        pool_options = pool_options.before_acquire(move |_conn, _meta| {
            let gate = gate.clone();
            Box::pin(async move {
                if gate.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    gate.reached.notify_one();
                    gate.go.notified().await;
                }
                Ok(true)
            })
        });
    }
    pool_options.connect_lazy_with(options)
}

/// Polls until a backend of `app` is parked on a heavyweight lock (an advisory lock, a
/// relation lock, or another transaction's uncommitted row). The wait is on observed
/// database state with a hang guard, never on elapsed time.
pub(crate) async fn wait_blocked(pool: &PgPool, app: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
              WHERE application_name = $1 AND wait_event_type = 'Lock'",
        )
        .bind(app)
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "racer {app} never parked on a lock"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------------------
// Held locks
// ---------------------------------------------------------------------------------------

/// A transaction holding `party_id`'s advisory lock: every production op on that party parks
/// on it until the returned transaction commits or rolls back.
pub(crate) async fn hold_party_lock(fx: &Fixture, party_id: &str) -> Transaction<'static, Postgres> {
    let mut tx = fx.pool.begin().await.unwrap();
    fx.svc.store.lock_party_tx(&mut tx, party_id).await.unwrap();
    tx
}

/// A transaction holding `ACCESS EXCLUSIVE` on `party.parties`: a leave that already DELETEd
/// its member row parks at the leader-pick SELECT, BEFORE it draws its revision.
pub(crate) async fn hold_parties_table(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE party.parties IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

// ---------------------------------------------------------------------------------------
// Op + state helpers
// ---------------------------------------------------------------------------------------

pub(crate) fn who(player: &str) -> Identity {
    Identity::player(player)
}

pub(crate) async fn create_party(fx: &Fixture, leader: &str) -> String {
    fx.svc.create(who(leader)).await.unwrap().party_id
}

/// `leader` invites `member` by handle; returns the invite id.
pub(crate) async fn invite_id(fx: &Fixture, leader: &str, member: &str) -> String {
    fx.svc
        .invite(who(leader), fx.handle(member))
        .await
        .unwrap()
        .invite_id
}

/// `leader` invites `member`, who accepts; returns the member's resulting view.
pub(crate) async fn join(fx: &Fixture, leader: &str, member: &str) -> PartyView {
    let invite = invite_id(fx, leader, member).await;
    fx.svc.accept(who(member), invite).await.unwrap()
}

pub(crate) async fn party_of(pool: &PgPool, player: &str) -> Option<String> {
    sqlx::query_scalar("SELECT party_id::text FROM party.members WHERE player_id = $1::uuid")
        .bind(player)
        .fetch_optional(pool)
        .await
        .unwrap()
}

pub(crate) async fn members_of(pool: &PgPool, party_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT player_id::text FROM party.members WHERE party_id = $1::uuid \
          ORDER BY joined_at, player_id",
    )
    .bind(party_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `(revision, leader_id)` of the party row, `None` once it is gone.
pub(crate) async fn party_row(pool: &PgPool, party_id: &str) -> Option<(i64, String)> {
    sqlx::query_as("SELECT revision, leader_id::text FROM party.parties WHERE id = $1::uuid")
        .bind(party_id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

pub(crate) async fn player_rev(pool: &PgPool, player: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT revision FROM party.player_revisions WHERE player_id = $1::uuid")
        .bind(player)
        .fetch_optional(pool)
        .await
        .unwrap()
}

pub(crate) async fn invite_exists(pool: &PgPool, invite_id: &str) -> bool {
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM party.invites WHERE id = $1::uuid")
        .bind(invite_id)
        .fetch_optional(pool)
        .await
        .unwrap();
    found.is_some()
}

pub(crate) async fn invite_count_for_party(pool: &PgPool, party_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM party.invites WHERE party_id = $1::uuid")
        .bind(party_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub(crate) async fn expire_lease(pool: &PgPool, player: &str) {
    sqlx::query(
        "UPDATE party.members SET lease_until = now() - interval '1 minute' \
          WHERE player_id = $1::uuid",
    )
    .bind(player)
    .execute(pool)
    .await
    .unwrap();
}

pub(crate) async fn expire_invite(pool: &PgPool, invite_id: &str) {
    sqlx::query("UPDATE party.invites SET expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(invite_id)
        .execute(pool)
        .await
        .unwrap();
}

/// Seconds until the player's lease expires, by the database clock.
pub(crate) async fn lease_remaining(pool: &PgPool, player: &str) -> f64 {
    sqlx::query_scalar(
        "SELECT extract(epoch FROM (lease_until - now()))::float8 FROM party.members \
          WHERE player_id = $1::uuid",
    )
    .bind(player)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub(crate) async fn set_lease_near_expiry(pool: &PgPool, player: &str) {
    sqlx::query(
        "UPDATE party.members SET lease_until = now() + interval '5 seconds' \
          WHERE player_id = $1::uuid",
    )
    .bind(player)
    .execute(pool)
    .await
    .unwrap();
}

pub(crate) fn is_no_party(view: &PartyView) -> bool {
    view.party_id.is_empty() && view.leader_id.is_empty() && view.members.is_empty()
}

/// The client rule of decision 11: a snapshot applies only if its revision is at least the
/// high-water mark. Returns the view the client ends on.
pub(crate) fn apply_client_rule(snapshots: impl IntoIterator<Item = PartyView>) -> Option<PartyView> {
    let mut hw = 0;
    let mut current = None;
    for snapshot in snapshots {
        if snapshot.revision >= hw {
            hw = snapshot.revision;
            current = Some(snapshot);
        }
    }
    current
}

pub(crate) fn assert_status<T: std::fmt::Debug>(got: Result<T, Error>, want: Status, what: &str) {
    match got {
        Err(e) if e.status == want => {}
        other => panic!("{what}: expected {want:?}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// PARTY_LEASE_SECS bounds
// ---------------------------------------------------------------------------------------

pub(crate) struct EnvGuard(Option<std::ffi::OsString>);

impl EnvGuard {
    pub(crate) fn take() -> EnvGuard {
        EnvGuard(std::env::var_os(LEASE_ENV))
    }
    pub(crate) fn set(&self, value: impl AsRef<std::ffi::OsStr>) {
        std::env::set_var(LEASE_ENV, value);
    }
    pub(crate) fn unset(&self) {
        std::env::remove_var(LEASE_ENV);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var(LEASE_ENV, v),
            None => std::env::remove_var(LEASE_ENV),
        }
    }
}

/// Once-wrong branch: only an UNSET variable takes the default. `PARTY_LEASE_SECS=${X}` with
/// `X` unset expands to empty, so blank must fail startup rather than silently become 60.
#[tokio::test]
async fn lease_env_unset_is_the_default_and_blank_fails() {
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.unset();
    assert_eq!(lease_secs_from_env().unwrap(), 60);
    for blank in ["", "   ", "\t"] {
        guard.set(blank);
        let err = lease_secs_from_env().unwrap_err().to_string();
        assert!(err.contains(LEASE_ENV), "blank {blank:?} must fail naming the variable: {err}");
    }
}

/// Once-wrong branch: the range check `10..=600` (off-by-one on either edge) and the parse.
#[tokio::test]
async fn lease_env_enforces_the_range_and_the_parse() {
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    for bad in ["9", "601", "0", "-1", "abc", "30.5", "30s"] {
        guard.set(bad);
        let err = lease_secs_from_env().unwrap_err().to_string();
        assert!(err.contains(LEASE_ENV), "{bad:?} must fail naming the variable: {err}");
    }
    for (raw, want) in [("10", 10), ("600", 600), (" 45 ", 45)] {
        guard.set(raw);
        assert_eq!(lease_secs_from_env().unwrap(), want, "{raw:?} is usable");
    }
}

/// Once-wrong branch: the `VarError::NotUnicode` arm, which a naive `unwrap_or(default)` would
/// swallow into the default.
#[cfg(unix)]
#[tokio::test]
async fn lease_env_non_unicode_fails() {
    use std::os::unix::ffi::OsStrExt;
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.set(std::ffi::OsStr::from_bytes(&[0x66, 0xff, 0x6f]));
    let err = lease_secs_from_env().unwrap_err().to_string();
    assert!(err.contains("not valid unicode"), "got {err}");
}

/// Once-wrong branch: a bad value must fail `register` (startup), not first use; and a good
/// value must reach the `Service` the module provides (the lease the rows actually carry).
#[tokio::test(flavor = "multi_thread")]
async fn register_fails_on_a_bad_lease_and_threads_a_good_one_into_the_service() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();

    guard.set("601");
    let lazy = sqlx::PgPool::connect_lazy(&testdb::dsn()).unwrap();
    let ctx = Context::with_db(lazy);
    let err = Party::new().register(&ctx).expect_err("601 must fail startup");
    assert!(err.to_string().contains(LEASE_ENV), "got {err}");

    guard.set("30");
    let fx = Fixture::new(&pool).await;
    fx.run(move |fx| async move {
        let ctx = Context::with_db(fx.pool.clone());
        let module = Party::new();
        module.register(&ctx).expect("a usable lease registers");
        let svc = module.svc();
        svc.directory
            .set(fx.dir.clone() as Arc<dyn Directory>)
            .ok()
            .expect("directory set once");
        let p = fx.player();
        svc.create(who(&p)).await.unwrap();
        let remaining = lease_remaining(&fx.pool, &p).await;
        assert!(
            (20.0..=31.0).contains(&remaining),
            "PARTY_LEASE_SECS=30 must be the lease the service stamps, got {remaining}s"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------------------
// Module wiring
// ---------------------------------------------------------------------------------------

/// `init` contributes the 11 ops, the edge registration and a "party" readiness check that
/// reads ready before `start` (dark until the sweep is seeded), and `start`/`stop` is
/// bounded and keeps readiness green (a controlled stop is not a stalled sweep).
#[tokio::test(flavor = "multi_thread")]
async fn module_init_contributes_ops_edge_and_readiness_and_stop_is_controlled() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.unset();
    ensure_schema(&pool).await;

    let transport = asyncevents::testing::transport(pool.clone());
    let ctx = Context::with_db_and_transport(pool.clone(), transport.handle());
    let dir = Arc::new(FakeDirectory::new());
    ctx.registry()
        .provide::<dyn Directory>(registry::key("accounts", "directory"), dir.clone() as Arc<dyn Directory>);
    let module = Party::new();
    assert_eq!(module.requires(), vec!["accounts".to_string()]);
    module.register(&ctx).unwrap();
    module.init(&ctx).unwrap();

    assert_eq!(ctx.contributions(opsapi::SLOT).len(), 11, "one op per Player method");
    assert_eq!(ctx.contributions(edge::EDGE_SLOT).len(), 1, "edge face contributed unconditionally");
    let checks = ctx.contributions(httpmw::READINESS_SLOT);
    let party_check = checks.iter().find(|c| c.name() == "party").expect("party readiness check");
    party_check.run().await.expect("an unstarted sweep reads ready");

    module.start(&ctx).await.unwrap();
    party_check.run().await.expect("a started, healthy sweep reads ready");
    tokio::time::timeout(Duration::from_secs(5), module.stop(&ctx))
        .await
        .expect("stop resolves inside the app's 5s grace")
        .unwrap();
    party_check.run().await.expect("a controlled stop never reads as a stall");
}

// ---------------------------------------------------------------------------------------
// Conformance fixtures execute their failing branch
// ---------------------------------------------------------------------------------------

/// Once-wrong branch: the `target_handle` byte cap in `Player::invite` itself. Over the cap
/// answers `Invalid`; AT the cap passes the cap and reaches the dead store (`Internal`), so
/// the fixture cannot be satisfied by rejecting everything.
#[test]
fn conformance_target_handle_cap_rejects_only_over_the_cap() {
    assert!(conformance::conformance_target_handle_rejected(
        accountsapi::MAX_HANDLE_BYTES + 1
    ));
    assert!(!conformance::conformance_target_handle_rejected(accountsapi::MAX_HANDLE_BYTES));
    assert!(!conformance::conformance_target_handle_rejected(1));
}

/// Once-wrong branch: a Directory outage on the invite target is `Unavailable` (503), not
/// `Internal` and not `NotFound` — against a Directory that answers a clean miss, the same
/// production step answers `NotFound`.
#[tokio::test]
async fn conformance_directory_outage_is_unavailable_while_a_miss_is_not_found() {
    let err = conformance::conformance_directory_outage()
        .await
        .expect_err("the failing directory must fail the step");
    assert_eq!(err.status, Status::Unavailable);

    let svc = Service::new(
        sqlx::PgPool::connect_lazy(&testdb::dsn()).unwrap(),
        Arc::new(push::Push::new()),
        DEFAULT_LEASE_SECS,
    );
    svc.directory
        .set(Arc::new(FakeDirectory::new()) as Arc<dyn Directory>)
        .ok()
        .unwrap();
    let miss = svc.resolve_target("Nobody#0000".to_string()).await.unwrap_err();
    assert_eq!(miss.status, Status::NotFound);
}

// ---------------------------------------------------------------------------------------
// sweep::Liveness (paused clock)
// ---------------------------------------------------------------------------------------

fn lazy_service() -> Arc<Service> {
    Arc::new(Service::new(
        sqlx::PgPool::connect_lazy(&testdb::dsn()).unwrap(),
        Arc::new(push::Push::new()),
        DEFAULT_LEASE_SECS,
    ))
}

/// Once-wrong branch: an unstarted sweep must read ready (`last == 0` is "never seeded"), or
/// a cold boot is red before `start` runs.
#[tokio::test(start_paused = true)]
async fn liveness_unseeded_never_reads_stalled() {
    let liveness = sweep::Liveness::default();
    tokio::time::advance(Duration::from_secs(100_000)).await;
    liveness.check(sweep::STALL_MAX).expect("never seeded is ready");
}

/// Once-wrong branch: the stall comparison `> stall_max` — exactly `STALL_MAX` since the last
/// healthy stamp is still ready, one second more is red, and a fresh stamp restores it.
#[tokio::test(start_paused = true)]
async fn liveness_goes_red_only_after_stall_max_without_a_healthy_pass() {
    assert!(sweep::STALL_MAX > sweep::SWEEP_INTERVAL + sweep::SWEEP_PASS_BUDGET);
    let liveness = sweep::Liveness::default();
    tokio::time::advance(Duration::from_secs(1)).await;
    liveness.mark_pass_ok();
    tokio::time::advance(sweep::STALL_MAX).await;
    liveness.check(sweep::STALL_MAX).expect("exactly STALL_MAX is not yet stale");
    tokio::time::advance(Duration::from_secs(1)).await;
    let err = liveness.check(sweep::STALL_MAX).expect_err("past STALL_MAX is red");
    assert!(err.contains("no healthy party sweep pass"), "got {err}");
    liveness.mark_pass_ok();
    liveness.check(sweep::STALL_MAX).expect("a healthy pass restores readiness");
}

/// Once-wrong branch: `set_stopping` suppresses the stall verdict, so a process that is
/// shutting down never flips `/readyz` red because its sweep stopped stamping.
#[tokio::test(start_paused = true)]
async fn liveness_stopping_suppresses_the_stall_verdict() {
    let liveness = sweep::Liveness::default();
    tokio::time::advance(Duration::from_secs(1)).await;
    liveness.mark_pass_ok();
    tokio::time::advance(sweep::STALL_MAX + Duration::from_secs(5)).await;
    liveness.check(sweep::STALL_MAX).expect_err("stale before stopping");
    liveness.set_stopping();
    liveness.check(sweep::STALL_MAX).expect("stopping is never stalled");
}

/// Once-wrong branch: the supervision wrapper's `!stopping` guard. A controlled stop
/// (`set_stopping` BEFORE the signal) is not a dead sweep; the identical exit WITHOUT
/// `set_stopping` is, so the guard is what separates the two.
#[tokio::test]
async fn controlled_stop_is_not_dead_but_an_uncontrolled_exit_is() {
    let controlled = sweep::Liveness::default();
    let (tx, rx) = tokio::sync::watch::channel(false);
    controlled.set_stopping();
    tx.send(true).unwrap();
    sweep::spawn(lazy_service(), controlled.clone(), rx).await.unwrap();
    controlled.check(sweep::STALL_MAX).expect("a controlled stop is not dead");

    let uncontrolled = sweep::Liveness::default();
    let (tx, rx) = tokio::sync::watch::channel(false);
    tx.send(true).unwrap();
    sweep::spawn(lazy_service(), uncontrolled.clone(), rx).await.unwrap();
    let err = uncontrolled
        .check(sweep::STALL_MAX)
        .expect_err("a loop that exits while the module runs is dead");
    assert!(err.contains("died"), "got {err}");
}

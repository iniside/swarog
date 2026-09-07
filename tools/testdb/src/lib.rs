//! `testdb` — the ONE authority deciding whether a database test may be skipped.
//!
//! Every crate with live-Postgres tests dev-depends on this crate and obtains its pool
//! from [`test_pool`]. The default is STRICT: an unreachable cluster PANICS, so a green
//! `cargo test --workspace` can never mean "the DB tests silently did not run". The only
//! way to skip is [`SKIP_ENV`], explicitly truthy.
//!
//! The per-skip warning below is NOT the safety net — libtest captures a passing test's
//! output, so it is invisible in a default run. What keeps the opt-out from relocating
//! the false green into an env var is `verifyctl`'s `test` stage, which reads
//! [`skip_allowed`] and REFUSES to run while it is on.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use sqlx::PgPool;

pub mod drift;

/// Fallback DSN when `DATABASE_URL` is unset — the workspace default.
pub const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The one spelling of the opt-out. Unset (or any non-truthy value) means STRICT.
pub const SKIP_ENV: &str = "TESTDB_ALLOW_SKIP";

/// How long a connect may take before the suite decides Postgres is not there.
pub const CONNECT_BOUND: Duration = Duration::from_secs(3);

/// The DSN the live tests target.
pub fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

/// `true` only when [`SKIP_ENV`] is EXPLICITLY set truthy (`1`/`true`/`on`,
/// case-insensitive) — the workspace's dev-switch convention. Unset is `false`.
pub fn skip_allowed() -> bool {
    matches!(
        std::env::var(SKIP_ENV),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    )
}

/// Opens the local Postgres. Returns `Some` when connected; when the cluster is
/// unreachable it PANICS unless [`SKIP_ENV`] is on, in which case it warns and returns
/// `None` so the caller's `else { return }` skips that test.
///
/// Two mechanisms guard the connect bound, because crates that arm tokio's `test-util`
/// run `start_paused` tests, and a paused runtime auto-advances every virtual timer the
/// moment it idles (`runtime/time/mod.rs`'s `park_thread_timeout` parks for zero and
/// jumps the clock — being blocked on a socket does not stop it), which would report a
/// HEALTHY cluster as unreachable:
///
/// 1. A live `spawn_blocking` task spans the connect. On a current-thread runtime — the
///    only flavour `start_paused` allows — that inhibits auto-advance for its lifetime
///    (`runtime/blocking/schedule.rs`), which is what keeps SQLX's own internal acquire
///    timeout from elapsing instantly. Without it the connect returns `PoolTimedOut` in
///    microseconds against a running Postgres.
/// 2. The outer bound is a REAL thread timer rather than `tokio::time`, so the decision
///    that the cluster is absent can never be made by the virtual clock either.
pub async fn test_pool() -> Option<PgPool> {
    let dsn = dsn();
    let pool = connect(&dsn).await;
    match decide(pool.is_some(), skip_allowed()) {
        Verdict::Use => pool,
        Verdict::Skip => {
            eprintln!(
                "WARNING: {SKIP_ENV} is ON — postgres was not reachable at {dsn} within \
                 {CONNECT_BOUND:?} and this crate's database tests are being SKIPPED. \
                 A green run does NOT mean they passed."
            );
            None
        }
        Verdict::Fail => panic!(
            "no postgres at {dsn} within {CONNECT_BOUND:?} — the database tests cannot run, \
             and reporting them green would be a lie. Start the local cluster, point \
             DATABASE_URL at one, or set {SKIP_ENV}=1 to skip them explicitly."
        ),
    }
}

/// What an absent cluster means. The whole decision, free of I/O and env.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Connected — hand the pool to the test.
    Use,
    /// No cluster, and the opt-out is explicitly on — skip with a warning.
    Skip,
    /// No cluster and no opt-out — the run must go red.
    Fail,
}

pub fn decide(connected: bool, skip_allowed: bool) -> Verdict {
    match (connected, skip_allowed) {
        (true, _) => Verdict::Use,
        (false, true) => Verdict::Skip,
        (false, false) => Verdict::Fail,
    }
}

async fn connect(dsn: &str) -> Option<PgPool> {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let inhibitor = tokio::task::spawn_blocking(move || {
        let _ = released.recv();
    });
    let (bound_timer, bound) = Bound::start();
    let owned = dsn.to_string();
    let pool = tokio::select! {
        connected = PgPool::connect(&owned) => connected.ok(),
        _ = bound => None,
    };
    bound_timer.release();
    let _ = release.send(());
    let _ = inhibitor.await;
    pool
}

/// The real-clock half of the connect bound: a thread that fires `bound` once
/// [`CONNECT_BOUND`] has elapsed, and exits IMMEDIATELY once released. The early release
/// is not cosmetic — one parked thread per DB test, workspace-wide, is enough scheduler
/// pressure to perturb other crates' timing-sensitive tests (it reproducibly broke
/// `asyncevents::stop_terminates_active_backend_…`, which is green on either side of this
/// crate's introduction but red with an unconditional sleep here). That crate-level
/// symptom does not reproduce at crate scale, so what pins the early release is this
/// crate's `the_bound_timer_thread_exits_as_soon_as_the_connect_answers`, which joins the
/// thread and fails if it outlives the release.
struct Bound {
    done: Arc<(Mutex<bool>, Condvar)>,
    thread: std::thread::JoinHandle<()>,
}

impl Bound {
    fn start() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let timer = Arc::clone(&done);
        let (elapsed_tx, elapsed) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            let (done, wake) = &*timer;
            let (done, _) = wake
                .wait_timeout_while(done.lock().unwrap(), CONNECT_BOUND, |done| !*done)
                .unwrap();
            if !*done {
                let _ = elapsed_tx.send(());
            }
        });
        (Self { done, thread }, elapsed)
    }

    /// Signals the timer and returns its handle (tests join it; `connect` drops it).
    fn release(self) -> std::thread::JoinHandle<()> {
        let (done, wake) = &*self.done;
        *done.lock().unwrap() = true;
        wake.notify_all();
        self.thread
    }
}


#[cfg(test)]
mod tests;
#[cfg(test)]
mod drift_tests;

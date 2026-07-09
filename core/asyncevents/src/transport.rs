//! The V2 [`bus::Transport`]: producers append to the XID-ordered shared log
//! ([`crate::store::append`], on the CALLER's open domain tx), consumers register
//! [`bus::SubscriptionSpec`]s that [`crate::Plane::start`] reconciles into
//! `asyncevents.subscriptions` and the pull workers ([`crate::worker`]) drive.
//! There is no outbox, no relay, no `POST /events` sink and no per-process origin:
//! every process reads the one shared log, restricted to its own subscription ids.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use bus::{AnyTx, EventContract, HistoryPolicy, StartPosition, SubscriptionSpec, Transport, TxHandler};
use sqlx::PgConnection;

use crate::store;

/// One locally-registered durable subscription: the consumer-owned descriptor plus
/// the topic/version it binds and the handler the worker invokes per delivery.
/// `history` is the publisher's retention policy carried by a typed `EventType`
/// (`Some` for `on_tx`), or `None` for a raw `on_tx_raw` subscriber — reconcile
/// seeds the `history_contracts` row from it when present (the producer owns it
/// regardless; see [`crate::catalog`]).
#[derive(Clone)]
pub(crate) struct SubEntry {
    pub spec: SubscriptionSpec,
    pub topic: String,
    pub version: u32,
    pub history: Option<HistoryPolicy>,
    pub handler: Arc<dyn TxHandler>,
}

impl SubEntry {
    /// The immutable identity of a subscription row: topic + contract version +
    /// start position, canonically rendered. Stored as `spec_hash`; a re-registration
    /// under the same id with a DIFFERENT identity fails startup (the checkpoint
    /// would silently mean something else).
    pub(crate) fn spec_hash(&self) -> String {
        format!("{}|v{}|{}", self.topic, self.version, start_desc(&self.spec.start))
    }

    /// The `start_kind` column value.
    pub(crate) fn start_kind(&self) -> &'static str {
        match self.spec.start {
            StartPosition::Genesis => "genesis",
            StartPosition::AfterRegistration => "after_registration",
            StartPosition::Explicit(_) => "explicit",
        }
    }
}

fn start_desc(start: &StartPosition) -> String {
    match start {
        StartPosition::Genesis => "genesis".to_string(),
        StartPosition::AfterRegistration => "after_registration".to_string(),
        StartPosition::Explicit(p) => format!("explicit:{}/{}/{}", p.generation, p.xid, p.tie),
    }
}

/// The shared transport state: the local subscription table, live from
/// [`crate::Plane::new`] — BEFORE the `Context` (and thus any module wiring)
/// exists — so every `on_tx`, whether from a module's `init` or a stub factory's
/// `register`, appends to a present list. [`crate::Plane::start`] snapshots it.
#[derive(Default)]
pub struct LogTransport {
    subs: Mutex<Vec<SubEntry>>,
    /// The native-writer `history_contracts` seed guard: `(topic, version)` pairs
    /// whose retention contract this process has already reconciled on a prior
    /// emit. Purely a per-process round-trip optimization — the DDL is
    /// `ON CONFLICT DO NOTHING`, so a concurrent double-seed is harmless.
    contracts_seeded: Mutex<HashSet<(String, u32)>>,
}

impl LogTransport {
    pub(crate) fn new() -> LogTransport {
        LogTransport::default()
    }

    /// Snapshot of every locally-registered subscription (for reconcile + workers).
    pub(crate) fn snapshot(&self) -> Vec<SubEntry> {
        self.subs.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Transport for LogTransport {
    /// Appends one event to the shared log inside the PRODUCER's domain tx (the
    /// [`AnyTx`] erases `&mut *tx`) via `asyncevents.append_event` — the single
    /// writer implementation (shared advisory lock, generation read, xid8 stamp) —
    /// so the event commits iff the domain change commits.
    ///
    /// The downcast is THE producer-side engine gate: this plane's log is Postgres,
    /// so a producer whose store tx is any other engine gets
    /// [`bus::Error::TxEngineMismatch`] at its FIRST EMIT.
    async fn enqueue_tx(
        &self,
        mut tx: AnyTx<'_>,
        contract: &EventContract,
        payload: &[u8],
    ) -> Result<(), bus::Error> {
        let conn = tx.downcast::<PgConnection>()?;
        // Native-writer path (a) for `history_contracts`: on this process's FIRST
        // emit of a (topic, version), seed its retention contract on the producer's
        // own tx (atomic with the event) — and FAIL LOUDLY if a stored row already
        // records a DIFFERENT policy, never silently adopting it.
        let key = (contract.topic.to_string(), contract.version);
        let first = self.contracts_seeded.lock().unwrap().insert(key.clone());
        if first {
            if let Err(err) =
                store::ensure_history_contract(conn, contract.topic, contract.version, contract.history)
                    .await
            {
                // Un-mark on failure so a transient error retries next emit (and a
                // policy conflict stays loud every time until the code is fixed).
                self.contracts_seeded.lock().unwrap().remove(&key);
                return Err(err);
            }
        }
        store::append(conn, contract, payload).await?;
        Ok(())
    }

    /// Records an in-process durable subscription. Called during module wiring
    /// (any phase — the list is live from [`crate::Plane::new`]), so it only
    /// appends; [`crate::Plane::start`] later reconciles these into
    /// `asyncevents.subscriptions` and hands them to the pull workers.
    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        version: u32,
        history: Option<HistoryPolicy>,
        handler: Arc<dyn TxHandler>,
    ) {
        self.subs.lock().unwrap().push(SubEntry {
            spec,
            topic: topic.to_string(),
            version,
            history,
            handler,
        });
    }
}

/// A v1/7-day contract shape shared by the in-crate tests.
#[cfg(test)]
pub(crate) fn test_contract(topic: &'static str) -> EventContract {
    EventContract {
        topic,
        version: 1,
        history: bus::HistoryPolicy::MinRetention { days: 7 },
    }
}

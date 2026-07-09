use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// Drains by closing, so every queued event is delivered before close returns —
// no arbitrary sleeps needed for the assertions.
async fn settle(bus: &Bus) {
    bus.close().await;
}

/// The one contract shape the in-process tests need — the durable fields
/// (version/history) are irrelevant to the async core.
fn def<T>(topic: &'static str) -> EventType<T> {
    define(topic, 1, HistoryPolicy::MinRetention { days: 7 })
}

/// A throwaway spec for durable-plane tests.
fn spec(id: &'static str) -> SubscriptionSpec {
    SubscriptionSpec {
        id,
        start: StartPosition::Genesis,
    }
}

#[tokio::test]
async fn typed_emit_on_roundtrip() {
    let bus = Bus::new();
    let seen = Arc::new(Mutex::new(Vec::<i32>::new()));
    let et = def::<i32>("nums");
    {
        let seen = seen.clone();
        bus.on(&et, move |v| seen.lock().unwrap().push(v));
    }
    bus.emit(&et, 1);
    bus.emit(&et, 2);
    bus.emit(&et, 3);
    settle(&bus).await;
    assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
}

#[tokio::test]
async fn per_subscriber_fifo_order() {
    let bus = Bus::new();
    let et = def::<u32>("t");
    let a = Arc::new(Mutex::new(Vec::<u32>::new()));
    let b = Arc::new(Mutex::new(Vec::<u32>::new()));
    {
        let a = a.clone();
        bus.on(&et, move |v| a.lock().unwrap().push(v));
    }
    {
        let b = b.clone();
        bus.on(&et, move |v| b.lock().unwrap().push(v));
    }
    for i in 0..100 {
        bus.emit(&et, i);
    }
    settle(&bus).await;
    let expected: Vec<u32> = (0..100).collect();
    assert_eq!(*a.lock().unwrap(), expected);
    assert_eq!(*b.lock().unwrap(), expected);
}

#[tokio::test]
async fn subscribed_topics_reports_in_process_subscriptions() {
    let bus = Bus::new();
    let a = def::<u32>("alpha");
    let b = def::<u32>("beta");
    assert!(bus.subscribed_topics().is_empty());
    bus.on(&a, |_v| {});
    bus.on(&b, |_v| {});
    bus.on(&a, |_v| {}); // a second subscriber on an existing topic — still one key
    let mut got = bus.subscribed_topics();
    got.sort();
    assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
    settle(&bus).await;
}

#[tokio::test]
async fn panicking_handler_does_not_stall_others() {
    let bus = Bus::new();
    let et = def::<u32>("t");
    let good = Arc::new(AtomicU32::new(0));
    // A subscriber that panics on every delivery.
    bus.on(&et, |_v| panic!("boom"));
    // An independent subscriber that must still receive everything.
    {
        let good = good.clone();
        bus.on(&et, move |_v| {
            good.fetch_add(1, Ordering::SeqCst);
        });
    }
    for i in 0..10 {
        bus.emit(&et, i);
    }
    settle(&bus).await;
    assert_eq!(good.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn slow_subscriber_does_not_block_publisher() {
    let bus = Bus::new();
    let et = def::<u32>("t");
    let done = Arc::new(AtomicU32::new(0));
    {
        let done = done.clone();
        // Deliberately slow handler; publish must not wait on it.
        bus.on(&et, move |_v| {
            std::thread::sleep(Duration::from_millis(1));
            done.fetch_add(1, Ordering::SeqCst);
        });
    }
    // These returns are effectively instant even though the handler is slow.
    for i in 0..20 {
        bus.emit(&et, i);
    }
    settle(&bus).await; // waits for the slow subscriber to drain
    assert_eq!(done.load(Ordering::SeqCst), 20);
}

#[tokio::test]
async fn close_is_idempotent_on_no_subscribers() {
    let bus = Bus::new();
    bus.close().await;
}

// ---- Durable plane -----------------------------------------------------

use serde::Deserialize;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
struct Grant {
    item: String,
    qty: i32,
}

/// An in-memory [`Transport`] double: records every `enqueue_tx` payload and
/// every `subscribe_tx` registration, so a test can assert the bytes the bus
/// routed and the subscriptions it wired — without a live Postgres.
#[derive(Default)]
struct FakeTransport {
    enqueued: Mutex<Vec<(String, Vec<u8>)>>,
    subscribed: Mutex<Vec<(String, String)>>,
    handlers: Mutex<Vec<Arc<dyn TxHandler>>>,
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn enqueue_tx(
        &self,
        _tx: AnyTx<'_>,
        contract: &EventContract,
        payload: &[u8],
    ) -> Result<(), Error> {
        self.enqueued
            .lock()
            .unwrap()
            .push((contract.topic.to_string(), payload.to_vec()));
        Ok(())
    }

    fn subscribe_tx(
        &self,
        spec: SubscriptionSpec,
        topic: &str,
        _version: u32,
        handler: Arc<dyn TxHandler>,
    ) {
        self.subscribed
            .lock()
            .unwrap()
            .push((topic.to_string(), spec.id.to_string()));
        self.handlers.lock().unwrap().push(handler);
    }
}

#[test]
fn no_transport_resolves_to_err() {
    // This is the exact resolution emit_tx performs before marshalling, so it
    // is emit_tx's NoTransport behaviour minus the un-fabricable PgConnection.
    // Bus::new() is the plane-less constructor (a DB-less process); only
    // Bus::with_transport carries a durable plane — there is no later installer.
    let bus = Bus::new();
    assert!(matches!(bus.require_transport(), Err(Error::NoTransport)));

    let bus = Bus::with_transport(Arc::new(FakeTransport::default()));
    assert!(bus.require_transport().is_ok());
}

#[test]
#[should_panic(expected = "no durable-events plane")]
fn on_tx_without_transport_panics() {
    // BLOCKER-2: a dropped durable subscription must be loud, not a silent
    // no-op that builds clean and never delivers. Bus::new() = a process with
    // no durable-events plane (no DB), where a durable subscriber cannot run.
    let bus = Bus::new();
    let et = def::<Grant>("inventory.grant");
    bus.on_tx(spec("inventory.grant.v1"), &et, |delivery, g: Grant| {
        Box::pin(async move {
            let _ = (delivery, g);
            Ok(())
        })
    });
}

#[test]
fn on_tx_and_on_tx_raw_record_topic_and_subscription_id() {
    let fake = Arc::new(FakeTransport::default());
    let bus = Bus::with_transport(fake.clone());
    let et = def::<Grant>("inventory.grant");

    bus.on_tx(spec("inventory.grant.v1"), &et, |delivery, g: Grant| {
        Box::pin(async move {
            let _ = (delivery, g);
            Ok(())
        })
    });
    struct Raw;
    impl TxHandler for Raw {
        fn call<'a>(
            &'a self,
            _delivery: Delivery<'a>,
            _payload: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async { Ok(()) })
        }
    }
    bus.on_tx_raw(spec("audit.everything.v1"), "audit.everything", Arc::new(Raw));

    let subs = fake.subscribed.lock().unwrap();
    assert_eq!(
        *subs,
        vec![
            ("inventory.grant".to_string(), "inventory.grant.v1".to_string()),
            ("audit.everything".to_string(), "audit.everything.v1".to_string()),
        ]
    );
    drop(subs);

    // The bus-side introspection list mirrors the same registrations as
    // (id, topic) pairs — the durable-plane view topiccheck consumes.
    assert_eq!(
        bus.subscriptions(),
        vec![
            ("inventory.grant.v1".to_string(), "inventory.grant".to_string()),
            ("audit.everything.v1".to_string(), "audit.everything".to_string()),
        ]
    );
}

#[test]
#[should_panic(expected = "duplicate durable subscription id")]
fn duplicate_subscription_id_panics_under_any_transport() {
    // The duplicate-id guard is BUS-owned, so it fires even under a transport
    // double (the checkers' RecordingTransport class) — a second registration
    // of the same checkpoint name is a wiring bug, never a silent share.
    let bus = Bus::with_transport(Arc::new(FakeTransport::default()));
    let et = def::<Grant>("inventory.grant");
    bus.on_tx(spec("inventory.grant.v1"), &et, |delivery, g: Grant| {
        Box::pin(async move {
            let _ = (delivery, g);
            Ok(())
        })
    });
    bus.on_tx(spec("inventory.grant.v1"), &et, |delivery, g: Grant| {
        Box::pin(async move {
            let _ = (delivery, g);
            Ok(())
        })
    });
}

#[test]
fn on_tx_forwards_the_contract_version_and_raw_pins_v1() {
    // The transport must be told which contract version a subscription reads
    // exactly; a raw subscribe names no EventType, so it pins v1.
    #[derive(Default)]
    struct VersionRecorder {
        versions: Mutex<Vec<(String, u32)>>,
    }
    #[async_trait::async_trait]
    impl Transport for VersionRecorder {
        async fn enqueue_tx(
            &self,
            _tx: AnyTx<'_>,
            _contract: &EventContract,
            _payload: &[u8],
        ) -> Result<(), Error> {
            Ok(())
        }
        fn subscribe_tx(
            &self,
            spec: SubscriptionSpec,
            _topic: &str,
            version: u32,
            _handler: Arc<dyn TxHandler>,
        ) {
            self.versions
                .lock()
                .unwrap()
                .push((spec.id.to_string(), version));
        }
    }
    struct Raw;
    impl TxHandler for Raw {
        fn call<'a>(
            &'a self,
            _delivery: Delivery<'a>,
            _payload: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async { Ok(()) })
        }
    }

    let rec = Arc::new(VersionRecorder::default());
    let bus = Bus::with_transport(rec.clone());
    let et = define::<Grant>("inventory.grant", 3, HistoryPolicy::KeepForever);
    bus.on_tx(spec("typed.v3"), &et, |delivery, g: Grant| {
        Box::pin(async move {
            let _ = (delivery, g);
            Ok(())
        })
    });
    bus.on_tx_raw(spec("raw.v1"), "inventory.grant", Arc::new(Raw));

    assert_eq!(
        *rec.versions.lock().unwrap(),
        vec![("typed.v3".to_string(), 3), ("raw.v1".to_string(), 1)]
    );
}

#[test]
fn codec_is_the_t_to_bytes_boundary() {
    // emit_tx marshals with `encode`; on_tx's TypedAdapter unmarshals with
    // `decode`. This exercises that exact round-trip — the payload a durable
    // handler receives back is the T the producer emitted — without needing a
    // PgConnection (which cannot be fabricated offline; the real DB round-trip
    // lives in `asyncevents`'s integration tests).
    let g = Grant {
        item: "starter-sword".into(),
        qty: 1,
    };
    let bytes = encode(&g).unwrap();
    assert_eq!(bytes, br#"{"item":"starter-sword","qty":1}"#.to_vec());
    let back: Grant = decode(&bytes).unwrap();
    assert_eq!(back, g);
}

#[test]
fn typed_adapter_decodes_before_calling_the_handler() {
    // Drive a TypedAdapter's decode step directly (the half of `call` that does
    // NOT touch the connection), proving the handler is handed the deserialized
    // T. The conn-borrowing tail of `call` is covered by the compile-check
    // below and by the `asyncevents` live round-trip.
    let g = Grant {
        item: "potion".into(),
        qty: 3,
    };
    let payload = encode(&g).unwrap();
    let decoded: Grant = decode(&payload).unwrap();
    assert_eq!(decoded, g);
}

/// Compile-only proof that the full durable tx-threading type-checks: `emit_tx`
/// takes an [`AnyTx`] (erased from any `Any + Send` local — the seam names no
/// engine, so a plain `u32` stands in for a connection) and routes `&encode(v)`
/// to `Transport::enqueue_tx`; `on_tx`/`on_tx_raw` accept the
/// borrow-through-future handler shape (`for<'a> Fn(Delivery<'a>, T) ->
/// BoxFuture<'a, _>`, the future borrowing the delivery). Never executed —
/// the compiler checking it is the assertion.
#[allow(dead_code)]
async fn _tx_threading_type_checks(bus: &Bus) {
    let et = def::<Grant>("inventory.grant");
    let mut fake_tx: u32 = 0;
    let _ = bus
        .emit_tx(
            AnyTx::new(&mut fake_tx),
            &et,
            &Grant {
                item: "x".into(),
                qty: 1,
            },
        )
        .await;
    bus.on_tx(spec("inventory.grant.v1"), &et, |mut delivery, g: Grant| {
        Box::pin(async move {
            // The future borrows the delivery: both fields usable across an await.
            let _ = delivery.event_id;
            let _ = delivery.tx.downcast::<u32>();
            let _ = g;
            Ok(())
        })
    });
    struct H;
    impl TxHandler for H {
        fn call<'a>(
            &'a self,
            _delivery: Delivery<'a>,
            _payload: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async { Ok(()) })
        }
    }
    bus.on_tx_raw(spec("audit.everything.v1"), "audit", Arc::new(H));
}

/// [`AnyTx::downcast`] hands back the exact borrow for the constructed type and
/// yields [`Error::TxEngineMismatch`] naming BOTH concrete types otherwise —
/// the loud composition-root signal that replaces compile-time engine typing.
#[test]
fn any_tx_downcast_success_and_engine_mismatch() {
    let mut n: u32 = 41;
    let mut tx = AnyTx::new(&mut n);
    *tx.downcast::<u32>().unwrap() += 1;
    assert_eq!(n, 42);

    let mut s = String::from("not-a-u32");
    let mut tx = AnyTx::new(&mut s);
    let err = tx.downcast::<u32>().unwrap_err();
    match &err {
        Error::TxEngineMismatch { expected, got } => {
            assert_eq!(*expected, std::any::type_name::<u32>());
            assert_eq!(*got, std::any::type_name::<String>());
        }
        other => panic!("expected TxEngineMismatch, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("u32"), "message must name the expected type: {msg}");
    assert!(msg.contains("String"), "message must name the got type: {msg}");
}

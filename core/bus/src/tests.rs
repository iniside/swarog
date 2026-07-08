use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// Drains by closing, so every queued event is delivered before close returns —
// no arbitrary sleeps needed for the assertions.
async fn settle(bus: &Bus) {
    bus.close().await;
}

#[tokio::test]
async fn typed_emit_on_roundtrip() {
    let bus = Bus::new();
    let seen = Arc::new(Mutex::new(Vec::<i32>::new()));
    let et = define::<i32>("nums");
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
    let et = define::<u32>("t");
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
    let a = define::<u32>("alpha");
    let b = define::<u32>("beta");
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
    let et = define::<u32>("t");
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
    let et = define::<u32>("t");
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
        _conn: &mut sqlx::PgConnection,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), Error> {
        self.enqueued
            .lock()
            .unwrap()
            .push((topic.to_string(), payload.to_vec()));
        Ok(())
    }

    fn subscribe_tx(&self, topic: &str, subscriber: &str, handler: Arc<dyn TxHandler>) {
        self.subscribed
            .lock()
            .unwrap()
            .push((topic.to_string(), subscriber.to_string()));
        self.handlers.lock().unwrap().push(handler);
    }
}

#[test]
fn no_transport_resolves_to_err() {
    // This is the exact resolution emit_tx performs before marshalling, so it
    // is emit_tx's NoTransport behaviour minus the un-fabricable PgConnection.
    let bus = Bus::new();
    assert!(matches!(bus.require_transport(), Err(Error::NoTransport)));

    bus.set_transport(Arc::new(FakeTransport::default()));
    assert!(bus.require_transport().is_ok());
}

#[test]
#[should_panic(expected = "transport already set")]
fn set_transport_panics_on_double_set() {
    let bus = Bus::new();
    bus.set_transport(Arc::new(FakeTransport::default()));
    bus.set_transport(Arc::new(FakeTransport::default())); // loud, not silent
}

#[test]
#[should_panic(expected = "no durable transport installed")]
fn on_tx_without_transport_panics() {
    // BLOCKER-2: a dropped durable subscription must be loud, not a silent
    // no-op that builds clean and never delivers.
    let bus = Bus::new();
    let et = define::<Grant>("inventory.grant");
    bus.on_tx(&et, "inventory", |conn, g: Grant| {
        Box::pin(async move {
            let _ = (conn, g);
            Ok(())
        })
    });
}

#[test]
fn on_tx_and_on_tx_raw_record_topic_and_subscriber() {
    let fake = Arc::new(FakeTransport::default());
    let bus = Bus::new();
    bus.set_transport(fake.clone());
    let et = define::<Grant>("inventory.grant");

    bus.on_tx(&et, "inventory", |conn, g: Grant| {
        Box::pin(async move {
            let _ = (conn, g);
            Ok(())
        })
    });
    struct Raw;
    impl TxHandler for Raw {
        fn call<'a>(
            &'a self,
            _conn: &'a mut sqlx::PgConnection,
            _payload: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async { Ok(()) })
        }
    }
    bus.on_tx_raw("audit.everything", "audit", Arc::new(Raw));

    let subs = fake.subscribed.lock().unwrap();
    assert_eq!(
        *subs,
        vec![
            ("inventory.grant".to_string(), "inventory".to_string()),
            ("audit.everything".to_string(), "audit".to_string()),
        ]
    );
}

#[test]
fn codec_is_the_t_to_bytes_boundary() {
    // emit_tx marshals with `encode`; on_tx's TypedAdapter unmarshals with
    // `decode`. This exercises that exact round-trip — the payload a durable
    // handler receives back is the T the producer emitted — without needing a
    // PgConnection (which cannot be fabricated offline; a real DB round-trip
    // lands in Step 6 with `messaging`).
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
    // below and by the Step-6 live round-trip.
    let g = Grant {
        item: "potion".into(),
        qty: 3,
    };
    let payload = encode(&g).unwrap();
    let decoded: Grant = decode(&payload).unwrap();
    assert_eq!(decoded, g);
}

/// Compile-only proof that the full durable tx-threading type-checks: `emit_tx`
/// takes `&mut PgConnection` and routes `&encode(v)` to `Transport::enqueue_tx`;
/// `on_tx`/`on_tx_raw` accept the borrow-through-future handler shape. Never
/// executed (no live DB) — the compiler checking it is the assertion.
#[allow(dead_code)]
async fn _tx_threading_type_checks(bus: &Bus, conn: &mut sqlx::PgConnection) {
    let et = define::<Grant>("inventory.grant");
    let _ = bus
        .emit_tx(
            conn,
            &et,
            &Grant {
                item: "x".into(),
                qty: 1,
            },
        )
        .await;
    bus.on_tx(&et, "inventory", |c, g: Grant| {
        Box::pin(async move {
            let _ = (c, g);
            Ok(())
        })
    });
    struct H;
    impl TxHandler for H {
        fn call<'a>(
            &'a self,
            _conn: &'a mut sqlx::PgConnection,
            _payload: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async { Ok(()) })
        }
    }
    bus.on_tx_raw("audit", "audit", Arc::new(H));
}

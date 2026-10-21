use super::*;

use std::sync::Mutex as StdMutex;

use tracing::Level;

// ---- A capturing subscriber -------------------------------------------------
//
// The latched `NoSink` log is a first-vs-subsequent BRANCH, so the assertion has to
// see the LEVEL each call emitted at, not just that `send` erred. `tracing` has no
// in-crate capture helper and `tracing-subscriber` is not a dependency here, so this
// is the smallest `Subscriber` that records levels; `with_default` scopes it to this
// test's thread, so a parallel test never lands in the buffer.

#[derive(Default)]
struct LevelLog {
    levels: Arc<StdMutex<Vec<Level>>>,
}

impl LevelLog {
    fn levels(&self) -> Vec<Level> {
        self.levels.lock().unwrap().clone()
    }

    fn subscriber(&self) -> LevelSubscriber {
        LevelSubscriber {
            levels: self.levels.clone(),
        }
    }
}

struct LevelSubscriber {
    levels: Arc<StdMutex<Vec<Level>>>,
}

impl tracing::Subscriber for LevelSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        self.levels.lock().unwrap().push(*event.metadata().level());
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

// ---- Fake sinks -------------------------------------------------------------

/// Records what it was handed and answers a fixed [`Delivered`], so a test can prove
/// both that `send` reached the sink unchanged and WHICH kind of answer came back.
struct FakeSink {
    answer: Delivered,
    seen: Seen,
}

impl Sink for FakeSink {
    fn send(&self, target: &Target, msg: &Message) -> Result<Delivered, Error> {
        self.seen.lock().unwrap().push((target.clone(), msg.clone()));
        Ok(self.answer)
    }
}

/// What a [`FakeSink`] recorded, in call order.
type Seen = Arc<StdMutex<Vec<(Target, Message)>>>;

fn fake_sink(answer: Delivered) -> (Arc<dyn Sink>, Seen) {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let sink: Arc<dyn Sink> = Arc::new(FakeSink {
        answer,
        seen: seen.clone(),
    });
    (sink, seen)
}

// ---- The sinkless handle ----------------------------------------------------

/// A sinkless process answers `NoSink` on EVERY call — and logs the mistake once at
/// `error!`, at `debug!` thereafter. The latch is per-handle state, so the level
/// sequence (not the error) is the only thing that distinguishes the first call from
/// the rest: a regression that drops the latch shows up here as three `ERROR`s, one
/// that latches too early as three `DEBUG`s.
#[test]
fn send_without_a_sink_errs_every_time_and_latches_the_log_after_the_first() {
    let log = LevelLog::default();
    let push = Push::new();
    let msg = Message::new("inbox.changed", b"{}".to_vec());

    tracing::subscriber::with_default(log.subscriber(), || {
        // Callsite interest is cached per process; rebuild it so a callsite first seen
        // under another test's (absent) subscriber cannot leave this one empty.
        tracing::callsite::rebuild_interest_cache();
        for _ in 0..3 {
            let err = push
                .send(&Target::Player("p1".into()), &msg)
                .expect_err("a sinkless handle never delivers");
            assert!(matches!(err, Error::NoSink), "{err}");
        }
    });

    assert_eq!(
        log.levels(),
        vec![Level::ERROR, Level::DEBUG, Level::DEBUG],
        "the first no-sink send is loud, every later one is not"
    );
}

/// The latch is per-handle, not per-process: a second process-shaped handle logs its
/// own first mistake loudly. (A `static` latch would make this the second handle's
/// `DEBUG`, hiding a real misconfiguration in a process that has one.)
#[test]
fn the_no_sink_latch_belongs_to_the_handle() {
    let log = LevelLog::default();
    let msg = Message::new("t", Vec::new());

    tracing::subscriber::with_default(log.subscriber(), || {
        tracing::callsite::rebuild_interest_cache();
        let _ = Push::new().send(&Target::All, &msg);
        let _ = Push::new().send(&Target::All, &msg);
    });

    assert_eq!(log.levels(), vec![Level::ERROR, Level::ERROR]);
}

// ---- Installation -----------------------------------------------------------

/// Two sinks are two answers to "where do this player's connections live"; the second
/// install is a loud boot failure, never a silently-ignored `OnceLock::set`.
#[test]
#[should_panic(expected = "push sink already installed")]
fn a_second_install_panics() {
    let push = Push::new();
    push.install(fake_sink(Delivered::Queued).0);
    push.install(fake_sink(Delivered::Local(1)).0);
}

/// With a sink installed, `send` hands the target and message through UNCHANGED and
/// returns the sink's own answer — and the two answers stay distinct kinds. `Local(0)`
/// (nobody addressed is connected here) must never read as `Queued` (accepted for
/// fan-out): collapsing them would turn "delivered to nobody" into "will be tried".
#[test]
fn send_delegates_to_the_sink_and_never_conflates_local_with_queued() {
    let msg = Message::new("inbox.changed", vec![1, 2, 3]);

    let local = Push::new();
    let (sink, seen) = fake_sink(Delivered::Local(3));
    local.install(sink);
    assert_eq!(
        local.send(&Target::Group("g".into()), &msg).unwrap(),
        Delivered::Local(3)
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[(Target::Group("g".into()), msg.clone())],
        "the sink receives the target and message verbatim"
    );

    let queued = Push::new();
    queued.install(fake_sink(Delivered::Queued).0);
    assert_eq!(
        queued.send(&Target::Group("g".into()), &msg).unwrap(),
        Delivered::Queued
    );

    assert_ne!(
        Delivered::Local(0),
        Delivered::Queued,
        "a count of zero is an observation; a queue handoff is a promise to try"
    );
}

// ---- The backplane codec ----------------------------------------------------

/// The batch codec is the seam `core/remote` encodes and `modules/gateway` decodes:
/// every [`Target`] variant survives it, and ORDER is the contract (one call per batch
/// exists precisely so frames cannot invert between topologies).
#[test]
fn encode_batch_round_trips_every_target_variant_in_order() {
    let batch = vec![
        Envelope::new(Target::Player("p1".into()), Message::new("a", vec![0, 255])),
        Envelope::new(Target::Group("g1".into()), Message::new("b", Vec::new())),
        Envelope::new(Target::All, Message::new("c", b"payload".to_vec())),
        Envelope::new(Target::Player("p2".into()), Message::new("d", vec![7])),
    ];

    let decoded = decode_batch(&encode_batch(1, 1, &batch).unwrap()).unwrap().envelopes;

    assert_eq!(decoded, batch, "every variant and the slice order survive the codec");
    assert_eq!(
        decoded.iter().map(|e| e.msg.topic.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
}

/// An empty batch is legal (it encodes and decodes to nothing), and garbage is a typed
/// `Codec` error rather than a panic on the receiving front.
#[test]
fn decode_batch_rejects_garbage_and_accepts_an_empty_batch() {
    assert!(decode_batch(&encode_batch(1, 1, &[]).unwrap()).unwrap().envelopes.is_empty());

    let err = decode_batch(b"not json").expect_err("garbage must not decode");
    assert!(matches!(err, Error::Codec(_)), "{err}");
}

/// The header is part of the codec: `producer` and `seq` survive the round trip next to
/// the envelopes, at both extremes of `u64`.
#[test]
fn batch_round_trips_producer_seq_and_envelopes() {
    let envelopes = vec![
        Envelope::new(Target::All, Message::new("a", vec![1])),
        Envelope::new(Target::Group("g".into()), Message::new("b", vec![2, 3])),
    ];
    for (producer, seq) in [(7, 9), (u64::MAX, u64::MAX), (0, 1)] {
        let decoded = decode_batch(&encode_batch(producer, seq, &envelopes).unwrap()).unwrap();
        assert_eq!(decoded, Batch { producer, seq, envelopes: envelopes.clone() });
    }
}

/// The old wire shape (a bare array of envelopes) is dead: a receiver must refuse it
/// rather than treat it as an unstamped batch.
#[test]
fn decode_batch_rejects_a_bare_envelope_array() {
    let bare = serde_json::to_vec(&vec![Envelope::new(Target::All, Message::new("a", vec![1]))])
        .unwrap();
    assert!(matches!(decode_batch(&bare), Err(Error::Codec(_))));
    assert!(matches!(decode_batch(b"[]"), Err(Error::Codec(_))));
}

/// `deny_unknown_fields`: a header carrying an extra key does not decode.
#[test]
fn decode_batch_rejects_an_unknown_header_key() {
    let ok = br#"{"producer":1,"seq":1,"envelopes":[]}"#;
    assert!(decode_batch(ok).is_ok());
    let extra = br#"{"producer":1,"seq":1,"envelopes":[],"extra":true}"#;
    assert!(matches!(decode_batch(extra), Err(Error::Codec(_))));
}

/// The sender budgets against `BATCH_HEADER_MAX`; if it drifts from the codec a stamped
/// chunk can exceed the frame budget.
#[test]
fn batch_header_max_equals_the_widest_encoded_header() {
    assert_eq!(
        BATCH_HEADER_MAX,
        encode_batch(u64::MAX, u64::MAX, &[]).unwrap().len()
    );
}

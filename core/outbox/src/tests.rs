use super::*;

#[test]
fn parse_subscribers_shape() {
    let m = parse_subscribers(
        "character.created=http://b/events,http://c/events; character.deleted = http://b/events ;;bad;=nourl;t2=",
    );
    assert_eq!(
        m.get("character.created").unwrap(),
        &vec!["http://b/events".to_string(), "http://c/events".to_string()]
    );
    assert_eq!(m.get("character.deleted").unwrap(), &vec!["http://b/events".to_string()]);
    // "bad" has no '=', "=nourl" has empty topic, "t2=" has empty url -> none recorded.
    assert!(!m.contains_key("bad"));
    assert!(!m.contains_key("t2"));
    assert_eq!(m.len(), 2);
}

#[test]
fn parse_subscribers_empty_is_empty_map() {
    assert!(parse_subscribers("").is_empty());
    assert!(parse_subscribers("   ").is_empty());
}

#[test]
fn valid_ident_accepts_and_rejects() {
    assert!(valid_ident("messaging"));
    assert!(valid_ident("_x9"));
    assert!(!valid_ident("9x"));
    assert!(!valid_ident("a.b"));
    assert!(!valid_ident(""));
    assert!(!valid_ident("drop table"));
}

#[tokio::test]
#[should_panic(expected = "invalid schema name")]
async fn new_panics_on_bad_schema() {
    let pool = PgPool::connect_lazy("postgres://x/y").unwrap();
    let _ = Relay::new(pool, "bad schema", "o", HashMap::new(), Vec::new());
}

// --- Property tests (ported from experiments/go-sketch/outbox/relay_prop_test.go) ---

use std::cell::RefCell;
use std::rc::Rc;

use proptest::prelude::*;

const TOPICS: [&str; 3] = ["a", "b", "c"];
const URL_POOL: [&str; 3] = ["http://u1", "http://u2", "http://u3"];

fn key(topic: &str, url: &str) -> String {
    format!("{topic}\0{url}")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Exercises [`deliver_with`]'s documented contract: per-(topic, url)
    /// stop-on-first-failure, all-or-nothing per row, no-subscriber rows always sent,
    /// and the returned ids are a strictly-ascending subsequence of the input
    /// (never reordered). Port of Go's `TestPropDeliverOrdering`.
    #[test]
    fn deliver_ordering(
        n in 0usize..=12,
        gaps in proptest::collection::vec(1i64..=3, 0..=12),
        topic_idxs in proptest::collection::vec(0usize..3, 0..=12),
        payload_lens in proptest::collection::vec(0usize..=8, 0..=12),
        // Per-topic subscriber subset: 0 = no subscribers; 1..=7 = a nonempty subset
        // of the 3-url pool (bit i set = URL_POOL[i] subscribes).
        sub_masks in proptest::array::uniform3(0u8..8),
        // Per-(topic, url) failure pattern: (fails?, fail-from-call-index).
        fail_flags in proptest::array::uniform9(any::<bool>()),
        fail_ats in proptest::array::uniform9(0usize..=13),
    ) {
        let n = n.min(gaps.len()).min(topic_idxs.len()).min(payload_lens.len());

        // Ascending, strictly-increasing ids.
        let mut ids = Vec::with_capacity(n);
        let mut next: i64 = 1;
        for gap in gaps.iter().take(n) {
            next += gap;
            ids.push(next);
        }

        let pending: Vec<OutRow> = (0..n)
            .map(|i| OutRow {
                id: ids[i],
                topic: TOPICS[topic_idxs[i]].to_string(),
                payload: vec![0u8; payload_lens[i]],
            })
            .collect();

        let mut subscribers: HashMap<String, Vec<String>> = HashMap::new();
        for (ti, &mask) in sub_masks.iter().enumerate() {
            if mask == 0 {
                continue;
            }
            let urls: Vec<String> = (0..3)
                .filter(|b| mask & (1 << b) != 0)
                .map(|b| URL_POOL[b].to_string())
                .collect();
            subscribers.insert(TOPICS[ti].to_string(), urls);
        }

        // fail_from_call[(topic,url)]: call index (per pair) at which it starts
        // failing forever after; None = never fails.
        let mut fail_from_call: HashMap<String, Option<usize>> = HashMap::new();
        let mut idx = 0;
        for topic in TOPICS {
            for url in URL_POOL {
                let k = key(topic, url);
                let from = if fail_flags[idx] { Some(fail_ats[idx]) } else { None };
                fail_from_call.insert(k, from);
                idx += 1;
            }
        }

        let call_count: Rc<RefCell<HashMap<String, usize>>> = Rc::new(RefCell::new(HashMap::new()));
        let posted_by_key: Rc<RefCell<HashMap<String, Vec<i64>>>> = Rc::new(RefCell::new(HashMap::new()));
        let fail_from_call = Rc::new(fail_from_call);

        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let sent = rt.block_on(async {
            let call_count = call_count.clone();
            let posted_by_key = posted_by_key.clone();
            let fail_from_call = fail_from_call.clone();
            deliver_with("s", &[], &subscribers, &pending, move |url, topic, event_id, _payload| {
                let k = key(&topic, &url);
                let idx = {
                    let mut cc = call_count.borrow_mut();
                    let e = cc.entry(k.clone()).or_insert(0);
                    let cur = *e;
                    *e += 1;
                    cur
                };
                // Recover the row id from event_id ("s:<id>").
                let row_id: i64 = event_id.rsplit(':').next().unwrap().parse().unwrap();
                posted_by_key.borrow_mut().entry(k.clone()).or_default().push(row_id);
                let should_fail = fail_from_call
                    .get(&k)
                    .and_then(|from| *from)
                    .map(|from| idx >= from)
                    .unwrap_or(false);
                Box::pin(async move {
                    if should_fail {
                        anyhow::bail!("injected failure");
                    }
                    Ok(())
                })
            })
            .await
        });

        let posted_by_key = posted_by_key.borrow();

        // (a) per-(topic, url) stop-on-first-failure: no row posted to a pair after
        // that pair's failing row must have a strictly larger id.
        for (k, posts) in posted_by_key.iter() {
            let Some(Some(from)) = fail_from_call.get(k) else { continue };
            if *from >= posts.len() {
                continue; // never actually reached the failing call
            }
            let failed_row_id = posts[*from];
            for &row_id in &posts[from + 1..] {
                prop_assert!(
                    row_id <= failed_row_id,
                    "pair {k:?}: row {row_id} posted after row {failed_row_id} failed"
                );
            }
        }

        // (b) all-or-nothing + (c) no-subscriber rows always sent.
        let sent_set: std::collections::HashSet<i64> = sent.iter().copied().collect();
        let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
        for row in &pending {
            let urls = subscribers.get(&row.topic).cloned().unwrap_or_default();
            if urls.is_empty() {
                prop_assert!(sent_set.contains(&row.id), "row {} (no subscribers) must always be sent", row.id);
                continue;
            }
            let mut expect_ok = true;
            for url in &urls {
                let k = key(&row.topic, url);
                if blocked.contains(&k) {
                    expect_ok = false;
                    continue;
                }
                let from = fail_from_call.get(&k).and_then(|f| *f);
                let posts = posted_by_key.get(&k).cloned().unwrap_or_default();
                let call_idx = posts.iter().position(|&id| id == row.id);
                let failed_this_call = matches!((call_idx, from), (Some(ci), Some(f)) if ci >= f);
                if failed_this_call {
                    blocked.insert(k);
                    expect_ok = false;
                }
            }
            prop_assert_eq!(expect_ok, sent_set.contains(&row.id), "row {}: expected sent={}, got sent={}", row.id, expect_ok, sent_set.contains(&row.id));
        }

        // (d) sent is a strictly-ascending subsequence of the input ids.
        let mut last_idx: isize = -1;
        let input_idx: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        for &id in &sent {
            let idx = *input_idx.get(&id).expect("sent id must be present in input");
            prop_assert!(idx as isize > last_idx, "sent ids out of input order: {sent:?}");
            last_idx = idx as isize;
        }
    }

    /// `parse_subscribers` round-trips an arbitrary topic->URLs map through the
    /// "topic=url1,url2;topic2=url3" wire format. Port of Go's
    /// `TestPropParseSubscribersRoundTrip`.
    #[test]
    fn parse_subscribers_roundtrip(
        topics in proptest::collection::hash_set("[a-zA-Z0-9_./:-]{1,10}", 0..5),
    ) {
        let mut want: HashMap<String, Vec<String>> = HashMap::new();
        let mut entries: Vec<String> = Vec::new();
        for (i, topic) in topics.iter().enumerate() {
            let urls: Vec<String> = (0..=(i % 4)).map(|j| format!("u{i}_{j}")).collect();
            want.insert(topic.clone(), urls.clone());
            entries.push(format!("{topic}={}", urls.join(",")));
        }
        let raw = entries.join(";");

        let got = parse_subscribers(&raw);
        prop_assert_eq!(got.len(), want.len());
        for (topic, urls) in &want {
            prop_assert_eq!(got.get(topic), Some(urls));
        }
    }
}

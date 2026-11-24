//! Live-Postgres tests of the audit log readers (the local DB is the test DB, via
//! `testdb`): the per-topic derivation of every logged topic and of malformed payloads
//! (H11), both time formats (H12), the keyset pager, each filter and the total. Every
//! fixture carries a per-test-unique marker and deletes its own rows, so rows other runs
//! or a dev fleet left in the shared `audit.log` never change an assertion.

use std::collections::HashSet;

use serde_json::{json, Value};
use sqlx::PgPool;

use controlplane::events::AdminAction;
use serde::Serialize;

use super::*;
use crate::{Audit, Context, Module};

/// A fixture payload as the producer writes it: the contract struct serialized, so a
/// renamed contract field breaks the derivation here instead of drawing "—" on the page.
fn payload(event: impl Serialize) -> Value {
    serde_json::to_value(event).expect("a contract event serializes")
}

fn admin(actor: &str, action: &str, target: &str, detail: &str) -> Value {
    payload(AdminAction { actor: actor.into(), action: action.into(), target: target.into(), detail: detail.into() })
}

fn wallet(player_id: &str, currency: &str, delta: i64, balance_after: i64, reason: &str, ledger_id: &str) -> Value {
    payload(walletevents::Changed {
        player_id: player_id.into(),
        currency: currency.into(),
        delta,
        balance_after,
        reason: reason.into(),
        ledger_id: ledger_id.into(),
    })
}

async fn test_pool() -> Option<PgPool> {
    let pool = testdb::test_pool().await?;
    Audit::new()
        .migrate(&Context::with_db(pool.clone()))
        .await
        .expect("migrate audit schema");
    Some(pool)
}

/// A per-test-unique marker, placed in every fixture's ids so a search finds only them.
async fn marker(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT 'mk' || replace(gen_random_uuid()::text, '-', '')")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Inserts one ledger row `age_s` seconds old and returns its id.
async fn insert(pool: &PgPool, topic: &str, payload: &Value, age_s: f64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit.log (topic, payload, at) \
         VALUES ($1, $2::jsonb, now() - make_interval(secs => $3)) RETURNING id",
    )
    .bind(topic)
    .bind(payload.to_string())
    .bind(age_s)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn delete(pool: &PgPool, ids: &[i64]) {
    sqlx::query("DELETE FROM audit.log WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
}

/// The derived `(time, actor, topic, action, target, detail)` of one row, through the
/// same SQL the readers use.
async fn derived(pool: &PgPool, id: i64) -> (String, String, String, String, String, String) {
    sqlx::query_as(&format!(
        "SELECT at_text, actor, topic, action, target, detail FROM ({}) r WHERE id = $1",
        derived_rows()
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn utc_today(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT (now() AT TIME ZONE 'UTC')::date::text")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn filter<'a>(topic: Option<&'a str>, actor: Option<&'a str>, search: &'a str) -> LogFilter<'a> {
    LogFilter { topic, actor, search: Some(search) }
}

#[test]
fn topic_options_are_the_logged_topics() {
    assert_eq!(topic_options(), DURABLE_TOPICS);
    assert_eq!(topic_options().len(), 16);
}

#[test]
fn contains_pattern_takes_the_needle_literally() {
    assert_eq!(contains_pattern("ab"), "%ab%");
    assert_eq!(contains_pattern(r"a%b_c\d"), r"%a\%b\_c\\d%");
}

/// H11: each of the 16 topics derives actor, action, target and detail from its payload
/// (field names from `api/*/events` and `controlplane::events::AdminAction`); the config
/// value never shows.
#[tokio::test(flavor = "multi_thread")]
async fn derives_every_logged_topic() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let p = format!("p-{m}");
    let cases: Vec<(&str, Value, [String; 4])> = vec![
        (
            "admin.action",
            admin(&format!("op-{m}"), "wallet.credit", &format!("player:{p}"), "ok"),
            [format!("op-{m}"), "wallet.credit".into(), format!("player:{p}"), "ok".into()],
        ),
        (
            "character.created",
            payload(charactersevents::Created {
                character_id: format!("c-{m}"),
                player_id: p.clone(),
                name: "Aria".into(),
                class: "mage".into(),
            }),
            [SYSTEM_ACTOR.into(), "created".into(), format!("character:c-{m}"), "Aria · mage".into()],
        ),
        (
            "character.deleted",
            payload(charactersevents::Deleted { character_id: format!("c-{m}"), player_id: p.clone() }),
            [SYSTEM_ACTOR.into(), "deleted".into(), format!("character:c-{m}"), format!("player:{p}")],
        ),
        (
            "player.registered",
            payload(accountsevents::PlayerRegistered {
                player_id: p.clone(),
                display_name: "Neo".into(),
                provider: "guest".into(),
            }),
            [SYSTEM_ACTOR.into(), "registered".into(), format!("player:{p}"), "Neo · guest".into()],
        ),
        (
            "player.promoted",
            payload(accountsevents::PlayerPromoted {
                player_id: p.clone(),
                from_provider: "guest".into(),
                to_provider: "epic".into(),
            }),
            [SYSTEM_ACTOR.into(), "promoted".into(), format!("player:{p}"), "guest → epic".into()],
        ),
        (
            "player.deleted",
            payload(accountsevents::PlayerDeleted { player_id: p.clone(), handle: "Neo#1234".into() }),
            [SYSTEM_ACTOR.into(), "deleted".into(), format!("player:{p}"), "Neo#1234".into()],
        ),
        (
            "config.changed",
            payload(configevents::Changed {
                namespace: format!("ns-{m}"),
                key: "motd".into(),
                value: Some("SECRET-VALUE".into()),
                operation: "update".into(),
                revision: 42,
            }),
            [SYSTEM_ACTOR.into(), "changed".into(), format!("ns-{m}/motd"), "update · rev 42".into()],
        ),
        (
            "match.finished",
            payload(matchevents::Finished { match_id: format!("mt-{m}"), winner: "red".into(), loser: "blue".into() }),
            [SYSTEM_ACTOR.into(), "finished".into(), format!("match:mt-{m}"), "winner red · loser blue".into()],
        ),
        (
            "wallet.changed",
            wallet(&p, "gold", 50, 150, "quest", "l1"),
            [SYSTEM_ACTOR.into(), "changed".into(), format!("player:{p}"), "+50 gold · quest".into()],
        ),
        (
            "friend.requested",
            payload(friendsevents::Requested {
                edge_id: format!("e-{m}"),
                requester_id: p.clone(),
                requester_handle: "A#1".into(),
                addressee_id: "q".into(),
                addressee_handle: "B#2".into(),
            }),
            [SYSTEM_ACTOR.into(), "requested".into(), format!("friendship:e-{m}"), "A#1 → B#2".into()],
        ),
        (
            "friend.accepted",
            payload(friendsevents::Accepted {
                edge_id: format!("e-{m}"),
                requester_id: p.clone(),
                requester_handle: "A#1".into(),
                addressee_id: "q".into(),
                addressee_handle: "B#2".into(),
            }),
            [SYSTEM_ACTOR.into(), "accepted".into(), format!("friendship:e-{m}"), "A#1 → B#2".into()],
        ),
        (
            "friend.removed",
            payload(friendsevents::Removed {
                edge_id: format!("e-{m}"),
                actor_id: p.clone(),
                actor_handle: "A#1".into(),
                other_id: "q".into(),
                other_handle: "B#2".into(),
                reason: friendsevents::REASON_UNFRIENDED.into(),
            }),
            [SYSTEM_ACTOR.into(), "removed".into(), format!("friendship:e-{m}"), "unfriended · A#1 → B#2".into()],
        ),
        (
            "group.created",
            payload(groupsevents::Created {
                group_id: format!("g-{m}"),
                name: "Guild".into(),
                creator_id: p.clone(),
                join_policy: "open".into(),
            }),
            [SYSTEM_ACTOR.into(), "created".into(), format!("group:g-{m}"), "Guild · open".into()],
        ),
        (
            "group.member_joined",
            payload(groupsevents::MemberJoined { group_id: format!("g-{m}"), player_id: p.clone(), role: "member".into() }),
            [SYSTEM_ACTOR.into(), "member_joined".into(), format!("group:g-{m}"), format!("player:{p} · member")],
        ),
        (
            "group.member_left",
            payload(groupsevents::MemberLeft {
                group_id: format!("g-{m}"),
                player_id: p.clone(),
                actor_id: p.clone(),
                reason: "left".into(),
            }),
            [SYSTEM_ACTOR.into(), "member_left".into(), format!("group:g-{m}"), format!("player:{p} · left")],
        ),
        (
            "group.role_changed",
            payload(groupsevents::RoleChanged {
                group_id: format!("g-{m}"),
                player_id: p.clone(),
                role: "officer".into(),
                actor_kind: "operator".into(),
                actor_id: "op".into(),
            }),
            [SYSTEM_ACTOR.into(), "role_changed".into(), format!("group:g-{m}"), format!("player:{p} → officer")],
        ),
    ];
    let covered: HashSet<&str> = cases.iter().map(|(t, _, _)| *t).collect();
    let logged: HashSet<&str> = DURABLE_TOPICS.iter().copied().collect();
    assert_eq!(covered, logged, "every logged topic has a derivation case");

    let mut ids = Vec::new();
    for (topic, payload, _) in &cases {
        ids.push(insert(&pool, topic, payload, 0.0).await);
    }
    // A negative wallet delta keeps its own sign.
    let debit = wallet(&p, "gems", -20, 0, "shop", "l2");
    let debit_id = insert(&pool, "wallet.changed", &debit, 0.0).await;
    let mut got = Vec::new();
    for id in &ids {
        got.push(derived(&pool, *id).await);
    }
    let debit_row = derived(&pool, debit_id).await;
    ids.push(debit_id);
    delete(&pool, &ids).await;

    for ((topic, _, want), (_, actor, got_topic, action, target, detail)) in cases.iter().zip(&got) {
        assert_eq!(got_topic, topic);
        assert_eq!([actor, action, target, detail], [&want[0], &want[1], &want[2], &want[3]], "{topic}");
        assert!(!target.contains("SECRET") && !detail.contains("SECRET"), "{topic} leaks the config value");
    }
    assert_eq!(debit_row.5, "-20 gems · shop");
}

/// D7: a `friend.removed` with `reason = "operator-removed"` reads "removed by operator" and
/// names both players (neither acted); every other reason keeps `<reason> · <actor> → <other>`.
#[tokio::test(flavor = "multi_thread")]
async fn friend_removed_operator_reason_names_both_players() {
    assert_eq!(OPERATOR_REMOVED, friendsevents::REASON_OPERATOR_REMOVED, "audit's literal drifted");
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let removed = |reason: &str| {
        payload(friendsevents::Removed {
            edge_id: format!("e-{m}"),
            actor_id: "p1".into(),
            actor_handle: "A#1".into(),
            other_id: "p2".into(),
            other_handle: "B#2".into(),
            reason: reason.into(),
        })
    };
    let operator = insert(&pool, "friend.removed", &removed(friendsevents::REASON_OPERATOR_REMOVED), 0.0).await;
    let unfriended = insert(&pool, "friend.removed", &removed(friendsevents::REASON_UNFRIENDED), 0.0).await;
    let (operator_row, unfriended_row) = (derived(&pool, operator).await, derived(&pool, unfriended).await);
    delete(&pool, &[operator, unfriended]).await;

    assert_eq!(operator_row.5, "removed by operator · A#1 ↔ B#2");
    assert_eq!(operator_row.3, "removed");
    assert_eq!(operator_row.4, format!("friendship:e-{m}"));
    assert_eq!(unfriended_row.5, "unfriended · A#1 → B#2");
}

/// H11: a payload missing its keys, with empty keys, or not an object at all derives "—"
/// for each missing value instead of failing the read; a topic with no derivation arm
/// shows "—" target and detail.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_payloads_derive_dashes() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let shapes = [json!({}), json!([1, 2]), json!("text"), json!(null), json!(7)];
    let mut ids = Vec::new();
    for topic in DURABLE_TOPICS {
        for shape in &shapes {
            ids.push((*topic, insert(&pool, topic, shape, 0.0).await));
        }
    }
    let unknown = format!("test.{m}");
    ids.push((unknown.as_str(), insert(&pool, &unknown, &json!({"player_id": "x"}), 0.0).await));
    let empty_admin = admin("", "", "", "");
    ids.push(("admin.action", insert(&pool, "admin.action", &empty_admin, 0.0).await));
    let mut got = Vec::new();
    for (topic, id) in &ids {
        got.push((*topic, derived(&pool, *id).await));
    }
    delete(&pool, &ids.iter().map(|(_, id)| *id).collect::<Vec<_>>()).await;

    for (topic, (_, actor, _, action, target, detail)) in got {
        assert_eq!(target, MISSING, "{topic}");
        // A summary keeps its separators and literal words; every payload value is "—".
        assert!(detail.contains(MISSING), "{topic}: {detail}");
        for word in detail.split([' ', '·', '→']).filter(|w| !w.is_empty()) {
            assert!(
                [MISSING, "rev", "winner", "loser"].contains(&word),
                "{topic}: unexpected {word:?} in {detail:?}"
            );
        }
        if topic == "admin.action" {
            assert_eq!((actor.as_str(), action.as_str(), detail.as_str()), (MISSING, MISSING, MISSING));
        } else {
            assert_eq!(actor, SYSTEM_ACTOR, "{topic}");
            assert_eq!(action, topic.split_once('.').unwrap().1, "{topic}");
        }
    }
}

/// H12: a row of today's UTC day shows `HH:MM:SS`, an older one `Mon DD HH:MM`, both
/// UTC. Retried when the UTC day turns during the test, so no real-clock race decides it.
#[tokio::test(flavor = "multi_thread")]
async fn time_shows_clock_today_and_date_before() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    for _ in 0..3 {
        let day = utc_today(&pool).await;
        let payload = admin(&format!("op-{m}"), "a", "t", "d");
        let today = insert(&pool, "admin.action", &payload, 0.0).await;
        let older = insert(&pool, "admin.action", &payload, 3.0 * 86400.0).await;
        let (today_text, older_text) = (derived(&pool, today).await.0, derived(&pool, older).await.0);
        let want: (String, String) = sqlx::query_as(
            "SELECT (SELECT to_char(at AT TIME ZONE 'UTC', 'HH24:MI:SS') FROM audit.log WHERE id = $1), \
                    (SELECT to_char(at AT TIME ZONE 'UTC', 'Mon DD HH24:MI') FROM audit.log WHERE id = $2)",
        )
        .bind(today)
        .bind(older)
        .fetch_one(&pool)
        .await
        .unwrap();
        let turned = utc_today(&pool).await != day;
        delete(&pool, &[today, older]).await;
        if turned {
            continue;
        }
        assert_eq!((today_text.clone(), older_text.clone()), want);
        let clock = |s: &str| s.len() == 8 && s.chars().enumerate().all(|(i, c)| if i == 2 || i == 5 { c == ':' } else { c.is_ascii_digit() });
        assert!(clock(&today_text), "{today_text}");
        let (mon, rest) = older_text.split_at(3);
        assert!(mon.chars().all(|c| c.is_ascii_alphabetic()), "{older_text}");
        assert_eq!(rest.len(), 9, "{older_text}");
        return;
    }
    panic!("the UTC day turned on every attempt");
}

/// The pager walks every matching row exactly once, newest first, `id` breaking a tie on
/// `at`, and the last page has no `next`.
#[tokio::test(flavor = "multi_thread")]
async fn keyset_pager_walks_every_row_once() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let actor = format!("op-{m}");
    let payload = admin(&actor, "a", &m, "d");
    let mut ids = Vec::new();
    for age in [240.0, 180.0, 120.0, 120.0, 60.0] {
        ids.push(insert(&pool, "admin.action", &payload, age).await);
    }
    // Two rows share one `at`, so only `id` orders them.
    sqlx::query("UPDATE audit.log SET at = (SELECT at FROM audit.log WHERE id = $1) WHERE id = $2")
        .bind(ids[2])
        .bind(ids[3])
        .execute(&pool)
        .await
        .unwrap();
    let store = Store::new(pool.clone());
    let f = filter(None, Some(actor.as_str()), &m);
    let mut walked = Vec::new();
    let mut sizes = Vec::new();
    let mut after = None;
    loop {
        let page = store.log_page(&f, after, 2).await.unwrap();
        sizes.push(page.rows.len());
        walked.extend(page.rows.iter().map(|r| r.id));
        match page.next {
            Some(next) => {
                assert_eq!(Some(next), page.rows.last().map(LogRow::cursor));
                after = Some(next);
            }
            None => break,
        }
        assert!(sizes.len() < 10, "pager does not terminate");
    }
    let total = store.log_total(&f).await.unwrap();
    let full = store.log_page(&f, None, PAGE_SIZE).await.unwrap();
    delete(&pool, &ids).await;

    assert_eq!(walked, vec![ids[4], ids[3], ids[2], ids[1], ids[0]]);
    assert_eq!(sizes, vec![2, 2, 1]);
    assert_eq!(total, 5);
    assert_eq!(full.rows.len(), 5);
    assert_eq!(full.next, None);
}

/// Topic, actor (an operator login or `system`) and search (target or detail, case
/// insensitive, `%`/`_` literal) each narrow the page, alone and together, and the
/// total counts exactly what the filters match.
#[tokio::test(flavor = "multi_thread")]
async fn filters_narrow_the_page_and_the_total() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let (op, op2) = (format!("op-{m}"), format!("op2-{m}"));
    let r1 = insert(&pool, "admin.action", &admin(&op, "wallet.credit", &format!("player:alpha-{m}"), "ok"), 40.0).await;
    let r2 = insert(&pool, "admin.action", &admin(&op2, "x", "", &format!("failed: beta-{m}")), 30.0).await;
    let r3 = insert(&pool, "wallet.changed", &wallet(&format!("alpha-{m}"), "gold", 5, 5, "q", "l"), 20.0).await;
    let r4 = insert(&pool, "group.created", &payload(groupsevents::Created {
            group_id: format!("gamma-{m}"),
            name: format!("BETA-{m} guild"),
            creator_id: "c".into(),
            join_policy: "open".into(),
        }), 10.0).await;
    let store = Store::new(pool.clone());
    let upper_alpha = format!("ALPHA-{}", m.to_uppercase());
    let beta = format!("beta-{m}");
    let literal_pct = format!("{m}%");
    let cases: Vec<(LogFilter, Vec<i64>)> = vec![
        (filter(None, None, &m), vec![r4, r3, r2, r1]),
        (filter(None, None, &upper_alpha), vec![r3, r1]),
        (filter(None, None, &beta), vec![r4, r2]),
        (filter(None, None, &literal_pct), vec![]),
        (filter(Some("wallet.changed"), None, &m), vec![r3]),
        (filter(Some("admin.action"), None, &m), vec![r2, r1]),
        (filter(None, Some(op.as_str()), &m), vec![r1]),
        (filter(None, Some(SYSTEM_ACTOR), &m), vec![r4, r3]),
        (filter(Some("admin.action"), Some(op2.as_str()), &beta), vec![r2]),
        (filter(Some("group.created"), Some(op.as_str()), &m), vec![]),
    ];
    let mut got = Vec::new();
    for (f, _) in &cases {
        let page = store.log_page(f, None, PAGE_SIZE).await.unwrap();
        let total = store.log_total(f).await.unwrap();
        got.push((page.rows.iter().map(|r| r.id).collect::<Vec<_>>(), total, page.rows));
    }
    let unfiltered = store.log_total(&LogFilter::default()).await.unwrap();
    delete(&pool, &[r1, r2, r3, r4]).await;

    for ((f, want), (ids, total, rows)) in cases.iter().zip(&got) {
        assert_eq!(ids, want, "{f:?}");
        assert_eq!(*total, want.len() as i64, "{f:?}");
        for row in rows {
            assert_eq!(row.is_operator(), row.actor != SYSTEM_ACTOR, "{f:?}");
        }
    }
    assert!(unfiltered >= 4);
}

/// The actor options are the distinct non-empty operator logins of `admin.action` rows,
/// sorted, then `system`; an `actor` key on another topic's payload is not an operator.
/// Assumes the shared DB holds fewer than 50 distinct operator logins (the option cap).
#[tokio::test(flavor = "multi_thread")]
async fn actor_options_are_operator_logins_then_system() {
    let Some(pool) = test_pool().await else { return };
    let m = marker(&pool).await;
    let op = format!("op-{m}");
    // Byte order differs from a linguistic collation for these: an upper-case login sorts
    // before every lower-case one, and a hyphen is not skipped ("op-z" before "opa").
    let upper = format!("Pz-{m}");
    let hyphen = format!("op-z{m}");
    let plain = format!("opa-{m}");
    let not_op = format!("notop-{m}");
    let ids = vec![
        insert(&pool, "admin.action", &admin(&op, "a", "", ""), 0.0).await,
        insert(&pool, "admin.action", &admin(&op, "b", "", ""), 0.0).await,
        insert(&pool, "admin.action", &admin("", "c", "", ""), 0.0).await,
        insert(&pool, "admin.action", &admin(&upper, "d", "", ""), 0.0).await,
        insert(&pool, "admin.action", &admin(&hyphen, "e", "", ""), 0.0).await,
        insert(&pool, "admin.action", &admin(&plain, "f", "", ""), 0.0).await,
        insert(&pool, "wallet.changed", &json!({"actor": not_op, "player_id": "x"}), 0.0).await,
    ];
    let options = Store::new(pool.clone()).actor_options().await.unwrap();
    delete(&pool, &ids).await;

    assert_eq!(options.last().map(String::as_str), Some(SYSTEM_ACTOR));
    assert_eq!(options.iter().filter(|a| **a == op).count(), 1, "{options:?}");
    assert!(!options.contains(&not_op), "{options:?}");
    assert!(!options.iter().any(String::is_empty), "{options:?}");
    for login in [&upper, &hyphen, &plain] {
        assert!(options.contains(login), "{login} missing from {options:?}");
    }
    let logins = &options[..options.len() - 1];
    let mut sorted = logins.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(logins, sorted.as_slice());
}

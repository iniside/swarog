use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::client::tests::{canned, canned_with_bodies};
use crate::report::Got;

fn client() -> Client {
    Client { nonce: "n0".to_string(), index: 0, timeout: Duration::from_secs(1) }
}

fn gateway_on(base: &str) -> Gateway {
    Gateway::new(base, 4).expect("client").scenario("client 0")
}

fn guest(id: &'static str) -> (u16, &'static str) {
    (201, id)
}

// Answers of the deletion of one account: ticket, receipt for `receipt`, then 401 on `me`.
fn deletion(receipt: &'static str) -> [(u16, &'static str); 3] {
    [(200, r#"{"ticket":"k","expires_at":"t"}"#), (200, receipt), (401, "unauthorized")]
}

const GUEST_A: &str =
    r#"{"player_id":"pa","token":"ta0","refresh_token":"ra0","access_expires_in_secs":900,"device_secret":"sa.x"}"#;
const GUEST_B: &str =
    r#"{"player_id":"pb","token":"tb0","refresh_token":"rb0","access_expires_in_secs":900,"device_secret":"sb.x"}"#;
const GUEST_C: &str =
    r#"{"player_id":"pc","token":"tc0","refresh_token":"rc0","access_expires_in_secs":900,"device_secret":"sc.x"}"#;
const RECEIPT_A: &str = r#"{"player_id":"pa","handle":"ga#1","deleted_at":"t"}"#;
const RECEIPT_B: &str = r#"{"player_id":"pb","handle":"gb#1","deleted_at":"t"}"#;
const RECEIPT_C: &str = r#"{"player_id":"pc","handle":"gc#1","deleted_at":"t"}"#;
const RECEIPT_D: &str = r#"{"player_id":"pd","handle":"Sim 0#1","deleted_at":"t"}"#;

/// Every answer of one scenario that loses nothing, in call order.
fn happy_answers() -> Vec<(u16, &'static str)> {
    let mut answers = vec![
        // Sessions: three guests, A's sign-in, refresh and link, then each handle.
        guest(GUEST_A),
        guest(GUEST_B),
        guest(GUEST_C),
        (200, r#"{"player_id":"pa","token":"ta1","refresh_token":"ra1","access_expires_in_secs":900}"#),
        (200, r#"{"player_id":"pa","token":"ta2","refresh_token":"ra2","access_expires_in_secs":900}"#),
        (200, r#"{"player_id":"pa","display_name":"guest:a","handle":"ga#1","identities":[]}"#),
        (200, r#"{"handle":"ga#1"}"#),
        (200, r#"{"handle":"gb#1"}"#),
        (200, r#"{"handle":"gc#1"}"#),
        // D registers, logs in and reads its profile.
        (201, r#"{"player_id":"pd","token":"td0","refresh_token":"rd0","access_expires_in_secs":900}"#),
        (200, r#"{"player_id":"pd","token":"td1","refresh_token":"rd1","access_expires_in_secs":900}"#),
        (200, r#"{"handle":"Sim 0#1"}"#),
        // Characters and inventory.
        (200, "{}"),
        (201, r#"{"id":"ch1","name":"Sim 0"}"#),
        (200, r#"[{"item_id":"starter_sword"}]"#),
        (204, ""),
        (200, "[]"),
        // Storage.
        (200, "{}"),
        (200, r#"{"owner_id":"pa","key":"slot","acl":"private"}"#),
        (204, ""),
        (404, "not found"),
        // Friends.
        (201, r#"{"edge_id":"e1","state":"pending"}"#),
        (204, ""),
        (201, r#"{"edge_id":"e2","state":"pending"}"#),
        (204, ""),
        (200, r#"{"items":[{"id":"n1","kind":"friend.accepted","created_at":"t","read_at":""}],"next_cursor":""}"#),
        (204, ""),
        (200, r#"{"items":[{"id":"n2","kind":"friend.requested","created_at":"t","read_at":""}],"next_cursor":""}"#),
        (204, ""),
        (204, ""),
        // Groups: open, invite, request.
        (201, r#"{"id":"g1","name":"open","my_state":"member"}"#),
        (200, r#"{"state":"member"}"#),
        (204, ""),
        (204, ""),
        (201, r#"{"id":"g2","name":"invite","my_state":"member"}"#),
        (201, ""),
        (204, ""),
        (204, ""),
        (204, ""),
        (201, r#"{"id":"g3","name":"request","my_state":"member"}"#),
        (200, r#"{"state":"requested"}"#),
        (204, ""),
        (204, ""),
        (204, ""),
        // Match.
        (202, ""),
        // Party.
        (201, r#"{"party_id":"py","leader_id":"pa","members":[{"player_id":"pa","ready":"not_ready"}]}"#),
        (201, r#"{"invite_id":"i1"}"#),
        (201, r#"{"invite_id":"i2"}"#),
        (200, r#"{"invites":[{"invite_id":"i1"}]}"#),
        (200, r#"{"party_id":"py","leader_id":"pa","members":[{"player_id":"pa","ready":"not_ready"},{"player_id":"pb","ready":"not_ready"}]}"#),
        (204, ""),
        (200, r#"{"lease_until":"t","revision":2}"#),
        (200, r#"{"party_id":"py","leader_id":"pa","members":[{"player_id":"pa","ready":"not_ready"},{"player_id":"pb","ready":"ready"}]}"#),
        (200, r#"{"party_id":"py","leader_id":"pb","members":[{"player_id":"pa","ready":"not_ready"},{"player_id":"pb","ready":"ready"}]}"#),
        (200, r#"{"party_id":"py","leader_id":"pb","members":[{"player_id":"pb","ready":"ready"}]}"#),
        (200, r#"{"party_id":"py","leader_id":"pb","members":[{"player_id":"pb","ready":"ready"}]}"#),
        (200, r#"{"party_id":"","leader_id":"","members":[]}"#),
    ];
    for receipt in [RECEIPT_A, RECEIPT_B, RECEIPT_C, RECEIPT_D] {
        answers.extend(deletion(receipt));
    }
    answers
}

fn received(lines: &mpsc::Receiver<String>) -> Vec<String> {
    lines.try_iter().collect()
}

fn assert_deletions_last(lines: &[String], accounts: usize) {
    let tail = &lines[lines.len() - 3 * accounts..];
    for (index, line) in tail.iter().enumerate() {
        let want = ["POST /accounts/delete/begin ", "POST /accounts/delete ", "GET /accounts/me "][index % 3];
        assert!(line.starts_with(want), "call {index} of the deletion tail is {line:?}, want {want:?}");
    }
    let deletions = lines.iter().filter(|line| line.starts_with("POST /accounts/delete/begin ")).count();
    assert_eq!(deletions, accounts, "every deletion is in the tail");
}

#[tokio::test]
async fn a_scenario_that_loses_nothing_covers_every_op_the_seed_can_skip_and_deletes_last() {
    let answers = happy_answers();
    let total = answers.len();
    let (base, lines) = canned_with_bodies(answers);
    let gateway = gateway_on(&base);
    run_client(&gateway, &client()).await;
    let report = gateway.take_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let lines = received(&lines);
    assert_eq!(lines.len(), total, "{lines:#?}");
    assert_deletions_last(&lines, 4);
    // The refresh runs once, on the federated sign-in's refresh token, never on an older one.
    let refreshes: Vec<&String> = lines.iter().filter(|line| line.starts_with("POST /accounts/refresh ")).collect();
    assert_eq!(refreshes.len(), 1, "{refreshes:?}");
    assert!(refreshes[0].contains(r#""refresh_token":"ra1""#), "{}", refreshes[0]);
    // Only the seed's own reads are left for the seeded phases: every op that a re-run of the
    // seed answers 409 or calls only when a row is missing answers its declared success here.
    let coverage = report.coverage(opscatalog::OPERATIONS);
    assert!(coverage.failed_only.is_empty(), "{:?}", coverage.failed_only);
    assert_eq!(
        coverage.uncovered,
        [
            "friends.list",
            "friends.pending",
            "groups.listMine",
            "groups.members",
            "groups.pending",
            "inventory.listMine",
            "leaderboard.topScores",
            "storage.list",
            "wallet.listCurrencies",
            "wallet.myBalances",
        ]
    );
}

#[tokio::test]
async fn a_lost_guest_skips_the_steps_that_need_it_and_still_deletes_the_others() {
    let mut answers = vec![
        (500, "boom"),
        guest(GUEST_B),
        guest(GUEST_C),
        (200, r#"{"handle":"gb#1"}"#),
        (200, r#"{"handle":"gc#1"}"#),
        (201, r#"{"player_id":"pd","token":"td0","refresh_token":"rd0","access_expires_in_secs":900}"#),
        (200, r#"{"player_id":"pd","token":"td1","refresh_token":"rd1","access_expires_in_secs":900}"#),
        (200, r#"{"handle":"Sim 0#1"}"#),
    ];
    for receipt in [RECEIPT_B, RECEIPT_C, RECEIPT_D] {
        answers.extend(deletion(receipt));
    }
    let total = answers.len();
    let (base, lines) = canned(answers);
    let gateway = gateway_on(&base);
    run_client(&gateway, &client()).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!((report.errors[0].op, &report.errors[0].got), ("accounts.createGuest", &Got::Status(500)));
    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.cause.as_str()).collect();
    assert_eq!(
        skipped,
        [
            "guest A skipped: its creation failed",
            "characters and storage skipped: guest A has no session",
            "friends, groups, match and party skipped: guests A, B and C do not all have a session",
        ]
    );
    let lines = received(&lines);
    assert_eq!(lines.len(), total, "{lines:#?}");
    assert_deletions_last(&lines, 3);
}

#[tokio::test]
async fn a_profile_read_that_still_answers_after_the_delete_is_an_error() {
    let (base, _lines) = canned(vec![
        (200, r#"{"ticket":"k","expires_at":"t"}"#),
        (200, RECEIPT_A),
        (200, r#"{"handle":"ga#1"}"#),
    ]);
    let gateway = gateway_on(&base);
    let account = Held { label: "A", player_id: "pa".to_string(), token: "ta".to_string() };
    delete_account(&gateway, &account).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!((report.errors[0].op, &report.errors[0].got), ("accounts.me", &Got::Status(200)));
}

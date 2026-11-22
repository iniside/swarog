use std::collections::BTreeSet;
use std::sync::mpsc;

use super::*;
use crate::readback::{self, first_unread, NotificationRow};
use crate::report::Got;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn seed_config(list: &[&str]) -> Config {
    match parse_args(&args(list)).expect("args parse") {
        Command::Seed(config) => config,
        Command::Help => panic!("expected Seed, got Help"),
    }
}

fn character(name: &str) -> CharacterPlan {
    CharacterPlan { name: name.to_string(), class: "mage".to_string() }
}

#[test]
fn plan_is_identical_across_calls() {
    let first = plan(30, 100);
    let second = plan(30, 100);
    assert_eq!(first, second);
    assert_eq!(first.players.len(), 30);
    assert_eq!(first.matches.len(), 100);
    assert!(!first.friendships.is_empty());
    assert!(!first.groups.is_empty());
}

// A change to SEED or the stream mixing must update these goldens deliberately: re-runs of the tool
// only converge because the same plan comes out of every process.
#[test]
fn plan_output_is_pinned_across_processes() {
    let seed = plan(30, 100);
    let first = &seed.matches[0];
    assert_eq!((first.report_id.as_str(), first.winner, first.loser), ("demo-match-0000", 0, 29));
    let last = &seed.matches[99];
    assert_eq!((last.report_id.as_str(), last.winner, last.loser), ("demo-match-0099", 5, 14));
    let player = &seed.players[0];
    assert_eq!(player.email, "demo-00@demo.local");
    assert_eq!(player.display_name, "Aldric");
    assert_eq!(player.characters, vec![CharacterPlan { name: "Oakenshield".to_string(), class: "ranger".to_string() }]);
    assert_eq!(player.save.value, r#"{"level":25,"playtime_secs":287755,"zone":"Emberfields"}"#);
    let group = &seed.groups[0];
    assert_eq!((group.name.as_str(), group.owner), ("Iron Vanguard", 0));
    assert_eq!(group.members, vec![19, 10, 25, 17, 1]);
}

#[test]
fn plan_is_not_constant_across_indices() {
    let seed = plan(30, 100);
    let distinct_pairs: BTreeSet<(usize, usize)> = seed.matches.iter().map(|m| (m.winner, m.loser)).collect();
    assert!(distinct_pairs.len() > 10);
    let distinct_saves: BTreeSet<&str> = seed.players.iter().map(|p| p.save.value.as_str()).collect();
    assert!(distinct_saves.len() > 10);
}

#[test]
fn plan_match_prefix_is_stable_when_count_grows() {
    let small = plan(30, 10);
    let large = plan(30, 100);
    assert_eq!(small.matches[..], large.matches[..10]);
    assert_eq!(small.players, large.players);
}

#[test]
fn matches_have_distinct_valid_participants_and_unique_report_ids() {
    for players in [2, 3, 30, 75] {
        let seed = plan(players, 200);
        assert_eq!(seed.matches.len(), 200);
        let mut ids = BTreeSet::new();
        for m in &seed.matches {
            assert_ne!(m.winner, m.loser, "players={players} {m:?}");
            assert!(m.winner < players && m.loser < players, "players={players} {m:?}");
            assert!(ids.insert(m.report_id.clone()), "duplicate report id {}", m.report_id);
        }
    }
}

#[test]
fn player_identities_and_characters_are_unique() {
    let seed = plan(75, 0);
    let emails: BTreeSet<&str> = seed.players.iter().map(|p| p.email.as_str()).collect();
    let names: BTreeSet<&str> = seed.players.iter().map(|p| p.display_name.as_str()).collect();
    assert_eq!(emails.len(), 75);
    assert_eq!(names.len(), 75);
    for player in &seed.players {
        assert!((1..=3).contains(&player.characters.len()), "{}", player.email);
        let character_names: BTreeSet<&str> = player.characters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(character_names.len(), player.characters.len(), "{}", player.email);
        assert!(player.characters.iter().all(|c| CLASSES.contains(&c.class.as_str())));
    }
}

#[test]
fn group_names_unique_and_membership_consistent() {
    for players in [1, 2, 3, 5, 30, 75] {
        let seed = plan(players, 0);
        assert_eq!(seed.groups.len(), GROUPS.len());
        let names: BTreeSet<&str> = seed.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names.len(), seed.groups.len());
        for (group, (name, policy, undecided)) in seed.groups.iter().zip(GROUPS) {
            assert_eq!(group.name, *name);
            assert_eq!(group.join_policy, *policy);
            assert!(["open", "request", "invite"].contains(&group.join_policy.as_str()));
            assert!(group.owner < players);
            let members: BTreeSet<usize> = group.members.iter().copied().collect();
            assert_eq!(members.len(), group.members.len(), "duplicate member in {}", group.name);
            assert!(!members.contains(&group.owner), "owner listed as member of {}", group.name);
            assert!(group.members.iter().all(|&m| m < players));
            assert!(group.members.len() >= 3.min(players - 1), "{} players={players}", group.name);
            assert!(group.members.len() <= 7.min(players - 1), "{} players={players}", group.name);
            let pending: BTreeSet<usize> = group.pending.iter().copied().collect();
            assert_eq!(pending.len(), group.pending.len(), "duplicate pending joiner in {}", group.name);
            assert!(!pending.contains(&group.owner), "owner pending in {}", group.name);
            assert!(pending.is_disjoint(&members), "member pending in {}", group.name);
            assert_eq!(group.pending.len(), (*undecided).min(players - 1 - members.len()), "{} players={players}", group.name);
        }
    }
}

#[test]
fn friendships_have_no_self_or_duplicate_unordered_pairs() {
    for players in [2, 3, 30, 75] {
        let seed = plan(players, 0);
        assert!(!seed.friendships.is_empty(), "players={players}");
        let mut seen = BTreeSet::new();
        for f in &seed.friendships {
            assert_ne!(f.requester, f.target, "players={players}");
            assert!(f.requester < players && f.target < players);
            assert!(
                seen.insert((f.requester.min(f.target), f.requester.max(f.target))),
                "duplicate pair {f:?} players={players}"
            );
        }
    }
}

#[test]
fn counts_honour_players_and_matches() {
    let seed = plan(7, 13);
    assert_eq!(seed.players.len(), 7);
    assert_eq!(seed.matches.len(), 13);
    assert!(plan(7, 0).matches.is_empty());
    assert_eq!(plan(40, 5).players.len(), 40);
}

#[test]
fn zero_players_yields_empty_plan() {
    let seed = plan(0, 100);
    assert!(seed.players.is_empty());
    assert!(seed.matches.is_empty());
    assert!(seed.friendships.is_empty());
    assert!(seed.groups.is_empty());
}

#[test]
fn one_player_has_no_matches_or_friends() {
    let seed = plan(1, 100);
    assert_eq!(seed.players.len(), 1);
    assert!(seed.matches.is_empty());
    assert!(seed.friendships.is_empty());
    assert!(seed.groups.iter().all(|g| g.owner == 0 && g.members.is_empty()));
}

#[test]
fn two_players_pair_only_each_other() {
    let seed = plan(2, 20);
    assert_eq!(seed.players.len(), 2);
    assert_eq!(seed.matches.len(), 20);
    assert!(seed.matches.iter().all(|m| m.winner + m.loser == 1));
    assert_eq!(seed.friendships.len(), 1);
    assert!(seed.groups.iter().all(|g| g.members.len() <= 1));
}

#[test]
fn inventory_shortfall_is_target_minus_held_never_negative() {
    assert_eq!(inventory_shortfall(100, 0), 100);
    assert_eq!(inventory_shortfall(100, 40), 60);
    assert_eq!(inventory_shortfall(100, 100), 0);
    assert_eq!(inventory_shortfall(100, 250), 0);
    assert_eq!(inventory_shortfall(0, 5), 0);
}

#[test]
fn missing_characters_skips_existing_by_name() {
    let planned = vec![character("Ashblade"), character("Kestrel"), character("Pyrewing")];
    let existing = vec!["Kestrel".to_string(), "Unrelated".to_string()];
    let missing = missing_characters(&planned, &existing);
    let names: Vec<&str> = missing.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Ashblade", "Pyrewing"]);
}

#[test]
fn missing_characters_empty_when_all_present() {
    let planned = vec![character("Ashblade"), character("Kestrel")];
    let existing = vec!["Kestrel".to_string(), "Ashblade".to_string()];
    assert!(missing_characters(&planned, &existing).is_empty());
}

#[test]
fn missing_characters_returns_all_when_none_exist() {
    let planned = vec![character("Ashblade"), character("Kestrel")];
    assert_eq!(missing_characters(&planned, &[]).len(), 2);
    assert!(missing_characters(&[], &["Kestrel".to_string()]).is_empty());
}

#[test]
fn friend_needs_accept_pending_true_accepted_false() {
    assert!(friend_needs_accept("pending").unwrap());
    assert!(!friend_needs_accept("accepted").unwrap());
}

#[test]
fn friend_needs_accept_unknown_state_errors() {
    for state in ["", "declined", "Pending", "blocked"] {
        let err = friend_needs_accept(state).unwrap_err();
        assert!(err.to_string().contains("unknown state"), "{state}: {err}");
    }
}

#[test]
fn parse_args_defaults() {
    assert_eq!(
        parse_args(&[]).unwrap(),
        Command::Seed(Config {
            base_url: DEFAULT_BASE_URL.to_string(),
            players: DEFAULT_PLAYERS,
            matches: DEFAULT_MATCHES,
            concurrency: DEFAULT_CONCURRENCY,
            event_timeout: DEFAULT_EVENT_TIMEOUT,
        })
    );
}

#[test]
fn parse_args_each_flag() {
    let config = seed_config(&["--base-url", "http://10.0.0.1:8082", "--players", "5", "--matches", "9"]);
    assert_eq!(config.base_url, "http://10.0.0.1:8082");
    assert_eq!(config.players, 5);
    assert_eq!(config.matches, 9);
    let only_players = seed_config(&["--players", "0"]);
    assert_eq!(only_players.players, 0);
    assert_eq!(only_players.matches, DEFAULT_MATCHES);
}

#[test]
fn parse_args_trims_trailing_slash() {
    assert_eq!(seed_config(&["--base-url", "http://host:1/"]).base_url, "http://host:1");
}

#[test]
fn parse_args_help_forms() {
    for flag in ["--help", "-h", "help"] {
        assert_eq!(parse_args(&args(&[flag])).unwrap(), Command::Help, "{flag}");
    }
    assert_eq!(parse_args(&args(&["--players", "3", "--help"])).unwrap(), Command::Help);
}

#[test]
fn parse_args_rejects_unknown_flag() {
    let err = parse_args(&args(&["--bogus"])).unwrap_err().to_string();
    assert!(err.contains("unknown argument"), "{err}");
}

#[test]
fn parse_args_rejects_missing_value() {
    for flag in ["--base-url", "--players", "--matches", "--concurrency"] {
        let err = parse_args(&args(&[flag])).unwrap_err().to_string();
        assert!(err.contains("requires a value"), "{flag}: {err}");
    }
}

#[test]
fn parse_args_rejects_bad_counts_and_empty_url() {
    for value in ["abc", "-1", "1.5", ""] {
        assert!(parse_args(&args(&["--players", value])).is_err(), "players {value:?}");
        assert!(parse_args(&args(&["--matches", value])).is_err(), "matches {value:?}");
    }
    assert!(parse_args(&args(&["--base-url", ""])).is_err());
    assert!(parse_args(&args(&["--base-url", "/"])).is_err());
}

#[test]
fn invite_rerun_responds_when_row_is_still_invited() {
    assert!(invite_needs_respond("invited").unwrap());
}

#[test]
fn invite_rerun_skips_respond_when_already_member() {
    assert!(!invite_needs_respond("member").unwrap());
}

#[test]
fn invite_rerun_rejects_any_other_state() {
    for state in ["requested", "", "Invited", "MEMBER", "member ", "declined", "banned"] {
        let err = invite_needs_respond(state).unwrap_err().to_string();
        assert!(err.contains("unexpected state"), "{state:?}: {err}");
    }
}

#[test]
fn parse_args_concurrency_sets_the_bound() {
    assert_eq!(seed_config(&["--concurrency", "4"]).concurrency, 4);
    assert_eq!(seed_config(&[]).concurrency, 16);
}

#[test]
fn parse_args_rejects_zero_and_non_number_concurrency() {
    for value in ["0", "abc", "-1", "1.5", ""] {
        let err = parse_args(&args(&["--concurrency", value])).unwrap_err().to_string();
        assert!(err.contains("positive integer"), "{value:?}: {err}");
    }
}

#[test]
fn usage_names_concurrency() {
    assert!(USAGE.contains("--concurrency <n>"));
}

fn seeded_player(characters: &[&str], items: &[(&str, i64)]) -> PlayerPlan {
    PlayerPlan {
        email: "demo-01@demo.local".to_string(),
        display_name: "Demo 01".to_string(),
        characters: characters.iter().map(|name| character(name)).collect(),
        items: items.iter().map(|&(item_id, qty)| ItemTarget { item_id: item_id.to_string(), qty }).collect(),
        save: save("slot1", "private"),
        public_save: save("public", "public_read"),
    }
}

fn save(key: &str, acl: &str) -> SavePlan {
    SavePlan { collection: "saves".to_string(), key: key.to_string(), value: "{}".to_string(), acl: acl.to_string() }
}

fn session() -> Session {
    Session { player_id: "p-1".to_string(), token: "t".to_string(), handle: "demo01".to_string() }
}

/// A gateway on the canned fake server; keep the receiver alive so the server thread can report.
fn fake_gateway(answers: Vec<(u16, &'static str)>) -> (Gateway, mpsc::Receiver<String>) {
    let (base, lines) = crate::client::tests::canned(answers);
    (Gateway::new(&base, 4).expect("client").scenario("player demo-01@demo.local"), lines)
}

#[tokio::test]
async fn character_create_409_after_the_list_showed_it_missing_is_an_error() {
    let (gateway, _lines) = fake_gateway(vec![(200, "[]"), (409, "character cap reached")]);
    seed_characters(&gateway, &seeded_player(&["Kestrel"], &[]), &session()).await;
    let report = gateway.take_report();
    assert_eq!(report.summary.characters, Tally::default());
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!((report.errors[0].op, &report.errors[0].got), ("characters.create", &Got::Status(409)));
}

#[tokio::test]
async fn inventory_grant_409_after_the_list_showed_a_shortfall_is_an_error() {
    let (gateway, _lines) = fake_gateway(vec![(200, "[]"), (409, "quantity ceiling")]);
    seed_inventory(&gateway, &seeded_player(&[], &[("sword", 2)]), &session()).await;
    let report = gateway.take_report();
    assert_eq!(report.summary.grants, Tally::default());
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!((report.errors[0].op, &report.errors[0].got), ("inventory.grant", &Got::Status(409)));
}

#[tokio::test]
async fn failed_character_list_records_the_skipped_creates() {
    let (gateway, _lines) = fake_gateway(vec![(500, "boom")]);
    seed_characters(&gateway, &seeded_player(&["Kestrel"], &[]), &session()).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "characters.list");
    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.cause.as_str()).collect();
    assert_eq!(skipped, ["character creation skipped: the character list failed"]);
    assert_eq!(report.skipped[0].scenario, "player demo-01@demo.local");
}

#[tokio::test]
async fn failed_friend_request_records_the_skipped_accept() {
    let (gateway, _lines) = fake_gateway(vec![(500, "boom")]);
    let target = Session { player_id: "p-2".to_string(), token: "t2".to_string(), handle: "demo02".to_string() };
    seed_friendship(&gateway, &session(), &target, true).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.cause.as_str()).collect();
    assert_eq!(skipped, ["friend accept skipped: the friend request failed"]);
}

#[test]
fn pending_friendships_are_disjoint_from_accepted_ones_as_unordered_pairs() {
    for players in [2, 3, 5, 30, 75] {
        let seed = plan(players, 0);
        let accepted: BTreeSet<(usize, usize)> =
            seed.friendships.iter().map(|f| (f.requester.min(f.target), f.requester.max(f.target))).collect();
        let mut pending = BTreeSet::new();
        for f in &seed.pending_friendships {
            assert_ne!(f.requester, f.target, "players={players}");
            assert!(f.requester < players && f.target < players);
            let pair = (f.requester.min(f.target), f.requester.max(f.target));
            assert!(!accepted.contains(&pair), "pending {f:?} is an accepted pair, players={players}");
            assert!(pending.insert(pair), "pending {f:?} repeats or reverses another, players={players}");
        }
    }
    assert_eq!(plan(30, 0).pending_friendships.len(), PENDING_FRIEND_REQUESTS);
    assert!(plan(2, 0).pending_friendships.is_empty(), "the only pair of two players is an accepted one");
}

#[test]
fn new_plan_parts_are_deterministic_and_present() {
    let (first, second) = (plan(30, 100), plan(30, 100));
    assert_eq!(first.pending_friendships, second.pending_friendships);
    assert_eq!(first.groups, second.groups);
    let undecided: Vec<&GroupPlan> = first.groups.iter().filter(|g| !g.pending.is_empty()).collect();
    assert_eq!(undecided.len(), 1);
    assert_eq!((undecided[0].join_policy.as_str(), undecided[0].pending.len()), ("request", 1));
    for player in &first.players {
        assert_eq!((player.save.acl.as_str(), player.public_save.acl.as_str()), ("private", "public_read"));
        assert_ne!(player.save.key, player.public_save.key, "{}", player.email);
    }
}

#[test]
fn the_public_save_reader_is_never_the_owner() {
    assert_eq!(public_save_reader(0, 0), None);
    assert_eq!(public_save_reader(0, 1), None);
    for players in 2..40 {
        for owner in 0..players {
            let reader = public_save_reader(owner, players).expect("two or more players have a reader");
            assert_ne!(reader, owner, "players={players}");
            assert!(reader < players);
        }
    }
}

#[test]
fn top_winner_takes_the_most_wins_and_the_lowest_index_on_a_tie() {
    let mut seed = plan(4, 0);
    assert_eq!(top_winner(&seed), None);
    let won = |winner, loser| MatchPlan { report_id: String::new(), winner, loser };
    seed.matches = vec![won(3, 0), won(2, 0), won(3, 1), won(2, 1)];
    assert_eq!(top_winner(&seed), Some(2));
    seed.matches.push(won(3, 2));
    assert_eq!(top_winner(&seed), Some(3));
}

fn notification(id: &str, kind: &str, created_at: &str, read_at: &str) -> NotificationRow {
    NotificationRow { id: id.to_string(), kind: kind.to_string(), created_at: created_at.to_string(), read_at: read_at.to_string() }
}

#[test]
fn mark_read_selection_is_the_oldest_unread_row_whatever_the_order() {
    let rows = vec![
        notification("n3", "friend.accepted", "2026-11-22T10:00:03Z", ""),
        notification("n1", "friend.requested", "2026-11-22T10:00:01Z", "2026-11-22T11:00:00Z"),
        notification("n2b", "friend.accepted", "2026-11-22T10:00:02Z", ""),
        notification("n2a", "wallet.credit", "2026-11-22T10:00:02Z", ""),
    ];
    let mut reversed = rows.clone();
    reversed.reverse();
    assert_eq!(first_unread(&rows).map(|r| r.id.as_str()), Some("n2a"));
    assert_eq!(first_unread(&reversed).map(|r| r.id.as_str()), Some("n2a"));
    assert_eq!(first_unread(&rows[1..2]), None, "a read row is never picked");
    assert_eq!(first_unread(&[]), None);
}

#[test]
fn every_expect_list_holds_its_ops_declared_success() {
    let source = include_str!("expect.rs");
    let consts = source.lines().filter(|line| line.starts_with("pub const ") && line.contains(": &[u16] = ")).count();
    assert_eq!(consts, expect::ALL.len(), "expect.rs: ALL must pair every const with its op");
    for (op, list) in expect::ALL {
        crate::client::check_expect(op, crate::client::route(op), list);
    }
}

#[test]
fn parse_args_event_timeout_sets_the_poll_deadline() {
    assert_eq!(seed_config(&["--event-timeout", "3"]).event_timeout, Duration::from_secs(3));
    assert!(parse_args(&args(&["--event-timeout", "0"])).is_err());
    assert!(parse_args(&args(&["--event-timeout", "soon"])).is_err());
    assert!(USAGE.contains("--event-timeout"));
}

fn target() -> Session {
    Session { player_id: "p-2".to_string(), token: "t2".to_string(), handle: "demo02".to_string() }
}

fn requests(lines: &mpsc::Receiver<String>) -> Vec<String> {
    lines.try_iter().collect()
}

#[tokio::test]
async fn a_pending_request_missing_from_the_targets_list_is_an_error_and_skips_the_accept() {
    let (gateway, lines) = fake_gateway(vec![
        (201, r#"{"edge_id":"e1","state":"pending"}"#),
        (200, r#"{"items":[],"next_cursor":""}"#),
    ]);
    assert_eq!(seed_friendship(&gateway, &session(), &target(), true).await, None);
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "friends.pending");
    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.cause.as_str()).collect();
    assert_eq!(skipped, ["friend accept skipped: the pending request is not in the target's list"]);
    assert_eq!(requests(&lines), ["POST /friends/requests HTTP/1.1", "POST /friends/requests/list HTTP/1.1"]);
}

#[tokio::test]
async fn an_accepted_request_lists_both_sides_and_owes_the_requester_an_inbox_row() {
    let (gateway, lines) = fake_gateway(vec![
        (201, r#"{"edge_id":"e1","state":"pending"}"#),
        (200, r#"{"items":[{"player_id":"p-1","edge_id":"e1","direction":"incoming"}],"next_cursor":""}"#),
        (204, ""),
        (200, r#"{"items":[{"player_id":"p-2","edge_id":"e1","direction":"outgoing"}],"next_cursor":""}"#),
        (200, r#"{"items":[{"player_id":"p-1","edge_id":"e1","direction":"incoming"}],"next_cursor":""}"#),
    ]);
    assert_eq!(seed_friendship(&gateway, &session(), &target(), true).await, Some(true));
    let report = gateway.take_report();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        requests(&lines),
        [
            "POST /friends/requests HTTP/1.1",
            "POST /friends/requests/list HTTP/1.1",
            "POST /friends/requests/e1/accept HTTP/1.1",
            "POST /friends/list HTTP/1.1",
            "POST /friends/list HTTP/1.1",
        ]
    );
}

#[tokio::test]
async fn a_planned_pending_request_is_checked_and_never_accepted() {
    let (gateway, lines) = fake_gateway(vec![
        (201, r#"{"edge_id":"e1","state":"pending"}"#),
        (200, r#"{"items":[{"player_id":"p-1","edge_id":"e1","direction":"incoming"}],"next_cursor":""}"#),
    ]);
    assert_eq!(seed_friendship(&gateway, &session(), &target(), false).await, Some(false));
    assert!(gateway.take_report().errors.is_empty());
    assert_eq!(requests(&lines).len(), 2, "no accept and no friend list for a pending pair");
}

#[tokio::test]
async fn a_rerun_on_an_accepted_edge_skips_the_pending_check_and_lists_both_sides() {
    let (gateway, lines) = fake_gateway(vec![
        (201, r#"{"edge_id":"e1","state":"accepted"}"#),
        (200, r#"{"items":[{"player_id":"p-2","edge_id":"e1","direction":"outgoing"}],"next_cursor":""}"#),
        (200, r#"{"items":[],"next_cursor":""}"#),
    ]);
    assert_eq!(seed_friendship(&gateway, &session(), &target(), true).await, Some(false));
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "friends.list", "the target's list lacks the requester");
    assert_eq!(
        requests(&lines),
        ["POST /friends/requests HTTP/1.1", "POST /friends/list HTTP/1.1", "POST /friends/list HTTP/1.1"]
    );
}

#[tokio::test]
async fn the_inbox_polls_for_the_owed_row_then_marks_the_oldest_unread_read() {
    let (gateway, lines) = fake_gateway(vec![
        (200, r#"{"items":[],"next_cursor":""}"#),
        (
            200,
            r#"{"items":[{"id":"n2","kind":"friend.accepted","created_at":"2026-11-22T10:00:02Z","read_at":""},{"id":"n1","kind":"friend.requested","created_at":"2026-11-22T10:00:01Z","read_at":""}],"next_cursor":""}"#,
        ),
        (204, ""),
    ]);
    readback::inbox(&gateway, &session(), 1, Duration::from_secs(10)).await;
    assert!(gateway.take_report().errors.is_empty());
    assert_eq!(
        requests(&lines),
        ["POST /notifications/list HTTP/1.1", "POST /notifications/list HTTP/1.1", "POST /notifications/n1/read HTTP/1.1"]
    );
}

#[tokio::test]
async fn an_owed_inbox_row_that_never_shows_is_a_timeout_naming_the_op() {
    let (gateway, _lines) = fake_gateway(vec![(200, r#"{"items":[],"next_cursor":""}"#)]);
    readback::inbox(&gateway, &session(), 1, Duration::ZERO).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "notifications.list");
    assert!(matches!(&report.errors[0].got, Got::Timeout(what) if what.contains("friend.accepted")));
}

#[tokio::test]
async fn a_leaderboard_without_the_top_winner_is_a_timeout() {
    let (gateway, _lines) = fake_gateway(vec![(200, r#"[{"player":"p-9","wins":3}]"#)]);
    readback::leaderboard(&gateway, "p-1", Duration::ZERO).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "leaderboard.topScores");
    assert!(matches!(&report.errors[0].got, Got::Timeout(what) if what.contains("p-1")));
}

#[tokio::test]
async fn the_starter_item_is_polled_until_it_shows() {
    let (gateway, lines) = fake_gateway(vec![(200, "[]"), (200, r#"[{"item_id":"starter_sword"}]"#)]);
    let characters = [CharacterRow { id: "c1".to_string(), name: "Kestrel".to_string() }];
    readback::starter_items(&gateway, &session(), &characters, Duration::from_secs(10)).await;
    assert!(gateway.take_report().errors.is_empty());
    assert_eq!(requests(&lines), ["GET /inventory/character/c1 HTTP/1.1", "GET /inventory/character/c1 HTTP/1.1"]);
}

#[tokio::test]
async fn an_empty_currency_catalog_names_the_dev_flag() {
    let (gateway, _lines) = fake_gateway(vec![(200, "[]"), (200, "[]")]);
    readback::wallet(&gateway, &session()).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "wallet.listCurrencies");
    assert!(matches!(&report.errors[0].got, Got::Shape(what) if what.contains("WALLET_DEV_SEED")));
}

#[tokio::test]
async fn the_owner_reads_a_pending_join_request_in_the_groups_pending_list() {
    let (gateway, _lines) = fake_gateway(vec![
        (200, r#"{"items":[{"player_id":"p-1","state":"member"}],"next_cursor":""}"#),
        (200, r#"{"items":[{"player_id":"p-3","state":"invited"}],"next_cursor":""}"#),
    ]);
    let asker = Session { player_id: "p-2".to_string(), token: "t2".to_string(), handle: "demo02".to_string() };
    let owner = session();
    readback::group_rows(&gateway, "g1", &owner, &[&owner], &[&asker]).await;
    let report = gateway.take_report();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].op, "groups.pending");
}

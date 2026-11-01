use std::collections::BTreeSet;

use super::*;

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
        for (group, (name, policy)) in seed.groups.iter().zip(GROUPS) {
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
fn classify_success_range_is_created() {
    for status in [200, 201, 202, 204, 299] {
        assert_eq!(classify(status), Ok(Outcome::Created), "{status}");
    }
}

#[test]
fn classify_409_is_already_exists() {
    assert_eq!(classify(409), Ok(Outcome::AlreadyExists));
}

#[test]
fn classify_429_is_retry() {
    assert_eq!(classify(429), Err(Rejection::Retry));
}

#[test]
fn classify_other_statuses_fail_with_code() {
    for status in [199, 300, 400, 401, 404, 500, 503] {
        assert_eq!(classify(status), Err(Rejection::Fail(status)), "{status}");
    }
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
    for flag in ["--base-url", "--players", "--matches"] {
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

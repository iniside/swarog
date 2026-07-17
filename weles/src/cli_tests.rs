use super::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn up_defaults_to_split() {
    let cmd = parse(args(&["up"])).unwrap();
    assert_eq!(cmd, Command::Up { topology: Topology::Split });
}

#[test]
fn up_monolith() {
    let cmd = parse(args(&["up", "monolith"])).unwrap();
    assert_eq!(cmd, Command::Up { topology: Topology::Monolith });
}

#[test]
fn up_rejects_duplicate_topology() {
    assert!(parse(args(&["up", "split", "monolith"])).is_err());
}

#[test]
fn up_rejects_the_removed_skip_build_flag() {
    // `--skip-build` no longer exists — weles never builds, so there is nothing
    // to skip. It must parse as an unknown argument, not silently succeed.
    assert!(parse(args(&["up", "--skip-build"])).is_err());
}

#[test]
fn up_accepts_the_borrowed_lease_marker_a_lender_appends() {
    // The branch that used to be wrong: `spawn_borrower` APPENDS this to the
    // child's argv, so before this arm existed `weles up split` under a lender
    // died in the parser with "unknown argument" — and `lock::acquire_or_borrow`
    // (which reads the same marker off `args_os`) was unreachable from the only
    // verb that takes a lease. Both positions, because the marker lands after
    // whatever the parent wrote.
    //
    // WHAT THIS TEST CANNOT DO, and where that lives instead: it parses weles's
    // OWN copy of the marker, so it stays green if processctl RENAMES the
    // argument weles is hand-copying — which re-creates the exact bug above,
    // silently. Zero-sharing means this crate cannot see processctl to compare;
    // verifyctl's `weles-wire-contract` stage can, and does
    // (`borrow_marker_diffs`). This test pins the parser; that one pins the
    // spelling.
    assert_eq!(
        parse(args(&["up", "split", crate::lock::BORROWED_LEASE_ARG])).unwrap(),
        Command::Up { topology: Topology::Split }
    );
    assert_eq!(
        parse(args(&["up", crate::lock::BORROWED_LEASE_ARG, "monolith"])).unwrap(),
        Command::Up { topology: Topology::Monolith }
    );
    assert_eq!(
        parse(args(&["up", crate::lock::BORROWED_LEASE_ARG])).unwrap(),
        Command::Up { topology: Topology::Split }
    );
}

#[test]
fn only_up_tolerates_the_borrowed_lease_marker() {
    // Narrow on purpose: `up` is the one rollout-bearing verb, so it is the one
    // that can be lent a lease. On any other verb the marker means the caller is
    // confused about what it spawned — say so rather than run something that
    // will never consume the credential it was handed.
    for verb in ["status", "down"] {
        assert!(parse(args(&[verb, crate::lock::BORROWED_LEASE_ARG])).is_err());
    }
    assert!(parse(args(&["deploy", "dir", crate::lock::BORROWED_LEASE_ARG])).is_err());
}

#[test]
fn deploy_parses_with_src_dir() {
    let cmd = parse(args(&["deploy", "some/build/out"])).unwrap();
    assert_eq!(
        cmd,
        Command::Deploy { src_dir: "some/build/out".to_string() }
    );
}

#[test]
fn deploy_requires_a_src_dir() {
    let err = parse(args(&["deploy"])).unwrap_err();
    assert!(err.to_string().contains("USAGE"), "must print USAGE: {err}");
}

#[test]
fn deploy_rejects_trailing_args() {
    assert!(parse(args(&["deploy", "dir-a", "dir-b"])).is_err());
}

#[test]
fn status_parses() {
    assert_eq!(parse(args(&["status"])).unwrap(), Command::Status);
}

#[test]
fn status_rejects_trailing_args() {
    assert!(parse(args(&["status", "extra"])).is_err());
}

#[test]
fn down_parses() {
    assert_eq!(parse(args(&["down"])).unwrap(), Command::Down);
}

#[test]
fn down_rejects_trailing_args() {
    assert!(parse(args(&["down", "extra"])).is_err());
}

#[test]
fn test_child_defaults() {
    let cmd = parse(args(&["__test-child"])).unwrap();
    assert_eq!(
        cmd,
        Command::TestChild {
            spawn_grandchild: false,
            ignore_graceful: false,
            stubborn_grandchild: false,
        }
    );
}

#[test]
fn test_child_spawn_grandchild() {
    let cmd = parse(args(&["__test-child", "--spawn-grandchild"])).unwrap();
    assert_eq!(
        cmd,
        Command::TestChild {
            spawn_grandchild: true,
            ignore_graceful: false,
            stubborn_grandchild: false,
        }
    );
}

#[test]
fn test_child_ignore_graceful() {
    let cmd = parse(args(&["__test-child", "--ignore-graceful"])).unwrap();
    assert_eq!(
        cmd,
        Command::TestChild {
            spawn_grandchild: false,
            ignore_graceful: true,
            stubborn_grandchild: false,
        }
    );
}

#[test]
fn test_child_stubborn_grandchild() {
    let cmd = parse(args(&["__test-child", "--stubborn-grandchild"])).unwrap();
    assert_eq!(
        cmd,
        Command::TestChild {
            spawn_grandchild: false,
            ignore_graceful: false,
            stubborn_grandchild: true,
        }
    );
}

#[test]
fn missing_command_errors_with_usage() {
    let err = parse(Vec::<String>::new()).unwrap_err();
    assert!(err.to_string().contains("USAGE"));
}

#[test]
fn unknown_command_errors() {
    assert!(parse(args(&["frobnicate"])).is_err());
}

#[test]
fn unknown_up_argument_errors() {
    assert!(parse(args(&["up", "--bogus"])).is_err());
}

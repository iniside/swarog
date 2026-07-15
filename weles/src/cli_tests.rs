use super::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn up_defaults_to_split() {
    let cmd = parse(args(&["up"])).unwrap();
    assert_eq!(
        cmd,
        Command::Up {
            topology: Topology::Split,
            skip_build: false,
        }
    );
}

#[test]
fn up_monolith() {
    let cmd = parse(args(&["up", "monolith"])).unwrap();
    assert_eq!(
        cmd,
        Command::Up {
            topology: Topology::Monolith,
            skip_build: false,
        }
    );
}

#[test]
fn up_skip_build() {
    let cmd = parse(args(&["up", "split", "--skip-build"])).unwrap();
    assert_eq!(
        cmd,
        Command::Up {
            topology: Topology::Split,
            skip_build: true,
        }
    );
}

#[test]
fn up_rejects_duplicate_topology() {
    assert!(parse(args(&["up", "split", "monolith"])).is_err());
}

#[test]
fn up_flag_before_topology_is_order_independent() {
    let cmd = parse(args(&["up", "--skip-build", "monolith"])).unwrap();
    assert_eq!(
        cmd,
        Command::Up {
            topology: Topology::Monolith,
            skip_build: true,
        }
    );
}

#[test]
fn up_accepts_idempotent_duplicate_flag() {
    // Pins the chosen policy: repeating a boolean flag is idempotent and
    // accepted; only conflicting values (two topologies) are rejected.
    let cmd = parse(args(&["up", "--skip-build", "--skip-build"])).unwrap();
    assert_eq!(
        cmd,
        Command::Up {
            topology: Topology::Split,
            skip_build: true,
        }
    );
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

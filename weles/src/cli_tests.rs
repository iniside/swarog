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

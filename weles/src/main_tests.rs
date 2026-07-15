use super::*;
use weles::cli::Topology;

#[test]
fn status_stub_bails() {
    // Pins the stubs-bail behavior until the later M0 steps land.
    let err = run(Command::Status).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

#[test]
fn up_stub_bails() {
    let err = run(Command::Up {
        topology: Topology::Monolith,
        skip_build: false,
    })
    .unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

#[test]
fn down_stub_bails() {
    let err = run(Command::Down).unwrap_err();
    assert!(err.to_string().contains("not implemented yet (M0 Step"));
}

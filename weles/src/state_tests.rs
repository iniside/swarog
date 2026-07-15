use super::*;

fn temp_dir(name: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "weles-state-{}-{}-{name}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create test temp dir");
    dir
}

fn sample_state() -> FleetState {
    FleetState {
        run_id: "abc123".to_string(),
        supervisor: ProcessIdentity {
            pid: 4242,
            started_unix: 1_752_500_000,
        },
        topology: "split".to_string(),
        control_endpoint: None,
        services: vec![
            ServiceState {
                name: "accounts-svc".to_string(),
                status: Status::Healthy,
                pid: Some(1001),
                restarts: 0,
            },
            ServiceState {
                name: "gateway-svc".to_string(),
                status: Status::Backoff,
                pid: None,
                restarts: 3,
            },
            ServiceState {
                name: "admin-svc".to_string(),
                status: Status::Failed,
                pid: None,
                restarts: 5,
            },
        ],
    }
}

#[test]
fn fleet_state_roundtrips_through_json() {
    let state = sample_state();
    let json = serde_json::to_string(&state).expect("serialize");
    let back: FleetState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, state);
}

#[test]
fn every_status_variant_roundtrips() {
    for status in [
        Status::Starting,
        Status::WaitingHealthy,
        Status::Healthy,
        Status::Backoff,
        Status::Restarting,
        Status::Failed,
        Status::Stopping,
        Status::Exited,
        Status::Stopped,
    ] {
        let json = serde_json::to_string(&status).expect("serialize status");
        let back: Status = serde_json::from_str(&json).expect("deserialize status");
        assert_eq!(back, status);
    }
}

#[test]
fn checkpoint_writes_parseable_state() {
    let dir = temp_dir("write");
    let path = dir.join("state.json");
    let state = sample_state();
    checkpoint(&path, &state).expect("checkpoint");
    let contents = std::fs::read_to_string(&path).expect("read state.json");
    let back: FleetState = serde_json::from_str(&contents).expect("valid JSON on disk");
    assert_eq!(back, state);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn checkpoint_replaces_prior_state_and_survives_a_stale_tmp() {
    let dir = temp_dir("atomic");
    let path = dir.join("state.json");

    // A previous run's state file plus a torn .tmp from a crash mid-write.
    let mut old = sample_state();
    old.run_id = "old-run".to_string();
    checkpoint(&path, &old).expect("write old state");
    std::fs::write(dir.join("state.json.tmp"), b"{ torn garbage").expect("plant stale tmp");

    let new = sample_state();
    checkpoint(&path, &new).expect("checkpoint over stale tmp");

    let contents = std::fs::read_to_string(&path).expect("read state.json");
    let back: FleetState = serde_json::from_str(&contents).expect("valid JSON after replace");
    assert_eq!(back, new, "the NEW state must fully replace the old one");
    assert!(
        !dir.join("state.json.tmp").exists(),
        "the tmp must have been renamed away, not left behind"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

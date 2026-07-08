//! rating tests. Everything runs in memory — no DB. They drive the private `Service`
//! directly: the MMR default, the +15/-15 match-result math, and the async `MmrReader`
//! read the `match` module resolves over the registry/edge.

use super::*;

/// An unseen player reads the 1000 default; the `MmrReader` capability returns it too.
#[tokio::test]
async fn unseen_player_defaults_to_1000() {
    let svc = Service::new();
    assert_eq!(svc.get("nobody"), 1000);
    assert_eq!(svc.mmr("nobody".into()).await.unwrap(), 1000);
}

/// A finished match moves the winner +15 and the loser -15 from their current ratings
/// (defaulting to 1000): the typed `match.finished` handler's exact effect.
#[test]
fn apply_result_moves_winner_up_and_loser_down() {
    let svc = Service::new();
    svc.apply_result("alice", "bob");
    assert_eq!(svc.get("alice"), 1015);
    assert_eq!(svc.get("bob"), 985);
}

/// Ratings ACCUMULATE across matches: a two-win streak is +30, and the once-beaten
/// player keeps dropping. Proves the handler reads-then-writes each player's live value.
#[test]
fn ratings_accumulate_across_matches() {
    let svc = Service::new();
    svc.apply_result("alice", "bob"); // alice 1015, bob 985
    svc.apply_result("alice", "carol"); // alice 1030, carol 985
    svc.apply_result("dave", "bob"); // dave 1015, bob 970

    assert_eq!(svc.get("alice"), 1030);
    assert_eq!(svc.get("bob"), 970);
    assert_eq!(svc.get("carol"), 985);
    assert_eq!(svc.get("dave"), 1015);
}

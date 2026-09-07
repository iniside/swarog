use super::{rpc_modules, self_check_rpc_list};

/// The retarget must not weaken the didn't-forget check for a SERVED domain: dropping one
/// real entry from the hand-list still dies, naming it. Runs against the real `api/` tree,
/// so it also proves `rpc_modules_from_fs` still sees every served domain's traits.
#[test]
fn a_served_domain_dropped_from_the_hand_list_still_fails() {
    let all: Vec<&'static str> = rpc_modules().iter().map(|(label, _)| *label).collect();
    for dropped in &all {
        let kept: Vec<&'static str> = all.iter().copied().filter(|l| l != dropped).collect();
        let error = self_check_rpc_list(&kept)
            .expect_err("dropping a served domain's rpc module must fail the self-check");
        let message = format!("{error:#}");
        assert!(
            message.contains(&format!("MISSING from rpc_modules(): {dropped}")),
            "error must name {dropped}: {message}"
        );
    }
}

/// The complement: the real hand-list matches the real served tree.
#[test]
fn the_committed_hand_list_matches_the_served_tree() {
    let all: Vec<&'static str> = rpc_modules().iter().map(|(label, _)| *label).collect();
    self_check_rpc_list(&all).expect("committed hand-list must match api/*/api of served domains");
}

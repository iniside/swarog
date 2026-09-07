use super::{rpc_modules, rpc_modules_from_fs_at, self_check_rpc_list, self_check_rpc_list_at};
use std::path::{Path, PathBuf};

/// The retarget must not weaken the didn't-forget check for a SERVED domain: dropping one
/// real entry from the hand-list still dies, naming it. Runs against the real `api/` tree,
/// so it also proves `rpc_modules_from_fs_at` still sees every served domain's traits.
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

fn synthetic_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "opscatalog-gen-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_rpc_domain(root: &Path, domain: &str, crate_name: &str, served: bool) {
    let api = root.join("api").join(domain).join("api");
    std::fs::create_dir_all(api.join("src")).unwrap();
    std::fs::write(api.join("Cargo.toml"), format!("[package]\nname = \"{crate_name}\"\n")).unwrap();
    std::fs::write(
        api.join("src").join("lib.rs"),
        "#[rpc(prefix = \"x\")]\npub trait Demo {\n}\n",
    )
    .unwrap();
    if served {
        let module = root.join("modules").join(domain);
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join("Cargo.toml"), "").unwrap();
    }
}

/// The retarget's proof, independent of what is on disk in this repo (the real-tree
/// tests above only distinguish "filtered" from "unfiltered" while some contract-only
/// domain happens to exist under `api/` -- the day `modules/groups` lands, deleting the
/// served filter from `rpc_modules_from_fs_at` would break neither of them). Builds a
/// synthetic root with one served domain and one contract-only domain (contracts under
/// `api/`, no `modules/` dir) and pins that the contract-only trait lands in
/// `unserved` (never `served`), and that including it in the hand-list produces the
/// STALE-with-domain message rather than the MISSING demand a lost filter would emit.
#[test]
fn a_contract_only_domain_lands_in_unserved_not_demanded_as_missing() {
    let root = synthetic_root("served-split");
    write_rpc_domain(&root, "served", "servedapi", true);
    write_rpc_domain(&root, "contractonly", "contractonlyapi", false);

    let served_label = "servedapi::demo_rpc";
    let contractonly_label = "contractonlyapi::demo_rpc";

    let found = rpc_modules_from_fs_at(&root).unwrap();
    assert!(
        found.served.contains(served_label),
        "served domain's trait must land in `served`: {:?}",
        found.served
    );
    assert_eq!(
        found.unserved.get(contractonly_label),
        Some(&"contractonly".to_string()),
        "contract-only domain's trait must land in `unserved` naming its api/ dir: {:?}",
        found.unserved
    );

    // Listing only the served label is sufficient -- the contract-only trait is not
    // demanded as MISSING.
    self_check_rpc_list_at(&root, &[served_label])
        .expect("a contract-only domain's trait must not be demanded in the hand-list");

    // Listing the contract-only label too (as if someone had hand-added it) must fail
    // with the STALE-with-domain message, not silently pass and never with MISSING.
    let error = self_check_rpc_list_at(&root, &[served_label, contractonly_label])
        .expect_err("a hand-listed contract-only trait must fail the self-check");
    let message = format!("{error:#}");
    assert!(
        message.contains(&format!("STALE in rpc_modules(): {contractonly_label}"))
            && message.contains("no modules/contractonly"),
        "error must name the STALE-with-domain message for {contractonly_label}: {message}"
    );

    let _ = std::fs::remove_dir_all(root);
}

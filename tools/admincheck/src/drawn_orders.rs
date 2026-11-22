//! The drawn order of every shared control-plane point, pinned in one blessed snapshot.
//!
//! `admin::drawn_orders` runs the portal's own merge and sort over every manifest the
//! monolith serves, so changing a `controlplane::order` constant, or a provider dropping a
//! contribution, moves `src/testdata/drawn_orders.txt` and fails this test. Without
//! `ADMIN_BLESS=1` a difference fails with a diff; `ADMIN_BLESS=1 cargo test -p admincheck
//! drawn_orders` rewrites the file and still fails, so a leaked `ADMIN_BLESS` never turns
//! the test green; with `CI` set it refuses to bless.

use std::fs;
use std::path::PathBuf;

const HEADER: &str = "# point | module | id | order, in drawn order; ADMIN_BLESS=1 cargo test -p admincheck drawn_orders\n";

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("testdata").join("drawn_orders.txt")
}

fn render() -> String {
    let manifests = checkmodules::monolith_manifests().expect("build the monolith manifests");
    let mut out = HEADER.to_string();
    for (point, module, id, order) in admin::drawn_orders(&manifests) {
        out.push_str(&format!("{point} | {module} | {id} | {order}\n"));
    }
    out
}

/// The differing lines between the first and the last that differ, `-` from the snapshot
/// and `+` from the render.
fn diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    let prefix = w.iter().zip(&g).take_while(|(a, b)| a == b).count();
    let suffix = w[prefix..].iter().rev().zip(g[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let mut out = format!("@@ line {} @@\n", prefix + 1);
    for line in &w[prefix..w.len() - suffix] {
        out.push_str(&format!("-{line}\n"));
    }
    for line in &g[prefix..g.len() - suffix] {
        out.push_str(&format!("+{line}\n"));
    }
    out
}

#[test]
fn drawn_orders_match_the_blessed_snapshot() {
    let got = render();
    let path = snapshot_path();
    let bless = std::env::var("ADMIN_BLESS").is_ok_and(|v| v == "1");
    if bless {
        assert!(
            std::env::var_os("CI").is_none(),
            "refusing ADMIN_BLESS=1 while CI is set: bless locally and commit the snapshot"
        );
    }
    let want = fs::read_to_string(&path).unwrap_or_default();
    if want == got {
        return;
    }
    if bless {
        fs::create_dir_all(path.parent().expect("testdata dir")).expect("create testdata");
        fs::write(&path, &got).expect("write the snapshot");
        panic!("drawn orders: blessed {}; rerun without ADMIN_BLESS", path.display());
    }
    panic!(
        "drawn orders differ from {} (re-bless with ADMIN_BLESS=1 cargo test -p admincheck drawn_orders)\n{}",
        path.display(),
        diff(&want, &got)
    );
}

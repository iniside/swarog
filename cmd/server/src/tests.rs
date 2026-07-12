//! `cmd/server` is the one crate that already imports both `accounts` and `admin`
//! (the fortress-exception cmd root), so it is the natural home for a cross-module
//! parity check that no single fortress can express on its own: the argon2 dummy-hash
//! LazyLock (Step 11, remediation round 4) exists independently in both modules, and
//! nothing else in the tree guards the two security-cost twins against silently
//! drifting apart (e.g. one module tightening `m`/`t`/`p` for a real reason while the
//! other is left behind, or vice versa a memory/time regression going unnoticed).

/// Asserts accounts' and admin's argon2id cost parameters (memory KiB, time,
/// parallelism, output key length) stay identical. Any intentional divergence must
/// change this test alongside the module it changes.
#[test]
fn accounts_and_admin_argon2_params_match() {
    let accounts_params = accounts::argon2_params_for_parity_test();
    let admin_params = admin::argon2_params_for_parity_test();
    assert_eq!(
        accounts_params, admin_params,
        "accounts and admin argon2 cost parameters (m, t, p, output len) drifted apart"
    );
}

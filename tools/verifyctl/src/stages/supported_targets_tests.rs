//! Pure tests for the stage's decision logic. The `cargo check` call needs a
//! live `Context`, so what is unit-tested here is everything that DECIDES — the
//! positive-control predicate over synthetic `--message-format=json` input,
//! including the previously-wrong branches (a silently-ignored `--target`, a
//! per-crate miss, an output-shape drift) — plus the const↔toml authority pin.

use super::*;

/// A `--message-format=json` fragment for one crate compiled FOR `target`. The
/// artifact path carries the triple exactly as live cargo emits it (verified
/// against real output on this machine), which is the load-bearing signal.
fn artifact_line(crate_name: &str, target: &str, fresh: bool) -> String {
    format!(
        r#"{{"reason":"compiler-artifact","package_id":"path+file:///repo/{crate_name}#0.1.0","target":{{"name":"{crate_name}","kind":["lib"]}},"fresh":{fresh},"filenames":["/repo/target/{target}/debug/deps/lib{crate_name}-abc123.rmeta"]}}"#
    )
}

/// A well-formed run for `target`: both crates produced a target-specific
/// artifact. This is the clean tree.
fn clean_run(target: &str, fresh: bool) -> String {
    format!(
        "{}\n{}\n{}\n",
        artifact_line("processctl", target, fresh),
        artifact_line("weles", target, fresh),
        r#"{"reason":"build-finished","success":true}"#
    )
}

#[test]
fn a_clean_run_yields_no_findings() {
    let target = "aarch64-apple-darwin";
    let json = clean_run(target, false);
    assert!(positive_control_findings(&json, target).is_empty());
    assert_eq!(
        target_specific_artifacts(&json, target),
        BTreeSet::from(["processctl".to_string(), "weles".to_string()])
    );
}

#[test]
fn a_cached_fresh_run_still_asserts_a_real_target_artifact() {
    // The failing branch a naive "require fresh:false" would break: a fully
    // cached tree (`fresh: true`) MUST still pass, because the artifact path
    // still carries the triple — so the positive control asserts a real
    // target-specific compile without demanding a rebuild every run.
    let target = "x86_64-unknown-linux-gnu";
    let json = clean_run(target, true);
    assert!(positive_control_findings(&json, target).is_empty());
}

#[test]
fn a_silently_ignored_target_is_caught() {
    // The previously-wrong branch this positive control exists for: cargo exits
    // 0 but the artifact landed in the HOST target dir (`/debug/`), not
    // `/<triple>/` — i.e. `--target` was ignored and nothing cross-checked. Both
    // crates must be flagged.
    let target = "x86_64-pc-windows-gnu";
    let host_only = format!(
        "{}\n{}\n",
        r#"{"reason":"compiler-artifact","target":{"name":"processctl","kind":["lib"]},"fresh":false,"filenames":["/repo/target/debug/deps/libprocessctl-abc.rmeta"]}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"weles","kind":["lib"]},"fresh":false,"filenames":["/repo/target/debug/deps/libweles-def.rmeta"]}"#,
    );
    let findings = positive_control_findings(&host_only, target);
    assert_eq!(findings.len(), 2, "{findings:?}");
    assert!(findings.iter().all(|f| f.contains(target)));
}

#[test]
fn a_per_crate_miss_is_reported_for_that_crate_only() {
    // Only processctl compiled for the target; weles is missing. Exactly one
    // finding, naming weles.
    let target = "aarch64-apple-darwin";
    let json = format!("{}\n", artifact_line("processctl", target, false));
    let findings = positive_control_findings(&json, target);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("`weles`"));
}

#[test]
fn an_output_shape_drift_is_caught_not_silently_passed() {
    // If cargo's JSON reason field ever changes, no compiler-artifact parses and
    // the positive control flags BOTH crates rather than passing vacuously — the
    // house "no green-on-broken-tooling" habit, applied to the tool's own shape.
    let target = "x86_64-unknown-linux-gnu";
    let drifted = format!(
        "{}\n{}\n",
        r#"{"reason":"compiler-output","target":{"name":"processctl"},"filenames":["/repo/target/x86_64-unknown-linux-gnu/debug/libprocessctl.rmeta"]}"#,
        r#"{"reason":"compiler-output","target":{"name":"weles"},"filenames":["/repo/target/x86_64-unknown-linux-gnu/debug/libweles.rmeta"]}"#,
    );
    assert_eq!(positive_control_findings(&drifted, target).len(), 2);
}

#[test]
fn garbage_lines_and_the_wrong_crate_are_ignored() {
    let target = "aarch64-apple-darwin";
    // A build-script artifact for the same target and an unrelated crate must
    // not satisfy the control; only the two named lib artifacts count.
    let json = format!(
        "not json\n{}\n{}\n",
        r#"{"reason":"compiler-artifact","target":{"name":"serde","kind":["lib"]},"fresh":false,"filenames":["/repo/target/aarch64-apple-darwin/debug/deps/libserde.rmeta"]}"#,
        clean_run(target, false),
    );
    assert!(positive_control_findings(&json, target).is_empty());
}

#[test]
fn a_target_path_is_detected_under_either_separator() {
    // A unix host reports `/`, a Windows host reports `\` in the artifact path.
    let t = "x86_64-pc-windows-gnu";
    assert!(contains_target_path(&format!("/repo/target/{t}/debug/x.rmeta"), t));
    assert!(contains_target_path(&format!(r"C:\repo\target\{t}\debug\x.rmeta"), t));
    // The host target dir (no triple component) must NOT match.
    assert!(!contains_target_path("/repo/target/debug/x.rmeta", t));
    // A bare substring, not bounded by separators, must not match either.
    assert!(!contains_target_path(&format!("/repo/{t}-notacomponent/x"), t));
}

#[test]
fn supported_targets_are_the_three_promised_triples() {
    assert_eq!(
        SUPPORTED_TARGETS,
        &[
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-gnu",
        ]
    );
    assert_eq!(CHECKED_CRATES, &["processctl", "weles"]);
}

#[test]
fn rust_toolchain_targets_equal_supported_targets() {
    // The two authorities — the checked promise (this const) and the declarative
    // provisioning (rust-toolchain.toml's `targets`) — must never drift: a target
    // in the toml but not here would auto-install yet go unchecked, and one here
    // but not the toml would fail to provision on a fresh box. This pins them
    // equal so neither can move without the other.
    let toml_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../rust-toolchain.toml");
    let text = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", toml_path.display()));
    let parsed: toml::Table = toml::from_str(&text).expect("rust-toolchain.toml is valid TOML");
    let targets: Vec<&str> = parsed["toolchain"]["targets"]
        .as_array()
        .expect("[toolchain].targets is an array")
        .iter()
        .map(|v| v.as_str().expect("each target is a string"))
        .collect();
    assert_eq!(
        targets, SUPPORTED_TARGETS,
        "rust-toolchain.toml targets must equal SUPPORTED_TARGETS"
    );
    assert_eq!(
        parsed["toolchain"]["channel"].as_str(),
        Some("stable"),
        "rust-toolchain.toml must pin the stable channel"
    );
}

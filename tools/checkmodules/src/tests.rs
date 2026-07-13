use super::*;
use std::collections::BTreeSet;
use std::path::Path;

/// Step 15 (G3): pins `split_process_modules()`'s hand-written process-name list to
/// the filesystem set of `cmd/*-svc` directories. The compile-time `vec!` of
/// `<name>_svc::modules(...)` calls can't be derived (each is a distinct crate
/// import), so this is the drift tripwire: a 13th `cmd/<name>-svc` crate fails this
/// test loudly until it's added to `split_process_modules()`. The verifyctl fortress
/// build list is independently derived from the same directory set.
#[test]
fn split_fleet_matches_cmd_dirs() {
    let from_fleet: BTreeSet<String> = split_process_modules()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();

    let cmd_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cmd");
    let from_fs: BTreeSet<String> = std::fs::read_dir(&cmd_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", cmd_dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("readable dir entry");
            if !entry.file_type().expect("file type").is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with("-svc").then_some(name)
        })
        .collect();

    assert_eq!(
        from_fleet, from_fs,
        "split_process_modules() must list exactly the cmd/*-svc directories \
         (fleet has {from_fleet:?}, filesystem has {from_fs:?}) -- register the \
         new svc crate in tools/checkmodules::split_process_modules()"
    );
}

/// Finding 8 remainder: the monolith (`cmd/server`) must host every domain module --
/// a `modules/<name>` dir with no corresponding `Module::name()` in `monolith_modules()`
/// would mean `cmd/server` silently stopped booting a fortress. Checked as a SUBSET
/// (dir names is a subset of monolith names), not equality: the monolith's list also
/// carries core-infra (`metrics`) and could in future carry a stub that isn't a
/// `modules/` dir at all -- those extras are fine, only a gap is not. All 12
/// `Module::name()` strings match their `modules/` dir names verbatim today
/// (including `match`: the crate is renamed `match_module` to dodge the Rust
/// keyword, but `name()` still returns `"match"`), so this is keyed off `name()`,
/// never the crate/dir string.
///
/// No exemption list exists (archcheck's `SVC_EXEMPT_MODULES` is empty and its own
/// test asserts it stays so) -- if a legitimately monolith-absent module ever
/// appears, THIS test is where its exemption gets added, with a comment explaining
/// why the monolith deliberately excludes it.
#[test]
fn monolith_hosts_every_modules_dir() {
    let monolith_names: BTreeSet<String> = monolith_modules()
        .iter()
        .map(|m| m.name().to_string())
        .collect();

    let modules_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules");
    let from_fs: BTreeSet<String> = std::fs::read_dir(&modules_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", modules_dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("readable dir entry");
            if !entry.file_type().expect("file type").is_dir() {
                return None;
            }
            Some(entry.file_name().to_string_lossy().into_owned())
        })
        .collect();

    let missing: BTreeSet<&String> = from_fs.difference(&monolith_names).collect();
    assert!(
        missing.is_empty(),
        "cmd/server's monolith_modules() is missing {missing:?} from modules/ \
         (monolith has {monolith_names:?}) -- either wire the missing module's \
         provider/stub into cmd/server's lib, or, if it is deliberately absent from \
         the monolith, add a documented exemption right here"
    );
}

/// Finding 8 remainder: every `cmd/<name>-svc` must construct its OWN module, not
/// merely stub other capabilities it consumes. Sound because `remote::Stub::name()`
/// is always the *provider's* name (the capability the svc is consuming remotely,
/// never itself) -- no svc in this tree stubs its own capability, and doing so
/// would be a bug this test would rightly fail. This is a semantic complement to
/// archcheck's `svc_lib_references_module` (rule 12, G2 leg): that one is a
/// source-layer text-token tripwire that runs without executing module code (so it
/// survives even a checker-harness bug); this one actually constructs the module
/// list and inspects real `Module::name()` values.
#[test]
fn each_svc_constructs_its_own_module() {
    for (name, mods) in split_process_modules() {
        let prefix = name
            .strip_suffix("-svc")
            .unwrap_or_else(|| panic!("split_process_modules() key {name:?} must end in -svc"));
        assert!(
            mods.iter().any(|m| m.name() == prefix),
            "cmd/{name}/src/lib.rs's modules() never constructs a `{prefix}` \
             Module -- it only stubs OTHER capabilities remotely; every svc must \
             host its own domain module locally, not merely remote::Stub it"
        );
    }
}

/// Step 6 (admin-hardening): every domain exposing player-facing HTTP ops (a `#[http(`
/// attribute in `api/<name>/api/src/lib.rs`) MUST be reachable from the front door -- in
/// the split, gateway-svc dispatches those ops Remote through a `remote::Stub` keyed by
/// the provider name. A domain with `#[http(` but no stub in gateway-svc would 404
/// through the gateway in the split while working in the monolith (the classic split-only
/// regression). This is the SEMANTIC complement to archcheck's textual rule-17 tripwire:
/// that one greps gateway-svc's lib.rs for `Stub::new("<name>"`; this one builds
/// gateway-svc's REAL module list and asserts `Module::name()` (== the provider name a
/// `remote::Stub` carries) covers every `#[http(`-bearing domain dir. The scan is the same
/// lower-tech filesystem walk as `monolith_hosts_every_modules_dir`. Checked as a SUBSET
/// (http domains ⊆ gateway names): extra stubs (apikeys, stubbed for the API-key
/// capability) are fine -- only a gap fails.
#[test]
fn gateway_stubs_every_http_domain() {
    let gateway_names: BTreeSet<String> = gateway_svc::modules(&checker_wiring(), None)
        .iter()
        .map(|m| m.name().to_string())
        .collect();

    let api_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api");
    let http_domains: BTreeSet<String> = std::fs::read_dir(&api_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", api_dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("readable dir entry");
            if !entry.file_type().expect("file type").is_dir() {
                return None;
            }
            let domain = entry.file_name().to_string_lossy().into_owned();
            let lib = entry.path().join("api").join("src").join("lib.rs");
            let text = std::fs::read_to_string(&lib).ok()?;
            let has_http = text.lines().any(|line| {
                let t = line.trim_start();
                !t.starts_with("//") && t.contains("#[http(")
            });
            has_http.then_some(domain)
        })
        .collect();

    let missing: BTreeSet<&String> = http_domains.difference(&gateway_names).collect();
    assert!(
        missing.is_empty(),
        "cmd/gateway-svc's modules() is missing a remote::Stub for {missing:?} from the \
         #[http(-bearing domains (gateway hosts {gateway_names:?}) -- add \
         remote::Stub::new(\"<domain>\", ...) to cmd/gateway-svc/src/lib.rs so the gateway \
         dispatches its player-facing ops Remote in the split"
    );
}

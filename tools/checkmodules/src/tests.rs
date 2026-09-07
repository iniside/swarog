use super::*;
use std::collections::BTreeSet;
use std::path::Path;

/// Step 15 (G3): pins `split_process_modules()`'s hand-written process-name list to
/// the filesystem set of `cmd/*-svc` directories. The compile-time `vec!` of
/// `<name>_svc::modules(...)` calls can't be derived (each is a distinct crate
/// import), so this is the drift tripwire: a 14th `cmd/<name>-svc` crate fails this
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
/// `modules/` dir at all -- those extras are fine, only a gap is not. All 13
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
/// attribute in `api/<name>/api/src/lib.rs`) MUST be reachable from the front door. Since
/// D2 (routing-as-data) gateway-svc builds its op route table from each peer's runtime
/// `__describe` manifest, not a compile-time `<name>rpc` route import: the `remote::Stub`
/// per provider contributes that provider's PEER_SLOT address set — the entry the describe
/// fetch iterates — NOT the domain's routes. A domain with `#[http(` but no stub in
/// gateway-svc therefore contributes no PEER_SLOT entry, the describe pass never dials it,
/// and it would 404 through the gateway in the split while working in the monolith (the
/// classic split-only regression, unchanged in cost by the mechanism swap). This is the
/// SEMANTIC complement (the "describe-FETCH-coverage" half of the invariant; the
/// "manifest-completeness" half is routecheck invariant 5, DESCRIBE-COMPLETE) to
/// archcheck's textual rule-17 tripwire: that one greps gateway-svc's lib.rs for
/// `Stub::new("<name>"`; this one builds gateway-svc's REAL module list and asserts
/// `Module::name()` (== the provider name a `remote::Stub` carries) covers every SERVED
/// `#[http(`-bearing domain dir. The scan is the same lower-tech filesystem walk as
/// `monolith_hosts_every_modules_dir`. Checked as a SUBSET (http domains ⊆ gateway names):
/// extra stubs (apikeys, stubbed for the API-key capability) are fine -- only a gap fails.
///
/// "Served" is `rpc_contract_model::served_domains`: a domain whose contracts have landed
/// ahead of its `modules/<name>` has nothing to answer a call, so it is not yet required in
/// gateway-svc's stub list -- the requirement starts with the module's commit.
#[test]
fn gateway_stubs_every_http_domain() {
    let gateway_names: BTreeSet<String> = gateway_svc::modules(&checker_wiring(), None, None, None)
        .iter()
        .map(|m| m.name().to_string())
        .collect();

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let http_domains = http_domains_needing_a_stub(&workspace_root);

    let missing: BTreeSet<&String> = http_domains.difference(&gateway_names).collect();
    assert!(
        missing.is_empty(),
        "cmd/gateway-svc's modules() is missing a remote::Stub for {missing:?} from the \
         #[http(-bearing domains (gateway hosts {gateway_names:?}) -- add \
         remote::Stub::new(\"<domain>\", ...) to cmd/gateway-svc/src/lib.rs so its PEER_SLOT \
         entry is present and the gateway's `__describe` fetch reaches it, lighting up its \
         player-facing routes Remote in the split"
    );
}

/// The domains a gateway stub is REQUIRED for, under `workspace_root`: an `api/<domain>`
/// whose contract sources carry a non-comment `#[http(` AND whose `modules/<domain>`
/// exists. Shared by the real-tree assertion above and the synthetic-root fixture below,
/// so the served filter has exactly ONE definition and cannot be dropped from the real
/// scan while a fixture keeps passing.
fn http_domains_needing_a_stub(workspace_root: &Path) -> BTreeSet<String> {
    let served = rpc_contract_model::served_domains(workspace_root)
        .unwrap_or_else(|e| panic!("failed to list served domains: {e}"));

    let api_dir = workspace_root.join("api");
    std::fs::read_dir(&api_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", api_dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("readable dir entry");
            if !entry.file_type().expect("file type").is_dir() {
                return None;
            }
            let domain = entry.file_name().to_string_lossy().into_owned();
            if !served.contains(&domain) {
                return None;
            }
            let src = entry.path().join("api").join("src");
            if !src.is_dir() {
                return None;
            }
            // EVERY contract source, not just lib.rs: a `#[http(` method moved into
            // `src/ops.rs` would otherwise make this rule match zero targets for the
            // domain and pass green over a domain unreachable in the split.
            let sources = rpc_contract_model::contract_sources(&src)
                .unwrap_or_else(|e| panic!("failed to list {}: {e}", src.display()));
            let has_http = sources.iter().any(|path| {
                let text = std::fs::read_to_string(path)
                    .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
                text.lines().any(|line| {
                    let t = line.trim_start();
                    !t.starts_with("//") && t.contains("#[http(")
                })
            });
            has_http.then_some(domain)
        })
        .collect()
}

/// The served filter, pinned INDEPENDENTLY of what is on disk in this repo. The real-tree
/// assertion above only distinguishes "filtered" from "unfiltered" while some contract-only
/// domain happens to exist under `api/`; the day its module lands, deleting the filter
/// would break no committed test. Here both cases are constructed: `served` has a module
/// and must be demanded, `contractonly`'s contracts landed ahead of its module and must
/// not be.
#[test]
fn only_a_domain_with_a_module_needs_a_gateway_stub() {
    let root = std::env::temp_dir().join(format!(
        "checkmodules-served-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (domain, served) in [("served", true), ("contractonly", false)] {
        let src = root.join("api").join(domain).join("api/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[http(verb = \"POST\", path = \"/x\", auth = \"none\", success = 200)]\n",
        )
        .unwrap();
        if served {
            let module = root.join("modules").join(domain);
            std::fs::create_dir_all(&module).unwrap();
            std::fs::write(module.join("Cargo.toml"), "").unwrap();
        }
    }

    assert_eq!(
        http_domains_needing_a_stub(&root),
        BTreeSet::from(["served".to_string()]),
        "a domain whose contracts landed ahead of its module must not demand a stub"
    );
    let _ = std::fs::remove_dir_all(root);
}

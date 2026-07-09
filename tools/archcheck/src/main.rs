//! `archcheck` — the fortress dependency-law gate (Step 5). It reads `cargo metadata`
//! and fails (exit 1) on any edge that breaks the dependency law from the plan:
//!
//!   1. **no `modules/X → modules/Y`** — a domain module never imports another
//!      module's impl crate (cross-module comms go through the bus or a contract),
//!   2. **no `modules/X → <foreign>rpc`** — a module may import its OWN `<name>rpc`
//!      glue (sanctioned, rule 5) but NEVER another domain's generated glue,
//!   3. **single front door** — only `cmd/gateway-svc` and `cmd/server` (the monolith)
//!      may depend on the `gateway` crate (the FrontDoor). A domain `*-svc` never hosts
//!      the front door; it serves its ops ONLY over the internal mTLS edge, and
//!      gateway-svc dispatches to it Remote.
//!   4. **`<name>api` transport-free** — a contract crate never depends on a raw
//!      transport crate nor on `edge`/`remote`; transport belongs only in `<name>rpc`.
//!   5. **every main lists `metrics`** — every `cmd/*-svc` and the monolith `cmd/server`
//!      depends on `metrics`. (There is no durable-events plane MODULE — the plane is
//!      app-owned infrastructure, not a `cmd`-listed module — so nothing analogous is
//!      required here.)
//!
//! "Own" is defined by path prefix: `modules/<name>/` owns `api/<name>/rpc/`. It also
//! greps `modules/` for a resurrected `Option<… edge::Server>` — the topology-leak
//! regression Step 3 removed — as a cheap tripwire.
//!
//! It is a `go-arch-lint`-equivalent: architecture, not correctness. Run by the
//! `fortress` verify stage; `cargo run -p archcheck` exits 0 on a clean tree.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[cfg(test)]
mod tests;

/// The crate that IS the public front door (FrontDoor module). Only the two front
/// processes below may depend on it.
const GATEWAY_CRATE: &str = "gateway";
/// The `cmd/<dir>` crates permitted to host the front door: the dedicated front process
/// and the monolith. Every other `cmd/*-svc` serves ops only over the internal edge.
const FRONT_DOOR_HOSTS: [&str; 2] = ["gateway-svc", "server"];

/// Modules sanctioned to ship WITHOUT a `cmd/<name>-svc` process. The fortress rule
/// (CLAUDE.md constraint 2) says every domain module compiles + boots as its own svc —
/// currently with NO exceptions: non-shipping demo crates live under `demos/`, not
/// `modules/` (webui moved there 2026-07-09), so nothing in `modules/` is exempt.
const SVC_EXEMPT_MODULES: &[&str] = &[];

/// The only `cmd/<dir>` crate permitted to depend on a `demos/*` crate: the monolith.
/// A demo is non-shipping by definition — the moment gateway-svc (or any other
/// process) imports one, it silently becomes production surface.
const DEMO_HOST: &str = "server";

/// Crate names a `<name>api` contract crate must never depend on (non-dev). The
/// workspace routes transport through the `edge`/`remote` core crates — those are the
/// realistic regression vector for a contract crate; the raw transport crates are
/// forbidden too as future-proofing (fact 6).
const FORBIDDEN_API_DEPS: &[&str] = &[
    "tokio", "quinn", "axum", "hyper", "sqlx", "tonic", "reqwest", "tower", "edge", "remote",
];

/// A workspace package's classification, derived from its manifest path.
#[derive(Debug, Clone)]
enum Kind {
    /// `modules/<name>/` — a domain module impl.
    Module(String),
    /// `api/<name>/rpc/` — a domain's generated transport glue.
    Rpc(String),
    /// `api/<name>/api/` — a domain's pure `#[rpc]` trait contract crate (transport-free).
    Api(String),
    /// `api/<name>/events/` — a domain's `bus::define` payload/descriptor crate
    /// (transport-free, importable by any module).
    Events(String),
    /// `cmd/<name>/` — a composition-root binary (its dir name, e.g. `characters-svc`).
    Cmd(String),
    /// `demos/<name>/` — a non-shipping demo crate (monolith-only by definition).
    Demo(String),
    /// Anything else (foundations, contract crates, tools).
    Other,
}

fn classify(manifest_path: &str) -> Kind {
    // Normalize Windows backslashes so the segment match is OS-independent.
    let p = manifest_path.replace('\\', "/");
    if let Some(name) = segment_after(&p, "/modules/") {
        // modules/<name>/Cargo.toml
        return Kind::Module(name);
    }
    if let Some(name) = segment_after(&p, "/cmd/") {
        // cmd/<name>/Cargo.toml
        return Kind::Cmd(name);
    }
    if let Some(name) = segment_after(&p, "/demos/") {
        // demos/<name>/Cargo.toml
        return Kind::Demo(name);
    }
    if let Some((_, rest)) = p.split_once("/api/") {
        // Note: split_once (not `.split("/api/").nth(1)`) is required — the `api`
        // sub-dir itself is literally named "api" (api/<name>/api/Cargo.toml), so a
        // full `.split()` would see TWO "/api/" occurrences and `nth(1)` would land on
        // the wrong segment. split_once always takes the tail after the FIRST match.
        // api/<name>/rpc/Cargo.toml  ->  rest = "<name>/rpc/Cargo.toml"
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 && parts[1] == "rpc" {
            return Kind::Rpc(parts[0].to_string());
        }
        // api/<name>/api/Cargo.toml  ->  rest = "<name>/api/Cargo.toml"
        if parts.len() >= 2 && parts[1] == "api" {
            return Kind::Api(parts[0].to_string());
        }
        // api/<name>/events/Cargo.toml  ->  rest = "<name>/events/Cargo.toml"
        if parts.len() >= 2 && parts[1] == "events" {
            return Kind::Events(parts[0].to_string());
        }
    }
    Kind::Other
}

/// The path segment immediately following `marker` (e.g. `/modules/` -> `config`).
fn segment_after(path: &str, marker: &str) -> Option<String> {
    path.split(marker).nth(1)?.split('/').next().map(String::from)
}

fn main() {
    let mut violations: Vec<String> = Vec::new();

    // --- 1+2: dependency-law edges from `cargo metadata` --------------------
    let out = Command::new(env_cargo())
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .expect("run cargo metadata");
    if !out.status.success() {
        eprintln!(
            "archcheck: cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::process::exit(2);
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse cargo metadata json");
    let packages = meta["packages"].as_array().expect("packages array");

    // name -> Kind, so a dependency (named by crate name) resolves to its manifest.
    let mut by_name: HashMap<String, Kind> = HashMap::new();
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or_default().to_string();
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        by_name.insert(name, classify(manifest));
    }

    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Module(dm) = classify(manifest) else {
            continue; // only modules are constrained as consumers
        };
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            // Only normal/build deps carry the runtime import graph; a dev-dependency
            // (tests) may legitimately reach a core crate like asyncevents.
            if dep["kind"].as_str() == Some("dev") {
                continue;
            }
            let dep_name = dep["name"].as_str().unwrap_or_default();
            match by_name.get(dep_name) {
                Some(Kind::Module(other)) if other != &dm => violations.push(format!(
                    "modules/{dm} depends on modules/{other} — a module must never import another module's impl crate (use the bus or a contract)"
                )),
                Some(Kind::Rpc(domain)) if domain != &dm => violations.push(format!(
                    "modules/{dm} depends on {dep_name} (api/{domain}/rpc) — a module may import only its OWN <name>rpc glue, never a foreign domain's"
                )),
                _ => {}
            }
        }
    }

    // --- 3: single front door — only the front processes may host the gateway ---
    // Every `cmd/*-svc` other than gateway-svc + the monolith must serve its ops ONLY
    // over the internal mTLS edge, so it must NOT depend on the `gateway` crate. Hosting
    // the FrontDoor in a domain svc duplicates the public front door and drags an accounts
    // stub in solely to feed the bearer verifier (post-port hardening, 2026-07-08).
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Cmd(cmd) = classify(manifest) else {
            continue; // only cmd/* binaries are constrained here
        };
        if FRONT_DOOR_HOSTS.contains(&cmd.as_str()) {
            continue; // gateway-svc + server (monolith) are the sanctioned front doors
        }
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["kind"].as_str() == Some("dev") {
                continue; // a dev-dependency (tests) is not the runtime front door
            }
            if dep["name"].as_str() == Some(GATEWAY_CRATE) {
                violations.push(format!(
                    "cmd/{cmd} depends on `{GATEWAY_CRATE}` — the FrontDoor is hosted ONLY by \
                     the front processes (cmd/gateway-svc, cmd/server); a domain svc never hosts \
                     it (serve ops over the internal mTLS edge, gateway-svc dispatches Remote)"
                ));
            }
        }
    }

    // --- 4: regression tripwire — Option<… edge::Server> under modules/ ------
    let modules_dir = workspace_root(meta.clone()).join("modules");
    for line in grep_option_edge_server(&modules_dir) {
        violations.push(line);
    }

    // --- 7: regression tripwire — cross-schema FK under modules/ --------------
    for line in grep_cross_schema_fk(&modules_dir) {
        violations.push(line);
    }

    // --- 8: regression tripwire — inline test modules under modules/ ----------
    for line in grep_inline_test_modules(&modules_dir) {
        violations.push(line);
    }

    // --- 5: <name>api / <name>events contract crates stay transport-free ------
    // The contract surface (`<name>api`, `<name>events`) must be importable by any
    // module without dragging in a transport dependency; only `<name>rpc` (its own
    // module's generated glue) is allowed to touch `edge`/`remote`/raw transport crates.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let (label, domain) = match classify(manifest) {
            Kind::Api(domain) => ("api", domain),
            Kind::Events(domain) => ("events", domain),
            _ => continue, // only <name>api / <name>events contract crates are constrained here
        };
        let deps = pkg["dependencies"].as_array().cloned().unwrap_or_default();
        for dep_name in forbidden_api_deps(&deps) {
            violations.push(format!(
                "api/{domain}/{label} depends on `{dep_name}` — a <name>{label} contract crate \
                 must stay transport-free (pure traits/payloads, importable by any module); \
                 transport belongs in <name>rpc, never <name>{label}"
            ));
        }
    }

    // --- 9: core/bus stays engine-free (AnyTx seam) ----------------------------
    // bus must never depend on `sqlx` under ANY dep kind (normal/dev/build) — the
    // AnyTx/Delivery seam is what makes the durable contract engine-neutral, and a
    // stray dev-dep on sqlx (e.g. a "quick test") would re-couple the bus to one
    // engine even though nothing in the runtime graph depends on it.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        if pkg["name"].as_str() != Some("bus") {
            continue;
        }
        if !matches!(classify(manifest), Kind::Other) {
            continue; // sanity: only the core/bus package (Kind::Other) is in scope
        }
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["name"].as_str() == Some("sqlx") {
                violations.push(
                    "core/bus must stay engine-free (AnyTx seam): found dep `sqlx`".to_string(),
                );
            }
        }
    }

    // --- 10: modules never runtime-dep the durable-events plane ----------------
    // The plane (`asyncevents`) is app-owned infrastructure injected at `Context`
    // construction (DB ⇒ plane), never a module dependency; a module reaching for it
    // directly would hard-wire a topology assumption. A dev-dependency stays allowed —
    // the sanctioned test-wiring pattern used across the fortress test suites.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Module(dm) = classify(manifest) else {
            continue;
        };
        let deps = pkg["dependencies"].as_array().cloned().unwrap_or_default();
        if has_non_dev_dep(&deps, "asyncevents") {
            violations.push(format!(
                "modules/{dm} depends on `asyncevents` outside dev-dependencies — the durable \
                 plane is app-owned infrastructure (DB ⇒ plane at Context construction), never \
                 a module dependency; modules are topology-blind (CLAUDE.md constraint 5)"
            ));
        }
    }

    // --- 11: regression tripwire — EVENTS_ env knobs read inside modules/ ------
    for line in grep_events_env(&modules_dir) {
        violations.push(line);
    }

    // --- 6: every cmd/*-svc + the monolith main lists `metrics` ---------------
    // CLAUDE.md: "every main lists metrics::Metrics::new() for GET /metrics." The
    // durable-events plane is app-owned infrastructure, not a listed module, so there is
    // no analogous per-main dependency to enforce here.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Cmd(cmd) = classify(manifest) else {
            continue; // only cmd/* binaries are constrained here
        };
        if !cmd_is_a_main(&cmd) {
            continue; // not a process main (e.g. a helper cmd/ crate, if any)
        }
        let deps = pkg["dependencies"].as_array().cloned().unwrap_or_default();
        if !has_non_dev_dep(&deps, "metrics") {
            violations.push(format!(
                "cmd/{cmd} does not depend on `metrics` — every main lists \
                 metrics::Metrics::new() for GET /metrics"
            ));
        }
    }

    // --- 12: fortress parity — every modules/<name> boots as cmd/<name>-svc ----
    // Scanned from the FILESYSTEM (dirs holding a Cargo.toml under modules/ and cmd/),
    // not from workspace metadata — the workspace `members` list is hand-maintained, so
    // a freshly created modules/<name> not yet registered there would be invisible to
    // `cargo metadata` but must still fail here. No per-module list to maintain:
    // `SVC_EXEMPT_MODULES` is the only allow-list, and it holds sanctioned EXCEPTIONS.
    let root = workspace_root(meta.clone());
    let fs_modules = crate_dirs(&root.join("modules"));
    let fs_cmds = crate_dirs(&root.join("cmd"));
    for line in missing_svc_violations(&fs_modules, &fs_cmds) {
        violations.push(line);
    }
    // The workspace-registered module set, for the "svc actually lists its module" leg.
    let module_names: Vec<String> = packages
        .iter()
        .filter_map(|pkg| match classify(pkg["manifest_path"].as_str().unwrap_or_default()) {
            Kind::Module(m) => Some(m),
            _ => None,
        })
        .collect();
    // …and the svc must actually LIST its module (an empty stub svc would otherwise
    // satisfy the existence check without booting the fortress).
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Cmd(cmd) = classify(manifest) else {
            continue;
        };
        let Some(module) = cmd.strip_suffix("-svc") else {
            continue; // the monolith `server` lists all modules by construction
        };
        if !module_names.iter().any(|m| m == module) {
            continue; // a svc with no same-named module (nothing to pair against)
        }
        let deps = pkg["dependencies"].as_array().cloned().unwrap_or_default();
        let boots_its_module = deps.iter().any(|dep| {
            dep["kind"].as_str() != Some("dev")
                && matches!(
                    by_name.get(dep["name"].as_str().unwrap_or_default()),
                    Some(Kind::Module(m)) if m == module
                )
        });
        if !boots_its_module {
            violations.push(format!(
                "cmd/{cmd} does not depend on modules/{module} — a domain svc must boot \
                 its own module (fortress rule, CLAUDE.md constraint 2)"
            ));
        }
    }

    // --- 13: demos stay non-shipping — only cmd/server may import a demos/* crate ---
    // A demo crate (demos/webui) is compiled + runnable but deliberately monolith-only;
    // any other consumer (a domain module, gateway-svc, another svc) would promote it
    // to shipping surface without anyone deciding that.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let consumer = classify(manifest);
        if matches!(&consumer, Kind::Cmd(c) if c == DEMO_HOST) {
            continue; // the monolith is the sanctioned demo host
        }
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["kind"].as_str() == Some("dev") {
                continue;
            }
            let dep_name = dep["name"].as_str().unwrap_or_default();
            if let Some(Kind::Demo(demo)) = by_name.get(dep_name) {
                violations.push(format!(
                    "{} depends on demos/{demo} — a demo crate is non-shipping and may be \
                     hosted ONLY by cmd/{DEMO_HOST} (the monolith)",
                    pkg["name"].as_str().unwrap_or_default()
                ));
            }
        }
    }

    if violations.is_empty() {
        println!("archcheck: OK — no module→module / module→foreign-rpc edges, single front door (only gateway-svc + server host `gateway`), no Option<edge::Server> in modules/, <name>api/<name>events crates stay transport-free, every cmd/*-svc + server lists `metrics`, no cross-schema FKs in modules/ DDL, no inline test modules in modules/, core/bus stays sqlx-free, no module runtime-deps `asyncevents`, no EVENTS_ env knobs read inside modules/, every modules/<name> boots as cmd/<name>-svc, demos/* imported only by cmd/server");
        return;
    }
    eprintln!("archcheck: FAIL — {} violation(s):", violations.len());
    for v in &violations {
        eprintln!("  - {v}");
    }
    std::process::exit(1);
}

/// The workspace root directory from `cargo metadata`.
fn workspace_root(meta: serde_json::Value) -> std::path::PathBuf {
    let root = meta["workspace_root"]
        .as_str()
        .expect("workspace_root")
        .to_string();
    std::path::PathBuf::from(root)
}

/// Honour `CARGO` (set when invoked via `cargo run`) so the right toolchain's cargo is
/// used; fall back to `cargo` on `PATH`.
fn env_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The names of every immediate subdirectory of `dir` that holds a `Cargo.toml` — the
/// filesystem's own answer to "which crates live under modules/ (or cmd/)", independent
/// of whether they're registered as workspace members yet.
fn crate_dirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir() && e.path().join("Cargo.toml").is_file())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect()
}

/// Fortress-parity check (rule 12): every `modules/<name>` (minus the sanctioned
/// [`SVC_EXEMPT_MODULES`]) must have a `cmd/<name>-svc` composition root. Inputs are
/// the module dir names and cmd dir names from a [`crate_dirs`] filesystem scan, so
/// the check follows the folders themselves — no per-module list to maintain.
fn missing_svc_violations(modules: &[String], cmds: &[String]) -> Vec<String> {
    modules
        .iter()
        .filter(|m| !SVC_EXEMPT_MODULES.contains(&m.as_str()))
        .filter_map(|m| {
            let svc = format!("{m}-svc");
            if cmds.iter().any(|c| c == &svc) {
                None
            } else {
                Some(format!(
                    "modules/{m} has no cmd/{svc} — every domain module must compile + boot \
                     as its own svc process (fortress rule, CLAUDE.md constraint 2); add \
                     cmd/{svc} (and wire split-proof + the fortress build list) or discuss \
                     a sanctioned exemption"
                ))
            }
        })
        .collect()
}

/// True if a `cmd/<dir>` crate is a process main subject to the "every main lists
/// `metrics`" rule: every `*-svc` plus the monolith `server` (not a helper `cmd/` crate,
/// if one is ever added).
fn cmd_is_a_main(dir: &str) -> bool {
    dir.ends_with("-svc") || dir == "server"
}

/// Returns whether `deps` (a `cargo metadata` package's `"dependencies"` array) contains
/// a NON-dev dependency named `name`. A dev-dependency (tests) is not part of the
/// runtime import graph.
fn has_non_dev_dep(deps: &[serde_json::Value], name: &str) -> bool {
    deps.iter()
        .any(|dep| dep["kind"].as_str() != Some("dev") && dep["name"].as_str() == Some(name))
}

/// Returns the names of every NON-dev dependency in `deps` that appears in
/// `FORBIDDEN_API_DEPS` — the transport crates a `<name>api` contract crate must never
/// import.
fn forbidden_api_deps(deps: &[serde_json::Value]) -> Vec<String> {
    deps.iter()
        .filter(|dep| dep["kind"].as_str() != Some("dev"))
        .filter_map(|dep| dep["name"].as_str())
        .filter(|name| FORBIDDEN_API_DEPS.contains(name))
        .map(String::from)
        .collect()
}

/// Returns the start byte-index of every non-overlapping occurrence of `pat` in `text`.
fn find_all(text: &str, pat: &str) -> Vec<usize> {
    let mut hits = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find(pat) {
        hits.push(start + pos);
        start += pos + pat.len();
    }
    hits
}

/// Every schema name declared via `CREATE SCHEMA IF NOT EXISTS <s>;` in `text`.
fn create_schemas(text: &str) -> Vec<String> {
    let marker = "CREATE SCHEMA IF NOT EXISTS ";
    find_all(text, marker)
        .into_iter()
        .filter_map(|i| {
            let rest = &text[i + marker.len()..];
            let end = rest.find(';')?;
            Some(rest[..end].trim().to_string())
        })
        .collect()
}

/// Every schema name referenced via `REFERENCES <schema>.<table>` in `text`.
fn references_schemas(text: &str) -> Vec<String> {
    let marker = "REFERENCES ";
    find_all(text, marker)
        .into_iter()
        .filter_map(|i| {
            let rest = &text[i + marker.len()..];
            let end = rest.find('.')?;
            Some(rest[..end].trim().to_string())
        })
        .collect()
}

/// Cross-schema FK check for one file's `text` (fact 8). `own_schema_fallback` is the
/// `modules/<name>` path segment, used both as the sanity-check target for a declared
/// schema and as the fallback "own schema" for a file with no DDL at all (a REFERENCES
/// there shouldn't happen, but if it does we still need an "own schema" to compare
/// against).
///
/// Hard-asserts (rather than silently mis-checking) the assumption the whole checker
/// rests on: a file declares AT MOST ONE schema, and when it does, the schema name
/// equals the owning module's directory name. Either violation is reported as a
/// "checker assumption violated" finding instead of a (potentially wrong) FK verdict.
fn cross_schema_fk_violations(text: &str, own_schema_fallback: &str) -> Vec<String> {
    let schemas = create_schemas(text);
    let own = match schemas.as_slice() {
        [] => own_schema_fallback.to_string(),
        [only] if only == own_schema_fallback => only.clone(),
        [only] => {
            return vec![format!(
                "checker assumption violated: declares schema `{only}` but lives under \
                 modules/{own_schema_fallback}/ — archcheck assumes CREATE SCHEMA name == \
                 the owning module's directory name"
            )];
        }
        many => {
            return vec![format!(
                "checker assumption violated: {} CREATE SCHEMA declarations ({many:?}) in one \
                 file — archcheck assumes exactly one per file",
                many.len()
            )];
        }
    };
    references_schemas(text)
        .into_iter()
        .filter(|s| s != &own)
        .map(|s| {
            format!(
                "REFERENCES {s}.… crosses from schema `{own}` into schema `{s}` — a relation to \
                 another module must be a plain id column, never a cross-schema FK \
                 (CLAUDE.md constraint 10)"
            )
        })
        .collect()
}

/// The `modules/<name>` path segment owning `file`, given the `modules/` root `dir`.
fn module_name_from_path(dir: &Path, file: &Path) -> Option<String> {
    file.strip_prefix(dir)
        .ok()?
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
}

/// Walks `dir` for `.rs` files and returns a cross-schema-FK violation per file, using
/// [`cross_schema_fk_violations`] against the file's owning module directory name.
fn grep_cross_schema_fk(dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some(module_name) = module_name_from_path(dir, &path) else {
                    continue;
                };
                for msg in cross_schema_fk_violations(&text, &module_name) {
                    hits.push(format!("{}: {msg}", path.display()));
                }
            }
        }
    }
    hits
}

/// If `line` contains a `mod` keyword (token-bounded, never a substring of a longer
/// identifier) immediately followed by the identifier `tests` or `<x>_tests`, returns
/// the byte offset in `line` right after that identifier. Used to discriminate a real
/// `mod tests`/`mod foo_tests` declaration from unrelated text (e.g. a `fn test_x()`,
/// which never matches — there is no `mod` keyword to find).
fn mod_test_ident_end(line: &str) -> Option<usize> {
    let idx = line.find("mod ")?;
    if idx > 0 {
        let prev = line.as_bytes()[idx - 1] as char;
        if prev.is_alphanumeric() || prev == '_' {
            return None; // "mod " is a suffix of a longer identifier, not the keyword
        }
    }
    let rest = &line[idx + 4..];
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let ident = &rest[..end];
    if ident == "tests" || (ident.ends_with("_tests") && ident != "_tests") {
        Some(idx + 4 + end)
    } else {
        None
    }
}

/// Given `lines` starting AT a line containing a `mod tests`/`mod <x>_tests` token
/// (`lines[0]`), decides whether it's an inline test-module body (`{ ... }`, violation)
/// or a declaration (`;`, pass — routed to `src/tests.rs` / `src/<file>_tests.rs`).
/// Implements the plan's str algorithm exactly (fact 9): take the substring after the
/// module identifier, `trim_start()`, and inspect the first byte; if that remainder is
/// empty (rustfmt-legal brace-on-next-line), peek the next non-blank line instead.
fn is_inline_test_mod(lines: &[&str]) -> bool {
    let Some(first) = lines.first() else {
        return false;
    };
    let Some(end) = mod_test_ident_end(first) else {
        return false;
    };
    let remainder = first[end..].trim_start();
    if let Some(c) = remainder.chars().next() {
        return c == '{';
    }
    for line in lines.iter().skip(1) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        return t.starts_with('{');
    }
    false // brace never found — don't guess, don't flag
}

/// Walks `dir` for `.rs` files (skipping `tests.rs` / `*_tests.rs`, which are the
/// sanctioned homes for test bodies — CLAUDE.md constraint 10) and returns a violation
/// per inline `mod tests { ... }` / `mod <x>_tests { ... }` body found via
/// [`is_inline_test_mod`]. Comment lines are skipped, mirroring `grep_option_edge_server`.
fn grep_inline_test_modules(dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name == "tests.rs" || file_name.ends_with("_tests.rs") {
                continue; // the sanctioned homes for test bodies
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue; // skip comments/doc — code only
                }
                if mod_test_ident_end(line).is_some() && is_inline_test_mod(&lines[i..]) {
                    hits.push(format!(
                        "{}:{}: inline test module body — tests must live in src/tests.rs or \
                         src/<file>_tests.rs, never inline (CLAUDE.md constraint 10)",
                        path.display(),
                        i + 1
                    ));
                }
            }
        }
    }
    hits
}

/// Walks `dir` for `.rs` files and returns a message per NON-comment line that pairs
/// `Option<` with `edge::Server` — the resurrected topology leak. A tripwire: a false
/// alarm (a stray mention) is cheaper than a silent regression.
fn grep_option_edge_server(dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    if t.starts_with("//") {
                        continue; // skip comments/doc — code only
                    }
                    if let (Some(o), Some(e)) = (line.find("Option<"), line.find("edge::Server")) {
                        if o < e {
                            hits.push(format!(
                                "{}:{}: Option<… edge::Server> — the topology leak Step 3 removed must not return to modules/",
                                path.display(),
                                i + 1
                            ));
                        }
                    }
                }
            }
        }
    }
    hits
}

/// True if `text` contains `pat` at a position whose PRECEDING byte is not an
/// identifier character (`[A-Za-z0-9_]`) — i.e. `pat` starts a fresh identifier rather
/// than continuing a longer one (e.g. `EVENTS_` inside `ASYNCEVENTS_READY`). A match at
/// byte offset 0 always counts (nothing precedes it).
fn contains_boundary_checked(text: &str, pat: &str) -> bool {
    find_all(text, pat).into_iter().any(|i| {
        i == 0 || {
            let prev = text.as_bytes()[i - 1] as char;
            !(prev.is_alphanumeric() || prev == '_')
        }
    })
}

/// Walks `dir` for `.rs` files and returns a message per NON-comment line containing a
/// boundary-checked `EVENTS_` match — an `EVENTS_*` env knob read directly inside
/// `modules/`. Modules are topology-blind (CLAUDE.md constraint 5): env-addressed
/// wiring like `EVENTS_ORIGIN`/`EVENTS_SUBSCRIBERS` belongs only in `cmd/*` composition
/// roots. The boundary check is required because a naive substring match also hits
/// `ASYNCEVENTS_READY` (a real, legitimate identifier) — see
/// [`contains_boundary_checked`].
fn grep_events_env(dir: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    if t.starts_with("//") {
                        continue; // skip comments/doc — code only
                    }
                    if contains_boundary_checked(line, "EVENTS_") {
                        hits.push(format!(
                            "{}:{}: EVENTS_ env knob read inside modules/ — modules are \
                             topology-blind (CLAUDE.md constraint 5); env-addressed wiring \
                             belongs only in cmd/* composition roots",
                            path.display(),
                            i + 1
                        ));
                    }
                }
            }
        }
    }
    hits
}

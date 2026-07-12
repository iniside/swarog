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
//!   6. **core purity** — a `core/*` foundation NEVER depends on a module or an `api/`
//!      crate (`<name>api`/`<name>events`/`<name>rpc`) or a demo; dependency only ever
//!      points module → core (CLAUDE.md hard constraint 1). The dual of rules 1+2.
//!   7. **svc constructs its module** — beyond the Cargo dep, a `cmd/<name>-svc`'s
//!      `src/lib.rs` must reference the `<module>::` token (heuristic source tripwire),
//!      proving `modules()` actually constructs the fortress it boots.
//!   8. **gateway stub coverage** — every domain exposing player-facing HTTP ops (a
//!      `#[http(` attribute in `api/<name>/api/src/lib.rs`) must have a
//!      `remote::Stub::new("<name>", …)` in `cmd/gateway-svc/src/lib.rs`, so the gateway
//!      can dispatch those ops Remote in the split. Extra stubs are fine; a missing one
//!      is a gap.
//!
//! (The above is a curated summary; the numbered rule comments in `main()` are the full
//! set, currently 1–17 plus the two svc-parity legs of rule 12.)
//!
//! "Own" is defined by path prefix: `modules/<name>/` owns `api/<name>/rpc/`. It also
//! greps `modules/` for a resurrected `Option<… edge::Server>` — the topology-leak
//! regression Step 3 removed — as a cheap tripwire.
//!
//! It is a `go-arch-lint`-equivalent: architecture, not correctness. Run by the
//! `fortress` verify stage; `cargo run -p archcheck` exits 0 on a clean tree.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;

#[cfg(test)]
mod tests;

/// The crate that IS the public front door (FrontDoor module). Only the two front
/// processes below may depend on it.
const GATEWAY_CRATE: &str = "gateway";
/// Tool-owned conformance policy must never enter a shipping process graph.
const CONFORMANCE_POLICY_CRATE: &str = "conformance";
/// The `cmd/<dir>` crates permitted to host the front door: the dedicated front process
/// and the monolith. Every other `cmd/*-svc` serves ops only over the internal edge.
const FRONT_DOOR_HOSTS: [&str; 2] = ["gateway-svc", "server"];

/// The DERIVED exception to the gateway-crate rule (Step 10): `tools/checkmodules`
/// builds BOTH deployment profiles by importing the `cmd/gateway-svc`/`cmd/server`
/// LIBS (each constructs `gateway::Gateway` internally to hand back the real module
/// list), `topiccheck`/`requirecheck` build their module set through `checkmodules`,
/// and `conformancecheck` (tools/conformance) imports `gateway` for factual probes.
/// None of the four ships a process or dispatches an op — they are the checker path,
/// not a second front
/// door — so they're allowlisted here rather than by relaxing rule 3 itself.
const GATEWAY_CHECKER_HOSTS: [&str; 4] =
    ["checkmodules", "topiccheck", "requirecheck", "conformancecheck"];

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

/// The Rust workspace source dirs the two pull-plane bans (below) scan. Docs
/// (`docs/`), agent `memory/`, and archived `experiments/` are deliberately out of
/// scope — only shipping workspace source is constrained.
const WORKSPACE_SRC_DIRS: [&str; 6] = ["core", "modules", "api", "cmd", "tools", "demos"];

/// Fully-retired vocabulary of the producer-push event plane (deleted in the pull-plane
/// cutover, plan Steps 3-4): the two env knobs of the old O(producers×consumers) config
/// graph and the unauthenticated HTTP sink route. None may survive anywhere in shipping
/// source — even a comment naming one signals code that still thinks in push delivery.
/// The route is banned in its EXACT quoted form (`"/events"`) so an unrelated `/events`
/// substring (a URL, prose) never false-positives.
const RETIRED_EVENT_TOKENS: &[&str] = &["EVENTS_SUBSCRIBERS", "EVENTS_ORIGIN", "\"/events\""];

/// The plane-owned SQL functions a module may reference by name in SQL. These are the
/// ONLY schema-qualified `asyncevents.` references permitted outside `core/asyncevents`
/// (the plane), `tools/eventctl` (the operator CLI), `tools/splitproof` (the split-proof
/// harness, which asserts plane/event state like the retired shell scripts' `pg` did),
/// and test files: the plane owns its tables and exposes this narrow function surface
/// (config's write trigger + its history seed), so a module never SELECTs/INSERTs a
/// plane table directly.
const ASYNCEVENTS_SQL_ALLOW: &[&str] = &["append_event(", "ensure_history_contract("];

/// The crate whose OWN source necessarily names the tokens/markers it bans (const
/// patterns, rule docs, unit-test fixtures) — excluded from both bans so the checker
/// does not flag itself. Path-prefix under the workspace root.
const BAN_SELF_EXCLUDE: &str = "tools/archcheck";

/// The textual marker in an `api/<name>/api/src/lib.rs` that means "this domain exposes
/// player-facing HTTP ops" (an `#[http(…)]` attribute on an `#[rpc]` method). This is the
/// ONLY authoritative signal for HTTP surface: the generated `route_bindings()` exists
/// even for wire-only crates (e.g. `ratingrpc`), so the attribute — never the glue — is
/// what rule 17 keys off. The provider/stub name is the DIR name (`api/<name>/api`), not
/// the crate name: `modules/match`'s crate is `match_module` but its provider name is
/// `match`, and `api/match/api` yields that directly.
const HTTP_OP_MARKER: &str = "#[http(";

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
    /// `core/<name>/` — a foundation crate (app, bus, registry, edge, …). Foundations
    /// never import a module or an `api/` crate (CLAUDE.md hard constraint 1).
    Core(String),
    /// Anything else (contract crates outside the above shapes, tools).
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
    if let Some(name) = segment_after(&p, "/core/") {
        // core/<name>/Cargo.toml — a foundation crate.
        return Kind::Core(name);
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

    // --- 18: conformance policy stays out of every shipping process graph ----
    // Roots are derived from cmd/* metadata: every *-svc plus the monolith server.
    // Walk normal/build edges between workspace packages once; no service list and no
    // per-root cargo-tree invocation can drift as composition roots are added.
    violations.extend(shipping_conformance_violations(packages));

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
    //
    // Scoped to every package (not just `Kind::Cmd`, Step 10): before Step 10's
    // per-process libs, `tools/checkmodules` already depended DIRECTLY on `gateway`
    // (to build its own module vec) and this check never saw it, because it only
    // scanned `Kind::Cmd` consumers — an unintentional blind spot. Now that
    // `checkmodules` reaches `gateway` only transitively (through the `cmd/gateway-svc`
    // / `cmd/server` libs' OWN direct dep, already permitted above), scanning every
    // package closes that historical gap instead of re-opening a narrower one.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let pkg_name = pkg["name"].as_str().unwrap_or_default();
        if let Kind::Cmd(cmd) = classify(manifest) {
            if FRONT_DOOR_HOSTS.contains(&cmd.as_str()) {
                continue; // gateway-svc + server (monolith) are the sanctioned front doors
            }
        }
        if GATEWAY_CHECKER_HOSTS.contains(&pkg_name) {
            continue; // the checker path (derived exception, Step 10) — see the const doc
        }
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["kind"].as_str() == Some("dev") {
                continue; // a dev-dependency (tests) is not the runtime front door
            }
            if dep["name"].as_str() == Some(GATEWAY_CRATE) {
                violations.push(format!(
                    "{pkg_name} depends on `{GATEWAY_CRATE}` — the FrontDoor is hosted ONLY by \
                     the front processes (cmd/gateway-svc, cmd/server) plus the checker path \
                     (tools/checkmodules, tools/topiccheck, tools/requirecheck, \
                     tools/conformance); a domain svc never hosts it (serve ops over the \
                     internal mTLS edge, gateway-svc dispatches Remote)"
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

    // --- 14: pull-plane cutover tripwires — retired push vocabulary + plane-table
    // access. The producer-push plane (EVENTS_* env graph, POST /events sink) is gone;
    // the plane owns its tables and exposes only a narrow SQL function surface.
    let root_dir = workspace_root(meta.clone());
    for line in grep_retired_event_tokens(&root_dir) {
        violations.push(line);
    }
    for line in grep_asyncevents_sql(&root_dir) {
        violations.push(line);
    }

    // --- 15: regression tripwire — a module querying a FOREIGN module's schema in
    // SQL literals. Schema set = the fortress module dir scan; each module reaches
    // another module's data only via a capability or durable events, never direct
    // cross-schema SQL. (`asyncevents.` stays with rule 14b; cross-schema FKs with rule 7.)
    for line in grep_foreign_schema_sql(&root_dir) {
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
        // The actual CRATE name of the module dependency — usually == the dir name, but a
        // module whose dir name is a Rust keyword renames its crate (modules/match →
        // `match_module`), so the source token must follow the crate name, not the dir.
        let module_crate = deps.iter().find_map(|dep| {
            if dep["kind"].as_str() == Some("dev") {
                return None;
            }
            let name = dep["name"].as_str().unwrap_or_default();
            match by_name.get(name) {
                Some(Kind::Module(m)) if m == module => Some(name.to_string()),
                _ => None,
            }
        });
        let Some(module_crate) = module_crate else {
            violations.push(format!(
                "cmd/{cmd} does not depend on modules/{module} — a domain svc must boot \
                 its own module (fortress rule, CLAUDE.md constraint 2)"
            ));
            continue; // no crate to reference-check against
        };
        // …and the svc's lib.rs must actually CONSTRUCT its module — a Cargo dep alone
        // (checked above) can be present while `modules()` never boxes the module. Heuristic
        // source tripwire (same caveat class as `is_inline_test_mod`): a boundary-checked
        // `<crate>::` token anywhere in cmd/<name>-svc/src/lib.rs is the evidence.
        let lib_path = root.join("cmd").join(&cmd).join("src").join("lib.rs");
        if !svc_lib_references_module(&lib_path, &module_crate) {
            violations.push(format!(
                "cmd/{cmd} depends on module '{module}' but src/lib.rs never references \
                 '{module_crate}::' — modules() likely doesn't construct it"
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

    // --- 16: core purity — a foundation never imports a module or an api/ crate ---
    // CLAUDE.md hard constraint 1: "Foundations (core/*) never depend on a module or an
    // `api/` crate. Dependency only ever points module → core." Enforced here as the dual
    // of rules 1+2 (which constrain modules as consumers): for every `Kind::Core(_)`
    // package, any NON-dev dependency resolving to a module or a contract crate
    // (`<name>api`/`<name>events`/`<name>rpc`) or a demo inverts the dependency arrow.
    for pkg in packages {
        let manifest = pkg["manifest_path"].as_str().unwrap_or_default();
        let Kind::Core(core) = classify(manifest) else {
            continue; // only core/* foundations are constrained as consumers here
        };
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["kind"].as_str() == Some("dev") {
                continue; // a dev-dependency (tests) is not the runtime import graph
            }
            let dep_name = dep["name"].as_str().unwrap_or_default();
            let kind = match by_name.get(dep_name) {
                Some(Kind::Module(_)) => "modules",
                Some(Kind::Api(_)) => "api/<name>/api",
                Some(Kind::Events(_)) => "api/<name>/events",
                Some(Kind::Rpc(_)) => "api/<name>/rpc",
                Some(Kind::Demo(_)) => "demos",
                _ => continue,
            };
            violations.push(format!(
                "core/{core} depends on `{dep_name}` ({kind}) — a foundation never imports a \
                 module or an api/ crate; dependency only ever points module → core \
                 (CLAUDE.md hard constraint 1)"
            ));
        }
    }

    // --- 17: gateway stub coverage — every #[http( domain is stubbed in gateway-svc ---
    // A domain that exposes player-facing HTTP ops must be reachable from the front door:
    // in the split, gateway-svc dispatches those ops Remote through a `remote::Stub` keyed
    // by the provider name. A domain with `#[http(` in its `api/<name>/api/src/lib.rs` but
    // no `Stub::new("<name>", …)` in cmd/gateway-svc would 404 through the gateway in the
    // split while working in the monolith — the classic split-only regression. Extra stubs
    // (apikeys is stubbed for the API-key capability, not an HTTP domain) are fine; only a
    // MISSING one is a gap. Textual complement to
    // `checkmodules::tests::gateway_stubs_every_http_domain`, which builds the real module
    // list and checks `Module::name()`.
    let gateway_lib = std::fs::read_to_string(
        root.join("cmd").join("gateway-svc").join("src").join("lib.rs"),
    )
    .unwrap_or_default();
    for line in gateway_stub_coverage_violations(&http_op_domains(&root.join("api")), &gateway_lib) {
        violations.push(line);
    }

    if violations.is_empty() {
        println!("archcheck: OK — no module→module / module→foreign-rpc edges, shipping process graphs exclude conformance policy, single front door (only gateway-svc + server host `gateway`), no Option<edge::Server> in modules/, <name>api/<name>events crates stay transport-free, every cmd/*-svc + server lists `metrics`, no cross-schema FKs in modules/ DDL, no inline test modules in modules/, core/bus stays sqlx-free, no module runtime-deps `asyncevents`, no EVENTS_ env knobs read inside modules/, no retired push-plane tokens (EVENTS_*/\"/events\") in workspace source, no schema-qualified asyncevents.<table> access outside the plane, no module queries a foreign module's schema in SQL, every modules/<name> boots as cmd/<name>-svc (and its svc lib.rs constructs it), demos/* imported only by cmd/server, no core/* foundation deps a module or api/ crate, every #[http( domain is stubbed in cmd/gateway-svc");
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

/// Heuristic tripwire (rule 12, G2 leg): true if `lib_path` (a `cmd/<name>-svc/src/lib.rs`)
/// references the boundary-checked token `<module>::` on any non-comment line — evidence
/// the svc's `modules()` actually constructs its module (e.g.
/// `Box::new(characters::Characters::new())`). A source token scan, NOT a full parse (same
/// caveat class as [`is_inline_test_mod`]): a module reached only through a re-export alias
/// could false-negative, but no such shape exists in the tree. An unreadable/absent lib.rs
/// yields `false` (a domain svc always has one — its absence is itself worth flagging).
///
/// Semantic complement: `tools/checkmodules::tests::each_svc_constructs_its_own_module`
/// actually builds each svc's real module list and asserts a `Module::name()` match —
/// same rule, different failure class (this one is a source-layer text scan that runs
/// without executing module code; that one executes `modules()` and inspects real
/// `Module` values).
fn svc_lib_references_module(lib_path: &Path, module: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(lib_path) else {
        return false;
    };
    let token = format!("{module}::");
    text.lines().any(|line| {
        let t = line.trim_start();
        !t.starts_with("//") && contains_boundary_checked(line, &token)
    })
}

/// Every domain whose `api/<name>/api/src/lib.rs` declares at least one [`HTTP_OP_MARKER`]
/// (`#[http(`) on a NON-comment line (boundary-checked, same style as the other grep
/// tripwires) — i.e. the domain exposes player-facing HTTP ops. The domain name is the
/// `api/<name>` DIR name, which IS the provider/stub name (see [`HTTP_OP_MARKER`]). A
/// missing/unreadable lib.rs simply contributes no domain.
fn http_op_domains(api_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(api_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let domain = e.file_name().to_str().map(String::from)?;
            let lib = e.path().join("api").join("src").join("lib.rs");
            let text = std::fs::read_to_string(&lib).ok()?;
            let has_http = text.lines().any(|line| {
                let t = line.trim_start();
                !t.starts_with("//") && contains_boundary_checked(line, HTTP_OP_MARKER)
            });
            has_http.then_some(domain)
        })
        .collect()
}

/// True if `gateway_lib` (the text of `cmd/gateway-svc/src/lib.rs`) constructs a
/// `remote::Stub` whose FIRST argument is the string literal `"<name>"`. rustfmt puts the
/// stub name on the line AFTER `Stub::new(`, so a flat `contains("Stub::new(\"x\"")` would
/// miss it — instead each `Stub::new(` site is inspected with leading whitespace (the
/// newline included) trimmed off before matching the literal.
fn gateway_stubs_domain(gateway_lib: &str, name: &str) -> bool {
    let marker = "Stub::new(";
    let needle = format!("\"{name}\"");
    find_all(gateway_lib, marker)
        .into_iter()
        .any(|i| gateway_lib[i + marker.len()..].trim_start().starts_with(&needle))
}

/// Rule 17: a violation per `#[http(`-bearing `http_domains` entry that has no
/// `remote::Stub::new("<name>", …)` in `gateway_lib` (gateway-svc's lib.rs text). Extra
/// stubs are fine — only a MISSING one is reported. Factored out so it is unit-testable
/// without a filesystem walk.
fn gateway_stub_coverage_violations(http_domains: &[String], gateway_lib: &str) -> Vec<String> {
    http_domains
        .iter()
        .filter(|d| !gateway_stubs_domain(gateway_lib, d))
        .map(|d| {
            format!(
                "domain `{d}` exposes HTTP ops (`{HTTP_OP_MARKER}` in api/{d}/api/src/lib.rs) \
                 but cmd/gateway-svc/src/lib.rs has no `Stub::new(\"{d}\"` — add \
                 remote::Stub::new(\"{d}\", ...) to cmd/gateway-svc/src/lib.rs so the gateway \
                 dispatches its player-facing ops Remote in the split"
            )
        })
        .collect()
}

/// True if a `cmd/<dir>` crate is a process main subject to the "every main lists
/// `metrics`" rule: every `*-svc` plus the monolith `server` (not a helper `cmd/` crate,
/// if one is ever added).
fn cmd_is_a_main(dir: &str) -> bool {
    dir.ends_with("-svc") || dir == "server"
}

/// One violation per shipping `cmd/*` root whose transitive normal/build workspace
/// dependency graph reaches the tool-owned conformance policy crate.
fn shipping_conformance_violations(packages: &[serde_json::Value]) -> Vec<String> {
    let package_by_name: HashMap<&str, &serde_json::Value> = packages
        .iter()
        .filter_map(|package| Some((package["name"].as_str()?, package)))
        .collect();
    let mut violations = Vec::new();

    for root in packages {
        let manifest = root["manifest_path"].as_str().unwrap_or_default();
        let Kind::Cmd(cmd) = classify(manifest) else {
            continue;
        };
        if !cmd_is_a_main(&cmd) {
            continue;
        }
        let root_name = root["name"].as_str().unwrap_or_default();
        let mut queue = VecDeque::from([(root_name, vec![root_name.to_string()])]);
        let mut seen = HashSet::from([root_name]);
        let mut found = None;

        while let Some((name, path)) = queue.pop_front() {
            let Some(package) = package_by_name.get(name) else {
                continue;
            };
            for dependency in package["dependencies"].as_array().into_iter().flatten() {
                if dependency["kind"].as_str() == Some("dev") {
                    continue;
                }
                let Some(dependency_name) = dependency["name"].as_str() else {
                    continue;
                };
                let mut dependency_path = path.clone();
                dependency_path.push(dependency_name.to_string());
                if dependency_name == CONFORMANCE_POLICY_CRATE {
                    found = Some(dependency_path);
                    break;
                }
                if package_by_name.contains_key(dependency_name) && seen.insert(dependency_name) {
                    queue.push_back((dependency_name, dependency_path));
                }
            }
            if found.is_some() {
                break;
            }
        }

        if let Some(path) = found {
            violations.push(format!(
                "cmd/{cmd} shipping dependency graph reaches `{CONFORMANCE_POLICY_CRATE}` via {} \
                 — conformance policy belongs only in tools/conformance; shipping modules may \
                 expose factual probes but never depend on its policy types",
                path.join(" -> ")
            ));
        }
    }
    violations
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

/// The `root`-relative path of `file`, backslashes normalized to `/` — so a prefix
/// match like [`BAN_SELF_EXCLUDE`] is OS-independent.
fn workspace_rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Collects every `.rs` file under the [`WORKSPACE_SRC_DIRS`] of `root`, skipping any
/// whose `root`-relative path starts with one of `skip_prefixes`. The two pull-plane
/// bans below share this walk; each applies its own per-line rule.
fn workspace_rs_files(root: &Path, skip_prefixes: &[&str]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in WORKSPACE_SRC_DIRS {
        let mut stack = vec![root.join(dir)];
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
                let rel = workspace_rel(root, &path);
                if skip_prefixes.iter().any(|p| rel.starts_with(p)) {
                    continue;
                }
                out.push(path);
            }
        }
    }
    out
}

/// True if `file` (root-relative) is a test file exempt from the SQL ban: `tests.rs`,
/// `*_tests.rs`, or any file under a `tests/` directory — the sanctioned homes for test
/// code, which may reach plane tables to set up / assert fixtures.
fn is_test_source(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || rel.split('/').any(|seg| seg == "tests")
}

/// Bans the retired producer-push vocabulary ([`RETIRED_EVENT_TOKENS`]) anywhere in
/// workspace source — INCLUDING comments, because a comment naming `EVENTS_SUBSCRIBERS`
/// or `POST "/events"` documents delivery machinery that no longer exists. The archcheck
/// crate itself is excluded ([`BAN_SELF_EXCLUDE`]): it necessarily names these tokens.
fn grep_retired_event_tokens(root: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    for path in workspace_rs_files(root, &[BAN_SELF_EXCLUDE]) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            for tok in RETIRED_EVENT_TOKENS {
                if line.contains(tok) {
                    hits.push(format!(
                        "{}:{}: retired push-plane token `{tok}` — the producer-push event \
                         plane (EVENTS_* env graph + POST /events sink) was replaced by the \
                         pull plane; no shipping source may name it (plan Steps 3-4)",
                        path.display(),
                        i + 1
                    ));
                }
            }
        }
    }
    hits
}

/// Every schema-qualified `asyncevents.<ident>` reference in `line` that is NOT an
/// allowlisted plane-function call ([`ASYNCEVENTS_SQL_ALLOW`]). Left-boundary checked so
/// a longer identifier ending in `asyncevents` never false-positives (a Rust path uses
/// `asyncevents::`, never `asyncevents.`, so a dot means SQL). Returns the referenced
/// object name(s). Factored out so it is unit-testable without a filesystem walk.
fn forbidden_asyncevents_refs(line: &str) -> Vec<String> {
    let marker = "asyncevents.";
    find_all(line, marker)
        .into_iter()
        .filter_map(|i| {
            if i > 0 {
                let prev = line.as_bytes()[i - 1] as char;
                if prev.is_alphanumeric() || prev == '_' {
                    return None; // continuation of a longer identifier, not schema-qualified
                }
            }
            let after = &line[i + marker.len()..];
            if ASYNCEVENTS_SQL_ALLOW.iter().any(|f| after.starts_with(f)) {
                return None; // an allowlisted plane-function call
            }
            let end = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            Some(after[..end].to_string())
        })
        .collect()
}

/// SQL context keywords that legitimately precede a schema-qualified table name
/// (`FROM inventory.items`, `INSERT INTO rating.ratings`, …). `DELETE FROM` is listed
/// explicitly for clarity even though its trailing `FROM` alone already matches.
const SQL_CONTEXT_KEYWORDS: &[&str] =
    &["FROM", "JOIN", "INTO", "UPDATE", "DELETE FROM", "TABLE", "EXISTS"];

/// True if `before` (the line text immediately preceding a `<schema>.` token) ends —
/// ignoring trailing whitespace — with one of [`SQL_CONTEXT_KEYWORDS`]
/// (case-insensitive), the keyword itself left-token-bounded so `XFROM` never counts as
/// `FROM`. This is the gate that separates real query text from the false-positive
/// minefield (topic literals, method-id strings) — neither of those is preceded by a
/// SQL keyword.
fn preceded_by_sql_keyword(before: &str) -> bool {
    let trimmed = before.trim_end();
    SQL_CONTEXT_KEYWORDS.iter().any(|kw| {
        let Some(head) = trimmed.len().checked_sub(kw.len()) else {
            return false;
        };
        // `get` (not slicing) so a non-char-boundary head can't panic on UTF-8 text.
        let Some(tail) = trimmed.get(head..) else {
            return false;
        };
        if !tail.eq_ignore_ascii_case(kw) {
            return false;
        }
        head == 0 || {
            let prev = trimmed.as_bytes()[head - 1] as char;
            !(prev.is_alphanumeric() || prev == '_')
        }
    })
}

/// Every FOREIGN-schema SQL reference in `line`: a `<schema>.` token that (a) names a
/// module OTHER than `own`, (b) starts a fresh identifier (left-boundary checked, so
/// `myinventory.` never matches), and (c) is immediately preceded — ignoring whitespace —
/// by a SQL context keyword ([`preceded_by_sql_keyword`]). Returns the offending schema
/// name(s). Factored out so it is unit-testable without a filesystem walk.
///
/// The keyword gate is deliberate: it catches real query text (`FROM inventory.items`,
/// `INSERT INTO rating.ratings`, `EXISTS (SELECT 1 FROM apikeys.keys)`) while ignoring the
/// dotted tokens that would otherwise false-positive — topic literals
/// (`'config.changed'` as an `append_event` arg) and method ids (`"characters.create"`),
/// none of which are preceded by a SQL keyword.
///
/// DECLARED LIMITATION — the scan is LINE-SCOPED: a multi-line SQL string that splits the
/// keyword from the schema token (`… FROM\n  other.items`) escapes the rule, because the
/// `FROM` and `other.` live on different lines. No such split exists in the tree today;
/// this is a drift tripwire, not full coverage (that stays with the deferred DB-role
/// isolation).
fn foreign_schema_sql_refs(line: &str, own: &str, schemas: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for s in schemas {
        if s == own {
            continue; // a module may freely query its OWN schema
        }
        let marker = format!("{s}.");
        for i in find_all(line, &marker) {
            if i > 0 {
                let prev = line.as_bytes()[i - 1] as char;
                if prev.is_alphanumeric() || prev == '_' {
                    continue; // continuation of a longer identifier, not schema-qualified
                }
            }
            if preceded_by_sql_keyword(&line[..i]) {
                hits.push(s.clone());
            }
        }
    }
    hits
}

/// Bans a module querying ANOTHER module's Postgres schema in SQL literals (finding 7,
/// scoped tripwire). The schema set is the fortress module dir scan ([`crate_dirs`] over
/// `modules/`); each module (own = its dir name) may name only its OWN schema in a SQL
/// keyword context. `asyncevents.` stays with [`grep_asyncevents_sql`] and cross-schema
/// FKs (`REFERENCES`) with [`grep_cross_schema_fk`] — this rule owns the read/write query
/// surface (FROM/JOIN/INTO/UPDATE/…). Comment lines and test sources are skipped (same
/// policy as `grep_asyncevents_sql`: tests may build cross-schema fixtures). See
/// [`foreign_schema_sql_refs`] for the declared line-scope limitation.
fn grep_foreign_schema_sql(root: &Path) -> Vec<String> {
    let modules_root = root.join("modules");
    let schemas = crate_dirs(&modules_root);
    let mut hits = Vec::new();
    for own in &schemas {
        let mut stack = vec![modules_root.join(own).join("src")];
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
                let rel = workspace_rel(root, &path);
                if is_test_source(&rel) {
                    continue; // tests may build cross-schema fixtures
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue; // skip comments/doc — code only
                    }
                    for schema in foreign_schema_sql_refs(line, own, &schemas) {
                        hits.push(format!(
                            "{}:{}: SQL references foreign schema `{schema}.` — module `{own}` \
                             may query only its OWN schema; a relation to another module is a \
                             plain id column resolved via capability or durable events \
                             (CLAUDE.md constraint 10)",
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

/// Bans schema-qualified `asyncevents.` table access in workspace source outside the
/// plane crate (`core/asyncevents`), the operator CLI (`tools/eventctl`), and test
/// files — with the sole exception of the allowlisted plane-function calls. Comment
/// lines are skipped (like the other tripwires): `asyncevents.` legitimately appears in
/// prose describing the plane function. The plane owns its tables; a module reaches
/// them only through the function surface.
fn grep_asyncevents_sql(root: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    let skip = [BAN_SELF_EXCLUDE, "core/asyncevents", "tools/eventctl", "tools/splitproof"];
    for path in workspace_rs_files(root, &skip) {
        let rel = workspace_rel(root, &path);
        if is_test_source(&rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue; // skip comments/doc — code only
            }
            for object in forbidden_asyncevents_refs(line) {
                hits.push(format!(
                    "{}:{}: schema-qualified `asyncevents.{object}` — the durable-events plane \
                     owns its tables; a module reaches them only through the plane function \
                     surface ({}), never direct SQL",
                    path.display(),
                    i + 1,
                    ASYNCEVENTS_SQL_ALLOW.join(", ")
                ));
            }
        }
    }
    hits
}

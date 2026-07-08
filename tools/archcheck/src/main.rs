//! `archcheck` — the fortress dependency-law gate (Step 5). It reads `cargo metadata`
//! and fails (exit 1) on any edge that breaks the dependency law from the plan:
//!
//!   1. **no `modules/X → modules/Y`** — a domain module never imports another
//!      module's impl crate (cross-module comms go through the bus or a contract),
//!   2. **no `modules/X → <foreign>rpc`** — a module may import its OWN `<name>rpc`
//!      glue (sanctioned, rule 5) but NEVER another domain's generated glue.
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

/// A workspace package's classification, derived from its manifest path.
#[derive(Debug, Clone)]
enum Kind {
    /// `modules/<name>/` — a domain module impl.
    Module(String),
    /// `api/<name>/rpc/` — a domain's generated transport glue.
    Rpc(String),
    /// Anything else (foundations, contract crates, cmd, tools).
    Other,
}

fn classify(manifest_path: &str) -> Kind {
    // Normalize Windows backslashes so the segment match is OS-independent.
    let p = manifest_path.replace('\\', "/");
    if let Some(name) = segment_after(&p, "/modules/") {
        // modules/<name>/Cargo.toml
        return Kind::Module(name);
    }
    if let Some(rest) = p.split("/api/").nth(1) {
        // api/<name>/rpc/Cargo.toml  ->  rest = "<name>/rpc/Cargo.toml"
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 && parts[1] == "rpc" {
            return Kind::Rpc(parts[0].to_string());
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
            // (tests) may legitimately reach a core crate like messaging.
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

    // --- 3: regression tripwire — Option<… edge::Server> under modules/ ------
    let modules_dir = workspace_root(meta.clone()).join("modules");
    for line in grep_option_edge_server(&modules_dir) {
        violations.push(line);
    }

    if violations.is_empty() {
        println!("archcheck: OK — no module→module / module→foreign-rpc edges, no Option<edge::Server> in modules/");
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

//! `admincheck` — validates every control-plane manifest the monolith serves.

mod checks;

const USAGE: &str = "usage: admincheck [--strict]";

fn main() {
    let mut strict = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--strict" => strict = true,
            "-h" | "--help" => return println!("{USAGE}"),
            _ => {
                eprintln!("admincheck: unknown argument {arg:?}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    // The monolith hosts every module, so its PROVIDER_SLOT is the full provider set
    // with no peer dialled; a split process would see only its own share.
    let manifests = checkmodules::monolith_manifests().unwrap_or_else(|error| {
        eprintln!("admincheck: build the monolith: {error:#}");
        std::process::exit(1);
    });

    if manifests.is_empty() {
        println!("admincheck: 0 control-plane manifests — no monolith module contributes to controlplane::PROVIDER_SLOT");
    } else {
        let mut modules: Vec<&str> = manifests.iter().map(|m| m.module.as_str()).collect();
        modules.sort_unstable();
        println!(
            "admincheck: {} control-plane manifest(s): {}",
            manifests.len(),
            modules.join(", ")
        );
    }

    let findings = checks::findings(&manifests);
    if findings.is_empty() {
        println!("admincheck: 0 findings");
        return;
    }
    eprintln!("admincheck: {} finding(s):", findings.len());
    for finding in &findings {
        eprintln!("  - {finding}");
    }
    if strict {
        std::process::exit(1);
    }
}

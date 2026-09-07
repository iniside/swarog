//! Tests for the scraper internals that are private to [`super`] (the crate-root
//! `tests.rs` can only reach `pub(crate)` items). Kept in a SEPARATE file per CLAUDE.md
//! hard constraint 10, wired from `scrape.rs` with `#[cfg(test)] #[path = …] mod`.

use super::{discover_api_sources, parse_all_api_crates, served_http_prefixes};
use std::path::{Path, PathBuf};

/// A synthetic workspace root nested under a path that itself contains an ADJACENT
/// `api/src` pair (`…/srv/api/src/checkout/`) — the checkout shape that made the deleted
/// `domain_of` path scan mis-attribute every contract source.
fn checkout_under_an_api_src_path(label: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join(format!(
            "csharp-scrape-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
        .join("srv/api/src/checkout");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_domain(root: &Path, domain: &str, prefix: &str) {
    let src = root.join("api").join(domain).join("api/src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        format!(
            "#[rpc(prefix = \"{prefix}\")]\npub trait Demo {{\n    \
             #[http(verb = \"POST\", path = \"/x\", auth = \"none\", success = 200)]\n    \
             async fn go(&self) -> Result<String, Error>;\n}}\n"
        ),
    )
    .unwrap();
}

/// The 6c09bea regression, pinned: a contract source's domain must come from the
/// `api/<domain>` DIRECTORY WALK, never from scanning the file's own path for an `api/src`
/// anchor. Under this checkout path the path scan answers `checkout` for every source —
/// a domain that is never in `served_domains`, so the served filter dropped every trait
/// and the provider-completeness gate ran against an empty list: exit 0, nothing red.
#[test]
fn a_sources_domain_comes_from_the_api_dir_walk_not_its_own_path() {
    let root = checkout_under_an_api_src_path("attribution");
    write_domain(&root, "social", "social");
    write_domain(&root, "quests", "quests");

    let discovered = discover_api_sources(&root).unwrap();
    let mut domains: Vec<&str> = discovered.iter().map(|(d, _)| d.as_str()).collect();
    domains.sort_unstable();
    domains.dedup();
    assert_eq!(
        domains,
        vec!["quests", "social"],
        "the domain must be the api/<domain> dir name, not a component of the checkout path"
    );

    // The same attribution has to survive onto the parsed trait — that is the field the
    // served filter in `scrape` matches against.
    let parsed = parse_all_api_crates(&root).unwrap();
    let mut attributed: Vec<(&str, &str)> = parsed
        .traits
        .iter()
        .map(|t| (t.prefix.as_str(), t.domain.as_str()))
        .collect();
    attributed.sort_unstable();
    assert_eq!(attributed, vec![("quests", "quests"), ("social", "social")]);
    assert!(
        parsed.traits.iter().all(|t| !t.http_methods.is_empty()),
        "the fixture traits must carry the #[http( methods the completeness gate counts"
    );

    let _ = std::fs::remove_dir_all(root);
}

fn plain_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "csharp-scrape-{label}-{}-{}",
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

/// The retarget's proof, independent of what is on disk in this repo (the real-tree
/// completeness gate only distinguishes "filtered" from "unfiltered" while some
/// contract-only domain happens to exist under `api/` -- the day `modules/groups`
/// lands, deleting the served filter from `served_http_prefixes` would break no
/// committed test). Builds a synthetic root with a served `#[http]`-bearing domain and
/// a contract-only one (contracts under `api/`, no `modules/` dir), and pins that only
/// the served domain's prefix comes back.
#[test]
fn a_contract_only_domains_http_trait_is_excluded_from_served_prefixes() {
    let root = plain_root("served-prefixes");
    write_domain(&root, "served", "served");
    write_domain(&root, "contractonly", "contractonly");
    std::fs::create_dir_all(root.join("modules").join("served")).unwrap();
    std::fs::write(root.join("modules").join("served").join("Cargo.toml"), "").unwrap();

    let parsed = parse_all_api_crates(&root).unwrap();
    let prefixes = served_http_prefixes(&root, &parsed).unwrap();

    assert_eq!(
        prefixes,
        vec!["served".to_string()],
        "only the served domain's #[http] trait prefix must be reachable; contractonly \
         must not appear: {prefixes:?}"
    );

    let _ = std::fs::remove_dir_all(root);
}

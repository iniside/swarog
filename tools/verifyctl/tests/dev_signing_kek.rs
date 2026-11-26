//! Pins the dev `ACCOUNTS_SIGNING_KEK` in the weles fleets and compose.dev.yaml to
//! `testdb::DEV_ACCOUNTS_SIGNING_KEK`: neither can depend on testdb, and verifyctl sees both
//! testdb and weles's own fleet loader.

use std::path::{Path, PathBuf};

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

#[test]
fn weles_fleets_seal_with_the_testdb_kek() {
    for (fleet, service) in
        [("weles/fleet.monolith.toml", "server"), ("weles/fleet.split.toml", "accounts-svc")]
    {
        let def = weles::fleet_toml::load(&repo(fleet)).expect(fleet);
        let accounts = def
            .services
            .iter()
            .find(|s| s.name == service)
            .unwrap_or_else(|| panic!("{fleet} has no {service}"));
        assert_eq!(
            accounts.env.get("ACCOUNTS_SIGNING_KEK").map(String::as_str),
            Some(testdb::DEV_ACCOUNTS_SIGNING_KEK),
            "{fleet} {service}"
        );
    }
}

#[test]
fn compose_dev_seals_with_the_testdb_kek() {
    let text = std::fs::read_to_string(repo("compose.dev.yaml")).expect("read compose.dev.yaml");
    let values: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- ACCOUNTS_SIGNING_KEK="))
        .collect();
    assert_eq!(values, [testdb::DEV_ACCOUNTS_SIGNING_KEK]);
}

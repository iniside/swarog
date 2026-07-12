use std::collections::{BTreeMap, BTreeSet};
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const BUILD_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY", "CARGO_HOME", "CARGO_HTTP_CAINFO", "CARGO_HTTP_PROXY",
    "CARGO_NET_GIT_FETCH_WITH_CLI", "CARGO_TARGET_DIR", "COMSPEC", "GIT_SSL_CAINFO",
    "HOME", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "PATH", "PATHEXT", "RUSTFLAGS",
    "RUSTUP_HOME", "SSL_CERT_DIR", "SSL_CERT_FILE", "SYSTEMROOT", "TEMP", "TMP",
    "USERPROFILE", "WINDIR", "all_proxy", "http_proxy", "https_proxy", "no_proxy",
];

const SERVICE_ENV_ALLOWLIST: &[&str] = &[
    "COMSPEC", "HOME", "PATH", "PATHEXT", "RUST_BACKTRACE", "RUST_LOG", "SYSTEMROOT",
    "TEMP", "TMP", "USERPROFILE", "WINDIR",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetInputs {
    pub database_url: String,
    pub edge_ca_cert: PathBuf,
    pub edge_ca_key: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FleetFlavor {
    Development,
    Proof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    pub name: &'static str,
    pub executable_package: &'static str,
    pub http_port: u16,
    pub edge_port: Option<u16>,
    pub player_port: Option<u16>,
    pub dependencies: Vec<&'static str>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FleetSpec {
    services: Vec<ServiceSpec>,
}

#[derive(Debug, Error)]
pub enum FleetError {
    #[error("unknown service {0}")]
    UnknownService(String),
    #[error("duplicate service {0}")]
    DuplicateService(String),
    #[error("service {service} depends on unknown service {dependency}")]
    UnknownDependency { service: String, dependency: String },
    #[error("fleet drift: cmd/*-svc on disk {on_disk:?} != canonical fleet {canonical:?}")]
    DiskDrift {
        on_disk: Vec<String>,
        canonical: Vec<String>,
    },
    #[error("read service directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl FleetSpec {
    fn new(services: Vec<ServiceSpec>) -> Result<Self, FleetError> {
        let names: BTreeSet<_> = services.iter().map(|service| service.name).collect();
        if names.len() != services.len() {
            let mut seen = BTreeSet::new();
            let duplicate = services
                .iter()
                .map(|service| service.name)
                .find(|name| !seen.insert(*name))
                .expect("different lengths imply a duplicate");
            return Err(FleetError::DuplicateService(duplicate.to_string()));
        }
        for service in &services {
            for dependency in &service.dependencies {
                if !names.contains(dependency) {
                    return Err(FleetError::UnknownDependency {
                        service: service.name.to_string(),
                        dependency: (*dependency).to_string(),
                    });
                }
            }
        }
        Ok(Self { services })
    }

    pub fn services(&self) -> &[ServiceSpec] {
        &self.services
    }

    pub fn service(&self, name: &str) -> Result<&ServiceSpec, FleetError> {
        self.services
            .iter()
            .find(|service| service.name == name)
            .ok_or_else(|| FleetError::UnknownService(name.to_string()))
    }

    pub fn validate_disk(&self, cmd_dir: &Path) -> Result<(), FleetError> {
        let entries = std::fs::read_dir(cmd_dir).map_err(|source| FleetError::ReadDirectory {
            path: cmd_dir.to_path_buf(),
            source,
        })?;
        let on_disk = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_type().ok().filter(|kind| kind.is_dir()).map(|_| entry))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with("-svc"));
        self.validate_names(on_disk)
    }

    pub fn validate_names(
        &self,
        names: impl IntoIterator<Item = String>,
    ) -> Result<(), FleetError> {
        let mut on_disk: Vec<_> = names.into_iter().collect();
        on_disk.sort();
        let mut canonical: Vec<_> = self
            .services
            .iter()
            .map(|service| service.name.to_string())
            .collect();
        canonical.sort();
        if on_disk == canonical {
            Ok(())
        } else {
            Err(FleetError::DiskDrift { on_disk, canonical })
        }
    }
}

pub fn build_environment() -> BTreeMap<String, String> {
    let mut env = inherited_environment(BUILD_ENV_ALLOWLIST);
    #[cfg(windows)]
    append_msvc_linker_path(&mut env);
    env
}

pub fn runtime_environment() -> BTreeMap<String, String> {
    inherited_environment(SERVICE_ENV_ALLOWLIST)
}

fn inherited_environment(allowlist: &[&str]) -> BTreeMap<String, String> {
    allowlist
        .iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).to_string(), value)))
        .collect()
}

#[cfg(windows)]
fn append_msvc_linker_path(env: &mut BTreeMap<String, String>) {
    let Some(program_files) = std::env::var_os("ProgramFiles(x86)") else {
        return;
    };
    let visual_studio = PathBuf::from(program_files).join("Microsoft Visual Studio");
    let Ok(releases) = std::fs::read_dir(visual_studio) else {
        return;
    };
    let mut candidates = Vec::new();
    for release in releases.filter_map(Result::ok) {
        let Ok(editions) = std::fs::read_dir(release.path()) else {
            continue;
        };
        for edition in editions.filter_map(Result::ok) {
            let tools = edition.path().join("VC/Tools/MSVC");
            let Ok(versions) = std::fs::read_dir(tools) else {
                continue;
            };
            for version in versions.filter_map(Result::ok) {
                let tool_root = version.path();
                let bin = tool_root.join("bin/Hostx64/x64");
                if bin.join("link.exe").is_file() {
                    candidates.push((tool_root, bin));
                }
            }
        }
    }
    candidates.sort();
    let Some((msvc_root, linker_dir)) = candidates.pop() else {
        return;
    };
    let sdk_root = PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").expect("ProgramFiles(x86) was present above"),
    )
    .join("Windows Kits/10");
    let sdk_version = newest_directory(&sdk_root.join("Lib"));

    let mut paths = vec![linker_dir];
    if let Some(version) = &sdk_version {
        let sdk_bin = sdk_root.join("bin").join(version).join("x64");
        if sdk_bin.is_dir() {
            paths.push(sdk_bin);
        }
    }
    if let Some(existing) = env.get("PATH") {
        paths.extend(std::env::split_paths(OsStr::new(existing)));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        env.insert("PATH".into(), path.to_string_lossy().into_owned());
    }

    let mut libraries = vec![msvc_root.join("lib/x64")];
    let mut includes = vec![msvc_root.join("include")];
    if let Some(version) = sdk_version {
        libraries.extend(
            ["ucrt/x64", "um/x64"]
                .into_iter()
                .map(|suffix| sdk_root.join("Lib").join(&version).join(suffix)),
        );
        includes.extend(
            ["ucrt", "shared", "um", "winrt", "cppwinrt"]
                .into_iter()
                .map(|suffix| sdk_root.join("Include").join(&version).join(suffix)),
        );
    }
    if let Ok(value) = std::env::join_paths(libraries.into_iter().filter(|path| path.is_dir())) {
        env.insert("LIB".into(), value.to_string_lossy().into_owned());
    }
    if let Ok(value) = std::env::join_paths(includes.into_iter().filter(|path| path.is_dir())) {
        env.insert("INCLUDE".into(), value.to_string_lossy().into_owned());
    }
}

#[cfg(windows)]
fn newest_directory(parent: &Path) -> Option<OsString> {
    let mut directories: Vec<_> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name())
        .collect();
    directories.sort();
    directories.pop()
}

pub fn game_backend_fleet(inputs: &FleetInputs, flavor: FleetFlavor) -> FleetSpec {
    let db = inputs.database_url.clone();
    let cert = inputs.edge_ca_cert.display().to_string();
    let key = inputs.edge_ca_key.display().to_string();
    let base = || {
        let mut env = runtime_environment();
        env.insert("DATABASE_URL".into(), db.clone());
        env.insert("EDGE_CA_CERT".into(), cert.clone());
        env.insert("EDGE_CA_KEY".into(), key.clone());
        env
    };
    let service = |name, http_port, edge_port: Option<u16>, dependencies: Vec<&'static str>| {
        let mut env = base();
        env.insert("PORT".into(), format!(":{http_port}"));
        if let Some(port) = edge_port {
            env.insert("EDGE_ADDR".into(), format!(":{port}"));
        }
        ServiceSpec {
            name,
            executable_package: name,
            http_port,
            edge_port,
            player_port: None,
            dependencies,
            env,
        }
    };
    let peer = |env: &mut BTreeMap<String, String>, key: &str, port: u16| {
        env.insert(format!("{key}_EDGE_ADDR"), format!("127.0.0.1:{port}"));
    };

    let mut accounts = service("accounts-svc", 8084, Some(9003), vec![]);
    let mut apikeys = service("apikeys-svc", 8091, Some(9009), vec![]);
    let audit = service("audit-svc", 8086, Some(9004), vec![]);
    let mut scheduler = service("scheduler-svc", 8087, Some(9005), vec![]);
    let rating = service("rating-svc", 8089, Some(9007), vec![]);
    let leaderboard = service("leaderboard-svc", 8090, Some(9008), vec![]);
    let mut matches = service("match-svc", 8088, Some(9006), vec!["rating-svc"]);
    peer(&mut matches.env, "RATING", 9007);
    let characters = service("characters-svc", 8080, Some(9000), vec![]);
    let config = service("config-svc", 8083, Some(9002), vec![]);
    let mut inventory = service(
        "inventory-svc",
        8081,
        Some(9001),
        vec!["characters-svc", "config-svc"],
    );
    peer(&mut inventory.env, "CHARACTERS", 9000);
    peer(&mut inventory.env, "CONFIG", 9002);

    let mut gateway_env = runtime_environment();
    gateway_env.insert("EDGE_CA_CERT".into(), cert.clone());
    gateway_env.insert("EDGE_CA_KEY".into(), key.clone());
    gateway_env.insert("PORT".into(), ":8082".into());
    gateway_env.insert("PLAYER_EDGE_ADDR".into(), ":9100".into());
    gateway_env.insert("TLS_MODE".into(), "off".into());
    for (name, port) in [
        ("CHARACTERS", 9000),
        ("INVENTORY", 9001),
        ("ACCOUNTS", 9003),
        ("MATCH", 9006),
        ("LEADERBOARD", 9008),
        ("APIKEYS", 9009),
    ] {
        peer(&mut gateway_env, name, port);
    }
    gateway_env.insert("ADMIN_HTTP_ADDR".into(), "127.0.0.1:8085".into());
    gateway_env.insert("ACCOUNTS_HTTP_ADDR".into(), "127.0.0.1:8084".into());
    let gateway = ServiceSpec {
        name: "gateway-svc",
        executable_package: "gateway-svc",
        http_port: 8082,
        edge_port: None,
        player_port: Some(9100),
        dependencies: vec![
            "characters-svc", "inventory-svc", "accounts-svc", "match-svc",
            "leaderboard-svc", "apikeys-svc", "admin-svc",
        ],
        env: gateway_env,
    };

    let mut admin = service(
        "admin-svc",
        8085,
        None,
        vec![
            "characters-svc", "inventory-svc", "config-svc", "accounts-svc", "audit-svc",
            "scheduler-svc", "apikeys-svc",
        ],
    );
    for (name, port) in [
        ("CHARACTERS", 9000),
        ("INVENTORY", 9001),
        ("CONFIG", 9002),
        ("ACCOUNTS", 9003),
        ("AUDIT", 9004),
        ("SCHEDULER", 9005),
        ("APIKEYS", 9009),
    ] {
        peer(&mut admin.env, name, port);
    }
    admin.env.insert("ADMIN_COOKIE_SECURE".into(), "0".into());
    admin
        .env
        .insert("TRUSTED_PROXY_CIDRS".into(), "127.0.0.1/32".into());

    match flavor {
        FleetFlavor::Development => {
            accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
            apikeys.env.insert("APIKEYS_DEV_SEED".into(), "1".into());
            inventory.env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
        }
        FleetFlavor::Proof => {
            accounts.env.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
            accounts.env.insert("EPIC_CLIENT_ID".into(), "test".into());
            accounts.env.insert("EPIC_CLIENT_SECRET".into(), "test".into());
            accounts
                .env
                .insert("EPIC_TOKEN_URL".into(), "http://127.0.0.1:1/token".into());
            apikeys.env.insert("APIKEYS_DEV_SEED".into(), "1".into());
            scheduler.env.insert("SCHEDULER_ENABLED".into(), "1".into());
            inventory.env.insert("INVENTORY_DEV_GRANT".into(), "1".into());
        }
    }

    FleetSpec::new(vec![
        accounts, apikeys, audit, scheduler, rating, leaderboard, matches, characters, config,
        inventory, gateway, admin,
    ])
    .expect("the built-in game backend fleet is internally valid")
}

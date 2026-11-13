use std::{
    ffi::OsString,
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use processctl::{
    OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownPolicy, SpawnSpec, WorkspaceLayout,
};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};

use crate::{
    model::{Outcome, SkipReason},
    runner::Context,
};

/// Ports the C# fixture server binds. `pub(crate)` so `weles_async_island` can
/// diff them against `weles::manifest::AGENT_PORT`: weles is zero-sharing and
/// cannot see this constant, so the collision check belongs where both are
/// visible.
pub(crate) const HTTP_PORT: u16 = 8099;
pub(crate) const PLAYER_PORT: u16 = 9100;
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";
const SHUTDOWN: ShutdownPolicy = ShutdownPolicy {
    graceful_timeout: Duration::from_secs(2),
    force_timeout: Duration::from_secs(5),
};
const CALLBACK_REJECTED: &str = "rejected by the provided RemoteCertificateValidationCallback";
const SYSTEM_CHAIN_REJECTED: &str = "gbclient: certificate rejected: policy errors RemoteCertificateChainErrors";
const NAME_MISMATCH: &str = "gbclient: certificate rejected: policy errors RemoteCertificateNameMismatch";
const ROOT_NOT_PINNED: &str = "gbclient: certificate rejected: chain does not build to the pinned CA";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    if ctx.resolve("dotnet").is_none() {
        return Ok(if ctx.options.install {
            Outcome::Fail
        } else {
            Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool)
        });
    }
    if ports_occupied() {
        ctx.note("csharp fixture requested port 8099/TCP or 9100/UDP is occupied")?;
        return Ok(Outcome::Fail);
    }
    let dotnet = ctx.resolve("dotnet").expect("checked above");
    if ctx.command(
        "dotnet-build",
        dotnet.clone(),
        [
            "build",
            "clients/csharp",
            "-c",
            "Release",
            "--disable-build-servers",
        ]
            .into_iter()
            .map(OsString::from)
            .collect(),
    )? != Outcome::Pass
    {
        return Ok(Outcome::Fail);
    }
    let server = server_executable(ctx);
    if !server.is_file() {
        return Ok(Outcome::Fail);
    }
    let Some(mut child) = boot(ctx, &server, "server", &[])? else {
        return Ok(Outcome::Fail);
    };

    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let cases = vec![
        (
            "c1",
            vec![
                "raw",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-client",
                "leaderboard.topScores",
            ],
            Expected::Success,
        ),
        (
            "c2",
            vec![
                "raw",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-client",
                "characters.create",
                r#"{"name":"x","class":""}"#,
            ],
            Expected::Error(&["Unauthorized"]),
        ),
        (
            "c3",
            vec![
                "raw",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-client",
                "--token",
                "bogus",
                "characters.ownerOf",
                r#"{"character_id":"z"}"#,
            ],
            Expected::Error(&["NotFound"]),
        ),
        (
            "c4",
            vec![
                "flow",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-client",
            ],
            Expected::Success,
        ),
        (
            "c5",
            vec![
                "raw",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-client",
                "match.report",
                Box::leak(
                    format!(
                        r#"{{"ReportId":"c5-{run_id}","Winner":"c5-winner","Loser":"c5-loser"}}"#
                    )
                    .into_boxed_str(),
                ),
            ],
            Expected::Error(&["Forbidden"]),
        ),
        (
            "c6",
            vec![
                "raw",
                "--addr",
                "127.0.0.1:9100",
                "--insecure",
                "--api-key",
                "dev-key-server",
                "match.report",
                Box::leak(
                    format!(
                        r#"{{"ReportId":"c6-{run_id}","Winner":"c6-winner","Loser":"c6-loser"}}"#
                    )
                    .into_boxed_str(),
                ),
            ],
            Expected::Success,
        ),
    ];
    let cases = cases
        .into_iter()
        .map(|(label, cli, expected)| (label, cli.into_iter().map(String::from).collect(), expected))
        .collect();
    let mut result = run_cases(ctx, &dotnet, &mut child, cases)?;
    let _ = child.shutdown(SHUTDOWN);

    let tls = mint_player_chain(&ctx.log_dir.join("csharp-player-tls"))?;
    if ports_occupied() {
        ctx.note("csharp TLS fixture requested port 8099/TCP or 9100/UDP is occupied")?;
        return Ok(Outcome::Fail);
    }
    let Some(mut child) = boot(
        ctx,
        &server,
        "tls-server",
        &[
            ("PLAYER_TLS_CERT_PATH", tls.chain.display().to_string()),
            ("PLAYER_TLS_KEY_PATH", tls.key.display().to_string()),
        ],
    )?
    else {
        return Ok(Outcome::Fail);
    };
    let cases = tls_cases(&tls.root.display().to_string(), &tls.stranger_root.display().to_string());
    if run_cases(ctx, &dotnet, &mut child, cases)? != Outcome::Pass {
        result = Outcome::Fail;
    }
    let _ = child.shutdown(SHUTDOWN);
    Ok(result)
}

/// Boots the fixture server with `extra` on top of the stage environment and waits
/// for `/healthz`; `None` when it never became healthy.
fn boot(
    ctx: &Context<'_>,
    server: &Path,
    label: &str,
    extra: &[(&str, String)],
) -> Result<Option<OwnedChild>> {
    let mut environment = ctx.environment().clone();
    environment.insert("PORT".into(), format!(":{HTTP_PORT}"));
    environment.insert("PLAYER_EDGE_ADDR".into(), format!(":{PLAYER_PORT}"));
    environment.insert(
        "DATABASE_URL".into(),
        ctx.database_url().unwrap_or(DEFAULT_DSN).into(),
    );
    environment.insert("APIKEYS_DEV_SEED".into(), "1".into());
    environment.insert("ACCOUNTS_DEV_AUTH".into(), "1".into());
    environment.insert("INVENTORY_DEV_GRANT".into(), "1".into());
    for (key, value) in extra {
        environment.insert((*key).into(), value.clone());
    }
    let mut child = OwnedChild::spawn(SpawnSpec {
        label: format!("verify-csharp-{label}"),
        executable: server.to_path_buf(),
        args: Vec::new(),
        env: environment
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        cwd: ctx.root.clone(),
        stdout: OutputDestination::File(ctx.stage_log(label, "out")),
        stderr: OutputDestination::File(ctx.stage_log(label, "err")),
        process_group: ProcessGroupPolicy::Owned,
    })?;
    if !wait_healthy(&mut child)? {
        let _ = child.shutdown(SHUTDOWN);
        return Ok(None);
    }
    Ok(Some(child))
}

fn run_cases(
    ctx: &mut Context<'_>,
    dotnet: &Path,
    child: &mut OwnedChild,
    cases: Vec<(&str, Vec<String>, Expected)>,
) -> Result<Outcome> {
    let mut result = Outcome::Pass;
    for (label, cli, expected) in cases {
        let mut args: Vec<String> = [
            "run",
            "--project",
            "clients/csharp",
            "-c",
            "Release",
            "--no-build",
            "--",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        args.extend(cli);
        let code = ctx.command_code(
            label,
            dotnet.to_path_buf(),
            args.into_iter().map(OsString::from).collect(),
            Duration::from_secs(30),
        )?;
        let output = format!(
            "{}\n{}",
            std::fs::read_to_string(ctx.stage_log(label, "out")).unwrap_or_default(),
            std::fs::read_to_string(ctx.stage_log(label, "err")).unwrap_or_default()
        );
        if !code.is_some_and(|code| predicate(code, &output, expected)) {
            result = Outcome::Fail;
        }
        if child.try_wait()?.is_some() {
            result = Outcome::Fail;
            break;
        }
    }
    Ok(result)
}

/// The cases against the fixture serving [`mint_player_chain`]'s leaf: the pin to the
/// root accepts it through the intermediate under the IP-literal default name
/// `localhost`; another name, the OS trust store and a stranger root each refuse it,
/// and each refusal's markers name its own cause.
fn tls_cases(root: &str, stranger_root: &str) -> Vec<(&'static str, Vec<String>, Expected)> {
    let case = |trust: &[&str], server_name: Option<&str>| {
        let mut cli = vec!["raw", "--addr", "127.0.0.1:9100"];
        cli.extend(trust);
        if let Some(server_name) = server_name {
            cli.extend(["--server-name", server_name]);
        }
        cli.extend(["--api-key", "dev-key-client", "leaderboard.topScores"]);
        cli.into_iter().map(String::from).collect::<Vec<_>>()
    };
    vec![
        ("t1", case(&["--ca", root], None), Expected::Success),
        (
            "t2",
            case(&["--ca", root], Some("not-localhost.verifyctl.test")),
            Expected::Error(&[CALLBACK_REJECTED, NAME_MISMATCH]),
        ),
        (
            "t3",
            case(&["--system-roots"], None),
            Expected::Error(&[CALLBACK_REJECTED, SYSTEM_CHAIN_REJECTED]),
        ),
        (
            "t4",
            case(&["--ca", stranger_root], None),
            Expected::Error(&[CALLBACK_REJECTED, ROOT_NOT_PINNED]),
        ),
    ]
}

struct PlayerChain {
    root: PathBuf,
    stranger_root: PathBuf,
    chain: PathBuf,
    key: PathBuf,
}

/// Mints a root CA, an intermediate CA under it and a `localhost` server leaf under
/// the intermediate into `dir`: the root alone, the chain file (leaf then
/// intermediate) and the leaf key. Also mints a stranger root with the root's name
/// but its own key, which signed nothing in the chain.
fn mint_player_chain(dir: &Path) -> Result<PlayerChain> {
    std::fs::create_dir_all(dir)?;
    let ca_params = |name: &str, constraint: BasicConstraints| -> Result<CertificateParams> {
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(constraint);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        Ok(params)
    };
    let root_key = KeyPair::generate()?;
    let root = ca_params("verifyctl csharp root", BasicConstraints::Unconstrained)?
        .self_signed(&root_key)?;
    let intermediate_key = KeyPair::generate()?;
    let intermediate = ca_params("verifyctl csharp intermediate", BasicConstraints::Constrained(0))?
        .signed_by(&intermediate_key, &root, &root_key)?;
    let leaf_key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec!["localhost".to_string()])?;
    leaf_params.distinguished_name = DistinguishedName::new();
    leaf_params.distinguished_name.push(DnType::CommonName, "localhost");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&leaf_key, &intermediate, &intermediate_key)?;
    let stranger_root = ca_params("verifyctl csharp root", BasicConstraints::Unconstrained)?
        .self_signed(&KeyPair::generate()?)?;
    let chain = PlayerChain {
        root: dir.join("root.pem"),
        stranger_root: dir.join("stranger-root.pem"),
        chain: dir.join("chain.pem"),
        key: dir.join("key.pem"),
    };
    std::fs::write(&chain.root, root.pem())?;
    std::fs::write(&chain.stranger_root, stranger_root.pem())?;
    std::fs::write(&chain.chain, format!("{}{}", leaf.pem(), intermediate.pem()))?;
    std::fs::write(&chain.key, leaf_key.serialize_pem())?;
    Ok(chain)
}

#[derive(Clone, Copy)]
enum Expected {
    Success,
    /// Exit code 1 with every marker in the output.
    Error(&'static [&'static str]),
}

fn predicate(code: i32, output: &str, expected: Expected) -> bool {
    match expected {
        Expected::Success => code == 0,
        Expected::Error(markers) => code == 1 && markers.iter().all(|marker| output.contains(marker)),
    }
}

fn ports_occupied() -> bool {
    TcpListener::bind(("127.0.0.1", HTTP_PORT)).is_err()
        || UdpSocket::bind(("127.0.0.1", PLAYER_PORT)).is_err()
}

fn server_executable(ctx: &Context<'_>) -> PathBuf {
    WorkspaceLayout::from_root(ctx.root.clone(), ctx.environment()).binary("debug", "server")
}

fn wait_healthy(child: &mut OwnedChild) -> Result<bool> {
    let runtime = tokio::runtime::Runtime::new()?;
    let client = health_client(&runtime)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(false);
        }
        if runtime
            .block_on(async {
                client
                    .get(format!("http://127.0.0.1:{HTTP_PORT}/healthz"))
                    .send()
                    .await
            })
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(false)
}

fn health_client(runtime: &tokio::runtime::Runtime) -> Result<reqwest::Client> {
    runtime.block_on(async {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
    })
    .map_err(Into::into)
}

#[cfg(test)]
#[path = "csharp_tests.rs"]
mod tests;

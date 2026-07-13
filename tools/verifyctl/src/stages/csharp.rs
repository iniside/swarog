use std::{
    ffi::OsString,
    net::{TcpListener, UdpSocket},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use processctl::{OutputDestination, OwnedChild, ProcessGroupPolicy, ShutdownPolicy, SpawnSpec};

use crate::{
    model::{Outcome, SkipReason},
    runner::Context,
};

const HTTP_PORT: u16 = 8099;
const PLAYER_PORT: u16 = 9100;
const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";
const SHUTDOWN: ShutdownPolicy = ShutdownPolicy {
    graceful_timeout: Duration::from_secs(2),
    force_timeout: Duration::from_secs(5),
};

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
    let mut child = OwnedChild::spawn(SpawnSpec {
        label: "verify-csharp-server".into(),
        executable: server,
        args: Vec::new(),
        env: environment
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        cwd: ctx.root.clone(),
        stdout: OutputDestination::File(ctx.stage_log("server", "out")),
        stderr: OutputDestination::File(ctx.stage_log("server", "err")),
        process_group: ProcessGroupPolicy::Owned,
    })?;
    if !wait_healthy(&mut child)? {
        let _ = child.shutdown(SHUTDOWN);
        return Ok(Outcome::Fail);
    }

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
            Expected::Error("Unauthorized"),
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
            Expected::Error("NotFound"),
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
            Expected::Error("Forbidden"),
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
    let mut result = Outcome::Pass;
    for (label, cli, expected) in cases {
        let mut args = vec![
            "run",
            "--project",
            "clients/csharp",
            "-c",
            "Release",
            "--no-build",
            "--",
        ];
        args.extend(cli);
        let code = ctx.command_code(
            label,
            dotnet.clone(),
            args.into_iter().map(OsString::from).collect(),
            Duration::from_secs(30),
        )?;
        let output = format!(
            "{}\n{}",
            std::fs::read_to_string(ctx.stage_log(label, "out")).unwrap_or_default(),
            std::fs::read_to_string(ctx.stage_log(label, "err")).unwrap_or_default()
        );
        if label == "c1" && code == Some(3) {
            result = Outcome::Skip(SkipReason::NotApplicablePlatform);
            break;
        }
        if !code.is_some_and(|code| predicate(code, &output, expected)) {
            result = Outcome::Fail;
        }
        if child.try_wait()?.is_some() {
            result = Outcome::Fail;
            break;
        }
    }
    let _ = child.shutdown(SHUTDOWN);
    Ok(result)
}

#[derive(Clone, Copy)]
enum Expected {
    Success,
    Error(&'static str),
}

fn predicate(code: i32, output: &str, expected: Expected) -> bool {
    match expected {
        Expected::Success => code == 0,
        Expected::Error(marker) => code == 1 && output.contains(marker),
    }
}

fn ports_occupied() -> bool {
    TcpListener::bind(("127.0.0.1", HTTP_PORT)).is_err()
        || UdpSocket::bind(("127.0.0.1", PLAYER_PORT)).is_err()
}

fn server_executable(ctx: &Context<'_>) -> PathBuf {
    let target = ctx
        .environment()
        .get("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.root.join("target"));
    let target = if target.is_absolute() {
        target
    } else {
        ctx.root.join(target)
    };
    target
        .join("debug")
        .join(format!("server{}", std::env::consts::EXE_SUFFIX))
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

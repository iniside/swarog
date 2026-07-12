use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context as _, Result};

use crate::{
    model::{Outcome, SkipReason},
    runner::{Context, Exit},
};

const BASELINE: &str = "docs/reference/public-api-baseline";
const VERSION: &str = "0.52.0";

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let crates = discover(&ctx.root)?;
    match ensure_tooling(ctx)? {
        Tooling::Ready => {}
        Tooling::NoInstall => {
            return Ok(Outcome::Skip(SkipReason::ExplicitNoInstallMissingTool));
        }
        Tooling::Failed => return Ok(Outcome::Fail),
    }
    let expected = crates.iter().cloned().collect::<BTreeSet<_>>();
    let existing = baseline_stems(&ctx.root.join(BASELINE))?;
    let mut failed = expected != existing;
    for package in crates {
        let label = format!("public-api-{package}");
        let args = [
            "+nightly",
            "public-api",
            "-p",
            package.as_str(),
            "-s",
            "--color=never",
        ];
        if ctx.cargo(&label, &args)? != Outcome::Pass {
            failed = true;
            continue;
        }
        let current = std::fs::read_to_string(ctx.stage_log(&label, "out"))?;
        let committed =
            std::fs::read_to_string(ctx.root.join(BASELINE).join(format!("{package}.txt")))
                .unwrap_or_default();
        let committed = committed
            .lines()
            .filter(|line| !line.starts_with("# cargo-public-api"))
            .collect::<Vec<_>>()
            .join("\n");
        if normalize(&current) != normalize(&committed) {
            failed = true;
        }
    }
    if failed {
        ctx.note(&format!(
            "public-api baseline set: live={expected:?}, baseline={existing:?}"
        ))?;
    }
    Ok(if failed { Outcome::Fail } else { Outcome::Pass })
}

enum Tooling {
    Ready,
    NoInstall,
    Failed,
}

fn ensure_tooling(ctx: &mut Context<'_>) -> Result<Tooling> {
    if ctx.cargo("public-api-tool", &["+nightly", "public-api", "--version"])? == Outcome::Pass {
        return Ok(Tooling::Ready);
    }
    if !ctx.options.install {
        return Ok(Tooling::NoInstall);
    }
    let Some(rustup) = ctx.resolve("rustup") else {
        return Ok(Tooling::Failed);
    };
    if ctx.command(
        "nightly-install",
        rustup,
        ["toolchain", "install", "nightly", "--profile", "minimal"]
            .into_iter()
            .map(OsString::from)
            .collect(),
    )? != Outcome::Pass
    {
        return Ok(Tooling::Failed);
    }
    Ok(
        if ctx.cargo(
            "public-api-install",
            &[
                "+nightly",
                "install",
                "cargo-public-api",
                "--locked",
                "--version",
                VERSION,
            ],
        )? == Outcome::Pass
        {
            Tooling::Ready
        } else {
            Tooling::Failed
        },
    )
}

pub fn bless(root: &Path) -> Result<Exit> {
    super::recover_pending_replacement(root)?;
    std::fs::create_dir_all(root.join("run/verify"))?;
    if !bless_tooling(root)? {
        return Ok(Exit::Failed);
    }
    let crates = discover(root)?;
    let temp = super::temp_dir(&root.join("run/verify"), "public-api-bless")?;
    let version = Command::new("cargo")
        .current_dir(root)
        .args(["+nightly", "public-api", "--version"])
        .output()
        .context("query cargo-public-api")?;
    if !version.status.success() {
        bail!("cargo-public-api is required for blessing");
    }
    let version_text = String::from_utf8_lossy(&version.stdout);
    let version = version_text.split_whitespace().nth(1).unwrap_or(VERSION);
    let mut proposals = Vec::new();
    for package in &crates {
        let output = Command::new("cargo")
            .current_dir(root)
            .args([
                "+nightly",
                "public-api",
                "-p",
                package,
                "-s",
                "--color=never",
            ])
            .output()?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&temp);
            return Ok(Exit::Failed);
        }
        let path = temp.join(format!("{package}.txt"));
        let mut text = format!(
            "# cargo-public-api {version} -- regenerate via cargo run -p verifyctl -- --bless-public-api\n"
        );
        text.push_str(&String::from_utf8(output.stdout)?);
        std::fs::write(&path, text)?;
        proposals.push((
            PathBuf::from(BASELINE).join(format!("{package}.txt")),
            Some(path),
        ));
    }
    let live = crates.into_iter().collect::<BTreeSet<_>>();
    for orphan in baseline_stems(&root.join(BASELINE))?.difference(&live) {
        proposals.push((PathBuf::from(BASELINE).join(format!("{orphan}.txt")), None));
    }
    proposals.sort_by(|a, b| a.0.cmp(&b.0));
    let result = super::replace_recoverably(root, &proposals);
    let _ = std::fs::remove_dir_all(temp);
    result?;
    Ok(Exit::Green)
}

fn bless_tooling(root: &Path) -> Result<bool> {
    if Command::new("cargo")
        .current_dir(root)
        .args(["+nightly", "public-api", "--version"])
        .status()?
        .success()
    {
        return Ok(true);
    }
    if !Command::new("rustup")
        .current_dir(root)
        .args(["toolchain", "install", "nightly", "--profile", "minimal"])
        .status()?
        .success()
    {
        return Ok(false);
    }
    Ok(Command::new("cargo")
        .current_dir(root)
        .args([
            "+nightly",
            "install",
            "cargo-public-api",
            "--locked",
            "--version",
            VERSION,
        ])
        .status()?
        .success())
}

pub(crate) fn discover(root: &Path) -> Result<Vec<String>> {
    let api = root.join("api");
    let mut packages = Vec::new();
    for domain in std::fs::read_dir(&api).with_context(|| format!("read {}", api.display()))? {
        let domain = domain?.path();
        if !domain.is_dir() {
            continue;
        }
        for leaf in ["api", "events"] {
            let manifest = domain.join(leaf).join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let value: toml::Table = toml::from_str(&std::fs::read_to_string(&manifest)?)
                .with_context(|| format!("parse {}", manifest.display()))?;
            let name = value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .with_context(|| format!("missing package.name in {}", manifest.display()))?;
            packages.push(name.to_owned());
        }
    }
    packages.sort();
    packages.dedup();
    Ok(packages)
}

fn baseline_stems(dir: &Path) -> Result<BTreeSet<String>> {
    if !dir.is_dir() {
        return Ok(BTreeSet::new());
    }
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("txt") {
            names.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_manifest_parsed_and_sorted() {
        let root = super::super::temp_dir(&std::env::temp_dir(), "verifyctl-discovery").unwrap();
        for (path, name) in [("api/z/events", "zevents"), ("api/a/api", "aapi")] {
            std::fs::create_dir_all(root.join(path)).unwrap();
            std::fs::write(
                root.join(path).join("Cargo.toml"),
                format!("[package]\nname = '{name}'\nversion = '0.1.0'\n"),
            )
            .unwrap();
        }
        assert_eq!(discover(&root).unwrap(), ["aapi", "zevents"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_and_orphan_baselines_are_deterministic() {
        let root = super::super::temp_dir(&std::env::temp_dir(), "verifyctl-baselines").unwrap();
        std::fs::write(root.join("zeta.txt"), "z").unwrap();
        std::fs::write(root.join("alpha.txt"), "a").unwrap();
        let actual = baseline_stems(&root).unwrap();
        assert_eq!(
            actual.iter().cloned().collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        let live = BTreeSet::from(["alpha".to_owned(), "beta".to_owned()]);
        assert_eq!(
            live.difference(&actual).cloned().collect::<Vec<_>>(),
            ["beta"]
        );
        assert_eq!(
            actual.difference(&live).cloned().collect::<Vec<_>>(),
            ["zeta"]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

use crate::{model::Outcome, runner::Context};
use anyhow::{bail, Context as _, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ROOT_DOCUMENTS: &[&str] = &["README.md", "CLAUDE.md", "AGENTS.md"];
const RETIRED_COMMANDS: &[&str] = &["./run.sh", "./run.ps1", "./verify.sh", "./verify.ps1"];
const RETIRED_LINKS: &[&str] = &["run.sh", "run.ps1", "verify.sh", "verify.ps1"];

pub fn run(ctx: &mut Context<'_>) -> Result<Outcome> {
    let findings = findings(&ctx.root)?;
    for finding in &findings {
        ctx.note(finding)?;
    }
    Ok(if findings.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail
    })
}

fn findings(root: &Path) -> Result<Vec<String>> {
    let packages = workspace_packages(root)?;
    let mut findings = BTreeSet::new();
    for document in current_documents(root)? {
        let contents = std::fs::read_to_string(&document)
            .with_context(|| format!("read current documentation {}", document.display()))?;
        check_document(root, &document, &contents, &packages, &mut findings);
    }
    Ok(findings.into_iter().collect())
}

fn current_documents(root: &Path) -> Result<Vec<PathBuf>> {
    let mut documents = ROOT_DOCUMENTS
        .iter()
        .map(|name| root.join(name))
        .collect::<Vec<_>>();
    for document in &documents {
        if !document.is_file() {
            bail!("required current document {} is missing", document.display());
        }
    }
    collect_reference_markdown(&root.join("docs/reference"), &mut documents)?;
    documents.sort();
    Ok(documents)
}

fn collect_reference_markdown(directory: &Path, documents: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("read current documentation directory {}", directory.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry under {}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_reference_markdown(&path, documents)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("md"))
        {
            documents.push(path);
        }
    }
    Ok(())
}

fn workspace_packages(root: &Path) -> Result<BTreeSet<String>> {
    let root_manifest_path = root.join("Cargo.toml");
    let root_manifest: toml::Value = std::fs::read_to_string(&root_manifest_path)
        .with_context(|| format!("read {}", root_manifest_path.display()))?
        .parse()
        .with_context(|| format!("parse {}", root_manifest_path.display()))?;
    let members = root_manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array);
    let mut packages = BTreeSet::new();
    for member in members.into_iter().flatten() {
        let member = member
            .as_str()
            .context("workspace member must be a string")?;
        if member.contains('*') || member.contains('?') || member.contains('[') {
            bail!("docs-current requires explicit workspace members, found {member}");
        }
        let manifest_path = root.join(member).join("Cargo.toml");
        let manifest: toml::Value = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read workspace member {}", manifest_path.display()))?
            .parse()
            .with_context(|| format!("parse workspace member {}", manifest_path.display()))?;
        let package = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .with_context(|| format!("workspace member {} has no package.name", member))?;
        packages.insert(package.to_owned());
    }
    Ok(packages)
}

fn check_document(
    root: &Path,
    document: &Path,
    contents: &str,
    packages: &BTreeSet<String>,
    findings: &mut BTreeSet<String>,
) {
    let relative = display_path(root, document);
    let mut in_fence = false;
    for (index, line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }

        check_links(root, document, &relative, line_number, line, findings);
        check_packages(&relative, line_number, line, packages, findings);
        if in_fence {
            check_retired_command(&relative, line_number, line.trim(), findings);
        } else if let Some(snippet) = standalone_inline_code(line) {
            check_retired_command(&relative, line_number, snippet, findings);
        }
    }
}

fn check_links(
    root: &Path,
    document: &Path,
    relative: &str,
    line_number: usize,
    line: &str,
    findings: &mut BTreeSet<String>,
) {
    for target in markdown_targets(line) {
        if ignored_link(&target) {
            continue;
        }
        let target_without_suffix = strip_query_and_fragment(&target);
        if target_without_suffix.is_empty() {
            continue;
        }
        let portable_target = target_without_suffix.replace('\\', "/");
        let resolved = if portable_target.starts_with('/') {
            root.join(portable_target.trim_start_matches('/'))
        } else {
            document
                .parent()
                .unwrap_or(root)
                .join(portable_target)
        };
        if retired_root_link(root, &resolved) {
            findings.insert(format!(
                "{relative}:{line_number}: retired wrapper link `{target}`"
            ));
            continue;
        }
        if !resolved.exists() {
            findings.insert(format!(
                "{relative}:{line_number}: missing local Markdown link `{target}`"
            ));
        }
    }
}

fn markdown_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut remaining = line;
    while let Some(marker) = remaining.find("](") {
        let after_marker = &remaining[marker + 2..];
        let Some(end) = after_marker.find(')') else {
            break;
        };
        let raw = after_marker[..end].trim();
        let target = if let Some(angle) = raw.strip_prefix('<') {
            angle.find('>').map(|end| &angle[..end])
        } else {
            raw.split_whitespace().next()
        };
        if let Some(target) = target.filter(|target| !target.is_empty()) {
            targets.push(target.to_owned());
        }
        remaining = &after_marker[end + 1..];
    }
    targets
}

fn ignored_link(target: &str) -> bool {
    let lowercase = target.to_ascii_lowercase();
    target.starts_with('#')
        || lowercase.starts_with("http://")
        || lowercase.starts_with("https://")
        || lowercase.starts_with("mailto:")
}

fn strip_query_and_fragment(target: &str) -> &str {
    let end = target
        .char_indices()
        .find_map(|(index, character)| matches!(character, '?' | '#').then_some(index))
        .unwrap_or(target.len());
    &target[..end]
}

fn retired_root_link(root: &Path, resolved: &Path) -> bool {
    let resolved = normalize_path(resolved);
    RETIRED_LINKS
        .iter()
        .any(|name| resolved == normalize_path(&root.join(name)))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn check_packages(
    relative: &str,
    line_number: usize,
    line: &str,
    packages: &BTreeSet<String>,
    findings: &mut BTreeSet<String>,
) {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if *token != "-p" {
            continue;
        }
        let package = tokens.get(index + 1).and_then(|token| package_token(token));
        match package {
            Some(package) if packages.contains(package) => {}
            Some(package) => {
                findings.insert(format!(
                    "{relative}:{line_number}: unknown workspace package after `-p`: `{package}`"
                ));
            }
            None => {
                findings.insert(format!(
                    "{relative}:{line_number}: missing workspace package immediately after `-p`"
                ));
            }
        }
    }
}

fn package_token(token: &str) -> Option<&str> {
    let token = token.trim_start_matches(|character| matches!(character, '`' | '\'' | '"'));
    let end = token
        .char_indices()
        .find_map(|(index, character)| {
            (!character.is_ascii_alphanumeric() && character != '-' && character != '_')
                .then_some(index)
        })
        .unwrap_or(token.len());
    (end > 0).then_some(&token[..end])
}

fn standalone_inline_code(line: &str) -> Option<&str> {
    let line = line.trim();
    let snippet = line.strip_prefix('`')?.strip_suffix('`')?;
    (!snippet.contains('`')).then_some(snippet.trim())
}

fn check_retired_command(
    relative: &str,
    line_number: usize,
    snippet: &str,
    findings: &mut BTreeSet<String>,
) {
    if snippet.trim_start().starts_with('#') {
        return;
    }
    for command in snippet
        .split_whitespace()
        .filter_map(retired_command_token)
    {
        findings.insert(format!(
            "{relative}:{line_number}: retired executable command `{command}`"
        ));
    }
}

fn retired_command_token(token: &str) -> Option<&'static str> {
    let token = token
        .trim_end_matches(';')
        .trim_matches(|character| matches!(character, '\'' | '"'))
        .replace('\\', "/");
    RETIRED_COMMANDS
        .iter()
        .copied()
        .find(|retired| *retired == token.as_str())
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "verifyctl-docs-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("docs/reference")).unwrap();
        root
    }

    fn check(contents: &str, packages: &[&str]) -> Vec<String> {
        let root = fixture_root("document");
        let document = root.join("README.md");
        std::fs::write(&document, contents).unwrap();
        let packages = packages
            .iter()
            .map(|package| (*package).to_owned())
            .collect();
        let mut findings = BTreeSet::new();
        check_document(&root, &document, contents, &packages, &mut findings);
        let _ = std::fs::remove_dir_all(root);
        findings.into_iter().collect()
    }

    #[test]
    fn reports_broken_links_packages_and_exact_retired_uses() {
        let contents = r#"[broken](missing.md)
```sh
cargo run -p missing-package
./verify.sh --fast
```
[legacy](run.ps1)
"#;
        let findings = check(contents, &["known-package"]);
        assert!(findings.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("missing local Markdown link `missing.md`")));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("unknown workspace package")
                && finding.contains("missing-package")));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("retired executable command `./verify.sh`")));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("retired wrapper link `run.ps1`")));
    }

    #[test]
    fn recognizes_retired_path_tokens_in_command_lines_only() {
        let positive = r#"```sh
./verify.sh;
$ ./run.sh
& .\run.ps1
bash -eux "./verify.sh"
sh -x './run.sh'
pwsh -NoProfile -File .\verify.ps1
powershell -ExecutionPolicy Bypass .\run.ps1
pwsh ./verify.ps1
```
`bash "./verify.sh"`
"#;
        let findings = check(positive, &[]);
        assert_eq!(
            findings
                .iter()
                .filter(|finding| finding.contains("retired executable command"))
                .count(),
            9
        );

        let negative = r#"Do not run `bash ./verify.sh --fast`; use verifyctl.
The retired path `./verify.sh` is mentioned as prose.
```sh
# bash ./verify.sh --fast is retired documentation
  # pwsh -File .\verify.ps1 is retired documentation
bash ./verify.sh.old --fast
bash experiments/go-sketch/verify.sh
pwsh .\experiments\go-sketch\verify.ps1
```
"#;
        assert!(check(negative, &[]).is_empty());
    }

    #[test]
    fn retired_links_are_limited_to_deleted_root_wrappers() {
        let root = fixture_root("retired-links");
        let archive = root.join("experiments/go-sketch");
        std::fs::create_dir_all(&archive).unwrap();
        for wrapper in RETIRED_LINKS {
            std::fs::write(archive.join(wrapper), "archived fixture").unwrap();
        }
        let document = root.join("README.md");
        let contents = r#"[root run sh](run.sh)
[root run ps1](.\run.ps1)
[root verify sh](verify.sh?old=1)
[root verify ps1](./verify.ps1#old)
[archive run sh](experiments/go-sketch/run.sh)
[archive run ps1](experiments\go-sketch\run.ps1)
[archive verify sh](experiments/go-sketch/verify.sh)
[archive verify ps1](experiments/go-sketch/verify.ps1)
"#;
        std::fs::write(&document, contents).unwrap();
        let mut found = BTreeSet::new();
        check_document(&root, &document, contents, &BTreeSet::new(), &mut found);
        assert_eq!(
            found
                .iter()
                .filter(|finding| finding.contains("retired wrapper link"))
                .count(),
            4
        );
        assert!(!found.iter().any(|finding| finding.contains("experiments")));
        assert!(!found
            .iter()
            .any(|finding| finding.contains("missing local Markdown link")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_suffix_stripping_external_links_and_negative_prose() {
        let root = fixture_root("legal");
        std::fs::write(root.join("guide.md"), "guide").unwrap();
        let document = root.join("README.md");
        let contents = r#"[guide](guide.md?view=1#section)
[web](https://example.com/missing.md) [mail](mailto:a@example.com) [anchor](#local)
The retired `run.sh` wrapper is gone, and `./run.sh.old` is not that command.
cargo run `-p` prose-only
"#;
        std::fs::write(&document, contents).unwrap();
        let mut found = BTreeSet::new();
        check_document(&root, &document, contents, &BTreeSet::new(), &mut found);
        assert!(found.is_empty(), "unexpected findings: {found:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn full_scan_excludes_plan_and_status_history() {
        let root = fixture_root("history");
        std::fs::create_dir_all(root.join("tools/known")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = ['tools/known']\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tools/known/Cargo.toml"),
            "[package]\nname = 'known-package'\nversion = '0.1.0'\n",
        )
        .unwrap();
        for document in ROOT_DOCUMENTS {
            std::fs::write(root.join(document), "").unwrap();
        }
        std::fs::write(
            root.join("docs/reference/current.md"),
            "```sh\ncargo run -p known-package\n```\nThe old `run.sh` wrapper was removed.\n",
        )
        .unwrap();
        for history in ["docs/plans/history.md", "docs/status/history.md"] {
            let path = root.join(history);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                "[broken](missing.md)\n```sh\ncargo run -p absent\n./run.sh\n```\n",
            )
            .unwrap();
        }
        assert!(findings(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}

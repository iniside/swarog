use super::*;

// ---- harness ------------------------------------------------------------------

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("testdata")
}

fn blessing() -> bool {
    std::env::var("ADMIN_BLESS").is_ok_and(|v| v == "1")
}

/// The golden form of a render: a line break between two tags that touch, so a change diffs
/// as the lines it touches. A newline the render holds itself becomes [`RAW_NEWLINE`] plus a
/// break, so two renders that differ only there still differ here.
pub(super) fn golden_form(name: &str, raw: &str) -> String {
    assert!(!raw.contains(RAW_NEWLINE), "golden case {name}: the render holds {RAW_NEWLINE:?}, the newline marker");
    let mut out = raw.replace('\n', &format!("{RAW_NEWLINE}\n")).replace("><", ">\n<");
    out.push('\n');
    out
}

/// A unified diff of the first run of differing lines only, with [`DIFF_CONTEXT`] lines
/// around it and each side capped at [`DIFF_MAX_LINES`].
fn compact_diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.split('\n').collect();
    let g: Vec<&str> = got.split('\n').collect();
    let prefix = w.iter().zip(&g).take_while(|(a, b)| a == b).count();
    let suffix = w[prefix..].iter().rev().zip(g[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (w_end, g_end) = (w.len() - suffix, g.len() - suffix);
    let start = prefix.saturating_sub(DIFF_CONTEXT);
    let after = (w_end + DIFF_CONTEXT).min(w.len());
    let mut out = format!(
        "--- golden\n+++ render\n@@ -{},{} +{},{} @@\n",
        start + 1,
        after - start,
        start + 1,
        (prefix - start) + (g_end - prefix) + (after - w_end)
    );
    let mut side = |sign: char, lines: &[&str]| {
        for line in lines.iter().take(DIFF_MAX_LINES) {
            out.push_str(&format!("{sign}{line}\n"));
        }
        if lines.len() > DIFF_MAX_LINES {
            out.push_str(&format!("{sign}… {} more lines\n", lines.len() - DIFF_MAX_LINES));
        }
    };
    side(' ', &w[start..prefix]);
    side('-', &w[prefix..w_end]);
    side('+', &g[prefix..g_end]);
    side(' ', &w[w_end..after]);
    out
}

fn ci_set() -> bool {
    std::env::var_os("CI").is_some()
}

/// The gate over `cases` in `dir`: `Ok` only when every case matches `<dir>/<name>.html` and
/// no `.html` there is an orphan. With `bless` it writes the differing and missing files,
/// deletes the orphans and still returns `Err`, so a bless never passes; with `bless` and
/// `ci` it refuses before touching a file.
fn gate(dir: &Path, cases: &[(String, String)], bless: bool, ci: bool) -> Result<(), String> {
    if bless && ci {
        return Err("refusing ADMIN_BLESS=1 while CI is set: bless locally and commit the goldens".to_string());
    }
    let mut failures = Vec::new();
    let (mut blessed, mut deleted) = (0, 0);
    if bless {
        fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    for (name, golden) in cases {
        let path = dir.join(format!("{name}.html"));
        match fs::read_to_string(&path) {
            Ok(want) if want == *golden => {}
            Ok(_) | Err(_) if bless => {
                fs::write(&path, golden).map_err(|e| format!("write {}: {e}", path.display()))?;
                eprintln!("golden: blessed {}", path.display());
                blessed += 1;
            }
            Ok(want) => failures.push(format!(
                "{name}: render differs from {}\n{}",
                path.display(),
                compact_diff(&want, golden)
            )),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                failures.push(format!("{name}: missing golden file {}", path.display()))
            }
            Err(e) => failures.push(format!("{name}: read {}: {e}", path.display())),
        }
    }
    let produced: BTreeSet<String> = cases.iter().map(|(name, _)| format!("{name}.html")).collect();
    let mut orphans: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|e| e.path()).map_err(|e| format!("read {}: {e}", dir.display())))
            .collect::<Result<Vec<_>, _>>()?,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    orphans.retain(|path| {
        path.extension().is_some_and(|ext| ext == "html")
            && !path.file_name().is_some_and(|n| produced.contains(&*n.to_string_lossy()))
    });
    orphans.sort();
    for path in orphans {
        if bless {
            fs::remove_file(&path).map_err(|e| format!("delete {}: {e}", path.display()))?;
            eprintln!("golden: deleted orphan {}", path.display());
            deleted += 1;
        } else {
            failures.push(format!("orphan golden file {}: no case produces it", path.display()));
        }
    }
    if bless {
        return Err(format!("blessed {blessed} file(s), deleted {deleted} orphan(s); rerun without ADMIN_BLESS"));
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} golden failure(s); run `ADMIN_BLESS=1 cargo test -p admin golden` to accept an intended change:\n\n{}",
        failures.len(),
        failures.join("\n")
    ))
}

#[test]
fn golden_renders_match_testdata() {
    let env = render::env().expect("templates compile");
    let first = goldens(&cases(&env));
    assert_eq!(first, goldens(&cases(&env)), "two renders of the same view models differ");
    let mut seen = BTreeSet::new();
    for (name, _) in &first {
        assert!(
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "golden case name {name:?} is not [a-z0-9_]+"
        );
        assert!(seen.insert(name.clone()), "golden case {name} is defined twice");
    }
    if let Err(msg) = gate(&testdata_dir(), &first, blessing(), ci_set()) {
        panic!("{msg}");
    }
}

/// The gate itself, on a scratch directory: a diff, a missing file and an orphan each fail, a
/// file that is not `.html` is ignored, a bless writes and deletes but still fails, and under
/// `CI` a bless refuses without touching a file.
#[test]
fn golden_gate_fails_on_diff_missing_orphan_and_bless() {
    let dir = std::env::temp_dir().join(format!("admin-golden-gate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let cases = vec![("alpha".to_string(), "<a>\n<b>\n".to_string()), ("beta".to_string(), "<x>\n".to_string())];
    let err = gate(&dir, &cases, false, false).unwrap_err();
    assert!(err.starts_with("2 golden failure(s)") && err.matches("missing golden file").count() == 2, "{err}");

    let err = gate(&dir, &cases, true, true).unwrap_err();
    assert!(err.starts_with("refusing ADMIN_BLESS=1 while CI is set"), "{err}");
    assert!(!dir.exists(), "a refused bless wrote {}", dir.display());

    let err = gate(&dir, &cases, true, false).unwrap_err();
    assert_eq!(err, "blessed 2 file(s), deleted 0 orphan(s); rerun without ADMIN_BLESS");
    assert_eq!(gate(&dir, &cases, false, false), Ok(()));
    let err = gate(&dir, &cases, true, false).unwrap_err();
    assert_eq!(err, "blessed 0 file(s), deleted 0 orphan(s); rerun without ADMIN_BLESS");

    fs::write(dir.join("alpha.html"), "<a>\n<B>\n").unwrap();
    let err = gate(&dir, &cases, false, false).unwrap_err();
    assert!(err.starts_with("1 golden failure(s)"), "{err}");
    assert!(err.contains("alpha: render differs") && err.contains("\n-<B>\n+<b>\n"), "{err}");

    fs::remove_file(dir.join("beta.html")).unwrap();
    fs::write(dir.join("gamma.html"), "<g>\n").unwrap();
    fs::write(dir.join("notes.txt"), "not a golden\n").unwrap();
    let err = gate(&dir, &cases, false, false).unwrap_err();
    assert!(err.starts_with("3 golden failure(s)"), "{err}");
    assert!(err.contains("alpha: render differs"), "{err}");
    assert!(err.contains("beta: missing golden file"), "{err}");
    assert!(err.contains("orphan golden file") && err.contains("gamma.html"), "{err}");

    let err = gate(&dir, &cases, true, true).unwrap_err();
    assert!(err.starts_with("refusing"), "{err}");
    assert!(dir.join("gamma.html").exists() && !dir.join("beta.html").exists());

    let err = gate(&dir, &cases, true, false).unwrap_err();
    assert_eq!(err, "blessed 2 file(s), deleted 1 orphan(s); rerun without ADMIN_BLESS");
    assert_eq!(gate(&dir, &cases, false, false), Ok(()));
    assert!(!dir.join("gamma.html").exists());
    assert!(dir.join("notes.txt").exists());
    assert_eq!(fs::read_to_string(dir.join("alpha.html")).unwrap(), "<a>\n<b>\n");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn golden_form_is_injective_over_render_newlines() {
    assert_ne!(golden_form("t", "<a><b>"), golden_form("t", "<a>\n<b>"));
    assert_eq!(golden_form("t", "<a><b>x</b>"), "<a>\n<b>x</b>\n");
}

#[test]
fn golden_diff_shows_only_the_first_differing_run() {
    let want: String = (0..40).map(|i| format!("<l{i}>\n")).collect();
    let got = want.replace("<l20>", "<L20>");
    let diff = compact_diff(&want, &got);
    assert_eq!(diff, "--- golden\n+++ render\n@@ -19,5 +19,5 @@\n <l18>\n <l19>\n-<l20>\n+<L20>\n <l21>\n <l22>\n");
}

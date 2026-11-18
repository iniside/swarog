//! Golden renders of the portal templates: every primitive macro, the panel chrome in each
//! `PanelState`, the slot placeholders, the modals and their re-renders, the row menu and the
//! search palette. Each case is a view model built in code and rendered through the production
//! `render::panel` / `render::panel_form_result` / `render::modal` / `render::fragment` path;
//! action items come from `routes::action_item` / `routes::panel_tools`.
//!
//! These goldens prove the render layer only: what a given view model draws. They do not
//! prove route behaviour (which state a panel resolves to, which instance is minted, which
//! status a POST answers); that belongs in route tests.
//!
//! Each case is compared with `src/testdata/<name>.html`. Without `ADMIN_BLESS=1` a diff, a
//! missing file, or a `.html` file no case produces fails the run.
//! `ADMIN_BLESS=1 cargo test -p admin golden` rewrites the set, deletes the orphans and then
//! fails, so a leaked `ADMIN_BLESS` never turns the gate green; with `CI` set it refuses to
//! bless at all.
//!
//! Drift guards:
//! - every macro of every `templates/*.tmpl` is listed in [`MACRO_CASES`], checked against the
//!   templates both ways. Each entry carries a signature, a literal of that macro's source and
//!   of no other macro's, nor of any template's text outside its macros, but the ones the entry
//!   names, and every case it lists must render it. Every listed case must also run the macro:
//!   the cases render a second time through templates whose every macro opens with a marker
//!   comment of its own ([`marked_templates`]), the listed case's render must hold the macro's
//!   marker, and every marked render, markers removed, must equal the production render;
//! - [`primitive_case`] and [`state_case`] match every `Primitive` and `PanelState` variant with
//!   no wildcard arm, so a new variant does not compile until it names a case. The number of
//!   distinct rendered cases they name must equal the number of variants parsed from the enums'
//!   own source, so a new variant whose case is not rendered, or that reuses another variant's
//!   case, fails;
//! - every variant of every enum `controlplane::drawn_enums` registers is converted into a view
//!   model by the render path while the cases render, as `render::seen` records it
//!   ([`golden_renders_convert_every_drawn_enum_variant`]).
//!
//! Inputs are fixed: one csrf, one form id, fixed `age_s`, and no clock or id generator,
//! because the render layer reads neither.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Axis, Banner, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    ContributionDecl, DataTable, Delta, DrawnEnum, Emphasis, EntityHeader, EntityRef, Field, FieldKind, FilterDef,
    FilterKind, FormSpec, KpiTile, Layout, Link, Manifest, PageInfo, Palette, PanelBody, Point, Primitive,
    PropertyList, QueueItem, QueueState, ReasonReq, Role, Row, Series, ServiceItem, ServiceState, ServiceStatus,
    Severity, SortDir, Then, TimeRange, TimeSeriesChart, Timeline, TimelineEntry, Tone, WorkQueue,
};
use minijinja::Environment;

use crate::engine::actions::{self, Chrome};
use crate::engine::columns::{ColumnOutcome, ListColumn};
use crate::engine::panels::{LastValue, PanelState};
use crate::engine::roles::Operator;
use crate::render::{
    self, ActionItem, ActionResultView, Crumb, DashboardView, FlashView, FormFrame, FormInstance, MenuView, ModalSpec,
    OperatorView, PageView, PanelInput, RevealView, SearchGroupView, SearchHitView, SearchView, ShellView, SlotView,
};
use crate::routes;

const CSRF: &str = "csrf-golden";
const MODULE: &str = "golden";
const CONTRIBUTION: &str = "panel";
const FORM_ID: &str = "form-golden-1";
/// A render's own newline in the golden form; see [`golden_form`].
const RAW_NEWLINE: char = '↵';
/// Unchanged lines shown around a difference.
const DIFF_CONTEXT: usize = 2;
/// Lines shown per side of a difference before the rest is counted.
const DIFF_MAX_LINES: usize = 12;

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
fn golden_form(name: &str, raw: &str) -> String {
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

/// The ids of a serialized `ActionItem` list.
fn action_ids(actions: &serde_json::Value) -> Vec<&str> {
    actions.as_array().expect("an action list").iter().map(|a| a["id"].as_str().expect("an action id")).collect()
}

/// What the WorkQueue table contract and `TableView.page_label` hand the templates. Until
/// Step 11 part b draws these fields the goldens of the same cases cannot see them, so this
/// pins the view model itself; the attempts bar and the row/tool split are drawn already,
/// and are pinned here as well as in the goldens.
#[test]
fn queue_and_table_views_carry_the_work_queue_table_contract() {
    let view = queue_table_contract_case().view();
    let q = &view["body"];
    assert_eq!(q["type"], "Queue");
    assert_eq!(q["item_label"], "Message");
    assert_eq!(
        q["columns"],
        serde_json::json!([
            {"label": "To", "num": false, "sorted": null, "contributed": false},
            {"label": "Account", "num": false, "sorted": null, "contributed": false},
            {"label": "Template", "num": false, "sorted": null, "contributed": false},
            {"label": "Size", "num": true, "sorted": null, "contributed": false},
        ])
    );
    assert_eq!(q["selectable"], true);
    assert_eq!(q["toolbar"], true);
    assert_eq!(q["search_placeholder"], "Recipient, template or message id");
    assert_eq!(q["q"], "anna");
    assert_eq!(q["search_url"], "/admin/_panel/golden/panel?template=welcome");
    assert_eq!(q["filters"], serde_json::json!([{"label": "Template", "value": "welcome"}]));
    assert_eq!(q["foot"], true);
    assert_eq!(q["total"], "3 of 309 messages");
    assert_eq!(q["size"], 25);
    assert_eq!(q["next_url"], "/admin/_panel/golden/panel?q=anna&template=welcome&cursor=c2");
    assert_eq!(q["page_label"], "1 / 13");
    let items = q["items"].as_array().expect("queue items");
    let cells = &items[0]["cells"];
    assert_eq!(cells[0]["cell"]["kind"], "user");
    assert_eq!(cells[0]["cell"]["href"], "/admin/p/mail/outbox?q=anna%40example.com&state=pending");
    assert_eq!(cells[1]["cell"]["kind"], "ref");
    assert_eq!(cells[1]["cell"]["href"], "/admin/e/account/a-17");
    assert_eq!(cells[2]["cell"]["value"], "welcome");
    let nums: Vec<bool> =
        cells.as_array().expect("cells").iter().map(|c| c["num"].as_bool().expect("a num flag")).collect();
    assert_eq!(nums, [false, false, false, true]);
    // Five squares whatever `max_attempts` says (2, 8 and 1 here); the title keeps the count.
    assert_eq!(items[0]["attempts"], serde_json::json!(["f", "", "", "", ""]));
    assert_eq!(items[1]["attempts"], serde_json::json!(["f", "f", "f", "f", "f"]));
    assert_eq!(items[1]["attempts_title"], "7 attempts");
    assert_eq!(items[2]["attempts"], serde_json::json!(["ok", "", "", "", ""]));
    assert_eq!(action_ids(&items[0]["actions"]), ["cancel"]);
    assert_eq!(action_ids(&items[1]["actions"]), ["retry", "cancel"]);
    assert!(action_ids(&items[2]["actions"]).is_empty());
    assert!(action_ids(&view["tools"]).is_empty(), "a QueueRow action is never a panel tool");

    let view = queue_panel_tool_case().view();
    assert_eq!(action_ids(&view["tools"]), ["pause"], "a PanelTool on a WorkQueue draws in the panel tools");
    assert_eq!(view["tools"][0]["queue_row"], false);
    assert_eq!(view["tools"][0]["enabled"], false);
    let item = &view["body"]["items"][0];
    assert_eq!(action_ids(&item["actions"]), ["cancel"], "an item never draws a PanelTool it lists");
    assert_eq!(item["actions"][0]["queue_row"], true);

    let view = queue_later_page_case().view();
    let q = &view["body"];
    assert_eq!(q["page_label"], serde_json::Value::Null, "a cursor does not say how far in it is");
    assert_eq!(q["next_url"], serde_json::Value::Null);
    assert_eq!(q["total"], "2 of 27 jobs");
    assert_eq!(q["foot"], true);
    assert_eq!(q["toolbar"], false);
    assert_eq!(q["selectable"], false);
    assert_eq!(q["item_label"], "Job");
    let view = queue_items_actions_case().view();
    assert_eq!(view["body"]["total"], "2 jobs");
    assert_eq!(view["body"]["page_label"], "1 / 1");
    let empty = PanelCase::new(PanelState::Empty, Some(body(Primitive::WorkQueue(work_queue("pending", Vec::new())))));
    assert_eq!(empty.view()["body"]["foot"], false);

    let page_label = |case: PanelCase| case.view()["body"]["page_label"].clone();
    assert_eq!(page_label(table_first_page_case()), "1 / 50");
    assert_eq!(
        page_label(PanelCase { query: posted(&[("cursor", "c2")]), ..table_first_page_case() }),
        serde_json::Value::Null
    );
    let rows = || vec![player_row("p-1001", "12", "active", false)];
    let table = |total: Option<u64>, next: Option<&str>| {
        PanelCase::new(PanelState::Ok, Some(body(players_table(rows(), total, next))))
    };
    assert_eq!(page_label(table(None, Some("c2"))), serde_json::Value::Null, "an unknown total with more pages");
    assert_eq!(page_label(table(None, None)), "1 / 1");
    assert_eq!(page_label(table(Some(0), None)), "1 / 1");
    assert_eq!(page_label(table(Some(26), Some("c2"))), "1 / 2");
}

/// A queue a search or a filter value narrowed to nothing keeps its toolbar, so the search
/// can be changed back; the same queue unfiltered, its filter at `any`, is plainly empty.
#[test]
fn a_queue_narrowed_to_nothing_keeps_its_toolbar() {
    let env = render::env().expect("templates compile");
    let render = |case: PanelCase| case.render(&env).expect("renders");
    for (what, raw) in [
        ("a search", render(queue_narrowed_case(&[("q", "zed")], Some("zed")))),
        ("a filter value", render(queue_narrowed_case(&[("template", "welcome")], None))),
    ] {
        assert!(raw.contains("class=\"tbl-toolbar\""), "{what}: the toolbar is drawn: {raw}");
        assert!(raw.contains("No matching items") && raw.contains("Nothing matches the search or filters."), "{what}");
        assert!(!raw.contains("Queue is empty"), "{what}");
    }
    let searched = render(queue_narrowed_case(&[("q", "zed")], Some("zed")));
    assert!(searched.contains("value=\"zed\""), "the search box keeps the search");
    let unfiltered = render(queue_narrowed_case(&[("template", "any")], None));
    assert!(!unfiltered.contains("tbl-toolbar"), "an unfiltered empty queue draws no toolbar: {unfiltered}");
    assert!(unfiltered.contains("Queue is empty") && !unfiltered.contains("No matching items"));
}

/// "Load older" from a page already loaded with `before=` (and a stray `cursor=`) carries
/// the timeline's new `before` and the other parameters, never the old paging.
#[test]
fn timeline_older_url_carries_only_the_new_before() {
    let case = PanelCase {
        query: posted(&[("q", "anna"), ("cursor", "c9"), ("before", "2026-10-22T00:00:00Z")]),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: Vec::new(),
                cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                live: false,
            }))),
        )
    };
    let older = case.view()["body"]["older_url"].clone();
    let older = older.as_str().expect("an older url");
    assert!(!older.contains("cursor"), "{older}");
    assert_eq!(older.matches("before=").count(), 1, "{older}");
    assert_eq!(older, "/admin/_panel/golden/panel?q=anna&before=2026-10-20T23%3A59%3A00Z");
}

/// The `<th>` texts, then each row's `<td>` inner HTML, of the one table in `raw`.
pub(crate) fn table_grid(raw: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let between = |s: &str, open: &str, close: &str| -> String {
        let at = s.find(open).unwrap_or_else(|| panic!("{open} in {s}")) + open.len();
        s[at..at + s[at..].find(close).unwrap_or_else(|| panic!("{close} in {s}"))].to_string()
    };
    let head = between(raw, "<thead><tr>", "</tr></thead>");
    let heads = head.split("<th").skip(1).map(|th| between(th, ">", "</th>")).collect();
    let rows = between(raw, "<tbody>", "</tbody>")
        .split("</tr>")
        .filter(|r| r.contains("<td"))
        .map(|r| r.split("<td").skip(1).map(|td| between(td, ">", "</td>")).collect())
        .collect();
    (heads, rows)
}

/// The list columns land at `list_columns_at` in contribution order, the ok column holds its
/// cell for the row it answered and an empty cell for every other row, the failed column the
/// alert marker in every row and one warn banner; the foot counts in the item noun.
#[test]
fn table_list_columns_draw_cells_markers_and_one_banner_per_failed_column() {
    let env = render::env().expect("templates compile");
    let raw = table_list_columns_case().render(&env).expect("renders");
    let (heads, rows) = table_grid(&raw);
    assert_eq!(heads, ["Player", "<span>Rank</span>", "<span>Guild</span>", "Level <span>↓</span>", "Status"]);
    assert_eq!(rows.len(), 4);
    assert!(rows[0][1].contains("gold"), "the answered player's cell: {:?}", rows[0]);
    for (i, row) in rows.iter().enumerate().skip(1) {
        assert_eq!(row[1], "", "row {i} was not answered: {row:?}");
    }
    let marker = r#"<span class="muted" title="guilds module unavailable">"#;
    for (i, row) in rows.iter().enumerate() {
        assert!(row[2].starts_with(marker), "row {i} draws the marker: {row:?}");
    }
    assert_eq!(raw.matches(r#"<div class="banner warn">"#).count(), 1, "{raw}");
    assert!(raw.contains("Guild column unavailable — guilds module did not answer. Other columns are live."));
    assert!(raw.contains("1,234 players · showing 1–4"), "the foot counts in the noun: {raw}");
    let user = &rows[0][0];
    assert!(!user.starts_with("<a "), "a row that opens an entity links the name inside: {user}");
    assert!(user.contains(r#"<a href="&#x2f;admin&#x2f;e&#x2f;player&#x2f;p-1001""#), "{user}");
}

/// An action's id and the `(entity, item)` it is bound to.
type Binding = (String, Option<String>, Option<String>);

/// The binding of each row's actions in `view`.
fn row_bindings(view: &serde_json::Value) -> Vec<Vec<Binding>> {
    view["body"]["rows"]
        .as_array()
        .expect("table rows")
        .iter()
        .map(|r| {
            r["actions"]
                .as_array()
                .expect("row actions")
                .iter()
                .map(|a| {
                    let vals: BTreeMap<String, String> =
                        serde_json::from_str(a["vals"].as_str().expect("vals")).expect("vals is JSON");
                    let id = a["id"].as_str().expect("an id").to_string();
                    (id, vals.get(actions::FIELD_ENTITY).cloned(), vals.get(actions::FIELD_ITEM).cloned())
                })
                .collect()
        })
        .collect()
}

/// On an entity tab, a row with no entity binds its actions to the panel's entity and a row
/// with its own entity keeps it; on a list page (no panel entity) the first row binds none.
/// `TableRow` actions draw on their rows only, never as tools.
#[test]
fn table_row_actions_bind_the_row_entity_else_the_panel_entity() {
    let some = |s: &str| Some(s.to_string());
    let on_tab = table_row_actions_case().view();
    let rows = row_bindings(&on_tab);
    let first: Vec<_> = ["inspect", "revoke", "open", "wipe"]
        .iter()
        .map(|id| (id.to_string(), some("player:p-1001"), some("sess-1")))
        .collect();
    assert_eq!(rows[0], first, "an entity-less row binds the panel's entity");
    assert_eq!(rows[1], [("inspect".to_string(), some("device:d-9"), some("sess-2"))], "a row's own entity wins");
    assert!(rows[2].is_empty());
    let url = on_tab["body"]["rows"][0]["actions"][0]["url"].as_str().expect("a url");
    assert_eq!(url, "/admin/_action/golden/inspect?entity=player%3Ap-1001&item=sess-1");
    assert_eq!(action_ids(&on_tab["tools"]), ["export"], "a TableRow action is never a panel tool");
    assert_eq!(on_tab["body"]["row_actions"], true);

    let on_list = PanelCase { entity: None, ..table_row_actions_case() }.view();
    let rows = row_bindings(&on_list);
    assert_eq!(rows[0][0], ("inspect".to_string(), None, some("sess-1")), "no panel entity to fall back to");
    assert_eq!(rows[1][0], ("inspect".to_string(), some("device:d-9"), some("sess-2")));
}

/// The header fragment's out-of-band `#crumb-cur` shows the header's crumb, else its title.
#[test]
fn header_crumb_replaces_the_last_breadcrumb_else_the_title_does() {
    let env = render::env().expect("templates compile");
    let render = |crumb: Option<&str>| {
        let header = EntityHeader {
            title: "Anna Kowalska".to_string(),
            crumb: crumb.map(str::to_string),
            subtitle: String::new(),
            status: None,
            badges: Vec::new(),
            avatar_seed: "p-1001".to_string(),
        };
        let case = PanelCase {
            mode: "header",
            ..PanelCase::new(PanelState::Ok, Some(body(Primitive::EntityHeader(header))))
        };
        case.render(&env).expect("renders")
    };
    let oob = |text: &str| format!(r#"<span class="cur" id="crumb-cur" hx-swap-oob="true">{text}</span>"#);
    assert!(render(Some("anna.k")).contains(&oob("anna.k")), "{}", render(Some("anna.k")));
    assert!(render(None).contains(&oob("Anna Kowalska")), "{}", render(None));
}

// ---- view-model builders -----------------------------------------------------------

fn cell(kind: CellKind, value: &str) -> Cell {
    Cell { kind, value: value.to_string(), tone: None, link: None, bar: None, more: Vec::new() }
}

fn toned(kind: CellKind, value: &str, tone: Tone) -> Cell {
    Cell { tone: Some(tone), ..cell(kind, value) }
}

fn bar(value: &str, pct: u8, tone: Option<Tone>) -> Cell {
    Cell { bar: Some(pct), tone, ..cell(CellKind::Mono, value) }
}

fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef { kind: kind.to_string(), id: id.to_string() }
}

fn column(key: &str, label: &str, align: Align) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind: CellKind::Text,
        sortable: false,
        align,
        sorted: None,
    }
}

fn page_link(module: &str, page: &str) -> Link {
    Link::Page { module: module.to_string(), page: page.to_string(), params: BTreeMap::new() }
}

fn props(items: Vec<(&str, Cell)>) -> PropertyList {
    PropertyList {
        items: items.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        columns: 1,
        item: None,
        code: None,
    }
}

fn summary() -> Primitive {
    Primitive::PropertyList(props(vec![
        ("Version", cell(CellKind::Mono, "1.4.2")),
        ("State", cell(CellKind::Badge, "running")),
        ("Region", cell(CellKind::Text, "eu-west")),
    ]))
}

fn body(primitive: Primitive) -> PanelBody {
    PanelBody::new(primitive)
}

fn decl(id: &str, label: &str, icon: &str, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point: ActionPoint::PanelTool(CONTRIBUTION.to_string()),
        label: label.to_string(),
        icon: icon.to_string(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Viewer,
        then,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        when_empty: false,
        prepare: false,
    }
}

fn confirm(title: &str, tone: ConfirmTone) -> ConfirmSpec {
    ConfirmSpec {
        title: title.to_string(),
        body: String::new(),
        tone,
        impact: Vec::new(),
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Confirm".to_string(),
    }
}

fn operator(role: Role) -> Operator {
    Operator { login: "golden".to_string(), role }
}

/// `d` as `routes::action_item` builds it for an operator of `role` on a module that `writes`,
/// unbound, as `routes::panel_tools` hands it to a panel.
fn action(module: &str, d: &ActionDecl, role: Role, writes: bool) -> ActionItem {
    routes::action_item(&operator(role), CSRF, module, writes, d, None)
}

fn tool(d: &ActionDecl) -> ActionItem {
    action(MODULE, d, Role::Admin, true)
}

/// One `render::panel` input: the fields `routes::panel_fragment` fills from a resolved
/// `PanelView`, owned here.
struct PanelCase {
    title: &'static str,
    icon: &'static str,
    mode: &'static str,
    state: PanelState,
    query: Vec<(String, String)>,
    entity: Option<EntityRef>,
    search: Option<&'static str>,
    error: Option<&'static str>,
    age_s: Option<u64>,
    last_value: Option<LastValue>,
    min_role: &'static str,
    fallback_title: &'static str,
    body: Option<PanelBody>,
    actions: Vec<ActionItem>,
    list_columns: Vec<ListColumn>,
    row_menu_kinds: Vec<String>,
    /// Renders the form with a fresh instance to post as.
    form: bool,
}

impl PanelCase {
    fn new(state: PanelState, body: Option<PanelBody>) -> PanelCase {
        PanelCase {
            title: "Golden panel",
            icon: "server",
            mode: "panel",
            state,
            query: Vec::new(),
            entity: None,
            search: None,
            error: None,
            age_s: None,
            last_value: None,
            min_role: "viewer",
            fallback_title: "",
            body,
            actions: Vec::new(),
            list_columns: Vec::new(),
            row_menu_kinds: Vec::new(),
            form: false,
        }
    }

    fn tile(state: PanelState, body: Option<PanelBody>) -> PanelCase {
        PanelCase { title: "Players online", icon: "users", mode: "tile", ..PanelCase::new(state, body) }
    }

    fn render(&self, env: &Environment<'_>) -> Result<String, minijinja::Error> {
        self.with_view(|view| render::fragment(env, "primitives.html", "panel_fragment", view))
    }

    /// The case's `render::panel` view model as JSON: what the template is handed, including
    /// the fields no template draws yet.
    fn view(&self) -> serde_json::Value {
        self.with_view(|view| serde_json::to_value(view).expect("a panel view serializes"))
    }

    fn with_view<T>(&self, f: impl FnOnce(&render::PanelRender) -> T) -> T {
        let path = format!("/admin/_panel/{MODULE}/{CONTRIBUTION}");
        let no_errors = BTreeMap::new();
        let input = PanelInput {
            module: MODULE,
            contribution: CONTRIBUTION,
            title: self.title,
            icon: self.icon,
            mode: self.mode,
            state: self.state,
            path: &path,
            query: &self.query,
            entity: self.entity.as_ref(),
            search: self.search,
            error: self.error,
            age_s: self.age_s,
            last_value: self.last_value.as_ref(),
            min_role: self.min_role,
            fallback_title: self.fallback_title,
            body: self.body.as_ref(),
            actions: &self.actions,
            list_columns: &self.list_columns,
            row_menu_kinds: &self.row_menu_kinds,
            csrf: CSRF,
            form: self.form.then(|| FormInstance {
                post_url: format!("/admin/_action/{MODULE}/save"),
                form_id: FORM_ID.to_string(),
                posted: None,
                errors: &no_errors,
                banner: None,
                locked: false,
            }),
        };
        f(&render::panel(&input))
    }
}

fn slot(mode: &'static str, denied: bool) -> SlotView {
    SlotView {
        module: MODULE.to_string(),
        contribution: CONTRIBUTION.to_string(),
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        url: format!("/admin/_panel/{MODULE}/{CONTRIBUTION}"),
        denied,
        min_role: "admin",
        mode,
    }
}

fn contribution(id: &str, point: Point, min_role: Role) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order: 0,
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        min_role,
        cache_ttl_s: None,
        deadline_ms: 800,
    }
}

/// The slot `routes::slot` builds for contribution `id` at `point` for a Viewer, so a higher
/// `min_role` draws as denied.
fn point_slot(id: &str, point: Point, min_role: Role, tiles: bool) -> SlotView {
    routes::slot(&operator(Role::Viewer), MODULE, &contribution(id, point, min_role), None, tiles)
}

/// The shell of a screen for a Viewer, with nothing in its nav, popovers or flash.
fn shell_view(crumb: &str) -> ShellView {
    ShellView {
        operator: OperatorView::new("golden", crate::role_name(Role::Viewer)),
        csrf: CSRF.to_string(),
        sections: Vec::new(),
        active: None,
        crumbs: vec![Crumb { label: crumb.to_string(), href: None }],
        scopes: Vec::new(),
        flash: None,
        deployment: None,
        health: None,
        notifications: None,
    }
}

/// `view` drawn as screen `name` of `shell.html`, as `routes::screen` draws it, cut to its
/// `<main>`: the shell around it is the same on every screen.
fn screen(env: &Environment<'_>, name: &str, view: &impl serde::Serialize) -> Result<String, minijinja::Error> {
    let ctx = minijinja::context! { screen => name, v => minijinja::Value::from_serialize(view) };
    let raw = render::page(env, "shell.html", &ctx)?;
    let (open, close) = ("<main class=\"main\">", "</main>");
    let start = raw.find(open).expect("the shell draws <main>");
    let end = start + raw[start..].find(close).expect("<main> closes") + close.len();
    Ok(raw[start..end].to_string())
}

fn kpi(delta: Option<Delta>) -> KpiTile {
    KpiTile {
        label: "Players online".to_string(),
        value: "12,408".to_string(),
        unit: None,
        delta,
        series: vec![3.0, 5.0, 4.0, 6.0, 8.0],
        spark: Palette::Chart1,
        capacity: None,
        link: None,
    }
}

fn delta(value: &str, tone: Option<Tone>) -> Option<Delta> {
    Some(Delta { value: value.to_string(), period: "vs yesterday".to_string(), tone })
}

fn service(name: &str, state: ServiceState, value: Option<&str>, note: Option<&str>) -> ServiceItem {
    ServiceItem {
        name: name.to_string(),
        state,
        version: Some("1.4.2".to_string()),
        value: value.map(str::to_string),
        uptime: None,
        note: note.map(str::to_string),
        history: vec![
            ("10:00".to_string(), ServiceState::Running),
            ("10:05".to_string(), ServiceState::Degraded),
            ("10:10".to_string(), ServiceState::Down),
            ("10:15".to_string(), state),
        ],
    }
}

fn services(count: usize, columns: u8, value_label: Option<&str>) -> Primitive {
    let all = [
        service("gateway-svc", ServiceState::Running, Some("33 ms"), None),
        service("billing-svc", ServiceState::Degraded, Some("lag 4 s"), Some("replica lagging")),
        service("chat-svc", ServiceState::Restarting, None, None),
        service("mail-svc", ServiceState::Crashed, Some("—"), Some("exit 137")),
        service("lore-svc", ServiceState::Down, None, None),
    ];
    Primitive::ServiceStatus(ServiceStatus {
        items: all.into_iter().take(count).collect(),
        columns,
        value_label: value_label.map(str::to_string),
    })
}

fn field(key: &str, label: &str, kind: FieldKind, default: Option<&str>) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required: false,
        help: None,
        default: default.map(str::to_string),
        readonly: false,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

/// One field of every `FieldKind`.
fn profile_form() -> FormSpec {
    FormSpec {
        fields: vec![
            Field {
                required: true,
                help: Some("Shown to other players.".to_string()),
                ..field("display_name", "Display name", FieldKind::String, Some("Anna"))
            },
            field("bio", "Bio", FieldKind::Text, None),
            field("level", "Level", FieldKind::Int, Some("12")),
            field("ratio", "Drop ratio", FieldKind::Float, Some("0.25")),
            field("muted", "Muted", FieldKind::Bool, Some("true")),
            field("tier", "Tier", FieldKind::Enum(strings(&["free", "plus", "pro"])), Some("plus")),
            field("regions", "Regions", FieldKind::Multi(strings(&["eu", "us", "asia"])), Some(r#"["eu","asia"]"#)),
            Field { readonly: true, ..field("meta", "Meta", FieldKind::Json, Some(r#"{"v":1}"#)) },
            field("owner", "Owner", FieldKind::Ref("player".to_string()), Some("p-1001")),
            field(
                "credit",
                "Credit",
                FieldKind::Money(strings(&["EUR", "USD"])),
                Some(r#"{"amount":"9.99","currency":"EUR"}"#),
            ),
            field("cooldown", "Cooldown", FieldKind::Duration, Some("30m")),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Save profile".to_string(),
        note: Some("Changes apply on next login.".to_string()),
        revision: Some(3),
    }
}

fn posted(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn player_cell(id: &str) -> Cell {
    Cell { link: Some(Link::Entity(entity("player", id))), ..cell(CellKind::User, id) }
}

/// The players `DataTable` of the `table_*` cases.
fn players_table(rows: Vec<Row>, total: Option<u64>, next: Option<&str>) -> Primitive {
    Primitive::DataTable(DataTable {
        columns: vec![
            column("player", "Player", Align::Left),
            column("level", "Level", Align::Right),
            column("status", "Status", Align::Left),
        ],
        rows,
        page: PageInfo { next_cursor: next.map(str::to_string), size: 25, total },
        filters: vec![FilterDef {
            key: "status".to_string(),
            label: "Status".to_string(),
            kind: FilterKind::Enum(strings(&["active", "banned"])),
        }],
        search_placeholder: Some("Search players".to_string()),
        selectable: true,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: String::new(),
    })
}

fn player_row(id: &str, level: &str, status: &str, highlight: bool) -> Row {
    Row {
        entity: Some(entity("player", id)),
        cells: vec![player_cell(id), cell(CellKind::Number, level), cell(CellKind::Badge, status)],
        highlight,
        item: None,
        actions: Vec::new(),
    }
}

fn list_column(module: &str, label: &str, align: Align, outcome: ColumnOutcome) -> ListColumn {
    ListColumn { module: module.to_string(), label: label.to_string(), align, outcome }
}

/// The players list page: sorted by level, its list columns inserted after the player
/// column, counted in players. Rows: two players, a guild and one with no entity. Two
/// `EntityListColumn`s for `player`: Rank answers p-1001 only, Guild failed.
fn table_list_columns_case() -> PanelCase {
    let status = |value: &str, more: Vec<Cell>| Cell { more, ..cell(CellKind::Label, value) };
    let row = |entity: Option<EntityRef>, cells: Vec<Cell>| Row {
        entity,
        cells,
        highlight: false,
        item: None,
        actions: Vec::new(),
    };
    let table = DataTable {
        columns: vec![
            column("player", "Player", Align::Left),
            Column { sorted: Some(SortDir::Desc), ..column("level", "Level", Align::Right) },
            column("status", "Status", Align::Left),
        ],
        list_columns_for: Some("player".to_string()),
        list_columns_at: 1,
        item_noun: "players".to_string(),
        selectable: false,
        ..match players_table(Vec::new(), Some(1234), Some("c2")) {
            Primitive::DataTable(t) => t,
            _ => unreachable!("players_table is a DataTable"),
        }
    };
    let rows = vec![
        row(
            Some(entity("player", "p-1001")),
            vec![
                player_cell("p-1001"),
                cell(CellKind::Number, "12"),
                status("active", vec![cell(CellKind::Muted, "since 2024")]),
            ],
        ),
        row(
            Some(entity("guild", "g-7")),
            vec![cell(CellKind::Text, "Night Owls"), cell(CellKind::Number, "30"), status("guild", Vec::new())],
        ),
        row(None, vec![cell(CellKind::Text, "system"), cell(CellKind::Number, "0"), status("ok", Vec::new())]),
        row(
            Some(entity("player", "p-1002")),
            vec![player_cell("p-1002"), cell(CellKind::Number, "40"), status("banned", Vec::new())],
        ),
    ];
    PanelCase {
        list_columns: vec![
            list_column(
                "ranks",
                "Rank",
                Align::Right,
                ColumnOutcome::Cells(BTreeMap::from([(
                    "p-1001".to_string(),
                    toned(CellKind::Badge, "gold", Tone::Ok),
                )])),
            ),
            list_column("guilds", "Guild", Align::Left, ColumnOutcome::Failed("connection refused".to_string())),
        ],
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(DataTable { rows, ..table }))))
    }
}

/// Two players of a table counted in players, with a next page.
fn noun_table(total: Option<u64>) -> Primitive {
    let rows = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    match players_table(rows, total, Some("c3")) {
        Primitive::DataTable(t) => Primitive::DataTable(DataTable { item_noun: "players".to_string(), ..t }),
        _ => unreachable!("players_table is a DataTable"),
    }
}

/// The players table ordered by level, lowest first.
fn sorted_ascending_table() -> Primitive {
    let rows = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    match players_table(rows, Some(2), None) {
        Primitive::DataTable(mut t) => {
            t.columns[1].sorted = Some(SortDir::Asc);
            Primitive::DataTable(t)
        }
        _ => unreachable!("players_table is a DataTable"),
    }
}

/// `d` drawn on each `Row` of [`CONTRIBUTION`] that lists it.
fn table_row(d: ActionDecl) -> ActionDecl {
    ActionDecl { point: ActionPoint::TableRow(CONTRIBUTION.to_string()), ..d }
}

/// A player's sessions tab: a detail table on the entity page of `player:p-1001` whose rows
/// carry `TableRow` actions. The first row has no entity, so its actions bind the panel's;
/// the second opens a device, so they bind the device. A `PanelTool` beside them draws in
/// the head only.
fn table_row_actions_case() -> PanelCase {
    let revoke = table_row(ActionDecl {
        danger: true,
        icon: String::new(),
        ..decl("revoke", "Revoke", "", Then::Confirm(confirm("Revoke session?", ConfirmTone::Danger)))
    });
    let inspect =
        table_row(ActionDecl { emphasis: Emphasis::Ghost, ..decl("inspect", "Inspect", "eye", Then::Invoke) });
    let open = table_row(decl("open", "Open", "external", Then::Link(page_link("platform", "logs"))));
    let wipe = table_row(ActionDecl { min_role: Role::Admin, ..decl("wipe", "Wipe", "trash", Then::Invoke) });
    let row = |entity: Option<EntityRef>, item: &str, actions: &[&str]| Row {
        entity,
        cells: vec![cell(CellKind::Mono, item), cell(CellKind::Time, "2026-10-21 09:12")],
        highlight: false,
        item: Some(item.to_string()),
        actions: strings(actions),
    };
    let table = DataTable {
        columns: vec![column("session", "Session", Align::Left), column("seen", "Last seen", Align::Left)],
        rows: vec![
            row(None, "sess-1", &["inspect", "revoke", "open", "wipe"]),
            row(Some(entity("device", "d-9")), "sess-2", &["inspect"]),
            row(None, "sess-3", &[]),
        ],
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: String::new(),
    };
    PanelCase {
        entity: Some(entity("player", "p-1001")),
        actions: vec![
            tool(&inspect),
            tool(&revoke),
            tool(&open),
            action(MODULE, &wipe, Role::Operator, true),
            tool(&decl("export", "Export", "download", Then::Invoke)),
        ],
        ..PanelCase::new(PanelState::Ok, Some(body(Primitive::DataTable(table))))
    }
}

/// A first page of 1,234 players, 25 a page: the pager reads `"1 / 50"`.
fn table_first_page_case() -> PanelCase {
    PanelCase::new(
        PanelState::Ok,
        Some(body(players_table(
            vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)],
            Some(1234),
            Some("c2"),
        ))),
    )
}

/// `d` bound to each `QueueItem` of [`CONTRIBUTION`] that lists it.
fn queue_row(d: ActionDecl) -> ActionDecl {
    ActionDecl { point: ActionPoint::QueueRow(CONTRIBUTION.to_string()), ..d }
}

fn queue_states() -> Vec<QueueState> {
    vec![
        QueueState { key: "pending".to_string(), label: "Pending".to_string(), count: 12, tone: None },
        QueueState { key: "failed".to_string(), label: "Failed".to_string(), count: 3, tone: Some(Tone::Err) },
        QueueState { key: "sent".to_string(), label: "Sent".to_string(), count: 1204, tone: Some(Tone::Ok) },
    ]
}

fn queue_item(id: &str, state: &str, attempts: u32, max_attempts: u32, actions: &[&str]) -> QueueItem {
    QueueItem {
        id: id.to_string(),
        entity: None,
        state: state.to_string(),
        attempts,
        max_attempts,
        next_attempt_at: None,
        last_error: None,
        payload_preview: None,
        actions: strings(actions),
        cells: Vec::new(),
    }
}

/// A queue with no extra column, toolbar, selection or page beyond the one shown.
fn work_queue(selected: &str, items: Vec<QueueItem>) -> WorkQueue {
    WorkQueue {
        states: queue_states(),
        selected: selected.to_string(),
        items,
        item_label: "Job".to_string(),
        item_noun: "jobs".to_string(),
        columns: Vec::new(),
        page: PageInfo { next_cursor: None, size: 25, total: None },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
    }
}

fn retry_decl() -> ActionDecl {
    decl("retry", "Retry", "refresh", Then::Invoke)
}

fn drop_decl() -> ActionDecl {
    ActionDecl { danger: true, ..decl("drop", "Drop", "trash", Then::Confirm(confirm("Drop job?", ConfirmTone::Danger))) }
}

/// `QueueRow` actions on each item: the first non-danger one labelled, the rest ghost icons.
fn queue_items_actions_case() -> PanelCase {
    let park = decl("park", "Park", "park", Then::Invoke);
    PanelCase {
        actions: vec![tool(&queue_row(retry_decl())), tool(&queue_row(drop_decl())), tool(&queue_row(park))],
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(work_queue(
                "failed",
                vec![
                    QueueItem {
                        entity: Some(entity("player", "p-1001")),
                        last_error: Some("timeout after 30s".to_string()),
                        payload_preview: Some(r#"{"amount":5}"#.to_string()),
                        ..queue_item("job-81", "failed", 3, 5, &["drop", "retry"])
                    },
                    QueueItem {
                        next_attempt_at: Some("2026-10-21 10:00".to_string()),
                        ..queue_item("job-82", "sent", 2, 5, &["park", "retry"])
                    },
                ],
            )))),
        )
    }
}

/// A `PanelTool` and a `QueueRow` action on one WorkQueue panel, both listed by the item and
/// both handed over by `routes::panel_tools`: the tool draws in the panel head only, the row
/// action on the item only.
fn queue_panel_tool_case() -> PanelCase {
    let manifest = Manifest {
        module: MODULE.to_string(),
        actions: vec![
            ActionDecl { implemented: false, ..decl("pause", "Pause sending", "pause", Then::Invoke) },
            queue_row(ActionDecl {
                danger: true,
                ..decl("cancel", "Cancel", "x", Then::Confirm(confirm("Cancel message?", ConfirmTone::Danger)))
            }),
        ],
        writes: true,
        ..Manifest::default()
    };
    PanelCase {
        actions: routes::panel_tools(&operator(Role::Admin), CSRF, MODULE, &manifest, CONTRIBUTION),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(work_queue(
                "pending",
                vec![queue_item("job-90", "pending", 0, 5, &["pause", "cancel"])],
            )))),
        )
    }
}

/// The mail outbox's table contract on a first page searched and filtered by URL: extra
/// columns with per-item cells, a selectable column, the toolbar, the foot, and attempts
/// counts below, at and above the fixed 5-square bar.
fn queue_table_contract_case() -> PanelCase {
    let to = |email: &str, state: &str| Cell {
        link: Some(Link::Page {
            module: "mail".to_string(),
            page: "outbox".to_string(),
            params: BTreeMap::from([
                ("q".to_string(), email.to_string()),
                ("state".to_string(), state.to_string()),
            ]),
        }),
        ..cell(CellKind::User, email)
    };
    let account = |id: &str| Cell { link: Some(Link::Entity(entity("account", id))), ..cell(CellKind::Ref, id) };
    let cancel = queue_row(ActionDecl {
        danger: true,
        ..decl("cancel", "Cancel", "x", Then::Confirm(confirm("Cancel message?", ConfirmTone::Danger)))
    });
    let with_cells = |item: QueueItem, cells: Vec<Cell>| QueueItem { cells, ..item };
    PanelCase {
        query: posted(&[("q", "anna"), ("template", "welcome")]),
        search: Some("anna"),
        actions: vec![tool(&queue_row(retry_decl())), tool(&cancel)],
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(WorkQueue {
                item_label: "Message".to_string(),
                item_noun: "messages".to_string(),
                columns: vec![
                    column("to", "To", Align::Left),
                    column("account", "Account", Align::Left),
                    column("template", "Template", Align::Left),
                    column("size", "Size", Align::Right),
                ],
                page: PageInfo { next_cursor: Some("c2".to_string()), size: 25, total: Some(309) },
                filters: vec![FilterDef {
                    key: "template".to_string(),
                    label: "Template".to_string(),
                    kind: FilterKind::Text,
                }],
                search_placeholder: Some("Recipient, template or message id".to_string()),
                selectable: true,
                ..work_queue(
                    "pending",
                    vec![
                        with_cells(
                            queue_item("msg-1001", "pending", 1, 2, &["cancel"]),
                            vec![
                                to("anna@example.com", "pending"),
                                account("a-17"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "2 KB"),
                            ],
                        ),
                        with_cells(
                            QueueItem {
                                last_error: Some("421 throttled".to_string()),
                                ..queue_item("msg-1002", "failed", 7, 8, &["retry", "cancel"])
                            },
                            vec![
                                to("anna.k@example.com", "failed"),
                                account("a-18"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "3 KB"),
                            ],
                        ),
                        with_cells(
                            queue_item("msg-1003", "sent", 1, 1, &[]),
                            vec![
                                to("annette@example.com", "sent"),
                                account("a-19"),
                                cell(CellKind::Text, "welcome"),
                                cell(CellKind::Bytes, "1 KB"),
                            ],
                        ),
                    ],
                )
            }))),
        )
    }
}

/// A page past the first: no `"1 / N"`, no next page, the count against the state's total.
fn queue_later_page_case() -> PanelCase {
    PanelCase {
        query: posted(&[("cursor", "c2")]),
        ..PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::WorkQueue(WorkQueue {
                page: PageInfo { next_cursor: None, size: 25, total: Some(27) },
                ..work_queue(
                    "pending",
                    vec![queue_item("job-26", "pending", 0, 5, &[]), queue_item("job-27", "pending", 0, 5, &[])],
                )
            }))),
        )
    }
}

/// A queue with a search box and a filter, and no item: under `query` and `search` it was
/// narrowed to nothing, or, unfiltered, it is empty in the selected state.
fn queue_narrowed_case(query: &[(&str, &str)], search: Option<&'static str>) -> PanelCase {
    PanelCase {
        query: posted(query),
        search,
        ..PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::WorkQueue(WorkQueue {
                filters: vec![FilterDef {
                    key: "template".to_string(),
                    label: "Template".to_string(),
                    kind: FilterKind::Text,
                }],
                search_placeholder: Some("Recipient, template or message id".to_string()),
                ..work_queue("pending", Vec::new())
            }))),
        )
    }
}

// ---- cases ---------------------------------------------------------------------

/// Every case as `(name, golden form)`, in a fixed order.
fn cases(env: &Environment<'_>) -> Vec<Case> {
    let mut out: Vec<Rendered> = Vec::new();
    let mut panel = |name: &'static str, case: PanelCase| {
        out.push(Rendered {
            name,
            result: case.render(env),
            primitive: case.body.as_ref().map(|b| primitive_case(&b.primitive)),
            state: (case.mode == "panel").then(|| state_case(case.state)),
        })
    };

    // ---- each PanelState, in panel chrome ----
    panel("state_ok", PanelCase::new(PanelState::Ok, Some(body(summary()))));
    panel("state_empty", PanelCase::new(PanelState::Empty, None));
    panel(
        "state_empty_payload",
        PanelCase::new(
            PanelState::Empty,
            Some(body(Primitive::Empty {
                title: "No grants yet".to_string(),
                detail: "Grants appear here once issued.".to_string(),
            })),
        ),
    );
    let readonly_tool = decl("restart", "Restart", "refresh", Then::Invoke);
    panel(
        "state_readonly",
        PanelCase {
            actions: vec![action(MODULE, &readonly_tool, Role::Admin, false)],
            ..PanelCase::new(
                PanelState::Readonly,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Info, text: "Managed by the deploy pipeline.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    panel(
        "state_stale_hides_module_banner",
        PanelCase {
            error: Some("deadline of 2000ms elapsed"),
            age_s: Some(185),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(
                PanelState::Stale,
                Some(PanelBody {
                    banner: Some(Banner { tone: Tone::Warn, text: "This banner shows only when fresh.".to_string() }),
                    ..body(summary())
                }),
            )
        },
    );
    panel(
        "state_error",
        PanelCase {
            error: Some("connection refused"),
            actions: vec![tool(&decl("restart", "Restart", "refresh", Then::Invoke))],
            ..PanelCase::new(PanelState::Error, None)
        },
    );
    panel("state_error_no_detail", PanelCase::new(PanelState::Error, None));
    panel("state_denied", PanelCase { min_role: "admin", ..PanelCase::new(PanelState::Denied, None) });

    // ---- KPI tiles: delta tones, then each state ----
    let tile = |d: Option<Delta>| PanelCase::tile(PanelState::Ok, Some(body(Primitive::KpiTile(kpi(d)))));
    panel("tile_delta_ok_up", tile(delta("+4.2%", Some(Tone::Ok))));
    panel(
        "tile_delta_err_down",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                unit: Some("ms".to_string()),
                spark: Palette::Chart2,
                link: Some(page_link("platform", "latency")),
                ..kpi(delta("+18 ms", Some(Tone::Err)))
            }))),
        ),
    );
    panel(
        "tile_delta_warn_bad_up_capacity",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                spark: Palette::Warn,
                capacity: Some((62.0, 100.0)),
                ..kpi(delta("+9", Some(Tone::Warn)))
            }))),
        ),
    );
    panel("tile_delta_info_flat", tile(delta("±0", Some(Tone::Info))));
    panel(
        "tile_delta_none_period_only",
        PanelCase::tile(
            PanelState::Ok,
            Some(body(Primitive::KpiTile(KpiTile {
                delta: Some(Delta { value: String::new(), period: "last 24 h".to_string(), tone: None }),
                series: vec![4.0],
                ..kpi(None)
            }))),
        ),
    );
    panel(
        "tile_stale",
        PanelCase {
            age_s: Some(185),
            error: Some("deadline of 2000ms elapsed"),
            ..PanelCase::tile(PanelState::Stale, Some(body(Primitive::KpiTile(kpi(delta("+4.2%", Some(Tone::Ok)))))))
        },
    );
    panel(
        "tile_error_last_value",
        PanelCase {
            error: Some("connection refused"),
            last_value: Some(LastValue { value: "12,408 players".to_string(), age_s: 420 }),
            ..PanelCase::tile(PanelState::Error, None)
        },
    );
    panel("tile_error", PanelCase { error: Some("connection refused"), ..PanelCase::tile(PanelState::Error, None) });
    panel("tile_empty", PanelCase::tile(PanelState::Empty, None));
    panel("tile_denied", PanelCase { min_role: "operator", ..PanelCase::tile(PanelState::Denied, None) });

    // ---- panel chrome: heading, meta, note, module banner in every tone ----
    panel(
        "chrome_heading_meta_note",
        PanelCase::new(
            PanelState::Ok,
            Some(PanelBody {
                heading: Some("eu-west-1".to_string()),
                meta: Some("3 nodes".to_string()),
                note: Some("Sampled every 30 s".to_string()),
                ..body(summary())
            }),
        ),
    );
    for (name, tone) in [
        ("chrome_banner_ok", Tone::Ok),
        ("chrome_banner_warn", Tone::Warn),
        ("chrome_banner_err", Tone::Err),
        ("chrome_banner_info", Tone::Info),
    ] {
        panel(
            name,
            PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    banner: Some(Banner { tone, text: "Module banner text.".to_string() }),
                    ..body(summary())
                }),
            ),
        );
    }

    // ---- tools: emphasis, trailing icon, danger, disabled, when_empty ----
    let primary = ActionDecl { emphasis: Emphasis::Primary, ..decl("deploy", "Deploy", "up", Then::Invoke) };
    let default = decl("edit", "Edit", "edit", Then::Form(profile_form()));
    let ghost = ActionDecl {
        emphasis: Emphasis::Ghost,
        ..decl("pause", "Pause", "pause", Then::Confirm(confirm("Pause?", ConfirmTone::Warn)))
    };
    let trailing = ActionDecl {
        emphasis: Emphasis::Ghost,
        trailing_icon: true,
        ..decl("logs", "Open logs", "external", Then::Link(page_link("platform", "logs")))
    };
    let danger = ActionDecl {
        danger: true,
        emphasis: Emphasis::Primary,
        ..decl("drain", "Drain", "trash", Then::Confirm(confirm("Drain?", ConfirmTone::Danger)))
    };
    let denied = ActionDecl { min_role: Role::Admin, ..decl("wipe", "Wipe", "trash", Then::Invoke) };
    let unimplemented = ActionDecl { implemented: false, ..decl("export", "Export", "download", Then::Invoke) };
    panel(
        "tools_emphasis",
        PanelCase {
            entity: Some(entity("server", "eu-1")),
            actions: vec![
                tool(&primary),
                tool(&default),
                tool(&ghost),
                tool(&trailing),
                tool(&danger),
                action(MODULE, &denied, Role::Operator, true),
                tool(&unimplemented),
            ],
            ..PanelCase::new(PanelState::Ok, Some(body(summary())))
        },
    );
    let create = ActionDecl {
        when_empty: true,
        emphasis: Emphasis::Primary,
        ..decl("create", "Create key", "plus", Then::Invoke)
    };
    panel(
        "tools_when_empty",
        PanelCase { actions: vec![tool(&create), tool(&default)], ..PanelCase::new(PanelState::Empty, None) },
    );
    panel(
        "tools_when_empty_payload",
        PanelCase {
            actions: vec![tool(&create), tool(&default)],
            ..PanelCase::new(
                PanelState::Empty,
                Some(body(Primitive::Empty {
                    title: "No keys".to_string(),
                    detail: "Create the first one.".to_string(),
                })),
            )
        },
    );

    // ---- PropertyList and cells ----
    let rotate = ActionDecl {
        danger: true,
        ..decl("rotate", "Rotate", "refresh", Then::Confirm(confirm("Rotate key?", ConfirmTone::Danger)))
    };
    let copy = decl("copy", "Copy", "copy", Then::Invoke);
    panel(
        "props_code_footer_tools",
        PanelCase {
            entity: Some(entity("apikey", "k-7")),
            actions: vec![tool(&rotate), tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Rotated 3 days ago".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        code: Some("curl -H 'X-Api-Key: gb_live_…' https://api.example/v1/ping".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel(
        "props_item_head_tools",
        PanelCase {
            actions: vec![tool(&copy)],
            ..PanelCase::new(
                PanelState::Ok,
                Some(PanelBody {
                    note: Some("Note in the head".to_string()),
                    ..body(Primitive::PropertyList(PropertyList {
                        item: Some("key-7".to_string()),
                        ..props(vec![("Scope", cell(CellKind::Mono, "read"))])
                    }))
                }),
            )
        },
    );
    panel(
        "props_two_columns",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(PropertyList {
                columns: 2,
                ..props(vec![
                    ("Version", cell(CellKind::Mono, "1.4.2")),
                    ("Region", cell(CellKind::Text, "eu-west")),
                    ("Nodes", cell(CellKind::Number, "3")),
                    ("Disk", cell(CellKind::Bytes, "1.2 GB")),
                ])
            }))),
        ),
    );
    panel(
        "cell_kinds",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Text", cell(CellKind::Text, "plain")),
                ("Text err", toned(CellKind::Text, "failing", Tone::Err)),
                ("Mono", cell(CellKind::Mono, "a1b2c3")),
                ("Mono warn", toned(CellKind::Mono, "slow", Tone::Warn)),
                ("Badge derived", cell(CellKind::Badge, "failed")),
                ("Badge unknown", cell(CellKind::Badge, "mystery")),
                ("Badge toned", toned(CellKind::Badge, "pending", Tone::Ok)),
                (
                    "Ref",
                    Cell { link: Some(Link::Entity(entity("item", "sword-7"))), ..cell(CellKind::Ref, "item:sword-7") },
                ),
                ("Number", cell(CellKind::Number, "1,204")),
                ("Bytes", cell(CellKind::Bytes, "512 KB")),
                ("Time", cell(CellKind::Time, "2026-10-21 09:12")),
                ("Time info", toned(CellKind::Time, "in 5 min", Tone::Info)),
                (
                    "User",
                    Cell {
                        link: Some(Link::Entity(entity("player", "p-1001"))),
                        ..cell(CellKind::User, "anna.kowalska")
                    },
                ),
            ])))),
        ),
    );
    panel(
        "cell_bar",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::PropertyList(props(vec![
                ("Default", bar("40%", 40, None)),
                ("Ok tone", bar("30%", 30, Some(Tone::Ok))),
                ("Warn", bar("75%", 75, Some(Tone::Warn))),
                ("Err", bar("100%", 100, Some(Tone::Err))),
                ("Clamped", bar("140%", 140, Some(Tone::Info))),
            ])))),
        ),
    );

    // ---- ServiceStatus: one and two columns, with and without value_label ----
    panel("services_one_column", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, None)))));
    panel("services_one_column_value_label", PanelCase::new(PanelState::Ok, Some(body(services(3, 1, Some("p95"))))));
    panel("services_two_columns", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, None)))));
    panel("services_two_columns_value_label", PanelCase::new(PanelState::Ok, Some(body(services(5, 2, Some("lag"))))));

    // ---- DataTable ----
    panel(
        "table_highlight_row_menu",
        PanelCase {
            query: posted(&[("status", "active"), ("cursor", "c1")]),
            row_menu_kinds: strings(&["player"]),
            ..PanelCase::new(
                PanelState::Ok,
                Some(body(players_table(
                    vec![
                        player_row("p-1001", "12", "active", false),
                        player_row("p-1002", "40", "banned", true),
                        Row {
                            entity: None,
                            cells: vec![
                                cell(CellKind::Text, "system"),
                                cell(CellKind::Number, "0"),
                                cell(CellKind::Badge, "ok"),
                            ],
                            highlight: false,
                            item: None,
                            actions: Vec::new(),
                        },
                    ],
                    Some(1234),
                    Some("c2"),
                ))),
            )
        },
    );
    panel(
        "table_empty_search",
        PanelCase {
            query: posted(&[("q", "zed")]),
            search: Some("zed"),
            ..PanelCase::new(PanelState::Empty, Some(body(players_table(Vec::new(), Some(0), None))))
        },
    );
    panel("table_first_page_label", table_first_page_case());
    panel("table_list_columns", table_list_columns_case());
    panel("table_row_actions", table_row_actions_case());
    panel(
        "table_noun_later_page",
        PanelCase {
            query: posted(&[("cursor", "c2")]),
            ..PanelCase::new(PanelState::Ok, Some(body(noun_table(Some(1234)))))
        },
    );
    panel("table_noun_no_total", PanelCase::new(PanelState::Ok, Some(body(noun_table(None)))));
    panel("table_sorted_ascending", PanelCase::new(PanelState::Ok, Some(body(sorted_ascending_table()))));
    panel("panel_untitled", PanelCase { title: "", ..PanelCase::new(PanelState::Ok, Some(body(summary()))) });

    // ---- Timeline ----
    let entry = |at: &str, actor: Option<EntityRef>, verb: &str, summary: &str| TimelineEntry {
        at: at.to_string(),
        actor,
        verb: verb.to_string(),
        target: None,
        summary: summary.to_string(),
        detail: None,
        icon: String::new(),
        tone: None,
    };
    panel(
        "timeline_two_days_older",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::Timeline(Timeline {
                entries: vec![
                    TimelineEntry {
                        target: Some(entity("item", "sword-7")),
                        detail: Some("batch 42".to_string()),
                        icon: "gift".to_string(),
                        tone: Some(Tone::Ok),
                        ..entry("2026-10-21T09:12:00Z", Some(entity("player", "p-1001")), "grant", "granted 1 item")
                    },
                    entry("not-a-time", Some(entity("user", "ops.anna")), "", "imported legacy entry"),
                    entry("2026-10-21T08:03:00Z", None, "", "nightly compaction finished"),
                    TimelineEntry {
                        detail: Some("attempt 2 of 5".to_string()),
                        icon: "refresh".to_string(),
                        tone: Some(Tone::Warn),
                        ..entry("2026-10-20T23:59:00Z", Some(entity("svc", "billing")), "retry", "webhook retried")
                    },
                ],
                cursor_before: Some("2026-10-20T23:59:00Z".to_string()),
                live: false,
            }))),
        ),
    );
    let no_entries = Timeline { entries: Vec::new(), cursor_before: None, live: false };
    panel("timeline_empty", PanelCase::new(PanelState::Empty, Some(body(Primitive::Timeline(no_entries)))));

    // ---- WorkQueue: row actions, a panel tool, the table contract and its foot ----
    panel("queue_items_actions", queue_items_actions_case());
    panel(
        "queue_empty",
        PanelCase::new(PanelState::Empty, Some(body(Primitive::WorkQueue(work_queue("pending", Vec::new()))))),
    );
    panel("queue_panel_tool", queue_panel_tool_case());
    panel("queue_table_contract", queue_table_contract_case());
    panel("queue_later_page", queue_later_page_case());
    panel("queue_search_empty", queue_narrowed_case(&[("q", "zed")], Some("zed")));
    panel("queue_filter_empty", queue_narrowed_case(&[("template", "welcome")], None));
    panel("queue_unfiltered_empty", queue_narrowed_case(&[("template", "any")], None));

    // ---- TimeSeriesChart ----
    panel(
        "chart_two_axes",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::TimeSeriesChart(TimeSeriesChart {
                buckets: strings(&["00:00", "06:00", "12:00", "18:00"]),
                series: vec![
                    Series {
                        label: "Requests".to_string(),
                        unit: "rps".to_string(),
                        values: vec![120.0, 340.0, 280.0, 410.0],
                        axis: Axis::Left,
                    },
                    Series {
                        label: "p95".to_string(),
                        unit: "ms".to_string(),
                        values: vec![35.5, 42.0, 38.0, 51.25],
                        axis: Axis::Right,
                    },
                ],
            }))),
        ),
    );

    // ---- the other draw modes: attention, header, fact, count, nav ----
    let attention = Primitive::AttentionItems(vec![
        AttentionItem {
            severity: Severity::Err,
            title: "billing-svc is down".to_string(),
            text: "No heartbeat for 4 min".to_string(),
            link: page_link("platform", "services"),
            action_label: "Open".to_string(),
            since: "4 min".to_string(),
        },
        AttentionItem {
            severity: Severity::Warn,
            title: "3 failed jobs".to_string(),
            text: "Webhook deliveries are retrying".to_string(),
            link: Link::Entity(entity("queue", "webhooks")),
            action_label: "Review".to_string(),
            since: "12 min".to_string(),
        },
    ]);
    let in_mode = |mode: &'static str, state: PanelState, primitive: Option<Primitive>| PanelCase {
        mode,
        error: (state == PanelState::Error).then_some("connection refused"),
        ..PanelCase::new(state, primitive.map(body))
    };
    panel("attention_ok", PanelCase { icon: "", ..in_mode("attention", PanelState::Ok, Some(attention)) });
    panel("attention_error", in_mode("attention", PanelState::Error, None));
    let header = EntityHeader {
        title: "Anna Kowalska".to_string(),
        crumb: None,
        subtitle: "player since 2024".to_string(),
        status: Some(cell(CellKind::Badge, "active")),
        badges: Vec::new(),
        avatar_seed: "p-1001".to_string(),
    };
    panel("header_ok", in_mode("header", PanelState::Ok, Some(Primitive::EntityHeader(header.clone()))));
    let crumbed = EntityHeader {
        crumb: Some("anna.k".to_string()),
        badges: vec![cell(CellKind::Label, "verified"), toned(CellKind::Label, "2FA off", Tone::Warn)],
        ..header
    };
    panel("header_crumb_badges", in_mode("header", PanelState::Ok, Some(Primitive::EntityHeader(crumbed))));
    panel("header_error", PanelCase { fallback_title: "p-1001", ..in_mode("header", PanelState::Error, None) });
    let one_cell = |c: Cell| Some(Primitive::Cell(c));
    panel("fact_cell", in_mode("fact", PanelState::Ok, one_cell(toned(CellKind::Badge, "verified", Tone::Ok))));
    panel("fact_error", in_mode("fact", PanelState::Error, None));
    panel("count_cell", in_mode("count", PanelState::Ok, one_cell(cell(CellKind::Number, "14"))));
    panel("count_error", in_mode("count", PanelState::Error, None));
    panel("nav_counter_err", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "3", Tone::Err))));
    panel("nav_counter_warn", in_mode("nav", PanelState::Ok, one_cell(toned(CellKind::Number, "7", Tone::Warn))));

    // ---- panel form: fresh, readonly, readonly beside an enabled tool ----
    panel(
        "panel_form",
        PanelCase { form: true, ..PanelCase::new(PanelState::Ok, Some(body(Primitive::Form(profile_form())))) },
    );
    panel("panel_form_readonly", PanelCase::new(PanelState::Readonly, Some(body(Primitive::Form(profile_form())))));
    // The module writes, but the viewer may not use the PanelForm action (so the panel is
    // readonly) while a PanelTool of the same panel stays enabled for them.
    let form_manifest = Manifest {
        module: MODULE.to_string(),
        actions: vec![
            ActionDecl {
                point: ActionPoint::PanelForm(CONTRIBUTION.to_string()),
                min_role: Role::Admin,
                ..decl("save", "Save", "check", Then::Invoke)
            },
            decl("refresh", "Refresh", "refresh", Then::Invoke),
            ActionDecl { min_role: Role::Admin, order: 1, ..decl("purge", "Purge", "trash", Then::Invoke) },
        ],
        writes: true,
        ..Manifest::default()
    };
    panel(
        "panel_form_readonly_enabled_tool",
        PanelCase {
            actions: routes::panel_tools(&operator(Role::Viewer), CSRF, MODULE, &form_manifest, CONTRIBUTION),
            ..PanelCase::new(PanelState::Readonly, Some(body(Primitive::Form(profile_form()))))
        },
    );

    // ---- the empty cells of the draw-mode x state matrix ----
    panel("fact_denied", PanelCase { min_role: "admin", ..in_mode("fact", PanelState::Denied, None) });
    panel(
        "state_stale_no_age",
        PanelCase {
            error: Some("deadline of 2000ms elapsed"),
            ..PanelCase::new(PanelState::Stale, Some(body(summary())))
        },
    );
    // `panels::resolve` errors a ColumnCells answer, so a panel never draws one: its body
    // draws nothing of the cells.
    panel(
        "column_cells",
        PanelCase::new(
            PanelState::Ok,
            Some(body(Primitive::ColumnCells(BTreeMap::from([
                ("Guild".to_string(), cell(CellKind::Text, "Night Owls")),
                ("Rank".to_string(), toned(CellKind::Badge, "officer", Tone::Info)),
            ])))),
        ),
    );

    // ---- a panel form shown again after a 422 and a 409: `routes::render_instance` ----
    let form_decl = ContributionDecl {
        id: CONTRIBUTION.to_string(),
        point: Point::Page("profile".to_string()),
        order: 0,
        title: "Golden panel".to_string(),
        icon: "server".to_string(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 0,
    };
    let chrome = Chrome {
        meta: Some("rev 3".to_string()),
        heading: Some("anna".to_string()),
        note: Some("Saved by ops".to_string()),
        banner: Some(Banner { tone: Tone::Info, text: "Edits sync within a minute.".to_string() }),
    };
    let player = entity("player", "p-1001");
    let frame_path = format!("/admin/_panel/{MODULE}/{CONTRIBUTION}");
    let frame_query = vec![("entity".to_string(), render::entity_param(&player))];
    let frame_tools = vec![tool(&decl("copy", "Copy", "copy", Then::Invoke))];
    let frame = FormFrame {
        module: MODULE,
        decl: &form_decl,
        mode: "panel",
        path: &frame_path,
        query: &frame_query,
        entity: Some(&player),
        actions: &frame_tools,
        csrf: CSRF,
    };
    let profile_spec = profile_form();
    let profile_body = posted(&[
        ("display_name", ""),
        ("bio", "hi"),
        ("level", "abc"),
        ("ratio", "0.5"),
        ("muted", "false"),
        ("tier", "pro"),
        ("regions", "us"),
        ("meta", r#"{"v":2}"#),
        ("owner", "p-2"),
        ("credit", "5"),
        ("credit__currency", "USD"),
        ("cooldown", "1h"),
    ]);
    let field_errors = BTreeMap::from([
        ("display_name".to_string(), "must not be empty".to_string()),
        ("level".to_string(), "must be an integer".to_string()),
    ]);
    let no_errors = BTreeMap::new();
    let rerender = |errors: &BTreeMap<String, String>, banner: Option<&str>, locked: bool| {
        let instance = FormInstance {
            post_url: format!("/admin/_action/{MODULE}/save"),
            form_id: FORM_ID.to_string(),
            posted: Some(&profile_body),
            errors,
            banner: banner.map(str::to_string),
            locked,
        };
        let rendered = render::panel_form_result(&frame, &profile_spec, &chrome, instance);
        render::fragment(env, "primitives.html", "panel_fragment", &rendered)
    };
    out.push(plain("panel_form_rerender_422_chrome", rerender(&field_errors, None, false)));
    out.push(plain(
        "panel_form_rerender_409_locked",
        rerender(&no_errors, Some("Someone else saved this form first."), true),
    ));

    // ---- slot placeholders: loading_block and the denied states, by draw mode ----
    for (name, mode, denied) in [
        ("slot_panel_loading", "panel", false),
        ("slot_panel_denied", "panel", true),
        ("slot_tile_loading", "tile", false),
        ("slot_tile_denied", "tile", true),
        ("slot_attention_loading", "attention", false),
        ("slot_attention_denied", "attention", true),
        ("slot_inline_loading", "fact", false),
    ] {
        out.push(plain(name, render::fragment(env, "primitives.html", "slot", &slot(mode, denied))));
    }
    let untitled = SlotView { title: String::new(), ..slot("panel", false) };
    out.push(plain("slot_untitled", render::fragment(env, "primitives.html", "slot", &untitled)));

    // ---- modals ----
    let modal = |d: &ActionDecl, spec: ModalSpec<'_>| {
        let view = render::modal(
            d,
            spec,
            format!("/admin/_action/{MODULE}/{}", d.id),
            CSRF.to_string(),
            FORM_ID.to_string(),
            None,
            &no_errors,
        );
        render::fragment(env, "modal.html", "modal", &view)
    };
    let ban_spec = ConfirmSpec {
        title: "Ban p-1001?".to_string(),
        body: "The player is signed out and cannot sign in.".to_string(),
        tone: ConfirmTone::Danger,
        impact: vec![
            ("Sessions".to_string(), cell(CellKind::Number, "3")),
            ("Status".to_string(), cell(CellKind::Badge, "banned")),
            ("Owner".to_string(), cell(CellKind::User, "anna.kowalska")),
        ],
        reason: ReasonReq::Required,
        reason_hint: Some("e.g. chargeback fraud, ticket #".to_string()),
        type_to_confirm: Some("p-1001".to_string()),
        confirm_label: "Ban".to_string(),
    };
    let ban = ActionDecl { danger: true, ..decl("ban", "Ban player", "", Then::Confirm(ban_spec.clone())) };
    out.push(plain("modal_confirm_danger_impact_reason", modal(&ban, ModalSpec::Confirm(&ban_spec))));
    let pause_spec = ConfirmSpec {
        body: "Deliveries stop until resumed.".to_string(),
        reason: ReasonReq::Optional,
        confirm_label: "Pause".to_string(),
        ..confirm("Pause webhooks?", ConfirmTone::Warn)
    };
    let pause = decl("pause", "Pause queue", "pause", Then::Confirm(pause_spec.clone()));
    out.push(plain("modal_confirm_warn_optional_reason", modal(&pause, ModalSpec::Confirm(&pause_spec))));
    let notice_spec = ConfirmSpec {
        body: "Players see the notice on their next sign-in.".to_string(),
        confirm_label: "Send".to_string(),
        ..confirm("Send the notice?", ConfirmTone::Info)
    };
    let notice = decl("notice", "Send notice", "", Then::Confirm(notice_spec.clone()));
    out.push(plain("modal_confirm_info_no_reason", modal(&notice, ModalSpec::Confirm(&notice_spec))));
    let grant_spec = FormSpec {
        fields: vec![
            Field { required: true, ..field("item_id", "Item", FieldKind::Ref("item".to_string()), None) },
            field("qty", "Quantity", FieldKind::Int, Some("1")),
        ],
        hidden: BTreeMap::new(),
        submit_label: "Grant".to_string(),
        note: Some("Granted items appear in the next sync.".to_string()),
        revision: None,
    };
    let grant = decl("grant", "Grant item", "gift", Then::Form(grant_spec.clone()));
    out.push(plain("modal_form", modal(&grant, ModalSpec::Form(&grant_spec))));
    // A modal shown again after a POST, through `render::modal_rerender` as
    // `routes::render_instance` calls it.
    let reshow = |d: &ActionDecl,
                  spec: ModalSpec<'_>,
                  body: &[(String, String)],
                  errors: &BTreeMap<String, String>,
                  banner: Option<&str>,
                  locked: bool| {
        let instance = FormInstance {
            post_url: format!("/admin/_action/{MODULE}/{}", d.id),
            form_id: FORM_ID.to_string(),
            posted: Some(body),
            errors,
            banner: banner.map(str::to_string),
            locked,
        };
        render::fragment(env, "modal.html", "result", &render::modal_rerender(d, spec, CSRF.to_string(), instance))
    };
    let grant_errors = BTreeMap::from([
        ("item_id".to_string(), "must not be empty".to_string()),
        ("qty".to_string(), "must be an integer".to_string()),
        ("stray".to_string(), "not a field".to_string()),
    ]);
    out.push(plain(
        "modal_rerender_form_errors_locked",
        reshow(
            &grant,
            ModalSpec::Form(&grant_spec),
            &posted(&[("item_id", ""), ("qty", "x")]),
            &grant_errors,
            Some("Someone else saved this form first."),
            true,
        ),
    ));
    out.push(plain(
        "modal_rerender_form_stray_error",
        reshow(
            &grant,
            ModalSpec::Form(&grant_spec),
            &posted(&[("item_id", "sword-7"), ("qty", "2")]),
            &grant_errors,
            None,
            false,
        ),
    ));
    let reason_errors = BTreeMap::from([("_reason".to_string(), "a reason is required".to_string())]);
    out.push(plain(
        "modal_rerender_confirm_reason_error",
        reshow(&ban, ModalSpec::Confirm(&ban_spec), &posted(&[("_reason", "")]), &reason_errors, None, false),
    ));
    let result = ActionResultView { modal: None, toast: Some("Queue paused".to_string()), tone: "info" };
    out.push(plain("modal_result_toast", render::fragment(env, "modal.html", "result", &result)));
    let flash = FlashView {
        message: "API key created".to_string(),
        reveal: Some(RevealView {
            label: "Key gb_live_…".to_string(),
            value: "gb_live_0123456789abcdef".to_string(),
            hint: None,
            expires_at: Some("2026-12-01".to_string()),
        }),
        tone: "ok",
    };
    out.push(plain("reveal_secret", render::fragment(env, "modal.html", "reveal", &flash)));

    // ---- row menu and search palette ----
    let row_entity = entity("player", "p-1001");
    let profile = decl("profile", "Open profile", "", Then::Link(Link::Entity(row_entity.clone())));
    let row_item = |module: &str, d: &ActionDecl, role: Role| {
        routes::action_item(&operator(role), CSRF, module, true, d, Some(&row_entity))
    };
    let menu_items = vec![
        row_item("players", &decl("kick", "Kick", "x", Then::Invoke), Role::Admin),
        row_item("players", &ban, Role::Admin),
        row_item("players", &profile, Role::Admin),
        row_item("economy", &grant, Role::Admin),
        row_item("economy", &ActionDecl { min_role: Role::Admin, ..grant.clone() }, Role::Viewer),
    ];
    let menu = MenuView { entity: row_entity.clone(), groups: render::menu_groups(menu_items) };
    out.push(plain("menu_row_actions", render::fragment(env, "primitives.html", "menu", &menu)));
    let hit = |title: &str, subtitle: &str, badge: Option<&str>| SearchHitView {
        hl: false,
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        badge: badge.map(str::to_string),
        href: format!("/admin/e/player/{subtitle}"),
    };
    let search = SearchView::new(
        "ann".to_string(),
        String::new(),
        vec![
            SearchGroupView {
                module: "players".to_string(),
                label: "Players".to_string(),
                icon: "user".to_string(),
                hits: vec![hit("Anna Kowalska", "p-1001", Some("active")), hit("Annette", "p-2040", None)],
                error: None,
            },
            SearchGroupView {
                module: "billing".to_string(),
                label: "Invoices".to_string(),
                icon: "coins".to_string(),
                hits: Vec::new(),
                error: Some("deadline of 800ms elapsed".to_string()),
            },
        ],
    );
    out.push(plain("search_results_and_error", render::fragment(env, "primitives.html", "search", &search)));
    let nothing = SearchView::new(
        "zzz".to_string(),
        String::new(),
        vec![SearchGroupView {
            module: "players".to_string(),
            label: "Players".to_string(),
            icon: "user".to_string(),
            hits: Vec::new(),
            error: None,
        }],
    );
    out.push(plain("search_no_matches", render::fragment(env, "primitives.html", "search", &nothing)));

    // ---- each Point's slot through routes::slot, each Layout's page, the dashboard's ranges ----
    let tab = || Point::EntityTab {
        kind: "player".to_string(),
        tab: "items".to_string(),
        tab_label: "Items".to_string(),
        columns: 1,
    };
    let points = [
        Point::Page("overview".to_string()),
        Point::DashboardTile,
        Point::DashboardAttention,
        Point::DashboardPanel,
        Point::DashboardWide,
        Point::EntityHeader("player".to_string()),
        tab(),
        Point::EntityTabCount { kind: "player".to_string(), tab: "items".to_string() },
        Point::EntitySummary("player".to_string()),
        Point::EntityListColumn { kind: "player".to_string(), label: "Guild".to_string(), align: Align::Left },
        Point::NavCounter,
    ];
    let roles = [Role::Viewer, Role::Operator, Role::Admin];
    let every_point: Result<Vec<String>, minijinja::Error> = points
        .into_iter()
        .zip(roles.into_iter().cycle())
        .enumerate()
        .map(|(i, (point, role))| {
            let s = point_slot(&format!("c{i}"), point, role, false);
            render::fragment(env, "primitives.html", "slot", &s)
        })
        .collect();
    out.push(plain("slot_every_point", every_point.map(|slots| slots.join("\n"))));
    let dashboard = DashboardView {
        shell: shell_view("Dashboard"),
        subtitle: "Golden deployment".to_string(),
        tiles: vec![point_slot("tile", Point::DashboardTile, Role::Viewer, false)],
        attention: vec![point_slot("attention", Point::DashboardAttention, Role::Viewer, false)],
        panels: vec![point_slot("panel", Point::DashboardPanel, Role::Operator, false)],
        wide: vec![point_slot("wide", Point::DashboardWide, Role::Admin, false)],
        ranges: render::ranges(TimeRange::H24),
    };
    out.push(plain("screen_dashboard_ranges", screen(env, "dashboard", &dashboard)));
    let page_point = || Point::Page("overview".to_string());
    for (name, layout) in [
        ("screen_page_stack", Layout::Stack(strings(&["a", "b"]))),
        ("screen_page_grid21", Layout::Grid21(strings(&["a"]), strings(&["b"]))),
        ("screen_page_grid2", Layout::Grid2(strings(&["a"]), strings(&["b"]))),
    ] {
        let ids = match &layout {
            Layout::Stack(a) => vec![a],
            Layout::Grid21(a, b) | Layout::Grid2(a, b) => vec![a, b],
        };
        let page = PageView {
            shell: shell_view("Golden page"),
            module: MODULE.to_string(),
            title: "Golden page".to_string(),
            subtitle: String::new(),
            head_actions: Vec::new(),
            tiles: vec![point_slot("tile", page_point(), Role::Viewer, true)],
            layout: render::layout_name(&layout),
            columns: ids
                .into_iter()
                .map(|column| column.iter().map(|id| point_slot(id, page_point(), Role::Viewer, false)).collect())
                .collect(),
            missing: Vec::new(),
        };
        out.push(plain(name, screen(env, "page", &page)));
    }

    out.into_iter()
        .map(|r| {
            let raw = r.result.unwrap_or_else(|e| panic!("golden case {} failed to render: {e:#}", r.name));
            Case {
                name: r.name,
                golden: golden_form(r.name, &raw),
                raw,
                primitive: r.primitive,
                state: r.state,
            }
        })
        .collect()
}

/// One rendered case with what [`golden_cases_cover_every_macro_variant_and_state`] checks.
struct Case {
    name: &'static str,
    golden: String,
    /// The render itself, which [`MACRO_CASES`] signatures are searched in.
    raw: String,
    /// [`primitive_case`] of a `render::panel` case's body.
    primitive: Option<&'static str>,
    /// [`state_case`] of a `render::panel` case drawn in panel chrome.
    state: Option<&'static str>,
}

struct Rendered {
    name: &'static str,
    result: Result<String, minijinja::Error>,
    primitive: Option<&'static str>,
    state: Option<&'static str>,
}

fn plain(name: &'static str, result: Result<String, minijinja::Error>) -> Rendered {
    Rendered { name, result, primitive: None, state: None }
}

fn goldens(cases: &[Case]) -> Vec<(String, String)> {
    cases.iter().map(|c| (c.name.to_string(), c.golden.clone())).collect()
}

// ---- drift guards ----------------------------------------------------------------

/// The case that renders each `Primitive` variant as a panel body. No wildcard arm: a new
/// variant does not compile until it names its case.
fn primitive_case(p: &Primitive) -> &'static str {
    match p {
        Primitive::DataTable(_) => "table_highlight_row_menu",
        Primitive::PropertyList(_) => "state_ok",
        Primitive::KpiTile(_) => "tile_delta_ok_up",
        Primitive::Timeline(_) => "timeline_two_days_older",
        Primitive::Form(_) => "panel_form",
        Primitive::WorkQueue(_) => "queue_items_actions",
        Primitive::ServiceStatus(_) => "services_one_column",
        Primitive::TimeSeriesChart(_) => "chart_two_axes",
        Primitive::AttentionItems(_) => "attention_ok",
        Primitive::ColumnCells(_) => "column_cells",
        Primitive::Cell(_) => "fact_cell",
        Primitive::EntityHeader(_) => "header_ok",
        Primitive::Empty { .. } => "state_empty_payload",
    }
}

/// The case that draws each `PanelState` in panel chrome. No wildcard arm.
fn state_case(state: PanelState) -> &'static str {
    match state {
        PanelState::Ok => "state_ok",
        PanelState::Empty => "state_empty",
        PanelState::Stale => "state_stale_hides_module_banner",
        PanelState::Error => "state_error",
        PanelState::Readonly => "state_readonly",
        PanelState::Denied => "state_denied",
    }
}

/// The sources the variant counts come from: the enums themselves, so a variant added there
/// raises the count the rendered cases must reach.
const PRIMITIVE_SOURCE: &str = include_str!("../../controlplane/src/lib.rs");
const PANEL_STATE_SOURCE: &str = include_str!("engine/panels.rs");

/// The variant names of `enum <name>` in `source`, in order: the identifiers that open each
/// top-level item of its body, past comments and attributes.
fn enum_variants(source: &str, name: &str) -> Result<Vec<String>, String> {
    let open = format!("enum {name} {{");
    let at = source.find(&open).ok_or_else(|| format!("no `{open}` in the source"))?;
    let chars: Vec<char> = source[at + open.len()..].chars().collect();
    let (mut i, mut depth, mut expect) = (0, 0usize, true);
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '#' && depth == 0 {
            // An attribute: skip to its closing bracket.
            let mut brackets = 0usize;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '[' => brackets += 1,
                    ']' => {
                        brackets -= 1;
                        if brackets == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            i += 1;
        } else if depth == 0 && expect && (c.is_alphabetic() || c == '_') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            expect = false;
        } else {
            match c {
                '(' | '{' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                '}' if depth == 0 => return Ok(out),
                '}' => depth -= 1,
                ',' if depth == 0 => expect = true,
                _ => {}
            }
            i += 1;
        }
    }
    Err(format!("`{open}` is never closed"))
}

/// Fails unless `rendered`, the distinct cases the variant-to-case match names for the
/// variants that render, holds one case per variant of `enum <name>` in `source`. The match
/// has no wildcard, so a new variant names either a new case, which must render, or an
/// existing one, which leaves the count short.
fn check_variant_coverage(source: &str, name: &str, rendered: &BTreeSet<&str>) -> Result<(), String> {
    let variants = enum_variants(source, name)?;
    if variants.len() != rendered.len() {
        return Err(format!(
            "{name} has {} variant(s) {variants:?} but {} distinct case(s) render them: {rendered:?}",
            variants.len(),
            rendered.len()
        ));
    }
    Ok(())
}

/// A template macro, a signature only it holds, and the cases whose render must hold it.
struct MacroCase {
    template: &'static str,
    name: &'static str,
    /// A class, attribute or text the macro writes: a literal of the macro's own source and of
    /// no other macro's, nor of any template's text outside its macros, but [`MacroCase::also`].
    signature: &'static str,
    /// Other macros whose source holds the signature too, and `<stem>.html` for a template
    /// whose text outside its macros does. A macro whose own source does not hold it (one that
    /// only dispatches) must call each of these.
    also: &'static [&'static str],
    /// Cases whose render holds the signature and runs the macro.
    cases: &'static [&'static str],
}

const fn mc(
    template: &'static str,
    name: &'static str,
    signature: &'static str,
    also: &'static [&'static str],
    cases: &'static [&'static str],
) -> MacroCase {
    MacroCase { template, name, signature, also, cases }
}

/// Every macro of every `templates/*.tmpl`, with its signature and the cases that reach it.
const MACRO_CASES: &[MacroCase] = &[
    mc("primitives", "icon", "<svg class=\"i ", &[], &["state_ok", "modal_form"]),
    mc("primitives", "cell_one", "user-cell", &[], &["cell_kinds", "table_list_columns"]),
    mc("primitives", "cell", "user-cell", &["cell_one"], &["cell_kinds", "modal_confirm_danger_impact_reason"]),
    mc("primitives", "hx_action", "hx-disabled-elt=\"this\"", &[], &["tools_emphasis", "menu_row_actions"]),
    mc("primitives", "action_content", "<svg class=\"i ", &["icon"], &["tools_emphasis", "table_row_actions"]),
    mc(
        "primitives",
        "action_button",
        "danger-ghost",
        &["queue_action", "row_button"],
        &["tools_emphasis", "props_code_footer_tools"],
    ),
    mc(
        "primitives",
        "row_button",
        "style=\"text-decoration:none\">",
        &["action_button", "menu_item"],
        &["table_row_actions"],
    ),
    mc("primitives", "menu_item", "menu-item", &[], &["menu_row_actions"]),
    mc("primitives", "menu", "menu-group", &[], &["menu_row_actions"]),
    mc("primitives", "toast", "data-toast", &[], &["modal_result_toast"]),
    mc("primitives", "state_block", "state-icon", &[], &["state_empty", "state_error", "state_denied"]),
    mc("primitives", "loading_block", "class=\"sk grow\"", &[], &["slot_panel_loading"]),
    mc("primitives", "denied_detail", "Requires role <span class=\"tag\">", &[], &["state_denied", "slot_panel_denied"]),
    mc(
        "primitives",
        "fetch_attrs",
        "hx-trigger=\"load\"",
        &["shell.html"],
        &["slot_panel_loading", "slot_tile_loading"],
    ),
    mc(
        "primitives",
        "slot",
        "<div class=\"panel-tools\"></div>",
        &["shell.html"],
        &["slot_panel_loading", "slot_panel_denied"],
    ),
    mc(
        "primitives",
        "table_toolbar",
        "tbl-toolbar",
        &[],
        &["table_highlight_row_menu", "table_empty_search", "queue_search_empty", "queue_filter_empty"],
    ),
    mc("primitives", "table_foot", "class=\"pager\"", &[], &["table_highlight_row_menu"]),
    mc("primitives", "table", "aria-label=\"Row actions\"", &[], &["table_highlight_row_menu"]),
    mc(
        "primitives",
        "props",
        "<dl class=\"props\"",
        &[],
        &["state_ok", "props_code_footer_tools", "props_two_columns"],
    ),
    mc("primitives", "kpi", "<div class=\"kpi-value\">", &[], &["tile_delta_ok_up", "tile_stale"]),
    mc("primitives", "timeline", "<ul class=\"tl\">", &[], &["timeline_two_days_older"]),
    mc("primitives", "field", "<label class=\"fl\">", &[], &["panel_form", "panel_form_rerender_422_chrome", "modal_form"]),
    mc(
        "primitives",
        "form_fields",
        "<div class=\"form\">",
        &["panel_form"],
        &["modal_form", "modal_rerender_form_errors_locked"],
    ),
    mc(
        "primitives",
        "panel_form",
        "form-actions",
        &[],
        &["panel_form", "panel_form_readonly", "panel_form_rerender_409_locked"],
    ),
    mc("primitives", "queue_counts", "q-counts", &[], &["queue_items_actions", "queue_empty", "queue_search_empty"]),
    mc("primitives", "queue_action", " danger-ghost\"", &["row_button"], &["queue_items_actions"]),
    mc("primitives", "queue", "class=\"attempts\"", &[], &["queue_items_actions"]),
    mc("primitives", "services", "class=\"svc\"", &[], &["services_one_column", "services_two_columns"]),
    mc("primitives", "services_body", "class=\"grid g-2\"", &[], &["services_two_columns"]),
    mc("primitives", "chart", "<svg class=\"chart\"", &[], &["chart_two_axes"]),
    mc("primitives", "chart_legend", "<div class=\"legend\">", &[], &["chart_two_axes"]),
    mc("primitives", "attention_rows", "<a class=\"btn sm\" href=\"", &[], &["attention_ok"]),
    mc("primitives", "body", "<div class=\"row\"><h1>", &["panel_fragment", "shell.html"], &["header_ok"]),
    mc("primitives", "empty_body", "No results", &[], &["table_empty_search"]),
    mc("primitives", "panel_fragment", "data-panel-url", &[], &["state_ok", "tile_delta_ok_up", "nav_counter_err"]),
    mc("primitives", "search", "palette-group", &[], &["search_results_and_error"]),
    mc("modal", "modal", "data-modal-close>Cancel", &[], &["modal_confirm_danger_impact_reason", "modal_form"]),
    mc("modal", "result", "data-toast", &["toast"], &["modal_result_toast"]),
    mc("modal", "reveal", "data-reveal", &[], &["reveal_secret"]),
];

/// Every `{%- macro` / `{% macro` of `source` with its body, up to its `endmacro`.
fn template_macros(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for open in ["{%- macro ", "{% macro "] {
        for (at, _) in source.match_indices(open) {
            let rest = &source[at + open.len()..];
            let Some((name, _)) = rest.split_once('(') else { continue };
            let body = rest.split_once("endmacro").map_or(rest, |(body, _)| body);
            out.push((name.trim().to_string(), body.to_string()));
        }
    }
    out
}

/// The text of `source` outside its macros and its `{# #}` comments, which render nothing.
fn outside_macros(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(at) = ["{%- macro ", "{% macro "].iter().filter_map(|open| rest.find(open)).min() {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let end = after.find("endmacro").and_then(|e| after[e..].find("%}").map(|c| e + c + 2)).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    let mut text = String::new();
    let mut rest = out.as_str();
    while let Some(at) = rest.find("{#") {
        text.push_str(&rest[..at]);
        rest = rest[at..].find("#}").map_or("", |e| &rest[at + e + 2..]);
    }
    text.push_str(rest);
    text
}

/// The marker [`marked_templates`] opens `template`'s `name` macro with.
fn marker(template: &str, name: &str) -> String {
    format!("<!--mc:{template}/{name}-->")
}

/// `templates` with every macro's body opening with its [`marker`], so a render holds the
/// marker of every macro that ran.
fn marked_templates(templates: &[(String, String)]) -> Vec<(String, String)> {
    templates
        .iter()
        .map(|(t, src)| {
            let mut at_tag_end: Vec<(usize, String)> = Vec::new();
            for open in ["{%- macro ", "{% macro "] {
                for (at, _) in src.match_indices(open) {
                    let rest = &src[at + open.len()..];
                    let Some((name, _)) = rest.split_once('(') else { continue };
                    let Some(close) = src[at..].find("%}") else { continue };
                    // Past a `-%}`'s trimmed whitespace, which a marker before it would keep.
                    let mut end = at + close + 2;
                    if src[..at + close].ends_with('-') {
                        end += src[end..].len() - src[end..].trim_start().len();
                    }
                    at_tag_end.push((end, marker(t, name.trim())));
                }
            }
            at_tag_end.sort();
            let mut marked = src.clone();
            for (at, m) in at_tag_end.into_iter().rev() {
                marked.insert_str(at, &m);
            }
            (t.clone(), marked)
        })
        .collect()
}

/// `raw` without the markers of [`marked_templates`].
fn unmark(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("<!--mc:") {
        out.push_str(&rest[..at]);
        rest = rest[at..].find("-->").map_or("", |e| &rest[at + e + 3..]);
    }
    out.push_str(rest);
    out
}

/// An environment of `templates` under the names `render::env` gives them. The sources are
/// leaked: a test builds a handful.
fn env_of(templates: &[(String, String)]) -> Environment<'static> {
    let mut env = Environment::new();
    for (t, src) in templates {
        let name: &'static str = Box::leak(format!("{t}.html").into_boxed_str());
        let src: &'static str = Box::leak(src.clone().into_boxed_str());
        env.add_template(name, src).unwrap_or_else(|e| panic!("template {name} compiles: {e}"));
    }
    env
}

/// Every template under `src/templates` as (file stem, source).
fn templates() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("templates");
    let mut out: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("templates entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "tmpl"))
        .map(|p| {
            let stem = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.split('.').next()).unwrap_or_default();
            (stem.to_string(), fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())))
        })
        .collect();
    out.sort();
    out
}

/// Checks `entries` against the macros of `templates` both ways and against `renders` and
/// `marked` (case name → render through `templates` and through [`marked_templates`] of them):
/// each signature is held by exactly the sources its entry names, and every listed case's
/// render holds it and its marked render the macro's [`marker`]. Returns every problem.
fn check_macro_cases(
    entries: &[MacroCase],
    templates: &[(String, String)],
    renders: &BTreeMap<&str, &str>,
    marked: &BTreeMap<&str, &str>,
) -> Result<(), Vec<String>> {
    let macros: Vec<(&str, String, String)> = templates
        .iter()
        .flat_map(|(t, src)| template_macros(src).into_iter().map(move |(m, body)| (t.as_str(), m, body)))
        .collect();
    let outside: Vec<(String, String)> =
        templates.iter().map(|(t, src)| (format!("{t}.html"), outside_macros(src))).collect();
    let declared: BTreeSet<(&str, &str)> = macros.iter().map(|(t, m, _)| (*t, m.as_str())).collect();
    let listed: BTreeSet<(&str, &str)> = entries.iter().map(|e| (e.template, e.name)).collect();
    let mut problems = Vec::new();
    for (t, m) in declared.difference(&listed) {
        problems.push(format!("template macro {t}/{m} has no MACRO_CASES entry"));
    }
    for (t, m) in listed.difference(&declared) {
        problems.push(format!("MACRO_CASES entry {t}/{m} names no template macro"));
    }
    if listed.len() != entries.len() {
        problems.push("MACRO_CASES lists a macro twice".to_string());
    }
    for e in entries {
        let holders: BTreeSet<&str> = macros
            .iter()
            .filter(|(_, _, body)| body.contains(e.signature))
            .map(|(_, m, _)| m.as_str())
            .chain(outside.iter().filter(|(_, text)| text.contains(e.signature)).map(|(t, _)| t.as_str()))
            .collect();
        let others: BTreeSet<&str> = holders.iter().copied().filter(|m| *m != e.name).collect();
        let also: BTreeSet<&str> = e.also.iter().copied().collect();
        if others != also {
            problems.push(format!(
                "{}/{}: signature {:?} is held by {holders:?}; the entry names itself and {also:?}",
                e.template, e.name, e.signature
            ));
        }
        if !holders.contains(e.name) {
            let own = macros.iter().find(|(t, m, _)| *t == e.template && m == e.name).map(|(_, _, b)| b.as_str());
            let calls_all = !e.also.is_empty() && e.also.iter().all(|a| own.is_some_and(|b| b.contains(&format!("{a}("))));
            if !calls_all {
                problems.push(format!(
                    "{}/{}: its source does not hold signature {:?} and it does not call every macro that does",
                    e.template, e.name, e.signature
                ));
            }
        }
        if e.cases.is_empty() {
            problems.push(format!("{}/{} lists no case", e.template, e.name));
        }
        for case in e.cases {
            match renders.get(case) {
                None => problems.push(format!("{}/{} names case {case}, which does not exist", e.template, e.name)),
                Some(render) if !render.contains(e.signature) => problems.push(format!(
                    "{}/{}: case {case} does not render signature {:?}",
                    e.template, e.name, e.signature
                )),
                Some(_) => {}
            }
            if !marked.get(case).is_some_and(|m| m.contains(&marker(e.template, e.name))) {
                problems.push(format!("{}/{}: case {case} does not run the macro", e.template, e.name));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[test]
fn golden_cases_cover_every_macro_variant_and_state() {
    let env = render::env().expect("templates compile");
    let cases = cases(&env);
    let by_name: BTreeMap<&str, &Case> = cases.iter().map(|c| (c.name, c)).collect();
    let renders: BTreeMap<&str, &str> = cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let marked_cases = self::cases(&env_of(&marked_templates(&templates())));
    let marked: BTreeMap<&str, &str> = marked_cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    assert_eq!(marked.len(), renders.len(), "the marked templates render every case");
    for (name, raw) in &marked {
        assert_eq!(&unmark(raw), renders[name], "case {name}: the marked render, unmarked, is the production render");
    }

    if let Err(problems) = check_macro_cases(MACRO_CASES, &templates(), &renders, &marked) {
        panic!("MACRO_CASES drift:\n  {}", problems.join("\n  "));
    }

    let primitives: BTreeSet<&str> = cases.iter().filter_map(|c| c.primitive).collect();
    check_variant_coverage(PRIMITIVE_SOURCE, "Primitive", &primitives).unwrap_or_else(|e| panic!("{e}"));
    let states: BTreeSet<&str> = cases.iter().filter_map(|c| c.state).collect();
    check_variant_coverage(PANEL_STATE_SOURCE, "PanelState", &states).unwrap_or_else(|e| panic!("{e}"));
    for (named, own) in
        primitives.iter().map(|n| (*n, by_name.get(n).and_then(|c| c.primitive))).chain(
            states.iter().map(|n| (*n, by_name.get(n).and_then(|c| c.state))),
        )
    {
        assert_eq!(own, Some(named), "case {named} is named for a variant it does not render");
    }
}

/// Every variant of every enum `controlplane::drawn_enums` registers is converted into a view
/// model while the golden cases render.
#[test]
fn golden_renders_convert_every_drawn_enum_variant() {
    let env = render::env().expect("templates compile");
    // Collected over the golden run alone: a variant converted elsewhere (a route test, another case builder) is no golden that draws it.
    let ((), record) = render::seen::collect(|| drop(cases(&env)));
    if let Err(problems) = check_drawn(controlplane::drawn_enums(), &record) {
        panic!("drawn enum variants no golden case converts:\n  {}", problems.join("\n  "));
    }
}

/// Every problem of `record` against `drawn`: an empty registry, each declared variant no case
/// converted, each enum no case converted at all, and each converted enum not registered.
fn check_drawn(drawn: &[DrawnEnum], record: &render::seen::Record) -> Result<(), Vec<String>> {
    if drawn.is_empty() {
        return Err(vec!["controlplane::drawn_enums() is empty: no variants! entry is linked".to_string()]);
    }
    let mut drawn: Vec<&DrawnEnum> = drawn.iter().collect();
    drawn.sort_by_key(|e| e.name);
    let mut problems = Vec::new();
    for e in &drawn {
        let Some(seen) = record.get(e.name) else {
            let all: Vec<String> = e.variants.iter().map(|v| format!("{}::{v}", e.name)).collect();
            problems.push(format!("{} is converted by no golden case: {}", e.name, all.join(", ")));
            continue;
        };
        for v in e.variants.iter().filter(|v| !seen.converted.contains(*v)) {
            problems.push(format!("{}::{v} is converted by no golden case", e.name));
        }
    }
    for name in record.keys().filter(|n| !drawn.iter().any(|e| e.name == **n)) {
        problems.push(format!("{name} is converted but not registered through variants!"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A record short one variant, one missing an enum, one holding an unregistered enum and an
/// empty registry each fail, naming what is missing.
#[test]
fn check_drawn_names_each_unconverted_variant_and_enum() {
    let drawn = [
        DrawnEnum { name: "Tone", variants: &["Ok", "Warn"] },
        DrawnEnum { name: "Axis", variants: &["Left", "Right"] },
    ];
    let entry = |e: &DrawnEnum, converted: &[&'static str]| {
        (e.name, render::seen::Converted { declared: e.variants, converted: converted.iter().copied().collect() })
    };
    let full = render::seen::Record::from([entry(&drawn[0], &["Ok", "Warn"]), entry(&drawn[1], &["Left", "Right"])]);
    assert_eq!(check_drawn(&drawn, &full), Ok(()));

    let short = render::seen::Record::from([entry(&drawn[0], &["Ok"]), entry(&drawn[1], &["Left", "Right"])]);
    assert_eq!(check_drawn(&drawn, &short), Err(vec!["Tone::Warn is converted by no golden case".to_string()]));

    let no_axis = render::seen::Record::from([entry(&drawn[0], &["Ok", "Warn"])]);
    assert_eq!(
        check_drawn(&drawn, &no_axis),
        Err(vec!["Axis is converted by no golden case: Axis::Left, Axis::Right".to_string()])
    );

    let mut extra = full.clone();
    extra.extend([entry(&DrawnEnum { name: "Fresh", variants: &["A"] }, &["A"])]);
    assert_eq!(
        check_drawn(&drawn, &extra),
        Err(vec!["Fresh is converted but not registered through variants!".to_string()])
    );

    assert_eq!(
        check_drawn(&[], &full),
        Err(vec!["controlplane::drawn_enums() is empty: no variants! entry is linked".to_string()])
    );
}

#[test]
fn enum_variants_reads_the_real_enums() {
    let primitive = enum_variants(PRIMITIVE_SOURCE, "Primitive").expect("Primitive parses");
    assert_eq!(primitive.first().map(String::as_str), Some("DataTable"));
    assert_eq!(primitive.last().map(String::as_str), Some("Empty"));
    assert!(primitive.iter().any(|v| v == "AttentionItems"), "a tuple variant over a generic: {primitive:?}");
    let states = enum_variants(PANEL_STATE_SOURCE, "PanelState").expect("PanelState parses");
    assert_eq!(states, ["Ok", "Empty", "Stale", "Error", "Readonly", "Denied"]);
    let tricky = "enum T {\n    /// A, B\n    #[serde(rename = \"x,y\")]\n    A { x: u8, y: (u8, u8) },\n    /* C, */ B(Vec<u8>),\n    D = 3,\n}";
    assert_eq!(enum_variants(tricky, "T").expect("T parses"), ["A", "B", "D"]);
}

/// A scratch copy of each enum source with one variant more fails the coverage check against
/// the cases rendered today.
#[test]
fn variant_coverage_fails_on_a_new_enum_variant() {
    let env = render::env().expect("templates compile");
    let cases = cases(&env);
    let primitives: BTreeSet<&str> = cases.iter().filter_map(|c| c.primitive).collect();
    let states: BTreeSet<&str> = cases.iter().filter_map(|c| c.state).collect();
    for (source, name, rendered) in
        [(PRIMITIVE_SOURCE, "Primitive", &primitives), (PANEL_STATE_SOURCE, "PanelState", &states)]
    {
        check_variant_coverage(source, name, rendered).unwrap_or_else(|e| panic!("today's {name}: {e}"));
        let open = format!("enum {name} {{");
        let scratch = source.replacen(&open, &format!("{open}\n    Fake(Fake),"), 1);
        assert_ne!(scratch, source, "the scratch copy of {name} gained a variant");
        let err = check_variant_coverage(&scratch, name, rendered).expect_err("one variant more must fail");
        assert!(err.contains("Fake"), "{err}");
    }
}

/// A signature the listed case does not render, and a signature another macro holds, each fail.
#[test]
fn macro_cases_fail_on_a_wrong_signature() {
    let env = render::env().expect("templates compile");
    let cases = cases(&env);
    let renders: BTreeMap<&str, &str> = cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let templates = templates();
    let marked_cases = self::cases(&env_of(&marked_templates(&templates)));
    let marked: BTreeMap<&str, &str> = marked_cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let with = |name: &str, signature: &'static str, also: &'static [&'static str]| -> Vec<MacroCase> {
        MACRO_CASES
            .iter()
            .map(|e| {
                let (signature, also) = if e.name == name { (signature, also) } else { (e.signature, e.also) };
                mc(e.template, e.name, signature, also, e.cases)
            })
            .collect()
    };
    let problems =
        |entries: &[MacroCase]| check_macro_cases(entries, &templates, &renders, &marked).err().unwrap_or_default();
    assert!(problems(&with("menu", "menu-group", &[])).is_empty(), "the unchanged entry passes");

    // `state-icon` is state_block's literal and menu calls no macro that holds it.
    let not_rendered = problems(&with("menu", "state-icon", &["state_block"]));
    assert!(
        not_rendered.iter().any(|p| p == "primitives/menu: case menu_row_actions does not render signature \"state-icon\""),
        "{not_rendered:?}"
    );
    assert!(
        not_rendered.iter().any(|p| p.starts_with("primitives/menu: its source does not hold signature")),
        "{not_rendered:?}"
    );
    // `palette-group` is search's literal, not menu's, and the entry does not say so.
    let foreign = problems(&with("menu", "palette-group", &[]));
    assert!(foreign.iter().any(|p| p.starts_with("primitives/menu: signature \"palette-group\" is held by")), "{foreign:?}");
    // A signature shared with a macro the entry does not name.
    let shared = problems(&with("action_button", "danger-ghost", &[]));
    assert!(shared.iter().any(|p| p.starts_with("primitives/action_button: signature")), "{shared:?}");
}

/// A macro added to any template, a dead entry and a listed case that does not exist each fail.
#[test]
fn macro_cases_fail_on_template_drift() {
    let renders: BTreeMap<&str, &str> = BTreeMap::new();
    let mut templates = templates();
    assert!(templates.iter().any(|(t, _)| t == "shell"), "every template is read, not only the macro ones");
    templates.push(("shell2".to_string(), "{%- macro extra(x) -%}<i class=\"extra\"></i>{%- endmacro %}".to_string()));
    let problems = check_macro_cases(MACRO_CASES, &templates, &renders, &renders).expect_err("drift fails");
    assert!(problems.iter().any(|p| p == "template macro shell2/extra has no MACRO_CASES entry"), "{problems:?}");
    assert!(problems.iter().any(|p| p == "primitives/icon names case state_ok, which does not exist"), "{problems:?}");
    let dead = [mc("modal", "gone", "x", &[], &["state_ok"])];
    let problems = check_macro_cases(&dead, &templates, &renders, &renders).expect_err("a dead entry fails");
    assert!(problems.iter().any(|p| p == "MACRO_CASES entry modal/gone names no template macro"), "{problems:?}");
}

/// The cases rendered through `templates` (a scratch copy of the real ones) as (name, render)
/// pairs, plainly and through [`marked_templates`] of them.
fn scratch_renders(templates: &[(String, String)]) -> (Vec<Case>, Vec<Case>) {
    (cases(&env_of(templates)), cases(&env_of(&marked_templates(templates))))
}

/// `templates` with `from` replaced by `to` once in `template`'s source.
fn edited(templates: &[(String, String)], template: &str, from: &str, to: &str) -> Vec<(String, String)> {
    templates
        .iter()
        .map(|(t, src)| {
            if t != template {
                return (t.clone(), src.clone());
            }
            assert_eq!(src.matches(from).count(), 1, "{template} holds {from:?} once");
            (t.clone(), src.replacen(from, to, 1))
        })
        .collect()
}

/// The problems `check_macro_cases` reports for [`MACRO_CASES`] over scratch `templates`.
fn scratch_problems(templates: &[(String, String)]) -> Vec<String> {
    let (plain, marked) = scratch_renders(templates);
    let renders: BTreeMap<&str, &str> = plain.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let marked: BTreeMap<&str, &str> = marked.iter().map(|c| (c.name, c.raw.as_str())).collect();
    check_macro_cases(MACRO_CASES, templates, &renders, &marked).err().unwrap_or_default()
}

/// A listed case that still renders the signature through another macro, but no longer runs
/// the entry's own, fails; the two wave-throughs the signature rule alone let pass.
#[test]
fn macro_cases_fail_on_a_case_that_does_not_run_the_macro() {
    let real = templates();
    assert_eq!(scratch_problems(&real), Vec::<String>::new(), "the real templates pass");

    // The modal draws its fields with panel_form, which writes `<div class="form">` too.
    let modal = edited(&real, "modal", "{{ ui.form_fields(m.form) }}", "{{ ui.panel_form(m.form) }}");
    let problems = scratch_problems(&modal);
    assert_eq!(
        problems,
        [
            "primitives/form_fields: case modal_form does not run the macro",
            "primitives/form_fields: case modal_rerender_form_errors_locked does not run the macro",
        ],
        "only the marker rule catches it: the signature is still rendered"
    );

    // The header fragment writes the row itself, as panel_fragment's fallback does.
    let header = edited(
        &real,
        "primitives",
        "{%- if live and p.body and p.body.type == \"Header\" %}{{ body(p.body, p) }}</div>",
        "{%- if live and p.body and p.body.type == \"Header\" %}<div class=\"row\"><h1>{{ p.body.title }}</h1></div></div>",
    );
    assert_eq!(scratch_problems(&header), ["primitives/body: case header_ok does not run the macro"]);
}

/// Template text outside any macro holds a signature like a macro does; a `{# #}` comment
/// renders nothing and does not.
#[test]
fn macro_cases_fail_on_a_signature_written_outside_any_macro() {
    let env = render::env().expect("templates compile");
    let cases = cases(&env);
    let renders: BTreeMap<&str, &str> = cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let marked_cases = self::cases(&env_of(&marked_templates(&templates())));
    let marked: BTreeMap<&str, &str> = marked_cases.iter().map(|c| (c.name, c.raw.as_str())).collect();
    let with_extra = |extra: &str| {
        let mut t = templates();
        t.push(("extra".to_string(), extra.to_string()));
        check_macro_cases(MACRO_CASES, &t, &renders, &marked).err().unwrap_or_default()
    };
    assert_eq!(with_extra("{# <div class=\"menu-group\"> #}<p></p>"), Vec::<String>::new(), "a comment holds nothing");
    let problems = with_extra("<div class=\"menu-group\"></div>");
    assert_eq!(
        problems,
        ["primitives/menu: signature \"menu-group\" is held by {\"extra.html\", \"menu\"}; the entry names itself and {}"]
    );

    // `body`'s row is written by shell.html outside any macro, which the entry must name.
    let unnamed: Vec<MacroCase> = MACRO_CASES
        .iter()
        .map(|e| {
            let also: &'static [&'static str] = if e.name == "body" { &["panel_fragment"] } else { e.also };
            mc(e.template, e.name, e.signature, also, e.cases)
        })
        .collect();
    let problems = check_macro_cases(&unnamed, &templates(), &renders, &marked).err().unwrap_or_default();
    assert_eq!(
        problems,
        [concat!(
            "primitives/body: signature \"<div class=\\\"row\\\"><h1>\" is held by ",
            "{\"body\", \"panel_fragment\", \"shell.html\"}; the entry names itself and {\"panel_fragment\"}"
        )]
    );
}

/// An untitled panel and an untitled slot draw no head.
#[test]
fn an_untitled_panel_or_slot_draws_no_head() {
    let env = render::env().expect("templates compile");
    let panel = PanelCase { title: "", ..PanelCase::new(PanelState::Ok, Some(body(summary()))) };
    let raw = panel.render(&env).expect("renders");
    assert!(raw.contains(r#"<div class="panel-body"#) && !raw.contains("panel-head"), "{raw}");
    let untitled = SlotView { title: String::new(), ..slot("panel", false) };
    let raw = render::fragment(&env, "primitives.html", "slot", &untitled).expect("renders");
    assert!(raw.contains(r#"<div class="panel-body"#) && !raw.contains("panel-head"), "{raw}");
}

/// A table counted in an item noun shows the total alone past the first page, and the rows
/// shown without a total.
#[test]
fn a_noun_table_foot_counts_past_the_first_page_and_without_a_total() {
    let env = render::env().expect("templates compile");
    let later = PanelCase {
        query: posted(&[("cursor", "c2")]),
        ..PanelCase::new(PanelState::Ok, Some(body(noun_table(Some(1234)))))
    };
    let raw = later.render(&env).expect("renders");
    assert!(raw.contains("<span>1,234 players</span>"), "{raw}");
    let raw = PanelCase::new(PanelState::Ok, Some(body(noun_table(None)))).render(&env).expect("renders");
    assert!(raw.contains("<span>2 players</span>"), "{raw}");
}

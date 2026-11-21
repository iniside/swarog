use super::*;

/// A template macro, a signature only it holds, and the cases whose render must hold it.
pub(super) struct MacroCase {
    pub(super) template: &'static str,
    pub(super) name: &'static str,
    /// A class, attribute or text the macro writes: a literal of the macro's own source and of
    /// no other macro's, nor of any template's text outside its macros, but [`MacroCase::also`].
    pub(super) signature: &'static str,
    /// Other macros whose source holds the signature too, and `<stem>.html` for a template
    /// whose text outside its macros does. A macro whose own source does not hold it (one that
    /// only dispatches) must call each of these.
    pub(super) also: &'static [&'static str],
    /// Cases whose render holds the signature and runs the macro.
    pub(super) cases: &'static [&'static str],
}

pub(super) const fn mc(
    template: &'static str,
    name: &'static str,
    signature: &'static str,
    also: &'static [&'static str],
    cases: &'static [&'static str],
) -> MacroCase {
    MacroCase { template, name, signature, also, cases }
}

/// Every macro of every `templates/*.tmpl`, with its signature and the cases that reach it.
pub(super) const MACRO_CASES: &[MacroCase] = &[
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
    mc("primitives", "refused", "data-refusal=\"", &[], &["tools_emphasis", "menu_row_actions_down"]),
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
        "panels",
        "<div class=\"stack\"",
        &["shell.html"],
        &["screen_page_stack", "screen_page_grid21", "screen_entity_tab_tiles"],
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
    mc(
        "primitives",
        "kpi",
        "<div class=\"kpi-value\">",
        &[],
        &["tile_delta_ok_up", "tile_stale", "kpi_tiles", "kpi_tiles_panel_point", "kpi_panel_point"],
    ),
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
        &["panel_form", "panel_form_readonly", "panel_form_rerender_409_locked", "panel_form_rerender_mode_revoke"],
    ),
    mc("primitives", "queue_counts", "q-counts", &[], &["queue_items_actions", "queue_empty", "queue_search_empty"]),
    mc("primitives", "queue_action", " danger-ghost\"", &["row_button"], &["queue_items_actions"]),
    mc("primitives", "queue", "class=\"attempts\"", &[], &["queue_items_actions"]),
    mc("primitives", "services", "class=\"svc\"", &[], &["services_one_column", "services_two_columns"]),
    mc("primitives", "services_body", "class=\"grid g-2\"", &["shell.html"], &["services_two_columns"]),
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
pub(super) fn marked_templates(templates: &[(String, String)]) -> Vec<(String, String)> {
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
pub(super) fn env_of(templates: &[(String, String)]) -> Environment<'static> {
    let mut env = Environment::new();
    for (t, src) in templates {
        let name: &'static str = Box::leak(format!("{t}.html").into_boxed_str());
        let src: &'static str = Box::leak(src.clone().into_boxed_str());
        env.add_template(name, src).unwrap_or_else(|e| panic!("template {name} compiles: {e}"));
    }
    env
}

/// Every template under `src/templates` as (file stem, source).
pub(super) fn templates() -> Vec<(String, String)> {
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
pub(super) fn check_macro_cases(
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
    check_variant_coverage("Primitive", Primitive::VARIANTS, &primitives).unwrap_or_else(|e| panic!("{e}"));
    let states: BTreeSet<&str> = cases.iter().filter_map(|c| c.state).collect();
    let state_variants = enum_variants(PANEL_STATE_SOURCE, "PanelState").expect("PanelState parses");
    let state_variants: Vec<&str> = state_variants.iter().map(String::as_str).collect();
    check_variant_coverage("PanelState", &state_variants, &states).unwrap_or_else(|e| panic!("{e}"));
    for (named, own) in
        primitives.iter().map(|n| (*n, by_name.get(n).and_then(|c| c.primitive))).chain(
            states.iter().map(|n| (*n, by_name.get(n).and_then(|c| c.state))),
        )
    {
        assert_eq!(own, Some(named), "case {named} is named for a variant it does not render");
    }
}

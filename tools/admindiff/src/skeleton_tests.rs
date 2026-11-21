use std::collections::BTreeSet;

use super::{
    diff_pages, head, page, prepare, validate_repeats, Outcome, Page, Strip, UnitKind, HINT_CHARS, MOCK_STRIP, PORTAL_STRIP,
};
use crate::html::{parse, Element};

/// The prepared `.app` of `src` with no strip list.
fn root(src: &str) -> Element {
    prepare(&parse(src).expect("fixture parses"), &[], "t").expect("fixture has .app")
}

fn page_of(body: &str) -> Page {
    page(&root(&format!(r#"<div class="app">{body}</div>"#)), false)
}

fn diff(mock: &str, portal: &str) -> BTreeSet<String> {
    diff_pages(&page_of(mock), &page_of(portal)).lines
}

fn set(lines: &[&str]) -> BTreeSet<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

/// The label of the first frame child of `body`, as a diff line names it.
fn label_of(body: &str) -> String {
    page_of(body).frame.children[0].label()
}

// ---- strip lists and the root ----------------------------------------------------

#[test]
fn a_dead_strip_entry_is_an_error_on_each_side() {
    let mock = parse(r#"<div class="app"><div class="frag"><div class="frag-label"></div></div><div class="annot-legend"></div></div>"#)
        .expect("parses");
    let e = format!("{:#}", prepare(&mock, MOCK_STRIP, "mock").expect_err("no .devbar"));
    assert_eq!(e, "mock: strip entries matching nothing on this page: .devbar");
    let portal = parse(r#"<div class="app"><section class="panel"></section></div>"#).expect("parses");
    let e = format!("{:#}", prepare(&portal, PORTAL_STRIP, "portal").expect_err("no .slot"));
    assert_eq!(e, "portal: strip entries matching nothing on this page: .slot");
}

#[test]
fn no_app_root_is_an_error() {
    let doc = parse(r#"<div class="page"><div class="slot"></div></div>"#).expect("parses");
    let e = format!("{:#}", prepare(&doc, PORTAL_STRIP, "portal").expect_err("no .app"));
    assert_eq!(e, "portal: no .app element");
}

#[test]
fn unwrap_keeps_the_children_and_remove_drops_the_subtree() {
    let doc = parse(concat!(
        r#"<body><div class="devbar outside"></div><div class="app">"#,
        r#"<div class="frag"><div class="frag-label">Panel · DataTable</div><section><p></p></section></div>"#,
        r#"<div class="devbar"><b></b></div><div class="annot-legend"><i></i></div></div></body>"#,
    ))
    .expect("parses");
    let app = prepare(&doc, MOCK_STRIP, "mock").expect("prepares");
    assert_eq!(page(&app, false).frame.canon, "div.app{section{p}}");
    let custom: &[(&str, Strip)] = &[("x", Strip::Unwrap)];
    let nested = parse(r#"<div class="app"><div class="x"><div class="x"><i></i></div></div></div>"#).expect("parses");
    assert_eq!(page(&prepare(&nested, custom, "t").expect("prepares"), false).frame.canon, "div.app{i}", "unwrap nests");
}

#[test]
fn a_head_sorts_and_dedups_its_classes() {
    let doc = parse(r#"<div class="z a m a"></div>"#).expect("parses");
    let crate::html::Node::Element(e) = &doc[0] else { unreachable!() };
    assert_eq!(head(e), "div.a.m.z");
}

// ---- validate_repeats ----------------------------------------------------------------

#[test]
fn validate_repeats_fails_on_a_dead_entry_and_passes_on_a_match_in_any_root() {
    let tables =
        root(r#"<div class="app"><table><tbody><tr></tr></tbody></table><ul><li></li></ul><select><option></option></select></div>"#);
    let rows = root(r#"<div class="app"><div class="svc"></div><div class="pop-row"></div></div>"#);
    let bars = root(r#"<div class="app"><span class="uptime"><i class="ok"></i></span></div>"#);
    validate_repeats(&[tables.clone(), rows.clone(), bars]).expect("each entry matches in some root");
    let e = format!("{:#}", validate_repeats(&[tables.clone(), rows.clone()]).expect_err("no .uptime > i"));
    assert_eq!(
        e,
        "REPEAT entries matching nothing in the mockup: .uptime > i (uptime bars: a bar's class is its sample's tone, which is data)"
    );
    let empty_uptime = root(r#"<div class="app"><span class="uptime"></span><i></i></div>"#);
    let e = format!("{:#}", validate_repeats(&[tables, rows, empty_uptime]).expect_err("an empty .uptime"));
    assert!(e.ends_with(": .uptime > i (uptime bars: a bar's class is its sample's tone, which is data)"), "{e}");
}

// ---- repeat groups -------------------------------------------------------------------

const ROW: &str = "<tr><td></td><td></td></tr>";

#[test]
fn n_rows_against_m_rows_of_one_shape_is_no_difference() {
    let mock = format!("<table><tbody>{}</tbody></table>", ROW.repeat(5));
    let portal = format!("<table><tbody>{ROW}</tbody></table>");
    assert_eq!(diff(&mock, &portal), set(&[]));
    assert_eq!(page_of(&mock).frame.canon, "div.app{table{tbody{tr*{tr{td,td}}}}}");
}

#[test]
fn n_options_against_m_options_collapse_like_rows() {
    let mock = r#"<select class="select"><option value="">all</option><option selected>a</option><option>b</option></select>"#;
    let portal = r#"<select class="select"><option>x</option></select>"#;
    assert_eq!(diff(mock, portal), set(&[]));
    assert_eq!(page_of(mock).frame.canon, "div.app{select.select{option*{option}}}");
}

#[test]
fn a_distinct_row_shape_is_reported_by_its_hash() {
    let odd = "<tr class=\"sel\"><td></td></tr>";
    let mock = format!("<table><tbody>{ROW}{odd}{ROW}</tbody></table>");
    let portal = format!("<table><tbody>{ROW}</tbody></table>");
    let odd_label = label_of(odd);
    assert!(odd_label.starts_with("tr.sel#"), "{odd_label}");
    assert_eq!(diff(&mock, &portal), set(&[&format!("frame div.app > table > tbody > tr*: mock-only {odd_label}")]));
    let leaf_row = "<tr></tr>";
    assert_eq!(
        diff(&format!("<table><tbody>{ROW}</tbody></table>"), &format!("<table><tbody>{ROW}{leaf_row}</tbody></table>")),
        set(&["frame div.app > table > tbody > tr*: portal-only tr"]),
        "a distinct shape on the portal side is portal-only"
    );
}

#[test]
fn order_inside_a_group_does_not_matter() {
    let (a, b) = ("<li class=\"a\"><b></b></li>", "<li class=\"b\"><i></i></li>");
    assert_eq!(diff(&format!("<ul>{a}{b}{a}</ul>"), &format!("<ul>{b}{a}</ul>")), set(&[]));
}

#[test]
fn uptime_bars_count_by_tag_whatever_their_class() {
    let mock = r#"<div class="uptime"><i class="ok"></i><i class="ok"></i><i class="warn"></i></div>"#;
    let portal = r#"<div class="uptime"><i class="err"></i></div>"#;
    assert_eq!(diff(mock, portal), set(&[]));
    assert_eq!(label_of(mock), "div.uptime#3f0d1e");
    // The same bars outside an `.uptime` compare by position and class.
    let lines = diff(&mock.replace("uptime", "bars"), &portal.replace("uptime", "bars"));
    assert!(!lines.is_empty(), "outside .uptime a bar's class is compared");
}

#[test]
fn duplicated_identical_siblings_outside_a_repeat_compare_by_count() {
    let three = r#"<div class="x"><span></span><span></span><span></span></div>"#;
    let lines = diff(three, r#"<div class="x"><span></span><span></span></div>"#);
    assert_eq!(lines, set(&["frame div.app > div.x: mock-only span"]));
    let more = diff(r#"<div class="x"><span></span></div>"#, r#"<div class="x"><span></span><span></span></div>"#);
    assert_eq!(more, set(&["frame div.app > div.x: portal-only span"]));
}

// ---- sequence ------------------------------------------------------------------------

#[test]
fn two_swapped_siblings_give_exactly_one_order_line() {
    let lines = diff("<header></header><nav></nav><main></main>", "<nav></nav><header></header><main></main>");
    assert_eq!(lines, set(&["frame div.app: order mock [header, nav, main] portal [nav, header, main]"]));
}

#[test]
fn a_dropped_cell_tool_or_chrome_slot_is_mock_only_and_an_extra_one_portal_only() {
    let head = r#"<table><thead><tr><th></th><th class="num"></th><th class="act"></th></tr></thead></table>"#;
    let no_num = head.replace(r#"<th class="num"></th>"#, "");
    assert_eq!(diff(head, &no_num), set(&["frame div.app > table > thead > tr: mock-only th.num"]));
    assert_eq!(diff(&no_num, head), set(&["frame div.app > table > thead > tr: portal-only th.num"]));

    let row = r#"<table><tr><td class="cb"></td><td></td><td class="act"></td></tr></table>"#;
    let no_cb = row.replace(r#"<td class="cb"></td>"#, "");
    assert_eq!(diff(row, &no_cb), set(&["frame div.app > table > tr: mock-only td.cb"]));

    let tools = r#"<div class="panel-tools"><span class="muted"></span><button class="btn sm"></button><button class="btn ghost sm"></button></div>"#;
    let one_tool = tools.replace(r#"<button class="btn ghost sm"></button>"#, "");
    assert_eq!(diff(tools, &one_tool), set(&["frame div.app > div.panel-tools: mock-only button.btn.ghost.sm"]));
    assert_eq!(diff(&one_tool, tools), set(&["frame div.app > div.panel-tools: portal-only button.btn.ghost.sm"]));

    let chrome = r#"<div class="panel-head"><span class="panel-title"></span><span class="panel-meta"></span><span class="ro-chip"></span></div>"#;
    let no_meta = chrome.replace(r#"<span class="panel-meta"></span>"#, "");
    assert_eq!(diff(chrome, &no_meta), set(&["frame div.app > div.panel-head: mock-only span.panel-meta"]));
    assert_eq!(diff(&no_meta, chrome), set(&["frame div.app > div.panel-head: portal-only span.panel-meta"]));
}

#[test]
fn a_child_that_moves_across_a_gap_is_moved() {
    let (a, b, c) = (r#"<p class="a"></p>"#, r#"<p class="b"></p>"#, r#"<p class="c"></p>"#);
    let lines = diff(&format!("<section></section>{a}{b}{c}<footer></footer>"), &format!("{a}{b}{c}<section></section>"));
    assert_eq!(lines, set(&["frame div.app: moved section", "frame div.app: mock-only footer"]));
}

#[test]
fn identical_children_with_differing_heads_give_the_head_line_only() {
    let (a, b) = (r#"<div class="card a"><span></span><b></b></div>"#, r#"<div class="card b"><span></span><b></b></div>"#);
    let lines = diff(a, b);
    assert_eq!(lines, set(&["frame div.app: mock div.a.card portal div.b.card"]));
}

#[test]
fn kin_children_pair_and_report_their_inner_difference() {
    let lines = diff(r#"<div class="card"><span></span><b></b></div>"#, r#"<div class="card"><span></span></div>"#);
    assert_eq!(lines, set(&["frame div.app > div.card: mock-only b"]));
    let (card, list) = (r#"<div class="card"><span></span></div>"#, r#"<div class="list"><span></span></div>"#);
    let mock_only = format!("frame div.app: mock-only {}", label_of(card));
    let portal_only = format!("frame div.app: portal-only {}", label_of(list));
    assert_eq!(diff(card, list), set(&[&mock_only, &portal_only]), "disjoint classes are not kin");
}

#[test]
fn kin_pairs_crossing_inside_one_gap_give_exactly_one_order_line() {
    let (p_m, q_m) = (r#"<div class="p"><span></span></div>"#, r#"<div class="q"><b></b></div>"#);
    let (p_p, q_p) = (r#"<div class="p"><em></em></div>"#, r#"<div class="q"><i></i></div>"#);
    let crossed = diff(&format!("{p_m}{q_m}"), &format!("{q_p}{p_p}"));
    let order: Vec<&String> = crossed.iter().filter(|l| l.contains(": order ")).collect();
    assert_eq!(
        order,
        [&format!(
            "frame div.app: order mock [{}, {}] portal [{}, {}]",
            label_of(p_m),
            label_of(q_m),
            label_of(q_p),
            label_of(p_p)
        )]
    );
    assert!(crossed.contains("frame div.app > div.p: mock-only span"), "{crossed:?}");
    assert!(crossed.contains("frame div.app > div.q: portal-only i"), "{crossed:?}");
    assert_eq!(crossed.len(), 5, "one order line and each pair's two inner lines: {crossed:?}");

    let straight = diff(&format!("{p_m}{q_m}"), &format!("{p_p}{q_p}"));
    assert!(straight.iter().all(|l| !l.contains(": order ")), "{straight:?}");
    assert_eq!(straight.len(), 4, "{straight:?}");
}

#[test]
fn unit_placeholders_are_kin_only_when_equal() {
    // Keys with a dot share the placeholder's would-be "tag", so without the rule they pair.
    let titled = |t: &str| format!(r#"<section class="panel"><span class="panel-title">{t}</span></section>"#);
    let lines = diff(&titled("v1.2"), &titled("v1.3"));
    assert_eq!(
        lines,
        set(&[
            r#"frame div.app: mock-only [panel "v1.2"]"#,
            r#"frame div.app: portal-only [panel "v1.3"]"#,
            r#"mock-only panel "v1.2""#,
            r#"portal-only panel "v1.3""#,
        ])
    );
    let panel = r#"<section class="panel"><span class="panel-title">A</span></section>"#;
    assert_eq!(diff(&format!("<nav></nav>{panel}"), panel), set(&["frame div.app: mock-only nav"]));
}

// ---- labels ----------------------------------------------------------------------------

#[test]
fn a_label_hashes_only_an_element_with_children() {
    assert_eq!(label_of("<span class=\"b a\"></span>"), "span.a.b");
    let (one, two) = (label_of("<div class=\"x\"><span></span></div>"), label_of("<div class=\"x\"><b></b></div>"));
    assert_eq!(one, "div.x#ef4fc2");
    assert_eq!(two, "div.x#7a5dae");
    assert_eq!(label_of(r#"<ul><li class="b"></li><li class="a"></li><li class="b"></li></ul>"#), "ul#4a51ac");
}

// ---- units -------------------------------------------------------------------------------

fn keys(body: &str) -> Vec<(UnitKind, String)> {
    page_of(body).units.into_iter().map(|u| (u.kind, u.key)).collect()
}

#[test]
fn a_unit_key_is_its_title_own_text_without_a_trailing_colon() {
    let units = keys(concat!(
        r#"<section class="panel"><span class="panel-title">Players <span class="badge">3</span></span></section>"#,
        r#"<section class="panel"><span class="panel-title">Paused: <span class="mono">sub-1</span></span></section>"#,
        r#"<a class="panel"><div class="kpi"><div class="kpi-label"><svg></svg>Max lag <span class="badge">stale</span></div></div></a>"#,
    ));
    assert_eq!(
        units,
        [
            (UnitKind::Panel, "Players".to_string()),
            (UnitKind::Panel, "Paused".to_string()),
            (UnitKind::Tile, "Max lag".to_string())
        ]
    );
}

#[test]
fn an_untitled_unit_is_keyed_by_position_and_a_repeated_key_is_numbered() {
    let untitled = r#"<section class="panel"><div class="panel-body"></div></section>"#;
    let titled = r#"<section class="panel"><span class="panel-title">A</span></section>"#;
    let tile = r#"<a class="panel"><div class="kpi-label"></div></a>"#;
    let page = page_of(&format!("{untitled}{titled}{tile}{titled}{untitled}{titled}"));
    let units: Vec<(UnitKind, String)> = page.units.iter().map(|u| (u.kind, u.key.clone())).collect();
    let p = |k: &str| (UnitKind::Panel, k.to_string());
    assert_eq!(units, [p("#1"), p("A"), (UnitKind::Tile, "#1".to_string()), p("A (2)"), p("#4"), p("A (3)")]);
    let placeholders: Vec<&str> = page.frame.children.iter().map(|c| c.head.as_str()).collect();
    assert_eq!(
        placeholders,
        [r##"[panel "#1"]"##, r#"[panel "A"]"#, r##"[tile "#1"]"##, r#"[panel "A (2)"]"#, r##"[panel "#4"]"##, r#"[panel "A (3)"]"#],
        "the frame keeps a keyed placeholder where each unit sat"
    );
}

#[test]
fn units_match_by_kind_and_key_and_the_rest_is_reported() {
    let panel = |t: &str| format!(r#"<section class="panel"><span class="panel-title">{t}</span></section>"#);
    let tile = |t: &str| format!(r#"<a class="panel"><div class="kpi-label">{t}</div></a>"#);
    let mock =
        page_of(&format!("<div class=\"grid\">{}{}{}</div>{}", tile("Shared"), tile("Mock tile"), panel("Shared"), panel("Gone")));
    let portal =
        page_of(&format!("<div class=\"grid\">{}{}</div>{}{}", tile("Shared"), panel("Shared"), panel("New"), tile("Gone")));
    let outcome = diff_pages(&mock, &portal);
    assert_eq!((outcome.matched_panels, outcome.matched_tiles), (1, 1));
    let reported = [r#"mock-only panel "Gone""#, r#"mock-only tile "Mock tile""#, r#"portal-only panel "New""#, r#"portal-only tile "Gone""#];
    for line in reported {
        assert!(outcome.lines.contains(line), "{line} in {:?}", outcome.lines);
    }
    let disjoint = diff_pages(&page_of(&panel("A")), &page_of(&panel("B")));
    assert_eq!(disjoint.matched_panels, 0, "the count run_diff fails on");
}

// ---- --text and svg ------------------------------------------------------------------

fn diff_text(mock: &str, portal: &str, text: bool) -> BTreeSet<String> {
    let page_with = |body: &str| page(&root(&format!(r#"<div class="app">{body}</div>"#)), text);
    diff_pages(&page_with(mock), &page_with(portal)).lines
}

#[test]
fn an_svg_drawing_compares_only_with_text() {
    let (mock, portal) = (r#"<svg class="i"><path></path></svg>"#, r#"<svg class="i"><circle></circle></svg>"#);
    assert_eq!(diff_text(mock, portal, false), set(&[]), "without --text an svg is its tag and classes");
    assert_eq!(
        diff_text(mock, portal, true),
        set(&["frame div.app > svg.i: mock-only path", "frame div.app > svg.i: portal-only circle"]),
        "with --text the drawing is compared"
    );
}

#[test]
fn text_compares_only_with_text() {
    let (mock, portal) = ("<p>Alpha</p>", "<p>Beta</p>");
    assert_eq!(diff_text(mock, portal, false), set(&[]), "without --text a text leaf is not in the skeleton");
    assert_eq!(diff_text(mock, portal, true), set(&[r#"frame div.app > p: mock #"Alpha" portal #"Beta""#]));
}

#[test]
fn script_and_style_text_is_never_compared() {
    let mock = "<script>var a = 1;</script><style>.a { color: red }</style>";
    let portal = "<script>var b = 2;</script><style>.b { color: blue }</style>";
    assert_eq!(diff_text(mock, portal, true), set(&[]), "script and style text is excluded even with --text");
    assert_eq!(page_of(mock).frame.canon, "div.app{script,style}", "the elements themselves stay");
    assert_eq!(
        diff_text("<p>var a = 1;</p>", "<p>var b = 2;</p>", true),
        set(&[r#"frame div.app > p: mock #"var a = 1;" portal #"var b = 2;""#]),
        "the same text outside script and style is compared"
    );
}

// ---- hints -------------------------------------------------------------------------------

fn outcome(mock: &str, portal: &str) -> Outcome {
    diff_pages(&page_of(mock), &page_of(portal))
}

fn tbody(rows: &str) -> String {
    format!("<table><tbody>{rows}</tbody></table>")
}

/// The only line of `o` and its hint.
fn only(o: &Outcome) -> (String, Option<String>) {
    assert_eq!(o.lines.len(), 1, "{:?}", o.lines);
    let line = o.lines.iter().next().expect("one line").clone();
    let hint = o.hints.get(&line).cloned();
    (line, hint)
}

/// The hint of a mock-only `row` beside a portal of [`ROW`] only.
fn row_hint(row: &str) -> Option<String> {
    only(&outcome(&tbody(&format!("{ROW}{row}")), &tbody(ROW))).1
}

const RUN_ROW: &str = r#"<tr class="x"><td><span class="mono">m-1</span></td><td><span class="badge">failed</span></td></tr>"#;

#[test]
fn a_mock_only_row_is_hinted_by_its_badge_then_its_first_cell() {
    let (line, hint) = only(&outcome(&tbody(&format!("{ROW}{RUN_ROW}")), &tbody(ROW)));
    assert_eq!(line, format!("frame div.app > table > tbody > tr*: mock-only {}", label_of(RUN_ROW)));
    assert_eq!(hint.as_deref(), Some("failed · m-1"));
    // The badge in the first cell is that cell's text too: it is printed once.
    let badge_first = r#"<tr class="x"><td><span class="badge">failed</span></td><td>m-1</td></tr>"#;
    assert_eq!(row_hint(badge_first).as_deref(), Some("failed"));
    // No badge: the first child with text, past an empty checkbox cell.
    let no_badge = r#"<tr class="x"><td class="cb"></td><td>m-2</td><td>z</td></tr>"#;
    assert_eq!(row_hint(no_badge).as_deref(), Some("m-2"));
    let no_text = r#"<tr class="x"><td></td><td><span class="badge"></span></td></tr>"#;
    assert_eq!(row_hint(no_text), None, "a row with no text has no hint");
}

#[test]
fn a_long_hint_is_cut_and_a_quote_becomes_an_apostrophe() {
    let cell = |text: &str| format!(r#"<tr class="x"><td>{text}</td></tr>"#);
    let long = row_hint(&cell(&"a".repeat(HINT_CHARS + 1))).expect("a hint");
    assert_eq!(long, format!("{}…", "a".repeat(HINT_CHARS)));
    let exact = row_hint(&cell(&"a".repeat(HINT_CHARS))).expect("a hint");
    assert_eq!(exact, "a".repeat(HINT_CHARS), "exactly the limit is not cut");
    // Counted in characters, not bytes: 40 two-byte characters fit.
    let wide = row_hint(&cell(&"é".repeat(HINT_CHARS))).expect("a hint");
    assert_eq!(wide, "é".repeat(HINT_CHARS));

    let o = outcome(&tbody(&format!("{ROW}{}", cell(r#"say "hi""#))), &tbody(ROW));
    let (line, hint) = only(&o);
    assert_eq!(hint.as_deref(), Some("say 'hi'"));
    // So the printed line, kept in an accept file, parses back to its key.
    let accepted = crate::accept::parse(&format!("{} | reason", o.shown(&line))).expect("parses");
    assert_eq!(accepted.keys().collect::<Vec<_>>(), [&line]);
}

#[test]
fn rows_of_one_shape_keep_the_first_rows_hint_and_never_split() {
    let row = |text: &str| format!(r#"<tr class="x"><td>{text}</td></tr>"#);
    let o = outcome(&tbody(&format!("{ROW}{}{}", row("first"), row("second"))), &tbody(ROW));
    let (line, hint) = only(&o);
    assert_eq!(line, format!("frame div.app > table > tbody > tr*: mock-only {}", label_of(&row("first"))));
    assert_eq!(hint.as_deref(), Some("first"));
    let swapped = outcome(&tbody(&format!("{ROW}{}{}", row("second"), row("first"))), &tbody(ROW));
    assert_eq!(only(&swapped).1.as_deref(), Some("second"), "the first in document order");
}

#[test]
fn a_portal_only_row_is_hinted_from_the_portal_side() {
    let (line, hint) = only(&outcome(&tbody(ROW), &tbody(&format!("{ROW}{RUN_ROW}"))));
    assert_eq!(line, format!("frame div.app > table > tbody > tr*: portal-only {}", label_of(RUN_ROW)));
    assert_eq!(hint.as_deref(), Some("failed · m-1"));
}

#[test]
fn a_non_repeat_unpaired_element_is_hinted_by_its_own_text_and_the_key_is_unchanged() {
    let o = outcome(r#"<div class="x"><span>Lore</span></div>"#, r#"<div class="x"></div>"#);
    let (line, hint) = only(&o);
    assert!(line.ends_with(": mock-only span"), "{line}");
    assert!(!line.contains("Lore"), "the hint is not in the key: {line}");
    assert_eq!(hint.as_deref(), Some("Lore"));
    assert!(o.shown(&line).ends_with(" (\"Lore\")"));
    let (plain, _) = only(&outcome(r#"<div class="x"><span>Other</span></div>"#, r#"<div class="x"></div>"#));
    assert_eq!(plain, line, "the key does not depend on the text");
}

#[test]
fn shown_adds_a_hint_only_when_the_unpaired_element_has_text() {
    let note = r#"<p class="note"><span class="badge">new</span><b>Text</b></p>"#;
    let o = outcome(&format!(r#"<div class="x"><span></span>{note}</div>"#), r#"<div class="x"><span></span></div>"#);
    let (line, hint) = only(&o);
    assert_eq!(line, format!("frame div.app > div.x: mock-only {}", label_of(note)));
    assert_eq!(hint.as_deref(), Some("new"), "every unpaired element is hinted");
    assert_eq!(o.shown(&line), format!("{line} (\"new\")"));

    let bare = outcome(r#"<div class="x"><span></span><i></i></div>"#, r#"<div class="x"><span></span></div>"#);
    let (line, hint) = only(&bare);
    assert_eq!(hint, None, "an element with no text has no hint");
    assert_eq!(bare.shown(&line), line);

    let hinted = outcome(&tbody(&format!("{ROW}{RUN_ROW}")), &tbody(ROW));
    let (line, _) = only(&hinted);
    assert_eq!(hinted.shown(&line), format!("{line} (\"failed · m-1\")"));
}

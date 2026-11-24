use super::*;

#[test]
fn enum_variants_reads_the_real_enums() {
    let states = enum_variants(PANEL_STATE_SOURCE, "PanelState").expect("PanelState parses");
    assert_eq!(states, ["Ok", "Empty", "Stale", "Error", "Readonly", "Denied"]);
    let tricky = "enum T {\n    /// A, B\n    #[serde(rename = \"x,y\")]\n    A { x: u8, y: (u8, u8) },\n    /* C, */ B(Vec<u8>),\n    D = 3,\n}";
    assert_eq!(enum_variants(tricky, "T").expect("T parses"), ["A", "B", "D"]);
}

/// Each enum's variants with one more fail the coverage check against the cases rendered today.
#[test]
fn variant_coverage_fails_on_a_new_enum_variant() {
    let env = render::env().expect("templates compile");
    let cases = cases(&env);
    let primitives: BTreeSet<&str> = cases.iter().filter_map(|c| c.primitive).collect();
    let states: BTreeSet<&str> = cases.iter().filter_map(|c| c.state).collect();
    let state_variants = enum_variants(PANEL_STATE_SOURCE, "PanelState").expect("PanelState parses");
    let state_variants: Vec<&str> = state_variants.iter().map(String::as_str).collect();
    for (name, variants, rendered) in
        [("Primitive", Primitive::VARIANTS, &primitives), ("PanelState", state_variants.as_slice(), &states)]
    {
        check_variant_coverage(name, variants, rendered).unwrap_or_else(|e| panic!("today's {name}: {e}"));
        let more: Vec<&str> = variants.iter().copied().chain(["Fake"]).collect();
        let err = check_variant_coverage(name, &more, rendered).expect_err("one variant more must fail");
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

/// A foot count of exactly one takes the noun's singular, every other count its plural, and
/// a table with no noun counts "row" or "rows" the same way.
#[test]
fn a_foot_count_of_one_takes_the_singular() {
    let one = vec![player_row("p-1001", "12", "active", false)];
    let two = vec![player_row("p-1001", "12", "active", false), player_row("p-1002", "40", "banned", false)];
    let counted = |rows: Vec<Row>, total: Option<u64>, noun: Option<Noun>| match players_table(rows, total, None) {
        Primitive::DataTable(t) => Primitive::DataTable(DataTable { item_noun: noun, ..t }),
        _ => unreachable!("players_table is a DataTable"),
    };
    let noun = || Some(Noun::new("player", "players"));
    let total = |p: Primitive| {
        let view = PanelCase::new(PanelState::Ok, Some(body(p))).view().to_string();
        let at = view.find(r#""total":""#).expect("a foot total") + 9;
        view[at..].split('"').next().unwrap_or_default().to_string()
    };
    assert_eq!(total(counted(one.clone(), Some(1), noun())), "1 player · showing 1–1");
    assert_eq!(total(counted(two.clone(), Some(2), noun())), "2 players · showing 1–2");
    assert_eq!(total(counted(one.clone(), None, noun())), "1 player");
    assert_eq!(total(counted(one.clone(), None, None)), "1 row");
    assert_eq!(total(counted(two, None, None)), "2 rows");
    let queue = |total: Option<u64>| {
        let q = work_queue("pending", vec![queue_item("j-1", "pending", 0, 5, &[])]);
        Primitive::WorkQueue(WorkQueue { page: PageInfo { total, ..q.page.clone() }, ..q })
    };
    assert_eq!(total(queue(Some(1))), "1 job");
    assert_eq!(total(queue(Some(3))), "1 of 3 jobs");
}

/// A copy action is drawn enabled, with its `data-copy` name, only in a panel whose body holds the
/// `data-copy-source` block it copies; a panel with no code block draws it disabled, never an
/// enabled button admin.js would find no source for.
#[test]
fn a_copy_action_is_drawn_enabled_only_where_the_panel_holds_its_source() {
    let env = render::env().expect("templates compile");
    let all = cases(&env);
    let raw = |name: &str| all.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("case {name}")).raw.clone();

    let with_code = raw("props_copy_code_footer");
    assert!(with_code.contains("data-copy-source=\"code\""), "{with_code}");
    assert!(with_code.contains("data-copy=\"code\""), "the footer's copy is enabled: {with_code}");
    assert!(!with_code.contains("No payload to copy"), "{with_code}");

    let no_code = raw("props_copy_no_code");
    assert!(!no_code.contains("data-copy-source"), "{no_code}");
    assert!(!no_code.contains("data-copy="), "no enabled copy button without a source: {no_code}");
    assert!(
        no_code.contains(r#"disabled title="No payload to copy""#) && no_code.contains("Copy payload"),
        "the head's copy is disabled with its hint: {no_code}"
    );
}

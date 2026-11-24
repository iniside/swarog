//! `head_actions`: a page head renders an action bound to no item or entity, so only a
//! `PageHead` of that same page may be named there.
//!
//! Every fixture is otherwise clean under the full [`findings`] run (the twin proves it), so
//! each misplaced case asserts the WHOLE finding list equals the one head-action line.

use controlplane::{
    ActionDecl, ActionPoint, ContributionDecl, EntityKindDecl, Emphasis, Field, FieldKind, FormMode, FormSpec, Layout,
    Manifest, NavEntry, NavSection, PageDecl, Point, Role, SearchDecl, Then,
};

use checkmodules::ProcessControlPlane;

use super::{findings, peer_findings};

const PAGE: &str = "outbox";

fn page(id: &str, head_actions: &[&str], layout: &[&str]) -> PageDecl {
    PageDecl {
        id: id.into(),
        title: id.into(),
        subtitle: String::new(),
        head_actions: head_actions.iter().map(|s| s.to_string()).collect(),
        tiles: Vec::new(),
        layout: Layout::Stack(layout.iter().map(|s| s.to_string()).collect()),
        params: Vec::new(),
    }
}

fn action(id: &str, point: ActionPoint) -> ActionDecl {
    ActionDecl {
        id: id.into(),
        point,
        label: id.into(),
        icon: "send".into(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then: Then::Invoke,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

/// Page `outbox` names `act` in its head; `act` sits at `point`. A second page and a
/// `queue` contribution on `outbox` plus a declared `message` kind make every non-PageHead
/// point otherwise valid, so `head_actions` is the only check left to object.
fn manifest(point: ActionPoint) -> Manifest {
    Manifest {
        module: "mail".into(),
        pages: vec![page(PAGE, &["act"], &["queue"]), page("other-page", &[], &[])],
        entity_kinds: vec![EntityKindDecl {
            kind: "message".into(),
            label: "Message".into(),
            icon: "mail".into(),
        }],
        contributions: vec![ContributionDecl {
            id: "queue".into(),
            point: Point::Page(PAGE.into()),
            order: 0,
            title: "Queue".into(),
            icon: "list".into(),
            min_role: Role::Viewer,
            cache_ttl_s: None,
            deadline_ms: 1000,
            readonly: None,
        }],
        actions: vec![action("act", point)],
        ..Manifest::default()
    }
}

#[test]
fn a_page_head_action_of_this_page_is_clean() {
    assert_eq!(findings(&[manifest(ActionPoint::PageHead(PAGE.into()))]), Vec::<String>::new());
}

#[test]
fn a_page_head_action_at_any_other_point_is_refused() {
    let cases = [
        (ActionPoint::QueueRow("queue".into()), r#"QueueRow("queue")"#),
        (ActionPoint::PanelTool("queue".into()), r#"PanelTool("queue")"#),
        (ActionPoint::PanelForm("queue".into()), r#"PanelForm("queue")"#),
        (ActionPoint::RowAction("message".into()), r#"RowAction("message")"#),
        (ActionPoint::HeaderAction("message".into()), r#"HeaderAction("message")"#),
        (ActionPoint::PageHead("other-page".into()), r#"PageHead("other-page")"#),
    ];
    for (point, shown) in cases {
        assert_eq!(
            findings(&[manifest(point)]),
            vec![format!(
                r#"mail: page "outbox" names head action "act", which is {shown}, not PageHead("outbox")"#
            )],
            "point {shown}"
        );
    }
}

#[test]
fn a_page_head_naming_no_action_of_the_module_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.pages[0].head_actions = vec!["ghost".into()];
    assert_eq!(
        findings(&[m]),
        vec![r#"mail: page "outbox" names head action "ghost", which is not an action of this module"#.to_string()]
    );
}

fn contribution(id: &str, point: Point) -> ContributionDecl {
    ContributionDecl {
        id: id.into(),
        point,
        order: 0,
        title: id.into(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 1000,
        readonly: None,
    }
}

/// The clean fixture of a writing module whose `queue` declares `readonly`, with a
/// `wipe` tool on it built by `wipe`.
fn readonly_queue(wipe: impl FnOnce(ActionDecl) -> ActionDecl) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.writes = true;
    m.contributions[0].readonly = Some("Read-only: the queue is inspected here.".into());
    m.actions.push(wipe(action("wipe", ActionPoint::PanelTool("queue".into()))));
    m
}

#[test]
fn an_implemented_write_on_a_readonly_contribution_is_refused() {
    assert_eq!(
        findings(&[readonly_queue(|a| a)]),
        vec![r#"mail: implemented action "wipe" writes from contribution "queue", which declares readonly"#.to_string()]
    );
}

#[test]
fn a_readonly_contribution_may_carry_an_unimplemented_action_or_a_link() {
    assert_eq!(findings(&[readonly_queue(|a| ActionDecl { implemented: false, ..a })]), Vec::<String>::new());
    let link = Then::Link(controlplane::Link::Page {
        module: "mail".into(),
        page: PAGE.into(),
        params: Default::default(),
    });
    assert_eq!(findings(&[readonly_queue(|a| ActionDecl { then: link, ..a })]), Vec::<String>::new());
}

/// The clean fixture with a `dl` action built by `dl` from an icon-only `TableRow` on `queue`.
fn icon_only_row(dl: impl FnOnce(ActionDecl) -> ActionDecl) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    let row = ActionDecl { icon_only: true, ..action("dl", ActionPoint::TableRow("queue".into())) };
    m.actions.push(dl(row));
    m
}

#[test]
fn icon_only_needs_a_table_row_with_an_icon() {
    assert_eq!(findings(&[icon_only_row(|a| a)]), Vec::<String>::new());
    assert_eq!(
        findings(&[icon_only_row(|a| ActionDecl { icon: String::new(), ..a })]),
        vec![r#"mail: action "dl" sets icon_only but has no icon"#.to_string()]
    );
    let tool = |a| ActionDecl { point: ActionPoint::PanelTool("queue".into()), ..a };
    assert_eq!(
        findings(&[icon_only_row(tool)]),
        vec![r#"mail: action "dl" sets icon_only but is not a TableRow"#.to_string()]
    );
}

#[test]
fn an_id_in_the_tiles_and_in_a_nested_row_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.pages[0].tiles = vec!["queue".into()];
    m.pages[0].layout = Layout::Rows(vec![Layout::Rows(vec![Layout::Stack(vec!["queue".into()])])]);
    let both = r#"mail: page "outbox" places "queue" in both its tiles and its layout"#;
    assert_eq!(findings(&[m]), vec![both.to_string()]);
}

#[test]
fn a_tab_count_or_tile_must_name_a_declared_tab_of_its_kind() {
    let tab = |id: &str| {
        let (kind, tab, tab_label) = ("message".into(), id.into(), id.into());
        Point::EntityTab { kind, tab, tab_label, columns: 1, params: Vec::new() }
    };
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.contributions.extend([
        contribution("body", tab("body")),
        contribution("body-count", Point::EntityTabCount { kind: "message".into(), tab: "body".into() }),
        contribution("body-tile", Point::EntityTabTile { kind: "message".into(), tab: "body".into() }),
    ]);
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.contributions.extend([
        contribution("ghost-count", Point::EntityTabCount { kind: "message".into(), tab: "ghost".into() }),
        contribution("other-tile", Point::EntityTabTile { kind: "player".into(), tab: "body".into() }),
    ]);
    let other = r#"mail: contribution "other-tile" targets entity kind "player", which no manifest declares"#;
    let none = "which no EntityTab declares";
    assert_eq!(
        findings(&[m]),
        vec![
            other.to_string(),
            format!(r#"mail: contribution "ghost-count" targets tab "ghost" of entity kind "message", {none}"#),
            format!(r#"mail: contribution "other-tile" targets tab "body" of entity kind "player", {none}"#),
        ]
    );
}

fn label(id: &str, kind: &str) -> ContributionDecl {
    contribution(id, Point::EntityLabel { kind: kind.into() })
}

/// A manifest of module `post` declaring no kind and contributing `contributions`.
fn post(contributions: Vec<ContributionDecl>) -> Manifest {
    Manifest { module: "post".into(), contributions, ..Manifest::default() }
}

#[test]
fn the_owner_with_one_label_of_its_kind_is_clean() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.contributions.push(label("message-label", "message"));
    assert_eq!(findings(&[m, post(Vec::new())]), Vec::<String>::new());
}

#[test]
fn a_kind_declared_by_two_manifests_is_refused() {
    let mut second = post(Vec::new());
    second.entity_kinds = manifest(ActionPoint::PageHead(PAGE.into())).entity_kinds;
    assert_eq!(
        findings(&[manifest(ActionPoint::PageHead(PAGE.into())), second]),
        vec![r#"entity kind "message" is declared by more than one manifest: mail, post"#.to_string()]
    );
}

#[test]
fn a_label_from_a_module_not_owning_the_kind_is_refused_once() {
    let owner = manifest(ActionPoint::PageHead(PAGE.into()));
    let undeclared = "which this module's manifest does not declare";
    assert_eq!(
        findings(&[owner.clone(), post(vec![label("post-label", "message")])]),
        vec![format!(r#"post: EntityLabel "post-label" labels entity kind "message", {undeclared}"#)]
    );
    assert_eq!(
        findings(&[owner, post(vec![label("post-a", "message"), label("post-b", "message")])]),
        vec![format!(r#"post: EntityLabel "post-a, post-b" labels entity kind "message", {undeclared}"#)]
    );
}

#[test]
fn a_label_of_a_kind_no_manifest_declares_is_only_entity_kinds_finding() {
    assert_eq!(
        findings(&[manifest(ActionPoint::PageHead(PAGE.into())), post(vec![label("post-label", "player")])]),
        vec![r#"post: contribution "post-label" targets entity kind "player", which no manifest declares"#.to_string()]
    );
}

#[test]
fn two_labels_of_its_kind_in_the_owner_manifest_are_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.contributions.extend([label("label-a", "message"), label("label-b", "message")]);
    assert_eq!(
        findings(&[m]),
        vec![r#"mail: more than one EntityLabel of entity kind "message": label-a, label-b"#.to_string()]
    );
}

fn mode(id: &str, action: &str) -> FormMode {
    FormMode { id: id.into(), label: id.into(), action: action.into(), submit_label: id.into() }
}

/// The clean head-action fixture plus `adjust`, a `PanelTool("queue")` form whose modes are
/// `modes`, and the `credit` and `debit` actions at its point.
fn moded(modes: Vec<FormMode>) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    let spec = FormSpec {
        fields: Vec::new(),
        hidden: Default::default(),
        submit_label: "Apply".into(),
        note: None,
        revision: None,
        title: None,
        modes,
        reason: controlplane::ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    };
    let tool = ActionPoint::PanelTool("queue".into());
    m.actions.push(ActionDecl { then: Then::Form(spec), ..action("adjust", tool.clone()) });
    m.actions.push(action("credit", tool.clone()));
    m.actions.push(action("debit", tool));
    m
}

#[test]
fn a_forms_modes_at_its_point_and_a_contributions_several_panel_forms_are_clean() {
    assert_eq!(findings(&[moded(vec![mode("credit", "credit"), mode("debit", "debit")])]), Vec::<String>::new());
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(action("grant", ActionPoint::PanelForm("queue".into())));
    m.actions.push(action("revoke", ActionPoint::PanelForm("queue".into())));
    assert_eq!(findings(&[m]), Vec::<String>::new(), "a panel form's modes sit at one PanelForm point");
}

#[test]
fn a_mode_naming_a_missing_misplaced_or_non_invoke_action_or_a_repeated_id_is_refused() {
    let prefix = r#"mail: action "adjust" form: "#;
    let ghost = moded(vec![mode("ghost", "ghost")]);
    let line = r#"mode "ghost" names action "ghost", which is not an action of this module"#;
    assert_eq!(findings(&[ghost]), vec![format!("{prefix}{line}")]);

    let mut elsewhere = moded(vec![mode("credit", "credit")]);
    elsewhere.actions[2].point = ActionPoint::PageHead("other-page".into());
    let line = r#"mode "credit" names action "credit", which sits at PageHead("other-page"), not at the form's point PanelTool("queue")"#;
    assert_eq!(findings(&[elsewhere]), vec![format!("{prefix}{line}")]);

    let mut prepared = moded(vec![mode("credit", "credit")]);
    prepared.actions[2].prepare = true;
    let refused = findings(&[prepared]);
    let line = r#"mode "credit" names action "credit", which is not a Then::Invoke action without prepare"#;
    assert!(refused.contains(&format!("{prefix}{line}")), "{refused:?}");

    let twice = moded(vec![mode("credit", "credit"), mode("credit", "debit")]);
    assert_eq!(findings(&[twice]), vec![format!(r#"{prefix}mode id "credit" is repeated"#)]);
}

/// A request id field the portal fills is clean with no default and refused with one.
#[test]
fn a_request_id_field_with_a_default_is_refused() {
    let with = |default: Option<&str>| {
        let mut m = moded(Vec::new());
        let adjust = m.actions.iter_mut().find(|a| a.id == "adjust").unwrap();
        let Then::Form(spec) = &mut adjust.then else { unreachable!() };
        spec.fields.push(Field {
            key: "rid".into(),
            label: "Request id".into(),
            kind: FieldKind::RequestId,
            required: false,
            help: None,
            default: default.map(Into::into),
            readonly: false,
            mono: false,
            placeholder: None,
        });
        m
    };
    assert_eq!(findings(&[with(None)]), Vec::<String>::new());
    let line = r#"mail: action "adjust" form: field "rid" is a request id, which the portal fills, and declares a default"#;
    assert_eq!(findings(&[with(Some("grant-7c1e0a"))]), vec![line.to_string()]);
}

/// `moded` with no modes, its `adjust` form carrying `fields` and `hidden`.
fn fielded(fields: Vec<Field>, hidden: &[(&str, &str)]) -> Manifest {
    let mut m = moded(Vec::new());
    let adjust = m.actions.iter_mut().find(|a| a.id == "adjust").unwrap();
    let Then::Form(spec) = &mut adjust.then else { unreachable!() };
    spec.fields = fields;
    spec.hidden = hidden.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    m
}

fn field(key: &str, kind: FieldKind, default: Option<&str>) -> Field {
    Field {
        key: key.into(),
        label: key.into(),
        kind,
        required: false,
        help: None,
        default: default.map(Into::into),
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

/// A plain key beside a distinct hidden key is clean; a key with `__` or a leading `_`, or
/// one a hidden key repeats, is refused through `controlplane::form_spec_errors`.
#[test]
fn a_reserved_or_hidden_field_key_is_refused() {
    let prefix = r#"mail: action "adjust" form: "#;
    let clean = fielded(vec![field("amount", FieldKind::Int, None)], &[("currency", "gold")]);
    assert_eq!(findings(&[clean]), Vec::<String>::new());

    for key in ["gold__amount", "_amount"] {
        let refused = fielded(vec![field(key, FieldKind::Int, None)], &[]);
        let line = format!("{prefix}field key {key:?} contains `__` or starts with `_`");
        assert_eq!(findings(&[refused]), vec![line], "{key}");
    }

    let hidden = fielded(vec![field("currency", FieldKind::String, None)], &[("currency", "gold")]);
    let line = format!(r#"{prefix}field key "currency" is also a hidden key"#);
    assert_eq!(findings(&[hidden]), vec![line]);
}

/// A default its kind decodes is clean; one it does not is refused with the codec's reason.
#[test]
fn an_undecodable_form_default_is_refused() {
    let with = |default: &str| fielded(vec![field("amount", FieldKind::Int, Some(default))], &[]);
    assert_eq!(findings(&[with("10")]), Vec::<String>::new());
    let line = r#"mail: action "adjust" form: field "amount" default does not decode: must be an integer"#;
    assert_eq!(findings(&[with("ten")]), vec![line.to_string()]);
}

/// The clean head-action fixture with `p` added by `p`, from a prepared `PanelTool("queue")`
/// Confirm: the one point and variant (with `Then::Form`) a `prepare` call is valid on.
fn prepared(p: impl FnOnce(ActionDecl) -> ActionDecl) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    let confirm = controlplane::ConfirmSpec {
        title: "Wipe".into(),
        body: "Wipe the queue.".into(),
        tone: controlplane::ConfirmTone::Danger,
        impact: Vec::new(),
        reason: controlplane::ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Wipe".into(),
    };
    let tool = action("p", ActionPoint::PanelTool("queue".into()));
    m.actions.push(p(ActionDecl { then: Then::Confirm(confirm), prepare: true, ..tool }));
    m
}

fn outbox_link() -> Then {
    Then::Link(controlplane::Link::Page { module: "mail".into(), page: PAGE.into(), params: Default::default() })
}

#[test]
fn a_prepared_confirm_on_a_panel_tool_is_clean() {
    assert_eq!(findings(&[prepared(|a| a)]), Vec::<String>::new());
}

#[test]
fn prepare_on_then_invoke_is_refused() {
    assert_eq!(
        findings(&[prepared(|a| ActionDecl { then: Then::Invoke, ..a })]),
        vec![r#"mail: action "p" sets prepare but is not Then::Confirm or Then::Form"#.to_string()]
    );
}

#[test]
fn prepare_on_then_link_is_refused() {
    assert_eq!(
        findings(&[prepared(|a| ActionDecl { then: outbox_link(), ..a })]),
        vec![r#"mail: action "p" sets prepare but is not Then::Confirm or Then::Form"#.to_string()]
    );
}

/// A `PanelForm` must be `Then::Invoke`, which `prepare` refuses too: no prepared panel
/// form gives the point finding alone.
#[test]
fn prepare_on_a_panel_form_action_is_refused() {
    let panel_form = |a| ActionDecl { point: ActionPoint::PanelForm("queue".into()), then: Then::Invoke, ..a };
    assert_eq!(
        findings(&[prepared(panel_form)]),
        vec![
            r#"mail: action "p" sets prepare but is not Then::Confirm or Then::Form"#.to_string(),
            r#"mail: PanelForm action "p" sets prepare, but a panel form has no modal"#.to_string(),
        ]
    );
}

#[test]
fn a_panel_form_action_that_is_not_then_invoke_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(ActionDecl { then: outbox_link(), ..action("open", ActionPoint::PanelForm("queue".into())) });
    assert_eq!(findings(&[m]), vec![r#"mail: PanelForm action "open" is not Then::Invoke"#.to_string()]);
}

#[test]
fn an_action_on_a_contribution_the_module_lacks_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(action("retry", ActionPoint::QueueRow("ghost".into())));
    assert_eq!(
        findings(&[m]),
        vec![r#"mail: action "retry" is QueueRow("ghost"), which is not a contribution of this module"#.to_string()]
    );
}

/// The clean fixture whose `outbox` tile row names `kpi`, a contribution at `point`.
fn tiled(point: Point) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.contributions.push(contribution("kpi", point));
    m.pages[0].tiles = vec!["kpi".into()];
    m
}

#[test]
fn a_tiles_id_on_this_page_is_clean() {
    assert_eq!(findings(&[tiled(Point::Page(PAGE.into()))]), Vec::<String>::new());
}

#[test]
fn a_tiles_id_whose_contribution_is_a_dashboard_tile_is_refused() {
    assert_eq!(
        findings(&[tiled(Point::DashboardTile)]),
        vec![r#"mail: page "outbox" lays out "kpi", which is not a contribution with Point::Page("outbox")"#.to_string()]
    );
}

/// The clean fixture with a second `outbox` head action `new`, both actions at `emphasis`.
fn two_heads(emphasis: Emphasis) -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.pages[0].head_actions.push("new".into());
    m.actions.push(action("new", ActionPoint::PageHead(PAGE.into())));
    m.actions.iter_mut().for_each(|a| a.emphasis = emphasis);
    m
}

#[test]
fn a_second_primary_head_action_is_refused() {
    let mut one = two_heads(Emphasis::Primary);
    one.actions[0].emphasis = Emphasis::Default;
    assert_eq!(findings(&[one]), Vec::<String>::new());
    assert_eq!(
        findings(&[two_heads(Emphasis::Primary)]),
        vec![r#"mail: page "outbox" has more than one primary head action: act, new"#.to_string()]
    );
}

#[test]
fn a_trailing_icon_off_a_link_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    let audit = action("audit", ActionPoint::PanelTool("queue".into()));
    m.actions.push(ActionDecl { then: outbox_link(), trailing_icon: true, ..audit });
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.actions[1].then = Then::Invoke;
    assert_eq!(findings(&[m]), vec![r#"mail: action "audit" sets trailing_icon but is not Then::Link"#.to_string()]);
}

#[test]
fn a_copy_off_a_panel_tool_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(ActionDecl { then: Then::Copy, ..action("copy", ActionPoint::PanelTool("queue".into())) });
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.actions[1].point = ActionPoint::PageHead(PAGE.into());
    assert_eq!(findings(&[m]), vec![r#"mail: action "copy" is Then::Copy but not a PanelTool"#.to_string()]);
}

#[test]
fn a_copy_shown_on_an_empty_panel_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(ActionDecl { then: Then::Copy, ..action("copy", ActionPoint::PanelTool("queue".into())) });
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.actions[1].when_empty = true;
    let refused = r#"mail: action "copy" is Then::Copy but sets when_empty, and an empty panel has no code block"#;
    assert_eq!(findings(&[m]), vec![refused.to_string()]);
}

#[test]
fn when_empty_off_a_panel_tool_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(ActionDecl { when_empty: true, ..action("compose", ActionPoint::PanelTool("queue".into())) });
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.actions[1].point = ActionPoint::QueueRow("queue".into());
    assert_eq!(findings(&[m]), vec![r#"mail: action "compose" sets when_empty but is not a PanelTool"#.to_string()]);
}

#[test]
fn a_module_declared_by_two_providers_is_refused() {
    let m = manifest(ActionPoint::PageHead(PAGE.into()));
    assert_eq!(findings(&[m.clone(), m]), vec!["mail: two providers declare this module".to_string()]);
}

/// Each of the six id lists of a manifest refuses a repeat, one finding per list, in order.
#[test]
fn a_repeated_id_in_any_id_list_is_refused() {
    let mut m = navigated();
    let search = SearchDecl { kind: "message".into(), label: "Messages".into(), icon: "mail".into(), deadline_ms: 800 };
    m.search.push(search);
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.contributions.push(m.contributions[0].clone());
    m.actions.push(m.actions[0].clone());
    m.pages.push(m.pages[1].clone());
    m.entries.push(m.entries[0].clone());
    m.search.push(m.search[0].clone());
    m.entity_kinds.push(m.entity_kinds[0].clone());
    assert_eq!(
        findings(&[m]),
        vec![
            r#"mail: duplicate contribution id "queue""#.to_string(),
            r#"mail: duplicate action id "act""#.to_string(),
            r#"mail: duplicate page id "other-page""#.to_string(),
            r#"mail: duplicate nav entry id "outbox""#.to_string(),
            r#"mail: duplicate search kind id "message""#.to_string(),
            r#"mail: duplicate entity kind id "message""#.to_string(),
        ]
    );
}

#[test]
fn an_action_targeting_an_undeclared_entity_kind_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.actions.push(action("ban", ActionPoint::RowAction("message".into())));
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.actions[1].point = ActionPoint::RowAction("player".into());
    assert_eq!(
        findings(&[m]),
        vec![r#"mail: action "ban" targets entity kind "player", which no manifest declares"#.to_string()]
    );
}

#[test]
fn an_id_longer_than_the_limit_is_refused() {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    let at_limit = "a".repeat(super::MAX_ID_BYTES);
    m.actions.push(action(&at_limit, ActionPoint::PanelTool("queue".into())));
    m.contributions.push(contribution(&at_limit, Point::DashboardTile));
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    let over = format!("{at_limit}b");
    let mut action_over = m.clone();
    action_over.actions[1].id = over.clone();
    assert_eq!(findings(&[action_over]), vec![format!(r#"mail: action id "{over}" is longer than 64 bytes"#)]);
    m.contributions[1].id = over.clone();
    assert_eq!(findings(&[m]), vec![format!(r#"mail: contribution id "{over}" is longer than 64 bytes"#)]);
}

/// The clean fixture with nav entry `outbox` in section `platform` opening page `outbox`,
/// counted by its `badge` `NavCounter`.
fn navigated() -> Manifest {
    let mut m = manifest(ActionPoint::PageHead(PAGE.into()));
    m.contributions.push(contribution("badge", Point::NavCounter));
    m.entries.push(NavEntry {
        section: "platform".into(),
        id: "outbox".into(),
        label: "Outbox".into(),
        icon: "mail".into(),
        order: 0,
        page: PAGE.into(),
        counter: Some("badge".into()),
    });
    m
}

#[test]
fn a_nav_entry_on_a_declared_page_section_and_counter_is_clean() {
    assert_eq!(findings(&[navigated()]), Vec::<String>::new());
}

#[test]
fn a_nav_entry_opening_an_undeclared_page_is_refused() {
    let mut m = navigated();
    m.entries[0].page = "ghost".into();
    assert_eq!(
        findings(&[m]),
        vec![r#"mail: nav entry "outbox" opens page "ghost", which this module does not declare"#.to_string()]
    );
}

#[test]
fn a_nav_counter_that_is_not_a_nav_counter_contribution_is_refused() {
    let mut m = navigated();
    m.entries[0].counter = Some("queue".into());
    let line = r#"mail: nav entry "outbox" names counter "queue", which is not a Point::NavCounter contribution"#;
    assert_eq!(findings(&[m]), vec![line.to_string()]);
}

#[test]
fn a_nav_entry_in_an_unknown_section_is_refused() {
    let mut m = navigated();
    m.entries[0].section = "ops".into();
    assert_eq!(
        findings(&[m.clone()]),
        vec![r#"mail: nav entry "outbox" names section "ops", which is neither fixed nor declared"#.to_string()]
    );
    m.sections.push(NavSection { id: "ops".into(), label: "Ops".into(), order: 50 });
    assert_eq!(findings(&[m]), Vec::<String>::new(), "a declared section is known");
}

/// A section redeclared exactly as it wins is clean; one that loses with another label or
/// order to the fixed `SECTIONS` entry is refused.
#[test]
fn a_section_declared_unlike_its_winner_is_refused() {
    let mut m = navigated();
    m.sections.push(NavSection { id: "platform".into(), label: "Platform".into(), order: 40 });
    assert_eq!(findings(&[m.clone()]), Vec::<String>::new());
    m.sections[0].label = "Ops".into();
    let line = r#"mail: section "platform" is declared as ("Ops", 40) but controlplane::SECTIONS declares it as ("Platform", 40)"#;
    assert_eq!(findings(&[m]), vec![line.to_string()]);
}

/// Two manifests declaring one section not in `SECTIONS`: the first by module name wins and
/// owns it; the other is clean when it declares it alike and refused, naming the owner, when
/// not.
#[test]
fn a_section_declared_unlike_another_modules_winner_is_refused() {
    let ops = |label: &str| NavSection { id: "ops".into(), label: label.into(), order: 50 };
    let mut bank = provider("bank");
    bank.sections.push(ops("Ops"));
    let mut mail = navigated();
    mail.sections.push(ops("Ops"));
    assert_eq!(findings(&[mail.clone(), bank.clone()]), Vec::<String>::new());
    mail.sections[0].label = "Operations".into();
    let line = r#"mail: section "ops" is declared as ("Operations", 50) but bank declares it as ("Ops", 50)"#;
    assert_eq!(findings(&[mail, bank]), vec![line.to_string()]);
}

/// A provider manifest for `module`; `peer_findings` reads only the module name.
fn provider(module: &str) -> Manifest {
    Manifest { module: module.into(), ..Manifest::default() }
}

fn process(process: &'static str, modules: &[&str], dialled: &[&str]) -> ProcessControlPlane {
    ProcessControlPlane {
        process,
        manifests: modules.iter().map(|m| provider(m)).collect(),
        dialled: dialled.iter().map(|s| s.to_string()).collect(),
    }
}

/// admin-svc hosts `admin` and dials `mail`, mail-svc hosts `mail`, idle-svc hosts nothing
/// and is not dialled, the monolith hosts `admin` and `mail`: clean. The portal's own
/// provider and the idle process pin the two filters of the undialled-host check. Each peer
/// case below adds to this split, so the one finding it asserts is that case's.
fn clean_split(admin_dials: &[&str]) -> Vec<ProcessControlPlane> {
    vec![
        process("admin-svc", &["admin"], admin_dials),
        process("mail-svc", &["mail"], &[]),
        process("idle-svc", &[], &[]),
    ]
}

/// The monolith of [`clean_split`] plus `extra`.
fn monolith(extra: &[&str]) -> Vec<Manifest> {
    ["admin", "mail"].iter().chain(extra).map(|m| provider(m)).collect()
}

#[test]
fn a_dialled_peer_whose_svc_hosts_a_provider_is_clean() {
    assert_eq!(peer_findings(&monolith(&[]), &clean_split(&["mail"])), Vec::<String>::new());
}

/// The once-silent branch: a dialled peer's process exists but hosts no provider.
#[test]
fn a_dialled_peer_whose_svc_hosts_no_provider_is_refused() {
    let mut split = clean_split(&["mail", "x"]);
    split.push(process("x-svc", &[], &[]));
    assert_eq!(
        peer_findings(&monolith(&[]), &split),
        vec![r#"admin-svc dials peer "x", but x-svc hosts no control-plane provider"#.to_string()]
    );
}

/// A dialled peer with no process gets only the no-process finding, not the no-provider one too.
#[test]
fn a_dialled_peer_with_no_svc_is_refused_once() {
    assert_eq!(
        peer_findings(&monolith(&[]), &clean_split(&["mail", "x"])),
        vec![r#"admin-svc dials peer "x", but no split process is x-svc"#.to_string()]
    );
}

#[test]
fn a_provider_host_admin_svc_does_not_dial_is_refused() {
    let mut split = clean_split(&["mail"]);
    split.push(process("ledger-svc", &["wallet", "audit"], &[]));
    assert_eq!(
        peer_findings(&monolith(&["wallet", "audit"]), &split),
        vec![r#"ledger-svc: hosts provider(s) audit, wallet but admin-svc does not dial peer "ledger""#.to_string()]
    );
}

#[test]
fn a_module_hosted_by_two_split_processes_is_refused() {
    let mut split = clean_split(&["mail", "post"]);
    split.push(process("post-svc", &["mail"], &[]));
    assert_eq!(
        peer_findings(&monolith(&[]), &split),
        vec!["mail: hosted by more than one split process: mail-svc, post-svc".to_string()]
    );
}

#[test]
fn a_provider_hosted_in_one_topology_only_is_refused() {
    assert_eq!(
        peer_findings(&monolith(&["chat"]), &clean_split(&["mail"])),
        vec!["chat: the monolith hosts this provider but no split process does".to_string()]
    );
    assert_eq!(
        peer_findings(&[provider("admin")], &clean_split(&["mail"])),
        vec!["mail: a split process hosts this provider but the monolith does not".to_string()]
    );
}

/// With no admin-svc the dial list is unchecked: an undialled host and a dial of nothing
/// raise no dial finding, only the missing-portal one.
#[test]
fn a_split_with_no_admin_svc_is_refused_once() {
    let split = vec![process("mail-svc", &["mail"], &["ghost"])];
    assert_eq!(
        peer_findings(&[provider("mail")], &split),
        vec!["no split process is named admin-svc, so no dial list is checked".to_string()]
    );
}

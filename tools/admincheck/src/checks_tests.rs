//! `head_actions`: a page head renders an action bound to no item or entity, so only a
//! `PageHead` of that same page may be named there.
//!
//! Every fixture is otherwise clean under the full [`findings`] run (the twin proves it), so
//! each misplaced case asserts the WHOLE finding list equals the one head-action line.

use controlplane::{
    ActionDecl, ActionPoint, ContributionDecl, EntityKindDecl, Emphasis, Layout, Manifest, PageDecl, Point,
    Role, Then,
};

use super::findings;

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
    let tab = |id: &str| Point::EntityTab { kind: "message".into(), tab: id.into(), tab_label: id.into(), columns: 1 };
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

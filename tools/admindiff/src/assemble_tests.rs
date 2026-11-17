use super::{assemble, is_pending_slot, pending_slots};
use crate::html::{parse, serialize, Node};

fn doc(src: &str) -> Vec<Node> {
    parse(src).expect("fixture parses")
}

fn assembled(page: &str, fragments: &[&str]) -> anyhow::Result<String> {
    assemble(doc(page), fragments.iter().map(|f| doc(f)).collect()).map(|d| serialize(&d))
}

#[test]
fn pending_slots_are_listed_in_document_order_without_searching_their_placeholders() {
    let page = doc(concat!(
        r#"<div class="app"><div class="slot" hx-get="/a"><div class="slot" hx-get="/placeholder"></div></div>"#,
        r#"<main><div class="slot">loaded</div><div hx-get="/not-a-slot"><span class="slot" hx-get="/b"></span></div></main>"#,
        r#"<div class="slot" hx-get="/c"></div></div>"#,
    ));
    assert_eq!(pending_slots(&page), ["/a", "/b", "/c"]);
    let loaded = parse(r#"<div class="slot" data-state="ok"></div>"#).expect("parses");
    let Node::Element(e) = &loaded[0] else { unreachable!() };
    assert!(!is_pending_slot(e), "a loaded slot carries no hx-get");
}

#[test]
fn fragments_replace_their_slots_in_document_order() {
    let page = r#"<div class="app"><p class="slot" hx-get="/a">loading</p><main><p class="slot" hx-get="/b">loading</p></main></div>"#;
    let out = assembled(page, &[r#"<i class="slot">A</i>"#, r#"<b class="slot">B</b><u>tail</u>"#]).expect("assembles");
    assert_eq!(out, r#"<div class="app"><i class="slot">A</i><main><b class="slot">B</b><u>tail</u></main></div>"#);
}

#[test]
fn a_fragment_count_mismatch_is_an_error_either_way() {
    let two = r#"<div class="slot" hx-get="/a"></div><div class="slot" hx-get="/b"></div>"#;
    let short = format!("{:#}", assembled(two, &["<i></i>"]).expect_err("one fragment for two slots"));
    assert!(short.contains("a pending slot has no fragment"), "{short}");
    let one = r#"<div class="slot" hx-get="/a"></div>"#;
    let long = format!("{:#}", assembled(one, &["<i></i>", "<b></b>"]).expect_err("two fragments for one slot"));
    assert!(long.contains("2 fragment(s) for fewer pending slots"), "{long}");
    assert_eq!(assembled(one, &["<i></i>"]).expect("one for one"), "<i></i>");
}

#[test]
fn an_oob_element_replaces_the_element_with_its_id() {
    let page = r#"<div class="app"><span class="avatar" id="entity-avatar">?</span><div class="slot" hx-get="/head"></div></div>"#;
    let fragment = r#"<div class="slot" data-state="ok"><h1>Anna</h1></div><span class="avatar lg" id="entity-avatar" hx-swap-oob="true">AK</span>"#;
    let out = assembled(page, &[fragment]).expect("assembles");
    assert_eq!(
        out,
        r#"<div class="app"><span class="avatar lg" id="entity-avatar" hx-swap-oob="true">AK</span><div class="slot" data-state="ok"><h1>Anna</h1></div></div>"#,
        "the oob element swaps by id and is not left inline"
    );
}

#[test]
fn an_oob_element_without_id_or_target_is_an_error() {
    let page = r#"<div class="app"><div class="slot" hx-get="/head"></div></div>"#;
    let no_id = format!("{:#}", assembled(page, &[r#"<i></i><span hx-swap-oob="true">x</span>"#]).expect_err("no id"));
    assert!(no_id.contains("an hx-swap-oob <span> has no id"), "{no_id}");
    let no_target =
        format!("{:#}", assembled(page, &[r#"<i></i><span id="nowhere" hx-swap-oob="true">x</span>"#]).expect_err("no target"));
    assert!(no_target.contains("hx-swap-oob #nowhere matches no element"), "{no_target}");
}

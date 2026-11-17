use super::{parse, serialize, Element, Node};

fn el(n: &Node) -> &Element {
    match n {
        Node::Element(e) => e,
        Node::Text(t) => panic!("expected an element, got text {t:?}"),
    }
}

fn elements(nodes: &[Node]) -> Vec<&Element> {
    nodes.iter().filter_map(|n| if let Node::Element(e) = n { Some(e) } else { None }).collect()
}

fn err(src: &str) -> String {
    format!("{:#}", parse(src).expect_err("must not parse"))
}

#[test]
fn void_elements_take_no_children_and_need_no_close() {
    let doc = parse("<div><br><img src=x><input type=checkbox checked><p>t</p></div>").expect("parses");
    let div = el(&doc[0]);
    let kids = elements(&div.children);
    assert_eq!(kids.iter().map(|e| e.tag.as_str()).collect::<Vec<_>>(), ["br", "img", "input", "p"]);
    assert!(kids[0].children.is_empty() && kids[1].children.is_empty() && kids[2].children.is_empty());
    assert_eq!(kids[2].attr("checked"), Some(""), "a bare attribute has an empty value");
    assert_eq!(kids[1].attr("src"), Some("x"), "an unquoted value");
}

#[test]
fn a_self_closed_element_does_not_open() {
    let doc = parse("<div class=\"a\"/><span></span>").expect("parses");
    assert_eq!(doc.len(), 2, "the span is the div's sibling, not its child");
    assert!(el(&doc[0]).children.is_empty());
    assert_eq!(el(&doc[1]).tag, "span");
}

#[test]
fn raw_text_elements_hold_markup_as_text() {
    let doc = parse("<script>if (a < b) { s = \"<div>\"; }</script><textarea><b>x</textarea><style>p>a{}</style>")
        .expect("an unbalanced <div> inside script is text, not an open tag");
    let script = el(&doc[0]);
    assert_eq!(script.children, [Node::Text("if (a < b) { s = \"<div>\"; }".to_string())]);
    assert_eq!(el(&doc[1]).children, [Node::Text("<b>x".to_string())]);
    assert_eq!(el(&doc[2]).children, [Node::Text("p>a{}".to_string())]);
    assert!(err("<script>never closed").contains("unterminated <script>"));
}

#[test]
fn a_stray_or_crossing_close_tag_is_an_error() {
    assert!(err("<p></p></x>").contains("stray </x>"), "{}", err("<p></p></x>"));
    let crossing = err("<a><b></a></b>");
    assert!(crossing.contains("</a>") && crossing.contains("closes <b>"), "{crossing}");
}

#[test]
fn an_unclosed_tag_is_an_error() {
    let e = err("<div><section><span></span>");
    assert!(e.contains("unclosed <div> <section>"), "{e}");
}

#[test]
fn comments_and_doctype_are_dropped() {
    let doc = parse("<!DOCTYPE html><!-- <b> --><i></i>").expect("parses");
    assert_eq!(doc.len(), 1);
    assert_eq!(el(&doc[0]).tag, "i");
    assert!(err("<!-- open").contains("unterminated comment"));
}

#[test]
fn attribute_values_are_entity_decoded() {
    let src = r#"<a href="&#x2f;admin&#x2F;x?a=1&amp;b=&lt;2&gt;&#47;" title='&quot;q&quot; &apos;&nbsp;' data-x="&bogus; & &#xzz;"></a>"#;
    let doc = parse(src).expect("parses");
    let a = el(&doc[0]);
    assert_eq!(a.attr("href"), Some("/admin/x?a=1&b=<2>/"));
    assert_eq!(a.attr("title"), Some("\"q\" ' "));
    assert_eq!(a.attr("data-x"), Some("&bogus; & &#xzz;"), "an unknown entity stays as written");
}

#[test]
fn own_text_leaves_out_nested_elements_and_text_takes_them() {
    let doc = parse("<span class=\"panel-title\">Players  <span class=\"badge\">3</span>\n online&amp;</span>")
        .expect("parses");
    let title = el(&doc[0]);
    assert_eq!(title.own_text(), "Players online&");
    assert_eq!(title.text(), "Players 3 online&");
    let with_script = parse("<div>a<script>x()</script><style>p{}</style>b</div>").expect("parses");
    assert_eq!(el(&with_script[0]).text(), "a b", "script and style text is not page text");
}

#[test]
fn class_queries_and_find_walk_in_document_order() {
    let doc = parse(r#"<div class=" a  b "><i id="1" class="x"></i><p><i id="2" class="x"></i></p></div>"#).expect("parses");
    let div = el(&doc[0]);
    assert_eq!(div.classes().collect::<Vec<_>>(), ["a", "b"]);
    assert!(div.has_class("b") && !div.has_class("a b"));
    assert_eq!(div.find(&|e| e.has_class("x")).and_then(|e| e.attr("id")), Some("1"));
    assert!(div.find(&|e| e.has_class("a")).is_none(), "find searches descendants, not the element itself");
    let mut all = Vec::new();
    super::find_all(&doc, &|e| e.tag == "i", &mut all);
    assert_eq!(all.iter().filter_map(|e| e.attr("id")).collect::<Vec<_>>(), ["1", "2"]);
}

#[test]
fn serialize_round_trips_through_parse() {
    let src = concat!(
        r#"<!-- dropped --><div class="a b" data-v="say &quot;hi&quot; &amp; go" hidden><br><img src="/x.png">"#,
        r#"<script>if (a < b) {}</script><p>t &amp; u<b>v</b></p><span/></div>"#,
    );
    let once = parse(src).expect("parses");
    let text = serialize(&once);
    assert!(text.contains(r#"data-v="say &quot;hi&quot; &amp; go""#), "{text}");
    assert!(text.contains("<br><img src=\"/x.png\"><script>"), "void elements have no close tag: {text}");
    assert!(text.contains("<span></span>"), "a self-closed element is written open and closed: {text}");
    assert_eq!(parse(&text).expect("the serialization parses"), once);
}

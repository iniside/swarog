//! htmx `outerHTML` assembly: each `.slot[hx-get]` is replaced by its fetched fragment, in
//! document order, so the portal side is the loaded page and never its placeholders.

use anyhow::{bail, Result};

use crate::html::{Element, Node};

/// A slot htmx fetches on load: `.slot` carrying `hx-get`.
pub fn is_pending_slot(e: &Element) -> bool {
    e.has_class("slot") && e.attr("hx-get").is_some()
}

/// The `hx-get` URL of every pending slot, in document order. A pending slot's own
/// placeholder children are not searched, since the swap discards them.
pub fn pending_slots(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    collect(nodes, &mut out);
    out
}

fn collect(nodes: &[Node], out: &mut Vec<String>) {
    for n in nodes {
        if let Node::Element(e) = n {
            match e.attr("hx-get") {
                Some(url) if e.has_class("slot") => out.push(url.to_string()),
                _ => collect(&e.children, out),
            }
        }
    }
}

/// Replaces the pending slots of `doc`, in [`pending_slots`] order, with `fragments`
/// (one parsed fragment per slot). A fragment's top-level elements carrying
/// `hx-swap-oob` replace the element with the same `id` instead, as htmx swaps them.
pub fn assemble(doc: Vec<Node>, fragments: Vec<Vec<Node>>) -> Result<Vec<Node>> {
    let expected = fragments.len();
    let mut inline = Vec::with_capacity(expected);
    let mut oob = Vec::new();
    for fragment in fragments {
        let (swap, rest): (Vec<Node>, Vec<Node>) = fragment
            .into_iter()
            .partition(|n| matches!(n, Node::Element(e) if e.attr("hx-swap-oob").is_some()));
        inline.push(rest);
        oob.extend(swap);
    }
    let mut queue = inline.into_iter();
    let mut doc = replace(doc, &mut queue)?;
    if queue.next().is_some() {
        bail!("{expected} fragment(s) for fewer pending slots");
    }
    for node in oob {
        let Node::Element(e) = node else { continue };
        let Some(id) = e.attr("id").map(str::to_string) else { bail!("an hx-swap-oob <{}> has no id", e.tag) };
        let mut target = Some(e);
        swap_by_id(&mut doc, &id, &mut target);
        if target.is_some() {
            bail!("hx-swap-oob #{id} matches no element in the page");
        }
    }
    Ok(doc)
}

fn replace(nodes: Vec<Node>, queue: &mut impl Iterator<Item = Vec<Node>>) -> Result<Vec<Node>> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            Node::Element(e) if is_pending_slot(&e) => {
                let Some(fragment) = queue.next() else { bail!("a pending slot has no fragment") };
                out.extend(fragment);
            }
            Node::Element(mut e) => {
                e.children = replace(std::mem::take(&mut e.children), queue)?;
                out.push(Node::Element(e));
            }
            text => out.push(text),
        }
    }
    Ok(out)
}

fn swap_by_id(nodes: &mut [Node], id: &str, with: &mut Option<Element>) {
    for node in nodes.iter_mut() {
        if with.is_none() {
            return;
        }
        if let Node::Element(e) = node {
            if e.attr("id") == Some(id) {
                *e = with.take().expect("checked above");
                return;
            }
            swap_by_id(&mut e.children, id, with);
        }
    }
}

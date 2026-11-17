//! The class skeleton of a page and the difference between two of them.
//!
//! A skeleton keeps element names, sorted class lists and nesting; text and the drawing
//! inside an `svg` only with `--text`. Identical siblings collapse to one, so N fake rows
//! against M real rows is not a difference. Panels and tiles are cut out of the page
//! frame and compared pairwise.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{bail, Result};

use crate::html::{find_in, Element, Node};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strip {
    /// The element is replaced by its children.
    Unwrap,
    /// The element and its subtree are dropped.
    Remove,
}

/// What the mockup adds for its own preview tooling, by class.
pub const MOCK_STRIP: &[(&str, Strip)] = &[
    ("frag", Strip::Unwrap),
    ("frag-label", Strip::Remove),
    ("devbar", Strip::Remove),
    ("annot-legend", Strip::Remove),
];

/// What the portal adds for htmx, by class. Attributes are never compared, so its `hx-*`
/// attributes need no entry.
pub const PORTAL_STRIP: &[(&str, Strip)] = &[("slot", Strip::Unwrap)];

/// The element both sides are compared from: the app shell. Popovers, the palette and the
/// modal root sit outside it; the mockup builds those in the DOM on interaction.
pub const ROOT_CLASS: &str = "app";

/// The `.app` element of `doc` with `strip` applied. Every strip entry must match on this
/// side, so a dead entry is an error.
pub fn prepare(doc: &[Node], strip: &[(&str, Strip)], side: &str) -> Result<Element> {
    let Some(root) = find_in(doc, &|e| e.has_class(ROOT_CLASS)) else { bail!("{side}: no .{ROOT_CLASS} element") };
    let mut dead = Vec::new();
    for (class, _) in strip {
        if root.find(&|e| e.has_class(class)).is_none() {
            dead.push(format!(".{class}"));
        }
    }
    if !dead.is_empty() {
        bail!("{side}: strip entries matching nothing on this page: {}", dead.join(", "));
    }
    let mut root = root.clone();
    root.children = apply(std::mem::take(&mut root.children), strip);
    Ok(root)
}

fn apply(nodes: Vec<Node>, strip: &[(&str, Strip)]) -> Vec<Node> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            Node::Element(mut e) => {
                let rule = strip.iter().find(|(class, _)| e.has_class(class)).map(|(_, s)| *s);
                let children = apply(std::mem::take(&mut e.children), strip);
                match rule {
                    Some(Strip::Remove) => {}
                    Some(Strip::Unwrap) => out.extend(children),
                    None => {
                        e.children = children;
                        out.push(Node::Element(e));
                    }
                }
            }
            text => out.push(text),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skel {
    /// `tag.class.class`, classes sorted and deduplicated; `#"text"` for a text leaf.
    pub head: String,
    /// Distinct children, first occurrence order.
    pub children: Vec<Skel>,
    /// The head and children in one string: equal canons are equal skeletons.
    pub canon: String,
}

impl Skel {
    fn new(head: String, children: Vec<Skel>) -> Skel {
        let mut seen = HashSet::new();
        let children: Vec<Skel> = children.into_iter().filter(|c| seen.insert(c.canon.clone())).collect();
        let canon = if children.is_empty() {
            head.clone()
        } else {
            format!("{head}{{{}}}", children.iter().map(|c| c.canon.as_str()).collect::<Vec<_>>().join(","))
        };
        Skel { head, children, canon }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitKind {
    Panel,
    Tile,
}

impl UnitKind {
    fn name(self) -> &'static str {
        match self {
            UnitKind::Panel => "panel",
            UnitKind::Tile => "tile",
        }
    }
}

/// A panel or tile cut out of the page.
#[derive(Clone, Debug)]
pub struct Unit {
    pub kind: UnitKind,
    /// Its title (`panel-title`) or label (`kpi-label`) own text, else `#<position>`.
    pub key: String,
    pub skel: Skel,
}

impl Unit {
    pub fn label(&self) -> String {
        format!("{} \"{}\"", self.kind.name(), self.key)
    }
}

/// A page as its frame (units replaced by `[panel]` / `[tile]`) and its units.
#[derive(Clone, Debug)]
pub struct Page {
    pub frame: Skel,
    pub units: Vec<Unit>,
}

pub fn head(e: &Element) -> String {
    let classes: BTreeSet<&str> = e.classes().collect();
    let mut head = e.tag.clone();
    for c in classes {
        head.push('.');
        head.push_str(c);
    }
    head
}

/// The skeleton of a prepared root. A unit is an outermost element of class `panel`: a
/// tile when it holds a `kpi-label`, else a panel.
pub fn page(root: &Element, text: bool) -> Page {
    let mut units = Vec::new();
    let mut positions = BTreeMap::new();
    let frame = build(root, text, Some((&mut units, &mut positions)));
    let mut seen: BTreeMap<(UnitKind, String), usize> = BTreeMap::new();
    for unit in &mut units {
        let n = seen.entry((unit.kind, unit.key.clone())).or_insert(0);
        *n += 1;
        if *n > 1 {
            unit.key = format!("{} ({})", unit.key, n);
        }
    }
    Page { frame, units }
}

type Cut<'a> = Option<(&'a mut Vec<Unit>, &'a mut BTreeMap<UnitKind, usize>)>;

fn build(e: &Element, text: bool, mut cut: Cut<'_>) -> Skel {
    if let Some((units, positions)) = cut.as_mut() {
        if e.has_class("panel") {
            let kind = if e.find(&|x| x.has_class("kpi-label")).is_some() { UnitKind::Tile } else { UnitKind::Panel };
            let position = positions.entry(kind).or_insert(0);
            *position += 1;
            let title_class = if kind == UnitKind::Tile { "kpi-label" } else { "panel-title" };
            let title = e.find(&|x| x.has_class(title_class)).map(|t| t.own_text()).unwrap_or_default();
            let title = title.trim_end_matches(|c: char| c == ':' || c.is_whitespace()).to_string();
            let key = if title.is_empty() { format!("#{position}") } else { title };
            units.push(Unit { kind, key, skel: build(e, text, None) });
            return Skel::new(format!("[{}]", kind.name()), Vec::new());
        }
    }
    let mut children = Vec::new();
    for child in &e.children {
        match child {
            Node::Element(_) if e.tag == "svg" && !text => {}
            Node::Element(c) => {
                let next = cut.as_mut().map(|(u, p)| (&mut **u, &mut **p));
                children.push(build(c, text, next));
            }
            Node::Text(t) if text && e.tag != "script" && e.tag != "style" => {
                let t = crate::html::collapse(&crate::html::decode(t));
                if !t.is_empty() {
                    children.push(Skel::new(format!("#\"{t}\""), Vec::new()));
                }
            }
            Node::Text(_) => {}
        }
    }
    Skel::new(head(e), children)
}

/// The differences of two pages, one line each. `matched` counts the panels found on both
/// sides.
pub struct Outcome {
    pub lines: BTreeSet<String>,
    pub matched_panels: usize,
    pub matched_tiles: usize,
}

pub fn diff_pages(mock: &Page, portal: &Page) -> Outcome {
    let mut lines = BTreeSet::new();
    diff_root("frame", &mock.frame, &portal.frame, &mut lines);
    let (mut matched_panels, mut matched_tiles) = (0, 0);
    let mut portal_left: Vec<&Unit> = portal.units.iter().collect();
    for m in &mock.units {
        match portal_left.iter().position(|p| p.kind == m.kind && p.key == m.key) {
            Some(at) => {
                let p = portal_left.remove(at);
                match m.kind {
                    UnitKind::Panel => matched_panels += 1,
                    UnitKind::Tile => matched_tiles += 1,
                }
                diff_root(&m.label(), &m.skel, &p.skel, &mut lines);
            }
            None => {
                lines.insert(format!("mock-only {}", m.label()));
            }
        }
    }
    for p in portal_left {
        lines.insert(format!("portal-only {}", p.label()));
    }
    Outcome { lines, matched_panels, matched_tiles }
}

fn diff_root(scope: &str, m: &Skel, p: &Skel, out: &mut BTreeSet<String>) {
    if m.head != p.head {
        out.insert(format!("{scope}: root mock {} portal {}", m.head, p.head));
    }
    diff(&format!("{scope} {}", m.head), m, p, out);
}

/// Children present on one side only are paired with the other side's kin leftovers
/// ([`kin`]), most similar first; a pair whose classes differ is reported, then compared
/// inside. Whatever stays unpaired is reported as mock-only or portal-only.
fn diff(path: &str, m: &Skel, p: &Skel, out: &mut BTreeSet<String>) {
    if m.canon == p.canon {
        return;
    }
    let p_canons: HashSet<&str> = p.children.iter().map(|c| c.canon.as_str()).collect();
    let m_canons: HashSet<&str> = m.children.iter().map(|c| c.canon.as_str()).collect();
    let m_left: Vec<&Skel> = m.children.iter().filter(|c| !p_canons.contains(c.canon.as_str())).collect();
    let p_left: Vec<&Skel> = p.children.iter().filter(|c| !m_canons.contains(c.canon.as_str())).collect();
    let mut candidates = Vec::new();
    for (mi, a) in m_left.iter().enumerate() {
        for (pi, b) in p_left.iter().enumerate() {
            if kin(a, b) {
                candidates.push((similarity(a, b), mi, pi));
            }
        }
    }
    candidates.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    let (mut m_used, mut p_used) = (vec![false; m_left.len()], vec![false; p_left.len()]);
    for (_, mi, pi) in candidates {
        if m_used[mi] || p_used[pi] {
            continue;
        }
        m_used[mi] = true;
        p_used[pi] = true;
        pair(path, m_left[mi], p_left[pi], out);
    }
    for (a, used) in m_left.iter().zip(&m_used) {
        if !used {
            out.insert(format!("{path}: mock-only {}", a.head));
        }
    }
    for (b, used) in p_left.iter().zip(&p_used) {
        if !used {
            out.insert(format!("{path}: portal-only {}", b.head));
        }
    }
}

fn pair(path: &str, a: &Skel, b: &Skel, out: &mut BTreeSet<String>) {
    if a.head != b.head {
        out.insert(format!("{path}: mock {} portal {}", a.head, b.head));
    }
    diff(&format!("{path} > {}", a.head), a, b, out);
}

fn tag(head: &str) -> &str {
    if head.starts_with('#') {
        return "#text";
    }
    head.split('.').next().unwrap_or(head)
}

fn classes(head: &str) -> HashSet<&str> {
    if head.starts_with('#') {
        return HashSet::new();
    }
    head.split('.').skip(1).collect()
}

/// Two variants of one element: the same head; or the same tag, a shared class unless one
/// side has none, and a shared child tag unless both are leaves.
fn kin(a: &Skel, b: &Skel) -> bool {
    if a.head == b.head {
        return true;
    }
    if tag(&a.head) != tag(&b.head) {
        return false;
    }
    let (ca, cb) = (classes(&a.head), classes(&b.head));
    if !ca.is_empty() && !cb.is_empty() && ca.is_disjoint(&cb) {
        return false;
    }
    if a.children.is_empty() && b.children.is_empty() {
        return true;
    }
    let tags: HashSet<&str> = a.children.iter().map(|c| tag(&c.head)).collect();
    b.children.iter().any(|c| tags.contains(tag(&c.head)))
}

/// Shared classes weigh three, shared distinct children two, shared child heads one.
fn similarity(a: &Skel, b: &Skel) -> usize {
    let canons: HashSet<&str> = a.children.iter().map(|c| c.canon.as_str()).collect();
    let heads: HashSet<&str> = a.children.iter().map(|c| c.head.as_str()).collect();
    let b_heads: HashSet<&str> = b.children.iter().map(|c| c.head.as_str()).collect();
    let shared = b.children.iter().filter(|c| canons.contains(c.canon.as_str())).count();
    3 * classes(&a.head).intersection(&classes(&b.head)).count() + 2 * shared + heads.intersection(&b_heads).count()
}

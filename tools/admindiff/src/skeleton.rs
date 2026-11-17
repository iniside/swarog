//! The class skeleton of a page and the difference between two of them.
//!
//! A skeleton keeps element names, sorted class lists and nesting, in order; text and the
//! drawing inside an `svg` only with `--text`. The items of a [`REPEAT`] container
//! collapse to their distinct shapes, so N fake rows against M real rows is not a
//! difference; every other sibling list compares by position. Panels and tiles are cut
//! out of the page frame, which keeps a keyed placeholder where each sat, and are
//! compared pairwise.

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

/// An element selector of the [`REPEAT`] list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Any,
    Tag(&'static str),
    Class(&'static str),
}

impl Sel {
    fn matches(self, e: &Element) -> bool {
        match self {
            Sel::Any => true,
            Sel::Tag(t) => e.tag == t,
            Sel::Class(c) => e.has_class(c),
        }
    }

    fn label(self) -> String {
        match self {
            Sel::Any => "*".to_string(),
            Sel::Tag(t) => t.to_string(),
            Sel::Class(c) => format!(".{c}"),
        }
    }
}

/// How a repeated item counts as a shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Its whole skeleton.
    Full,
    /// Its tag alone: every item is the same shape.
    Tag,
}

/// A container whose `item` children repeat with the data.
#[derive(Clone, Copy, Debug)]
pub struct Repeat {
    pub container: Sel,
    pub item: Sel,
    pub shape: Shape,
    pub why: &'static str,
}

/// The repeat containers, the only sibling lists that collapse. Each entry must match in
/// the mockup's pages ([`validate_repeats`]).
pub const REPEAT: &[Repeat] = &[
    Repeat { container: Sel::Tag("tbody"), item: Sel::Tag("tr"), shape: Shape::Full, why: "table rows" },
    Repeat { container: Sel::Tag("ul"), item: Sel::Tag("li"), shape: Shape::Full, why: "list and timeline items" },
    Repeat { container: Sel::Any, item: Sel::Class("svc"), shape: Shape::Full, why: "service rows" },
    Repeat { container: Sel::Any, item: Sel::Class("pop-row"), shape: Shape::Full, why: "attention rows" },
    Repeat {
        container: Sel::Class("uptime"),
        item: Sel::Tag("i"),
        shape: Shape::Tag,
        why: "uptime bars: a bar's class is its sample's tone, which is data",
    },
];

fn repeat_of(parent: &Element, child: &Element) -> Option<usize> {
    REPEAT.iter().position(|r| r.container.matches(parent) && r.item.matches(child))
}

/// Fails when a [`REPEAT`] entry matches no container item in any of `roots`.
pub fn validate_repeats(roots: &[Element]) -> Result<()> {
    let dead: Vec<String> = REPEAT
        .iter()
        .filter(|r| !roots.iter().any(|root| has_repeat(root, r)))
        .map(|r| format!("{} > {} ({})", r.container.label(), r.item.label(), r.why))
        .collect();
    if !dead.is_empty() {
        bail!("REPEAT entries matching nothing in the mockup: {}", dead.join(", "));
    }
    Ok(())
}

fn has_repeat(e: &Element, r: &Repeat) -> bool {
    let here = r.container.matches(e) && e.children.iter().any(|c| matches!(c, Node::Element(c) if r.item.matches(c)));
    here || e.children.iter().any(|c| matches!(c, Node::Element(c) if has_repeat(c, r)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skel {
    /// `tag.class.class`, classes sorted and deduplicated; `#"text"` for a text leaf;
    /// `[panel "key"]` / `[tile "key"]` for a cut-out unit; `<item>*` for a repeat group.
    pub head: String,
    /// In document order; a repeat group's are its distinct shapes, sorted.
    pub children: Vec<Skel>,
    /// The head and children in one string: equal canons are equal skeletons.
    pub canon: String,
}

impl Skel {
    fn new(head: String, children: Vec<Skel>) -> Skel {
        let canon = if children.is_empty() {
            head.clone()
        } else {
            format!("{head}{{{}}}", children.iter().map(|c| c.canon.as_str()).collect::<Vec<_>>().join(","))
        };
        Skel { head, children, canon }
    }

    fn group(item: Sel, shapes: Vec<Skel>) -> Skel {
        let mut by_canon: BTreeMap<String, Skel> = BTreeMap::new();
        for s in shapes {
            by_canon.entry(s.canon.clone()).or_insert(s);
        }
        Skel::new(format!("{}*", item.label()), by_canon.into_values().collect())
    }

    fn is_group(&self) -> bool {
        self.head.ends_with('*')
    }

    /// The head, plus a short hash of the canon when it has children, so two shapes with one
    /// head are two labels.
    pub fn label(&self) -> String {
        if self.children.is_empty() {
            return self.head.clone();
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.canon.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{}#{:06x}", self.head, hash & 0xff_ffff)
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
    /// Its title (`panel-title`) or label (`kpi-label`) own text, else `#<position>`; a
    /// repeated key gets ` (2)`, ` (3)`, and so on.
    pub key: String,
    pub skel: Skel,
}

impl Unit {
    pub fn label(&self) -> String {
        format!("{} \"{}\"", self.kind.name(), self.key)
    }
}

/// A page as its frame (each unit replaced by its keyed placeholder) and its units.
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

#[derive(Default)]
struct Cutter {
    units: Vec<Unit>,
    positions: BTreeMap<UnitKind, usize>,
    seen: BTreeMap<(UnitKind, String), usize>,
}

impl Cutter {
    /// Cuts `e` out as a unit and returns its placeholder.
    fn cut(&mut self, e: &Element, text: bool) -> Skel {
        let kind = if e.find(&|x| x.has_class("kpi-label")).is_some() { UnitKind::Tile } else { UnitKind::Panel };
        let position = self.positions.entry(kind).or_insert(0);
        *position += 1;
        let title_class = if kind == UnitKind::Tile { "kpi-label" } else { "panel-title" };
        let title = e.find(&|x| x.has_class(title_class)).map(|t| t.own_text()).unwrap_or_default();
        let title = title.trim_end_matches(|c: char| c == ':' || c.is_whitespace()).to_string();
        let mut key = if title.is_empty() { format!("#{position}") } else { title };
        let n = self.seen.entry((kind, key.clone())).or_insert(0);
        *n += 1;
        if *n > 1 {
            key = format!("{key} ({n})");
        }
        let unit = Unit { kind, key, skel: build(e, text, None) };
        let placeholder = Skel::new(format!("[{}]", unit.label()), Vec::new());
        self.units.push(unit);
        placeholder
    }
}

/// The skeleton of a prepared root. A unit is an outermost element of class `panel`: a
/// tile when it holds a `kpi-label`, else a panel.
pub fn page(root: &Element, text: bool) -> Page {
    let mut cutter = Cutter::default();
    let frame = build(root, text, Some(&mut cutter));
    Page { frame, units: cutter.units }
}

fn build(e: &Element, text: bool, mut cut: Option<&mut Cutter>) -> Skel {
    if let Some(cutter) = cut.as_deref_mut() {
        if e.has_class("panel") {
            return cutter.cut(e, text);
        }
    }
    let mut children = Vec::new();
    // Repeat entry -> (the group's slot in `children`, its items' shapes).
    let mut groups: BTreeMap<usize, (usize, Vec<Skel>)> = BTreeMap::new();
    for child in &e.children {
        match child {
            Node::Element(_) if e.tag == "svg" && !text => {}
            Node::Element(c) => match repeat_of(e, c) {
                Some(r) => {
                    let shape = match REPEAT[r].shape {
                        Shape::Full => build(c, text, cut.as_deref_mut()),
                        Shape::Tag => Skel::new(c.tag.clone(), Vec::new()),
                    };
                    let slot = match groups.get(&r) {
                        Some((slot, _)) => *slot,
                        None => {
                            children.push(Skel::new(String::new(), Vec::new()));
                            children.len() - 1
                        }
                    };
                    groups.entry(r).or_insert_with(|| (slot, Vec::new())).1.push(shape);
                }
                None => children.push(build(c, text, cut.as_deref_mut())),
            },
            Node::Text(t) if text && e.tag != "script" && e.tag != "style" => {
                let t = crate::html::collapse(&crate::html::decode(t));
                if !t.is_empty() {
                    children.push(Skel::new(format!("#\"{t}\""), Vec::new()));
                }
            }
            Node::Text(_) => {}
        }
    }
    for (r, (slot, shapes)) in groups {
        children[slot] = Skel::group(REPEAT[r].item, shapes);
    }
    Skel::new(head(e), children)
}

/// The differences of two pages, one line each, and how many units matched.
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

fn diff(path: &str, m: &Skel, p: &Skel, out: &mut BTreeSet<String>) {
    if m.canon == p.canon {
        return;
    }
    if m.is_group() && p.is_group() {
        let p_canons: HashSet<&str> = p.children.iter().map(|c| c.canon.as_str()).collect();
        let m_canons: HashSet<&str> = m.children.iter().map(|c| c.canon.as_str()).collect();
        let m_left: Vec<&Skel> = m.children.iter().filter(|c| !p_canons.contains(c.canon.as_str())).collect();
        let p_left: Vec<&Skel> = p.children.iter().filter(|c| !m_canons.contains(c.canon.as_str())).collect();
        let (m_un, p_un) = pair_kin(path, &m_left, &p_left, out);
        report_unpaired(path, &m_un, &p_un, out);
    } else {
        sequence(path, m, p, out);
    }
}

/// Positional comparison: the longest common subsequence of canons anchors the two lists,
/// and in each gap between anchors kin children pair up. The same children in another
/// order is one `order` line; a child left unpaired on both sides is `moved`.
fn sequence(path: &str, m: &Skel, p: &Skel, out: &mut BTreeSet<String>) {
    let (a, b) = (&m.children, &p.children);
    if a.iter().map(|c| &c.canon).eq(b.iter().map(|c| &c.canon)) {
        return;
    }
    let mut sorted_a: Vec<&str> = a.iter().map(|c| c.canon.as_str()).collect();
    let mut sorted_b: Vec<&str> = b.iter().map(|c| c.canon.as_str()).collect();
    sorted_a.sort_unstable();
    sorted_b.sort_unstable();
    if sorted_a == sorted_b {
        let labels = |xs: &[Skel]| xs.iter().map(Skel::label).collect::<Vec<_>>().join(", ");
        out.insert(format!("{path}: order mock [{}] portal [{}]", labels(a), labels(b)));
        return;
    }
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i].canon == b[j].canon { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut m_un, mut p_un): (Vec<&Skel>, Vec<&Skel>) = (Vec::new(), Vec::new());
    let (mut gap_a, mut gap_b): (Vec<&Skel>, Vec<&Skel>) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i].canon == b[j].canon {
            let (ma, pb) = pair_kin(path, &gap_a, &gap_b, out);
            m_un.extend(ma);
            p_un.extend(pb);
            gap_a.clear();
            gap_b.clear();
            i += 1;
            j += 1;
        } else if j >= b.len() || (i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1]) {
            gap_a.push(&a[i]);
            i += 1;
        } else {
            gap_b.push(&b[j]);
            j += 1;
        }
    }
    let (ma, pb) = pair_kin(path, &gap_a, &gap_b, out);
    m_un.extend(ma);
    p_un.extend(pb);
    let mut p_rest: Vec<&Skel> = p_un;
    let mut m_rest = Vec::new();
    for x in m_un {
        match p_rest.iter().position(|y| y.canon == x.canon) {
            Some(at) => {
                p_rest.remove(at);
                out.insert(format!("{path}: moved {}", x.label()));
            }
            None => m_rest.push(x),
        }
    }
    report_unpaired(path, &m_rest, &p_rest, out);
}

/// Pairs kin children of `m` and `p`, most similar first, and compares each pair; returns
/// the unpaired ones.
fn pair_kin<'a>(path: &str, m: &[&'a Skel], p: &[&'a Skel], out: &mut BTreeSet<String>) -> (Vec<&'a Skel>, Vec<&'a Skel>) {
    let mut candidates = Vec::new();
    for (mi, a) in m.iter().enumerate() {
        for (pi, b) in p.iter().enumerate() {
            if kin(a, b) {
                candidates.push((similarity(a, b), mi, pi));
            }
        }
    }
    candidates.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
    let (mut m_used, mut p_used) = (vec![false; m.len()], vec![false; p.len()]);
    for (_, mi, pi) in candidates {
        if m_used[mi] || p_used[pi] {
            continue;
        }
        m_used[mi] = true;
        p_used[pi] = true;
        let (a, b) = (m[mi], p[pi]);
        if a.head != b.head {
            out.insert(format!("{path}: mock {} portal {}", a.head, b.head));
        }
        diff(&format!("{path} > {}", a.head), a, b, out);
    }
    let m_un = m.iter().zip(&m_used).filter(|(_, used)| !**used).map(|(s, _)| *s).collect();
    let p_un = p.iter().zip(&p_used).filter(|(_, used)| !**used).map(|(s, _)| *s).collect();
    (m_un, p_un)
}

fn report_unpaired(path: &str, m: &[&Skel], p: &[&Skel], out: &mut BTreeSet<String>) {
    for a in m {
        out.insert(format!("{path}: mock-only {}", a.label()));
    }
    for b in p {
        out.insert(format!("{path}: portal-only {}", b.label()));
    }
}

fn tag(head: &str) -> &str {
    if head.starts_with('#') {
        return "#text";
    }
    head.split('.').next().unwrap_or(head)
}

fn classes(head: &str) -> HashSet<&str> {
    if head.starts_with('#') || head.starts_with('[') {
        return HashSet::new();
    }
    head.split('.').skip(1).collect()
}

/// Two variants of one element: the same head; or the same tag, a shared class unless one
/// side has none, and a shared child tag unless both are leaves. A unit placeholder is kin
/// only to an equal one.
fn kin(a: &Skel, b: &Skel) -> bool {
    if a.head == b.head {
        return true;
    }
    if a.head.starts_with('[') || b.head.starts_with('[') || tag(&a.head) != tag(&b.head) {
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

/// Shared classes weigh three, shared children two, shared child heads one.
fn similarity(a: &Skel, b: &Skel) -> usize {
    let canons: HashSet<&str> = a.children.iter().map(|c| c.canon.as_str()).collect();
    let heads: HashSet<&str> = a.children.iter().map(|c| c.head.as_str()).collect();
    let b_heads: HashSet<&str> = b.children.iter().map(|c| c.head.as_str()).collect();
    let shared = b.children.iter().filter(|c| canons.contains(c.canon.as_str())).count();
    3 * classes(&a.head).intersection(&classes(&b.head)).count() + 2 * shared + heads.intersection(&b_heads).count()
}

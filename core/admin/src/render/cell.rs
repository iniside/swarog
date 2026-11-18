use controlplane::{CellKind, EntityRef, Tone};
use serde::Serialize;
use super::*;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CellView {
    pub(crate) kind: &'static str,
    pub(crate) value: String,
    pub(crate) tone: &'static str,
    pub(crate) href: Option<String>,
    pub(crate) color: &'static str,
    pub(crate) initials: String,
    /// The bar's fill percentage, at most 100.
    pub(crate) bar: Option<u8>,
    /// The bar fill's `lag-bar` class: `""`, `w` or `e`.
    pub(crate) bar_tone: &'static str,
    /// `Cell::more`, flattened to one level; each has an empty `more`.
    pub(crate) more: Vec<CellView>,
    /// A user cell in a table row that opens an entity: its link sits on the name inside
    /// the cell, not around it.
    pub(crate) inner_link: bool,
    /// `Cell::row`: this value and its `more` sit in one `span.row`.
    pub(crate) row: bool,
}

/// With a bar, the cell's tone colours the fill only: `Warn` is `w`, `Err` is `e`, every
/// other tone the default fill.
pub(super) fn bar_tone(t: Option<Tone>) -> &'static str {
    if let Some(t) = &t {
        seen::record(t);
    }
    match t {
        Some(Tone::Warn) => "w",
        Some(Tone::Err) => "e",
        Some(Tone::Ok | Tone::Info | Tone::Accent) | None => "",
    }
}

pub(crate) fn cell(c: &controlplane::Cell) -> CellView {
    let mut more = Vec::new();
    let mut pending: Vec<&controlplane::Cell> = c.more.iter().rev().collect();
    while let Some(next) = pending.pop() {
        more.push(one_cell(next));
        pending.extend(next.more.iter().rev());
    }
    CellView { more, ..one_cell(c) }
}

/// `c` without its `more`.
fn one_cell(c: &controlplane::Cell) -> CellView {
    seen::record(&c.kind);
    let kind = match c.kind {
        CellKind::Text => "text",
        CellKind::Mono => "mono",
        CellKind::Badge => "badge",
        CellKind::Ref => "ref",
        CellKind::Number => "number",
        CellKind::Bytes => "bytes",
        CellKind::Time => "time",
        CellKind::User => "user",
        CellKind::Muted => "muted",
        CellKind::Label => "label",
        CellKind::MonoMuted => "mono_muted",
        CellKind::Toggle => "toggle",
        CellKind::Strong => "strong",
    };
    let tone = match (c.bar, c.tone, c.kind) {
        (Some(_), _, _) => "",
        (None, Some(t), _) => tone(t),
        (None, None, CellKind::Badge) => status_tone(&c.value),
        (None, None, _) => "",
    };
    CellView {
        kind,
        value: c.value.clone(),
        tone,
        href: c.link.as_ref().map(link_url),
        color: avatar_color(&c.value),
        initials: initials(&c.value),
        bar: c.bar.map(|pct| pct.min(100)),
        bar_tone: c.bar.map_or("", |_| bar_tone(c.tone)),
        more: Vec::new(),
        inner_link: false,
        row: c.row,
    }
}

pub(super) fn ref_cell(e: &EntityRef) -> CellView {
    let user = matches!(e.kind.as_str(), "player" | "user");
    CellView {
        kind: if user { "user" } else { "ref" },
        value: if user { e.id.clone() } else { entity_param(e) },
        tone: "",
        href: Some(entity_url(e)),
        color: avatar_color(&e.id),
        initials: initials(&e.id),
        bar: None,
        bar_tone: "",
        more: Vec::new(),
        inner_link: false,
        row: false,
    }
}

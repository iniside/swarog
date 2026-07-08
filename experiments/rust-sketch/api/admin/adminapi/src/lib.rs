//! `adminapi` — the contract between the admin portal and the modules that appear
//! in it (a port of Go's `api/admin/adminapi`). A module contributes an [`Item`]
//! into the core [`SLOT`] slot; the admin portal renders a navigable sidebar
//! grouping items by [`Item::section`], each opening a dedicated content page. The
//! admin never imports a module's implementation, and modules never import the
//! admin — both depend only on THIS contract (like the `<module>events` crates).
//!
//! This crate is types-only: no module, no impl. Later modules
//! `ctx.contribute(adminapi::SLOT, Item { .. })`. In Milestone 1 no admin PORTAL
//! renders these (that is M2), but the contributions must compile and the contrib
//! seam must work.
//!
//! # The Go `context.Context` → Rust translation
//! Go's `Render`/`Submit` take a `context.Context` that carries the request's
//! flattened query parameters (`adminapi.Params(ctx)["owner"]`, set by
//! `WithParams`). Rust has no ambient context, so the params travel as an explicit
//! [`Params`] argument. [`param`] mirrors Go's map-index-returns-"" on a miss.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

/// The core contribution slot the admin portal reads (Go's `adminapi.Slot`).
pub const SLOT: &str = "admin.item";

/// A request's flattened query parameters (first value per key), handed to a LOCAL
/// item's [`Item::render`]. The Rust stand-in for what Go carries on `context.Context`
/// via `WithParams`/`Params`, so a render can switch on a drill-down parameter (e.g.
/// `?owner=character:123`) without a signature change.
pub type Params = HashMap<String, String>;

/// The value of `params[key]`, or `""` when absent — mirroring Go's `Params(ctx)[key]`
/// map index, so a drill-down render can index safely without an `Option` dance.
pub fn param<'a>(params: &'a Params, key: &str) -> &'a str {
    params.get(key).map(String::as_str).unwrap_or("")
}

/// A LOCAL item's in-process render: reads a data snapshot and returns declarative
/// widgets. Synchronous — reading a cache/snapshot needs no I/O (Go's `Render` is
/// likewise pure over an already-loaded snapshot).
pub type RenderFn = Arc<dyn Fn(&Params) -> anyhow::Result<Content> + Send + Sync>;

/// A REMOTE item's fetch: hops the edge transport to pull a peer's [`ItemData`].
/// Async (a network round-trip) and owns its [`Params`] so the returned future is
/// `'static`. Unused in Milestone 1 (no remote admin yet) but part of the contract.
pub type RemoteFetchFn =
    Arc<dyn Fn(Params) -> BoxFuture<'static, Result<ItemData, ItemError>> + Send + Sync>;

/// A LOCAL form's submit: applies a posted edit. Async because it does DB I/O
/// (Go's `Submit` is a sync signature over `database/sql`; in async Rust it is a
/// future). Owns its [`Params`] so the returned future is `'static`.
pub type SubmitFn = Arc<dyn Fn(Params) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Errors a [`RemoteFetchFn`] can surface. [`ItemError::Absent`] is Go's
/// `ErrItemAbsent`: the peer has no admin surface, so the portal drops the item
/// silently instead of showing an error card.
#[derive(Debug, thiserror::Error)]
pub enum ItemError {
    #[error("adminapi: remote item has no admin surface")]
    Absent,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// One clickable entry in the admin sidebar, contributed by a module. The admin
/// groups items by [`Item::section`] into the menu; opening an item renders its
/// [`Content`] into the content area.
///
/// An item is either LOCAL (`render` set, called in-process) or REMOTE
/// (`remote_fetch` set, hops the edge to fetch [`ItemData`]; `section`/`label`/
/// `render` left empty and learned from the fetch). A closure field can't be
/// cloned by value cheaply — both are `Arc`, so [`Item`] is `Clone` for the
/// contribution slot.
#[derive(Clone)]
pub struct Item {
    /// Stable id, e.g. `"config"`; the remote-match key.
    pub id: String,
    /// Sidebar group label, e.g. `"Platform"`. First item creates it; rest append.
    pub section: String,
    /// The clickable menu entry + page title, e.g. `"Game Config & Flags"`.
    pub label: String,
    /// LOCAL: in-process render; `None` for a remote stub item.
    pub render: Option<RenderFn>,
    /// REMOTE: fetches [`ItemData`] over the edge; `None` for local items.
    /// [`ItemError::Absent`] ⇒ the portal skips the item.
    pub remote_fetch: Option<RemoteFetchFn>,
}

impl Item {
    /// Builds a LOCAL item (the only kind Milestone 1 modules contribute).
    pub fn local(
        id: impl Into<String>,
        section: impl Into<String>,
        label: impl Into<String>,
        render: RenderFn,
    ) -> Item {
        Item {
            id: id.into(),
            section: section.into(),
            label: label.into(),
            render: Some(render),
            remote_fetch: None,
        }
    }
}

/// The wire form a module's admin edge operation returns: a remote admin process
/// fetches it (one round-trip) to learn a remote item's `section`/`label` AND its
/// `content`, so the sidebar and page render from one fetch.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ItemData {
    pub id: String,
    pub section: String,
    pub label: String,
    pub content: Content,
}

/// What a section renders into: an optional KPI row, an optional table, and an
/// optional editable form. The admin owns the look; the module only declares data.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Content {
    #[serde(default)]
    pub kpis: Vec<Kpi>,
    #[serde(default)]
    pub table: Option<Table>,
    /// Optional editable form; `None` = read-only.
    #[serde(default)]
    pub form: Option<Form>,
}

/// An editable widget a LOCAL item can attach to its [`Content`]. The admin renders
/// `fields` as text inputs and, on POST, invokes `submit` in-process with the
/// posted values. Local-only: a remote item's form arrives with `submit == None`
/// (a closure can't marshal), so remote forms render read-only.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Form {
    /// Page slug this posts back to; the admin fills it in when rendering.
    #[serde(default)]
    pub action: String,
    /// Inputs to render, in order.
    #[serde(default)]
    pub fields: Vec<Field>,
    /// LOCAL-only: applies the edit; `None` across the remote wire.
    #[serde(skip)]
    pub submit: Option<SubmitFn>,
}

/// One input in a [`Form`]: a labelled text box pre-filled with `value`, whose
/// `name` is both the HTML input name and the key in the submit values map.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub value: String,
}

/// One headline stat. `sub` is an optional small subtitle, e.g. `"linked"`.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Kpi {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub sub: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

/// One table value. `badge` (one of `"green"`,`"amber"`,`"red"`,`"blue"`,`"grey"`)
/// renders a status pill; `mono` renders monospaced (IDs); otherwise plain text.
///
/// `link`, when set, makes the admin render the text as a drill-down anchor to
/// `/admin/<link>` (a page slug plus an optional query string, e.g.
/// `"inventory?owner=character:123"`). Module-authored, never client input.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Cell {
    pub text: String,
    #[serde(default)]
    pub badge: String,
    #[serde(default)]
    pub mono: bool,
    /// Optional drill-down target: admin renders `text` as `<a href="/admin/{link}">`.
    #[serde(default)]
    pub link: String,
}

impl Cell {
    /// A plain text cell.
    pub fn text(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            ..Cell::default()
        }
    }

    /// A monospaced cell (IDs).
    pub fn mono(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            mono: true,
            ..Cell::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_missing_is_empty() {
        let mut p = Params::new();
        p.insert("owner".into(), "character:123".into());
        assert_eq!(param(&p, "owner"), "character:123");
        assert_eq!(param(&p, "absent"), "");
    }

    #[test]
    fn content_serde_roundtrips_without_the_form_submit() {
        // ItemData (the wire form) round-trips; Form::submit is skipped, so a
        // deserialized form is read-only (submit == None) — the remote-item shape.
        let data = ItemData {
            id: "config".into(),
            section: "Platform".into(),
            label: "Game Config & Flags".into(),
            content: Content {
                kpis: vec![Kpi {
                    label: "Settings".into(),
                    value: "3".into(),
                    sub: String::new(),
                }],
                table: Some(Table {
                    columns: vec!["Namespace".into(), "Key".into(), "Value".into()],
                    rows: vec![vec![Cell::mono("game"), Cell::mono("name"), Cell::text("arena")]],
                }),
                form: Some(Form {
                    action: String::new(),
                    fields: vec![Field {
                        name: "game:name".into(),
                        label: "game / name".into(),
                        value: "arena".into(),
                    }],
                    submit: None,
                }),
            },
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: ItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.section, "Platform");
        assert_eq!(back.content.table.unwrap().rows[0][0].text, "game");
        assert!(back.content.form.unwrap().submit.is_none());
    }
}

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

use async_trait::async_trait;
use futures::future::BoxFuture;
use opsapi::Error;
use rpc_macro::rpc;
use serde::{Deserialize, Serialize};

/// The core contribution slot the admin portal reads (Go's `adminapi.Slot`).
pub const SLOT: contrib::Slot<Item> = contrib::Slot::new("admin.item");

/// The admin fan-out capability EVERY provider module implements: returns the
/// module's admin page ([`ItemData`]) so a REMOTE admin process can pull it over the
/// QUIC edge in one round-trip (Go's per-provider `adminData` op). WIRE-ONLY — no
/// `#[http]` (it rides the internal mTLS edge like `characters.ownerOf`) and no
/// caller identity (admin is process-authenticated, not player-scoped).
///
/// This is a CROSS-CUTTING contract (like [`Item`]/[`ItemData`]), not one domain's
/// capability, so the single `#[rpc]` trait lives HERE and every provider implements
/// it. The wire method is `admin.adminData`; because each `<name>-svc` serves it on
/// its OWN edge server, one method name per process is unambiguous — the admin's stub
/// dials the specific provider's edge. The generated transport glue (`Client`,
/// `register_server`) lives in the sibling `adminrpc` crate (which expands this
/// crate's `admin_admin_data_meta!` callback) — so THIS crate never depends on `edge`.
#[rpc(prefix = "admin")]
#[async_trait]
pub trait AdminData: Send + Sync {
    #[retry_safe]
    async fn admin_data(&self) -> Result<ItemData, Error>;
}

/// The OPT-IN write companion to [`AdminData`]: a provider that supports REMOTE admin
/// WRITES implements this so a remote admin process can POST a form edit over the same
/// QUIC edge (the wire method is `admin.adminSubmit`). WIRE-ONLY (no `#[http]`) and
/// process-authenticated exactly like [`AdminData`] — the operator's session + CSRF are
/// enforced IN the admin process BEFORE this edge call; the edge itself carries no
/// player identity.
///
/// The single method is a MUTATION, so it is deliberately NOT `#[retry_safe]`: it
/// defaults to `opsapi::RetryMode::Never` (fail-closed), so a lost response is never
/// silently replayed. `id` names the provider's page (the admin slug); `params` is the
/// flattened posted form — the SAME [`Params`] a LOCAL [`SubmitFn`] receives — and the
/// success value is a [`SubmitOutcome`] (carrying any show-once `reveal`).
///
/// A provider that does NOT implement this simply never registers the wire method, so
/// the edge answers `edge::Error::UnknownMethod` → [`opsapi::Status::NotFound`] and the
/// remote admin degrades that item to read-only — the existing graceful-absent
/// behaviour, no bespoke signalling.
///
/// Like [`AdminData`], the generated transport glue (`Client`, `register_server`) lives
/// in the sibling `adminrpc` crate (which expands this crate's `admin_admin_submit_meta!`
/// callback) — so THIS crate never depends on `edge`.
#[rpc(prefix = "admin")]
#[async_trait]
pub trait AdminSubmit: Send + Sync {
    async fn admin_submit(&self, id: String, params: Params) -> Result<SubmitOutcome, Error>;
}

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

/// A REMOTE item's admin WRITE: hops the edge to POST a form edit to the peer's
/// `admin.adminSubmit` — the write mirror of [`RemoteFetchFn`], carried PER-PROVIDER on
/// the same [`Item`] (never a single shared registry key, which would panic on the 2nd
/// provider and misroute in a split). Async (a network round-trip) and owns its
/// [`Params`] so the returned future is `'static`.
///
/// It surfaces the RAW [`opsapi::Error`], NOT [`SubmitError`], deliberately: the admin
/// process maps `Error::status` onto an HTTP status, and only the raw error can express
/// the two remote-specific outcomes [`SubmitError`] cannot — a provider that never
/// registered the wire method ([`opsapi::Status::NotFound`], via the edge's
/// `UnknownMethod` mapping) degrades the item to read-only (405), and a CAS miss
/// ([`opsapi::Status::Conflict`]) becomes a 409.
pub type RemoteSubmitFn =
    Arc<dyn Fn(Params) -> BoxFuture<'static, Result<SubmitOutcome, Error>> + Send + Sync>;

/// A LOCAL form's submit: applies a posted edit. Async because it does DB I/O
/// (Go's `Submit` is a sync signature over `database/sql`; in async Rust it is a
/// future). Owns its [`Params`] so the returned future is `'static`.
pub type SubmitFn =
    Arc<dyn Fn(Params) -> BoxFuture<'static, Result<SubmitOutcome, SubmitError>> + Send + Sync>;

/// Errors a local [`SubmitFn`] can surface. [`SubmitError::Conflict`] means the
/// form's posted expected-state evidence no longer matches the authoritative store;
/// [`SubmitError::Other`] preserves the ordinary validation/infrastructure failure
/// path.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("the submitted form is stale")]
    Conflict,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The successful result of a [`SubmitFn`] (or a remote [`AdminSubmit::admin_submit`]).
/// Ordinarily EMPTY (`SubmitOutcome::default()`); a form that MINTS a one-time secret
/// (e.g. a freshly generated API key) returns it in `reveal` so the admin can show it
/// EXACTLY ONCE after the POST — the value is never re-derivable from a later read. An
/// empty `reveal` means "nothing to show; redirect as before".
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SubmitOutcome {
    #[serde(default)]
    pub reveal: Vec<RevealItem>,
}

/// One show-once value surfaced by a [`SubmitOutcome`]: an operator-facing `label` and
/// the `value` to display exactly once (never persisted for re-display).
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RevealItem {
    pub label: String,
    pub value: String,
}

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
    /// REMOTE WRITE: POSTs a form edit over the edge to the peer's `admin.adminSubmit`.
    /// `None` for a LOCAL item (it uses `render`'s [`Form::submit`] instead) and for a
    /// remote peer that serves no write surface. Set ALONGSIDE [`Item::remote_fetch`]
    /// by the same per-provider factory, so one provider ⇒ one [`Item`] ⇒ both closures
    /// target that provider's edge.
    pub remote_submit: Option<RemoteSubmitFn>,
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
            remote_submit: None,
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
    /// Hidden values round-tripped through the browser. Optimistic-concurrency
    /// evidence uses names beginning with `_expected_`; these values are not secrets
    /// and are never included in the admin action's field-name audit detail.
    #[serde(default)]
    pub hidden: Vec<HiddenField>,
    /// LOCAL-only: applies the edit; `None` across the remote wire.
    #[serde(skip)]
    pub submit: Option<SubmitFn>,
}

/// The widget a [`Field`] renders as. Additive over the wire: the default is
/// [`FieldKind::Text`], so every historical `Field` (which never sets `kind`)
/// deserialises and renders as a plain text box exactly as before.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FieldKind {
    /// A single-line text input (the historical `Field` behaviour).
    #[default]
    Text,
    /// A single-choice dropdown over [`Field::options`]; the submitted value is the
    /// chosen option's `value`.
    Select,
    /// A set of independent checkboxes over [`Field::options`], all sharing the field's
    /// `name`. Each checked box posts that shared name once; the admin PORTAL collects
    /// the repeated posts and comma-joins the checked `value`s into ONE submit-param
    /// entry, so the owning module's [`SubmitFn`] receives a single comma-separated
    /// string under [`Field::name`] (it splits on `,`). No checkbox posts ⇒ the empty
    /// string.
    CheckboxGroup,
}

/// One choice in a [`FieldKind::Select`]/[`FieldKind::CheckboxGroup`] field: the wire
/// `value`, the operator-facing `label`, and whether it starts checked/selected.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub checked: bool,
}

/// One input in a [`Form`]: a labelled widget pre-filled with `value`, whose `name` is
/// both the HTML input name and the key in the submit values map. `kind` selects the
/// widget ([`FieldKind::Text`] by default); `options` supplies the choices for a
/// `Select`/`CheckboxGroup` (empty for a plain text field).
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub value: String,
    /// The widget kind; defaults to [`FieldKind::Text`] (additive over the wire).
    #[serde(default)]
    pub kind: FieldKind,
    /// Choices for a `Select`/`CheckboxGroup`; empty for a `Text` field.
    #[serde(default)]
    pub options: Vec<FieldOption>,
}

/// One hidden form input. Unlike [`Field`], it has no operator-facing label and is
/// not part of the visible-field audit trail.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct HiddenField {
    pub name: String,
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
                    fields: vec![
                        // A plain text field (kind defaults to Text, no options).
                        Field {
                            name: "game:name".into(),
                            label: "game / name".into(),
                            value: "arena".into(),
                            ..Default::default()
                        },
                        // A single-choice dropdown carrying options + a preselection.
                        Field {
                            name: "role".into(),
                            label: "Role".into(),
                            value: "client".into(),
                            kind: FieldKind::Select,
                            options: vec![
                                FieldOption {
                                    value: "client".into(),
                                    label: "Client".into(),
                                    checked: true,
                                },
                                FieldOption {
                                    value: "server".into(),
                                    label: "Server".into(),
                                    checked: false,
                                },
                            ],
                        },
                        // A checkbox group carrying options + checked flags.
                        Field {
                            name: "methods".into(),
                            label: "Methods".into(),
                            value: String::new(),
                            kind: FieldKind::CheckboxGroup,
                            options: vec![
                                FieldOption {
                                    value: "leaderboard.topScores".into(),
                                    label: "leaderboard.topScores".into(),
                                    checked: true,
                                },
                                FieldOption {
                                    value: "match.report".into(),
                                    label: "match.report".into(),
                                    checked: false,
                                },
                            ],
                        },
                    ],
                    hidden: vec![HiddenField {
                        name: "_expected_revision".into(),
                        value: "7".into(),
                    }],
                    submit: None,
                }),
            },
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: ItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.section, "Platform");
        assert_eq!(back.content.table.unwrap().rows[0][0].text, "game");
        let form = back.content.form.unwrap();
        assert_eq!(form.hidden[0].name, "_expected_revision");
        assert_eq!(form.hidden[0].value, "7");
        // `submit` is `#[serde(skip)]`, so a deserialized (remote) form is read-only.
        assert!(form.submit.is_none());

        // The plain text field survives with the default kind and no options.
        assert_eq!(form.fields[0].kind, FieldKind::Text);
        assert!(form.fields[0].options.is_empty());

        // The Select field's typed kind + options (with the preselected flag) marshal.
        let role = &form.fields[1];
        assert_eq!(role.kind, FieldKind::Select);
        assert_eq!(role.options.len(), 2);
        assert_eq!(role.options[0].value, "client");
        assert_eq!(role.options[0].label, "Client");
        assert!(role.options[0].checked);
        assert!(!role.options[1].checked);

        // The CheckboxGroup field's typed kind + per-option checked flags marshal.
        let methods = &form.fields[2];
        assert_eq!(methods.kind, FieldKind::CheckboxGroup);
        assert_eq!(methods.options.len(), 2);
        assert!(methods.options[0].checked);
        assert_eq!(methods.options[0].value, "leaderboard.topScores");
        assert!(!methods.options[1].checked);
    }

    #[test]
    fn submit_outcome_reveal_roundtrips() {
        // A show-once reveal survives the wire (the `AdminSubmit::admin_submit` success
        // value), and an empty outcome is the ordinary "nothing to show" default.
        let outcome = SubmitOutcome {
            reveal: vec![RevealItem {
                label: "secret".into(),
                value: "ak_generated_secret_value".into(),
            }],
        };
        let back: SubmitOutcome =
            serde_json::from_str(&serde_json::to_string(&outcome).unwrap()).unwrap();
        assert_eq!(back.reveal.len(), 1);
        assert_eq!(back.reveal[0].label, "secret");
        assert_eq!(back.reveal[0].value, "ak_generated_secret_value");
        assert_eq!(back, outcome);

        let empty: SubmitOutcome =
            serde_json::from_str(&serde_json::to_string(&SubmitOutcome::default()).unwrap())
                .unwrap();
        assert!(empty.reveal.is_empty());
    }
}

//! The `apikeys` control-plane provider: the API keys page (the Keys table with its status and
//! role filters, search and keyset pager, and the Roles table with each policy's methods and
//! key count), the key actions (create with a show-once secret, change role, revoke), the role
//! actions (create, edit the policy, delete) and the `apikey` search scope.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    encode_field, ActionDecl, ActionPoint, Align, Cell, CellKind, Column, ConfirmSpec, ConfirmTone, ContributionDecl,
    DataTable, Emphasis, EntityKindDecl, EntityRef, Field, FieldKind, FieldValue, FilterDef, FilterKind, FormSpec,
    InvokeCall, InvokeOutcome,
    Layout, Link, Manifest, NavEntry, Noun, PageDecl, PageInfo, PanelBody, PanelQuery, Point, PrepareCall, Primitive,
    Provider, ReasonReq, Role, Row, SearchDecl, SearchHit, SearchQuery, SecretReveal, Then, Tone,
    DEFAULT_PANEL_DEADLINE_MS, DEFAULT_SEARCH_DEADLINE_MS,
};
use opsapi::Error;

use crate::store::{
    validate_name, validate_policy, KeyRow, KeyStatus, Revoke, RoleRow, WriteError, KEY_NAME, MAX_NAME_BYTES, ROLE_NAME,
};
use crate::{internal, Service};

const MODULE: &str = "apikeys";
pub(crate) const PAGE: &str = "keys";
pub(crate) const KEY_KIND: &str = "apikey";
pub(crate) const ROLE_KIND: &str = "apirole";

pub(crate) const KEYS: &str = "keys";
pub(crate) const ROLES: &str = "roles";

pub(crate) const KEY_CREATE: &str = "key.create";
pub(crate) const KEY_ROTATE: &str = "key.rotate";
pub(crate) const KEY_CHANGE_ROLE: &str = "key.change-role";
pub(crate) const KEY_REVOKE: &str = "key.revoke";
pub(crate) const KEY_REQUESTS: &str = "key.requests";
pub(crate) const ROLE_CREATE: &str = "role.create";
pub(crate) const ROLE_EDIT: &str = "role.edit";
pub(crate) const ROLE_DELETE: &str = "role.delete";

pub(crate) const FIELD_NAME: &str = "name";
pub(crate) const FIELD_ROLE: &str = "role";
pub(crate) const FIELD_EXPIRES: &str = "expires";
pub(crate) const FIELD_METHODS: &str = "methods";
pub(crate) const FIELD_EXTRA_METHODS: &str = "extra_methods";
/// The one expiry a key can have: the store records none.
pub(crate) const NEVER: &str = "Never";

pub(crate) const FILTER_STATUS: &str = "status";
pub(crate) const FILTER_ROLE: &str = "role";
const ACTIVE: &str = "active";
const REVOKED: &str = "revoked";
/// The policy that admits every method.
const FULL: &str = "full";

pub(crate) const PAGE_SIZE: usize = 25;
/// The longest table search the provider runs; a longer one is refused before any statement.
pub(crate) const MAX_NEEDLE_BYTES: usize = 256;

/// The largest cursor µs `store::keys_page` compares exactly: its
/// `$4 * interval '1 microsecond'` multiplies through float8, exact up to 2^53.
const MAX_CURSOR_MICROS: i64 = 1 << 53;

pub(crate) const ROLES_FOOT: &str = "Policies list the op methods modules serve (e.g. leaderboard.topScores).";

/// The operator logins the FAKE "created by" cell draws from.
pub(crate) const FAKE_CREATORS: [&str; 4] = ["marta.k", "jonas.l", "priya.n", "sam.o"];
/// The FAKE "last used" values after "now".
pub(crate) const FAKE_LAST_USED: [&str; 4] = ["4 min ago", "2 h ago", "9 h ago", "1 d ago"];
/// The FAKE "requests (24 h)" values are below this.
pub(crate) const FAKE_REQUESTS_CAP: u64 = 50_000;

pub(crate) const REVOKE_BODY: &str = "Requests signed with this key fail immediately on every process.";
pub(crate) const ROLE_DELETE_BODY: &str = "A role still used by a key, revoked keys included, cannot be deleted.";

pub(crate) struct ApiKeysAdmin {
    svc: Arc<Service>,
}

impl ApiKeysAdmin {
    pub(crate) fn new(svc: Arc<Service>) -> ApiKeysAdmin {
        ApiKeysAdmin { svc }
    }

    async fn keys(&self, req: &PanelQuery) -> Result<PanelBody, Error> {
        let needle = req.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if needle.is_some_and(|n| n.len() > MAX_NEEDLE_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the search is longer than {MAX_NEEDLE_BYTES} bytes")));
        }
        let status = match req.params.get(FILTER_STATUS).map(String::as_str) {
            None => None,
            Some(ACTIVE) => Some(KeyStatus::Active),
            Some(REVOKED) => Some(KeyStatus::Revoked),
            Some(other) => return Err(Error::invalid(format!("{MODULE}: unknown status {other:?}"))),
        };
        let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
        let role_names: Vec<String> = roles.into_iter().map(|r| r.name).collect();
        let role = req.params.get(FILTER_ROLE).map(String::as_str);
        if role.is_some_and(|r| r.len() > MAX_NAME_BYTES) {
            return Err(Error::invalid(format!("{MODULE}: the role filter is longer than {MAX_NAME_BYTES} bytes")));
        }
        let after = req.cursor.as_deref().map(parse_cursor).transpose()?;
        let rows = self
            .svc
            .store
            .keys_page(needle, status, role, after, PAGE_SIZE as i64 + 1)
            .await
            .map_err(internal)?;
        let (total, active) = self.svc.store.key_counts().await.map_err(internal)?;
        let narrowed = needle.is_some() || status.is_some() || role.is_some();
        Ok(keys_body(rows, role_names, (!narrowed).then_some(total), active))
    }

    async fn roles(&self) -> Result<PanelBody, Error> {
        let rows = self.svc.store.roles_with_counts().await.map_err(internal)?;
        Ok(roles_body(&rows))
    }

    /// The bound key as it is now, or `NotFound`.
    async fn bound_key(&self, entity: Option<&EntityRef>) -> Result<KeyRow, Error> {
        let name = key_name(entity)?;
        let key = self.svc.key(name).await.map_err(internal)?;
        key.ok_or_else(|| Error::not_found(format!("{MODULE}: key {name:?} does not exist")))
    }

    async fn create_key(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let name = req.input.get(FIELD_NAME).map(String::as_str).unwrap_or_default();
        if let Err(e) = validate_name(&KEY_NAME, name) {
            return Ok(field_error(FIELD_NAME, e.to_string()));
        }
        if req.input.get(FIELD_EXPIRES).map(String::as_str) != Some(NEVER) {
            return Ok(field_error(FIELD_EXPIRES, format!("must be {NEVER}")));
        }
        let role = req.input.get(FIELD_ROLE).map(String::as_str).unwrap_or_default();
        if role.is_empty() {
            return Ok(field_error(FIELD_ROLE, "required".to_string()));
        }
        match self.svc.create_key(name, role).await {
            Ok((secret, _prefix)) => Ok(InvokeOutcome::Ok {
                message: String::new(),
                reveal: Some(SecretReveal {
                    label: format!("Key created — {name}"),
                    value: secret,
                    hint: None,
                    expires_at: None,
                    extra: Some(format!(
                        "Send it as the X-Api-Key header (HTTP) or the api_key field (socket). Role {role}."
                    )),
                }),
                redirect: None,
            }),
            Err(WriteError::Taken) => Ok(field_error(FIELD_NAME, "a key with this name exists".to_string())),
            Err(e) => Err(write_error(e)),
        }
    }

    /// A CAS on the revision the form was prepared with; a revoked key is refused before it.
    /// A miss is told apart by a re-read: a gone key is `NotFound`, a key revoked meanwhile the
    /// revoked refusal, a moved revision a `Conflict`, and an unmoved one means the role was
    /// the conflict.
    async fn change_role(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        key_name(req.entity.as_ref())?;
        let role = req.input.get(FIELD_ROLE).map(String::as_str).unwrap_or_default();
        if role.is_empty() {
            return Ok(field_error(FIELD_ROLE, "required".to_string()));
        }
        let expected = form_revision(req, "changing a role")?;
        let key = self.bound_key(req.entity.as_ref()).await?;
        if key.revoked {
            return Err(revoked_key(&key.name));
        }
        if key.role == role {
            return Ok(InvokeOutcome::NoOp { message: format!("Key {} already has role {role}", key.name) });
        }
        match self.svc.set_key_role(&key.name, expected, role).await {
            Ok(()) => Ok(InvokeOutcome::Ok { message: "Role changed".to_string(), reveal: None, redirect: None }),
            Err(WriteError::Conflict(msg)) => {
                let now = self.bound_key(req.entity.as_ref()).await?;
                if now.revoked {
                    return Err(revoked_key(&now.name));
                }
                if now.revision == expected {
                    return Err(Error::conflict(msg));
                }
                Ok(InvokeOutcome::Conflict { current_revision: now.revision.max(0) as u64 })
            }
            Err(e) => Err(write_error(e)),
        }
    }

    async fn revoke(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let name = key_name(req.entity.as_ref())?;
        match self.svc.revoke_key(name).await.map_err(write_error)? {
            Revoke::Revoked => Ok(InvokeOutcome::Ok { message: "Key revoked".to_string(), reveal: None, redirect: None }),
            Revoke::AlreadyRevoked => Ok(InvokeOutcome::NoOp { message: format!("Key {name} is already revoked") }),
            Revoke::Absent => Err(Error::not_found(format!("{MODULE}: key {name:?} does not exist"))),
        }
    }

    /// The bound role as it is now, or `NotFound`.
    async fn bound_role(&self, entity: Option<&EntityRef>) -> Result<RoleRow, Error> {
        let name = role_name(entity)?;
        let role = self.svc.role(name).await.map_err(internal)?;
        role.ok_or_else(|| Error::not_found(format!("{MODULE}: role {name:?} does not exist")))
    }

    async fn create_role(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let name = req.input.get(FIELD_NAME).map(String::as_str).unwrap_or_default();
        if let Err(e) = validate_name(&ROLE_NAME, name) {
            return Ok(field_error(FIELD_NAME, e.to_string()));
        }
        let policy = match compose_policy(&req.input) {
            Ok(policy) => policy,
            Err(msg) => return Ok(field_error(FIELD_METHODS, msg)),
        };
        match self.svc.create_role(name, &policy).await {
            Ok(()) => Ok(InvokeOutcome::Ok { message: "Role created".to_string(), reveal: None, redirect: None }),
            Err(WriteError::Taken) => Ok(field_error(FIELD_NAME, "a role with this name exists".to_string())),
            Err(e) => Err(write_error(e)),
        }
    }

    /// A CAS on the revision the form was prepared with, told apart by a re-read as in
    /// `change_role`. A policy naming the same methods as the stored one is a `NoOp`.
    async fn edit_role(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        role_name(req.entity.as_ref())?;
        let policy = match compose_policy(&req.input) {
            Ok(policy) => policy,
            Err(msg) => return Ok(field_error(FIELD_METHODS, msg)),
        };
        let expected = form_revision(req, "editing a policy")?;
        let role = self.bound_role(req.entity.as_ref()).await?;
        if same_methods(&role.policy, &policy) {
            return Ok(InvokeOutcome::NoOp { message: format!("Role {} already has this policy", role.name) });
        }
        match self.svc.set_role_policy(&role.name, expected, &policy).await {
            Ok(()) => Ok(InvokeOutcome::Ok { message: "Policy saved".to_string(), reveal: None, redirect: None }),
            Err(WriteError::Conflict(msg)) => {
                let now = self.bound_role(req.entity.as_ref()).await?;
                if now.revision == expected {
                    return Err(Error::conflict(msg));
                }
                Ok(InvokeOutcome::Conflict { current_revision: now.revision.max(0) as u64 })
            }
            Err(e) => Err(write_error(e)),
        }
    }

    /// No revision check (D1): a role still used by a key, revoked or not, is a 409 naming
    /// how many (K9, K10).
    async fn delete_role(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let name = role_name(req.entity.as_ref())?;
        if self.svc.delete_role(name).await.map_err(write_error)? {
            Ok(InvokeOutcome::Ok { message: "Role deleted".to_string(), reveal: None, redirect: None })
        } else {
            Ok(InvokeOutcome::NoOp { message: format!("Role {name} does not exist") })
        }
    }
}

/// The key a row action is bound to.
fn key_name(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(entity) if entity.kind == KEY_KIND => Ok(&entity.id),
        _ => Err(Error::invalid(format!("{MODULE}: this needs an API key"))),
    }
}

/// The role a row action is bound to.
fn role_name(entity: Option<&EntityRef>) -> Result<&str, Error> {
    match entity {
        Some(entity) if entity.kind == ROLE_KIND => Ok(&entity.id),
        _ => Err(Error::invalid(format!("{MODULE}: this needs an API role"))),
    }
}

/// The revision a CAS form was prepared with; `what` words the refusal.
fn form_revision(req: &InvokeCall, what: &str) -> Result<i64, Error> {
    req.expected_revision
        .and_then(|r| i64::try_from(r).ok())
        .ok_or_else(|| Error::invalid(format!("{MODULE}: {what} needs the form's revision")))
}

/// The methods of a stored policy, trimmed, blanks dropped; `full` is the one entry `full`.
fn policy_methods(policy: &str) -> Vec<&str> {
    policy.split(',').map(str::trim).filter(|m| !m.is_empty()).collect()
}

/// Whether two policies admit the same methods, whatever their order.
fn same_methods(a: &str, b: &str) -> bool {
    let a: std::collections::BTreeSet<&str> = policy_methods(a).into_iter().collect();
    let b: std::collections::BTreeSet<&str> = policy_methods(b).into_iter().collect();
    a == b
}

/// The policy a role form submits: `full` when it is picked or typed, else the de-duplicated
/// union of the picked and the typed methods, in that order. Nothing picked or typed, an
/// undecodable pick and a policy over the byte cap are the methods field's error.
pub(crate) fn compose_policy(input: &BTreeMap<String, String>) -> Result<String, String> {
    let picked: Vec<String> = match input.get(FIELD_METHODS).map(String::as_str) {
        None | Some("") => Vec::new(),
        Some(raw) => serde_json::from_str(raw).map_err(|_| "must be a JSON array of strings".to_string())?,
    };
    let typed = input.get(FIELD_EXTRA_METHODS).map(String::as_str).unwrap_or_default();
    let mut methods: Vec<&str> = Vec::new();
    for m in picked.iter().map(String::as_str).chain(typed.split(',')) {
        let m = m.trim();
        if m == FULL {
            return Ok(FULL.to_string());
        }
        if !m.is_empty() && !methods.contains(&m) {
            methods.push(m);
        }
    }
    if methods.is_empty() {
        return Err(format!("pick {FULL} or at least one method"));
    }
    let policy = methods.join(",");
    validate_policy(&policy).map_err(|e| e.to_string())?;
    Ok(policy)
}

fn field_error(key: &str, msg: String) -> InvokeOutcome {
    InvokeOutcome::FieldErrors(BTreeMap::from([(key.to_string(), msg)]))
}

/// A taken name is the caller's to word; it is never a NotFound (finding #2).
fn write_error(e: WriteError) -> Error {
    match e {
        WriteError::Conflict(msg) => Error::conflict(msg),
        WriteError::Taken => Error::conflict(e.to_string()),
        WriteError::Invalid(msg) => Error::invalid(msg),
        WriteError::Db(e) => internal(e),
    }
}

/// `"<created_at µs>.<name>"` of the last row shown.
pub(crate) fn cursor_of(row: &KeyRow) -> String {
    format!("{}.{}", row.created_us, row.name)
}

/// A hand-edited cursor is `Invalid` here, never a database error from the page query.
pub(crate) fn parse_cursor(raw: &str) -> Result<(i64, &str), Error> {
    let invalid = || Error::invalid(format!("{MODULE}: the page cursor is malformed"));
    let (micros, name) = raw.split_once('.').ok_or_else(invalid)?;
    if micros.is_empty() || !micros.bytes().all(|b| b.is_ascii_digit()) || name.is_empty() || name.len() > MAX_NAME_BYTES
    {
        return Err(invalid());
    }
    let micros: i64 = micros.parse().map_err(|_| invalid())?;
    if micros > MAX_CURSOR_MICROS {
        return Err(invalid());
    }
    Ok((micros, name))
}

fn cell(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell {
        kind,
        value: value.into(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

fn column(key: &str, label: &str, kind: CellKind) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align: Align::Left,
        sorted: None,
        wrap: false,
    }
}

fn select(key: &str, label: &str, options: Vec<String>) -> FilterDef {
    FilterDef {
        key: key.to_string(),
        label: label.to_string(),
        kind: FilterKind::Select(options),
    }
}

fn entity(kind: &str, id: &str) -> EntityRef {
    EntityRef {
        kind: kind.to_string(),
        id: id.to_string(),
    }
}

fn keys_page_link() -> Link {
    Link::Page {
        module: MODULE.to_string(),
        page: PAGE.to_string(),
        params: BTreeMap::new(),
    }
}

/// The refusal to re-role a revoked key: it stays on the role it was revoked under.
fn revoked_key(name: &str) -> Error {
    Error::conflict(format!("key {name} is revoked"))
}

fn status_text(revoked: bool) -> &'static str {
    if revoked {
        REVOKED
    } else {
        ACTIVE
    }
}

/// The FAKE operator a key was created by.
pub(crate) fn fake_creator(name: &str) -> &'static str {
    // FAKE: keys record no creator.
    FAKE_CREATORS[(controlplane::fake_hash("apikeys.created_by", name) % FAKE_CREATORS.len() as u64) as usize]
}

/// The FAKE last-used cell: a "now" badge or a dim relative time. A revoked key cannot be in
/// use, so it never draws "now": the pick that would is drawn as the oldest relative time.
pub(crate) fn fake_last_used(name: &str, revoked: bool) -> Cell {
    // FAKE: the gateway records no key use.
    let pick = (controlplane::fake_hash("apikeys.last_used", name) % (FAKE_LAST_USED.len() as u64 + 1)) as usize;
    match pick {
        0 if revoked => cell(CellKind::Dim, FAKE_LAST_USED[FAKE_LAST_USED.len() - 1]),
        0 => Cell { tone: Some(Tone::Ok), ..cell(CellKind::Badge, "now") },
        n => cell(CellKind::Dim, FAKE_LAST_USED[n - 1]),
    }
}

/// The FAKE requests in the last 24 hours, with thousands separators.
pub(crate) fn fake_requests(name: &str) -> String {
    // FAKE: the gateway counts no requests per key.
    let n = (controlplane::fake_hash("apikeys.requests_24h", name) % FAKE_REQUESTS_CAP).to_string();
    let mut out = String::new();
    for (i, digit) in n.chars().enumerate() {
        if i > 0 && (n.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn key_row(row: &KeyRow) -> Row {
    let created = Cell {
        row: true,
        more: vec![cell(CellKind::User, fake_creator(&row.name))],
        ..cell(CellKind::Dim, row.created.clone())
    };
    Row {
        entity: Some(entity(KEY_KIND, &row.name)),
        cells: vec![
            cell(CellKind::Strong, row.name.clone()),
            cell(CellKind::MonoMuted, format!("{}…", row.prefix)),
            cell(CellKind::Ref, row.role.clone()),
            created,
            fake_last_used(&row.name, row.revoked),
            cell(CellKind::Badge, status_text(row.revoked)),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The meta of the Keys panel over every key, whatever the table is narrowed to.
pub(crate) fn keys_meta(active: i64) -> String {
    format!("{active} {ACTIVE}")
}

/// The Keys table from up to `PAGE_SIZE + 1` rows: the surplus row only decides that a next
/// page exists, and the cursor is then the last drawn row. `total` is `None` when narrowed.
pub(crate) fn keys_body(mut rows: Vec<KeyRow>, role_names: Vec<String>, total: Option<i64>, active: i64) -> PanelBody {
    let next_cursor = if rows.len() > PAGE_SIZE {
        rows.truncate(PAGE_SIZE);
        rows.last().map(cursor_of)
    } else {
        None
    };
    let table = DataTable {
        columns: vec![
            column("name", "Name", CellKind::Strong),
            column("key", "Key", CellKind::MonoMuted),
            column("role", "Role", CellKind::Ref),
            column("created", "Created", CellKind::Dim),
            column("last_used", "Last used", CellKind::Dim),
            column("status", "Status", CellKind::Badge),
        ],
        rows: rows.iter().map(key_row).collect(),
        page: PageInfo {
            next_cursor,
            size: PAGE_SIZE as u32,
            total: total.map(|t| t.max(0) as u64),
        },
        filters: vec![
            select(FILTER_STATUS, "Status", vec![ACTIVE.to_string(), REVOKED.to_string()]),
            select(FILTER_ROLE, "Role", role_names),
        ],
        search_placeholder: Some("Name or key prefix".to_string()),
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("key", "keys")),
    };
    PanelBody {
        meta: Some(keys_meta(active)),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

/// A policy as one cell: one tag per method, or the one tag `full`.
pub(crate) fn scopes_cell(policy: &str) -> Cell {
    let mut tags = if policy.trim() == FULL {
        vec![cell(CellKind::Ref, FULL)].into_iter()
    } else {
        policy
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| cell(CellKind::Ref, m))
            .collect::<Vec<_>>()
            .into_iter()
    };
    match tags.next() {
        Some(first) => Cell { more: tags.collect(), ..first },
        None => cell(CellKind::Muted, "—"),
    }
}

fn role_row(row: &RoleRow) -> Row {
    Row {
        entity: Some(entity(ROLE_KIND, &row.name)),
        cells: vec![
            cell(CellKind::Strong, row.name.clone()),
            scopes_cell(&row.policy),
            cell(CellKind::Number, row.keys.to_string()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Roles table: every role, name-ordered, with no pager.
pub(crate) fn roles_body(rows: &[RoleRow]) -> PanelBody {
    let table = DataTable {
        columns: vec![
            column("role", "Role", CellKind::Strong),
            Column { wrap: true, ..column("scopes", "Scopes", CellKind::Ref) },
            Column { align: Align::Right, ..column("keys", "Keys", CellKind::Number) },
        ],
        rows: rows.iter().map(role_row).collect(),
        page: PageInfo {
            next_cursor: None,
            size: 0,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: None,
    };
    PanelBody {
        meta: Some(rows.len().to_string()),
        foot: Some(ROLES_FOOT.to_string()),
        ..PanelBody::new(Primitive::DataTable(table))
    }
}

pub(crate) fn search_hit(row: &KeyRow) -> SearchHit {
    SearchHit {
        link: keys_page_link(),
        title: row.name.clone(),
        subtitle: format!("{}… · role {}", row.prefix, row.role),
        badge: Some(status_text(row.revoked).to_string()),
    }
}

fn contribution(id: &str, order: i32, title: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point: Point::Page(PAGE.to_string()),
        order,
        title: title.to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

fn key_action(id: &str, label: &str, icon: &str, order: i32, min_role: Role, then: Then, implemented: bool) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point: ActionPoint::RowAction(KEY_KIND.to_string()),
        label: label.to_string(),
        icon: icon.to_string(),
        order,
        danger: false,
        multi: false,
        min_role,
        then,
        implemented,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

fn field(key: &str, label: &str, kind: FieldKind) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        required: true,
        help: None,
        default: None,
        readonly: false,
        mono: false,
        placeholder: None,
    }
}

fn role_names(roles: &[RoleRow]) -> Vec<String> {
    roles.iter().map(|r| r.name.clone()).collect()
}

/// A policy as plain text: `full`, or its methods joined by commas.
fn policy_text(policy: &str) -> String {
    policy.split(',').map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(", ")
}

/// The create form over the stored roles: the first is the default and its policy the help.
pub(crate) fn create_form(roles: &[RoleRow]) -> FormSpec {
    let first = roles.first();
    let name = Field {
        help: Some("Used in audit entries.".to_string()),
        mono: true,
        placeholder: Some("horde-agents-2".to_string()),
        ..field(FIELD_NAME, "Name", FieldKind::String)
    };
    let role = Field {
        help: first.map(|r| policy_text(&r.policy)),
        default: first.map(|r| r.name.clone()),
        ..field(FIELD_ROLE, "Role", FieldKind::Enum(role_names(roles)))
    };
    let expires = Field {
        default: Some(NEVER.to_string()),
        ..field(FIELD_EXPIRES, "Expires", FieldKind::Enum(vec![NEVER.to_string()]))
    };
    FormSpec {
        fields: vec![name, role, expires],
        hidden: BTreeMap::new(),
        submit_label: "Create key".to_string(),
        note: Some("The secret is shown once after creation.".to_string()),
        revision: None,
        title: Some("Create API key".to_string()),
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// The change-role form over the stored roles, defaulting to `key`'s role at `key`'s revision.
pub(crate) fn change_role_form(roles: &[RoleRow], key: Option<&KeyRow>) -> FormSpec {
    let role = Field {
        default: key.map(|k| k.role.clone()),
        ..field(FIELD_ROLE, "Role", FieldKind::Enum(role_names(roles)))
    };
    FormSpec {
        fields: vec![role],
        hidden: BTreeMap::new(),
        submit_label: "Change role".to_string(),
        note: None,
        revision: key.map(|k| k.revision.max(0) as u64),
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

fn revoke_confirm() -> ConfirmSpec {
    ConfirmSpec {
        title: "Revoke key?".to_string(),
        body: REVOKE_BODY.to_string(),
        tone: ConfirmTone::Danger,
        impact: Vec::new(),
        reason: ReasonReq::Optional,
        reason_hint: Some("e.g. leaked in CI log".to_string()),
        type_to_confirm: None,
        confirm_label: "Revoke key".to_string(),
    }
}

/// The revoke confirm bound to `key`; the name is the type-to-confirm token.
pub(crate) fn revoke_confirm_for(key: &KeyRow) -> ConfirmSpec {
    let id = Cell {
        row: true,
        more: vec![cell(CellKind::Text, format!("· {}", key.role))],
        ..cell(CellKind::Mono, format!("{}…", key.prefix))
    };
    ConfirmSpec {
        title: format!("Revoke {}?", key.name),
        impact: vec![
            ("Key".to_string(), id),
            ("Last used".to_string(), fake_last_used(&key.name, key.revoked)),
            ("Requests (24 h)".to_string(), cell(CellKind::Number, fake_requests(&key.name))),
        ],
        type_to_confirm: Some(key.name.clone()),
        ..revoke_confirm()
    }
}

/// The methods a role form offers: `full`, every served op method, then the methods some
/// stored policy names that the catalog lacks, sorted; so every stored policy can be
/// pre-checked.
pub(crate) fn method_options(roles: &[RoleRow]) -> Vec<String> {
    let mut options = vec![FULL.to_string()];
    options.extend(opscatalog::OPERATIONS.iter().map(|op| op.method.to_string()));
    let mut in_use: Vec<&str> = roles
        .iter()
        .flat_map(|r| policy_methods(&r.policy))
        .filter(|m| !options.iter().any(|o| o == m))
        .collect();
    in_use.sort_unstable();
    in_use.dedup();
    options.extend(in_use.into_iter().map(String::from));
    options
}

/// The role form over the stored roles' methods. Bound to `role` it edits that role: the
/// name is read-only, its methods are pre-checked and the CAS revision is its revision.
pub(crate) fn role_form(roles: &[RoleRow], role: Option<&RoleRow>) -> FormSpec {
    let kind = FieldKind::Multi(method_options(roles));
    let name = Field {
        mono: true,
        readonly: role.is_some(),
        default: role.map(|r| r.name.clone()),
        ..field(FIELD_NAME, "Name", FieldKind::String)
    };
    let picked = role.map(|r| policy_methods(&r.policy).into_iter().map(String::from).collect::<Vec<_>>());
    // Optional here: a role may hold only typed methods; `compose_policy` is the one rule
    // that a policy names `full` or at least one method.
    let methods = Field {
        required: false,
        default: picked.map(|p| encode_field(&kind, &FieldValue::Multi(p))),
        ..field(FIELD_METHODS, "Allowed methods", kind)
    };
    let extra = Field {
        required: false,
        mono: true,
        help: Some("Comma-separated; for methods no process serves yet.".to_string()),
        ..field(FIELD_EXTRA_METHODS, "Additional methods", FieldKind::String)
    };
    FormSpec {
        fields: vec![name, methods, extra],
        hidden: BTreeMap::new(),
        submit_label: if role.is_some() { "Save policy" } else { "Create role" }.to_string(),
        note: None,
        revision: role.map(|r| r.revision.max(0) as u64),
        title: role.is_none().then(|| "Create role".to_string()),
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

fn role_delete_confirm() -> ConfirmSpec {
    ConfirmSpec {
        title: "Delete role?".to_string(),
        body: ROLE_DELETE_BODY.to_string(),
        tone: ConfirmTone::Danger,
        impact: Vec::new(),
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Delete role".to_string(),
    }
}

/// The delete confirm bound to `role`.
pub(crate) fn role_delete_confirm_for(role: &RoleRow) -> ConfirmSpec {
    ConfirmSpec {
        title: format!("Delete role {}?", role.name),
        ..role_delete_confirm()
    }
}

fn role_action(id: &str, label: &str, icon: &str, order: i32, then: Then) -> ActionDecl {
    ActionDecl {
        point: ActionPoint::RowAction(ROLE_KIND.to_string()),
        prepare: true,
        ..key_action(id, label, icon, order, Role::Operator, then, true)
    }
}

pub(crate) fn manifest() -> Manifest {
    let requests = Link::Page {
        module: "audit".to_string(),
        page: "log".to_string(),
        params: BTreeMap::new(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: MODULE.to_string(),
            label: "API keys".to_string(),
            icon: "key".to_string(),
            order: order::platform_nav::APIKEYS,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "API keys".to_string(),
            subtitle: "Machine credentials for game servers, CI and integrations.".to_string(),
            head_actions: vec![KEY_CREATE.to_string()],
            tiles: Vec::new(),
            layout: Layout::Stack(vec![KEYS.to_string(), ROLES.to_string()]),
            params: vec![FILTER_STATUS.to_string(), FILTER_ROLE.to_string()],
        }],
        entity_kinds: vec![
            EntityKindDecl {
                kind: KEY_KIND.to_string(),
                label: "API key".to_string(),
                icon: "key".to_string(),
            },
            EntityKindDecl {
                kind: ROLE_KIND.to_string(),
                label: "API role".to_string(),
                icon: "shield".to_string(),
            },
        ],
        contributions: vec![contribution(KEYS, 10, "Keys"), contribution(ROLES, 20, "Roles")],
        // Every form and confirm is `prepare: true`: its options are the stored roles and its
        // text the bound key; the declared spec fixes only the variant.
        actions: vec![
            ActionDecl {
                point: ActionPoint::PageHead(PAGE.to_string()),
                emphasis: Emphasis::Primary,
                prepare: true,
                ..key_action(KEY_CREATE, "Create key", "plus", 10, Role::Operator, Then::Form(create_form(&[])), true)
            },
            // NEW: no secret rotation exists.
            key_action(KEY_ROTATE, "Rotate secret…", "refresh", 10, Role::Operator, Then::Invoke, false),
            ActionDecl {
                prepare: true,
                ..key_action(
                    KEY_CHANGE_ROLE,
                    "Change role…",
                    "shield",
                    20,
                    Role::Operator,
                    Then::Form(change_role_form(&[], None)),
                    true,
                )
            },
            ActionDecl {
                danger: true,
                prepare: true,
                ..key_action(KEY_REVOKE, "Revoke…", "ban", 30, Role::Operator, Then::Confirm(revoke_confirm()), true)
            },
            key_action(KEY_REQUESTS, "Requests by this key", "list", 40, Role::Viewer, Then::Link(requests), true),
            ActionDecl {
                point: ActionPoint::PanelTool(ROLES.to_string()),
                when_empty: true,
                prepare: true,
                ..key_action(ROLE_CREATE, "Role", "plus", 10, Role::Operator, Then::Form(role_form(&[], None)), true)
            },
            role_action(ROLE_EDIT, "Edit policy…", "edit", 10, Then::Form(role_form(&[], None))),
            ActionDecl {
                danger: true,
                ..role_action(ROLE_DELETE, "Delete role…", "trash", 20, Then::Confirm(role_delete_confirm()))
            },
        ],
        search: vec![SearchDecl {
            kind: KEY_KIND.to_string(),
            label: "API keys".to_string(),
            icon: "key".to_string(),
            deadline_ms: DEFAULT_SEARCH_DEADLINE_MS,
        }],
        writes: true,
    }
}

#[async_trait]
impl Provider for ApiKeysAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            KEYS => self.keys(&req).await,
            ROLES => self.roles().await,
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        let q = req.q.trim();
        if req.kind != KEY_KIND || q.is_empty() || q.len() > MAX_NEEDLE_BYTES {
            return Ok(Vec::new());
        }
        let limit = i64::from(req.limit.min(PAGE_SIZE as u32));
        let rows = self.svc.store.keys_page(Some(q), None, None, None, limit).await.map_err(internal)?;
        Ok(rows.iter().map(search_hit).collect())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        if req.input.values().any(|v| v.len() > controlplane::MAX_INPUT_VALUE_BYTES) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            KEY_CREATE => self.create_key(&req).await,
            KEY_ROTATE => Err(Error::forbidden("not implemented yet")),
            KEY_CHANGE_ROLE => self.change_role(&req).await,
            KEY_REVOKE => self.revoke(&req).await,
            ROLE_CREATE => self.create_role(&req).await,
            ROLE_EDIT => self.edit_role(&req).await,
            ROLE_DELETE => self.delete_role(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, Error> {
        match req.action.as_str() {
            KEY_CREATE => {
                let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
                if roles.is_empty() {
                    return Err(Error::conflict(format!("{MODULE}: no role exists; create a role first")));
                }
                Ok(Then::Form(create_form(&roles)))
            }
            KEY_CHANGE_ROLE => {
                let key = self.bound_key(req.entity.as_ref()).await?;
                if key.revoked {
                    return Err(revoked_key(&key.name));
                }
                let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
                Ok(Then::Form(change_role_form(&roles, Some(&key))))
            }
            KEY_REVOKE => Ok(Then::Confirm(revoke_confirm_for(&self.bound_key(req.entity.as_ref()).await?))),
            ROLE_CREATE => {
                let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
                Ok(Then::Form(role_form(&roles, None)))
            }
            ROLE_EDIT => {
                let name = role_name(req.entity.as_ref())?;
                let roles = self.svc.store.roles_with_counts().await.map_err(internal)?;
                let role = roles
                    .iter()
                    .find(|r| r.name == name)
                    .ok_or_else(|| Error::not_found(format!("{MODULE}: role {name:?} does not exist")))?;
                Ok(Then::Form(role_form(&roles, Some(role))))
            }
            ROLE_DELETE => Ok(Then::Confirm(role_delete_confirm_for(&self.bound_role(req.entity.as_ref()).await?))),
            other => Err(Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

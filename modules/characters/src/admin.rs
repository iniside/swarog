use std::collections::HashMap;

use async_trait::async_trait;
use charactersapi::Character;
use opsapi::Error;

use crate::{internal, Service, Store, ADMIN_ITEM_ID, ADMIN_LABEL, ADMIN_SECTION};

#[async_trait]
impl adminapi::AdminData for Service {
    /// The admin fan-out: this module's page as `adminapi::ItemData` (same
    /// Section/Label the local `Item` carries), served on the edge as
    /// `admin.adminData` so a remote admin process renders it cross-process. The
    /// request `params` (which now ride the wire) select the view — see
    /// [`admin_content`]. The `extensions` mirror the LOCAL `Item::with_extensions`
    /// exactly (one shared [`extension_entries`]) so local/remote can't drift.
    async fn admin_data(&self, params: adminapi::Params) -> Result<adminapi::ItemData, Error> {
        let content = admin_content(&self.store, &params).await.map_err(internal)?;
        Ok(adminapi::ItemData {
            id: ADMIN_ITEM_ID.into(),
            section: ADMIN_SECTION.into(),
            label: ADMIN_LABEL.into(),
            content,
            extensions: extension_entries(),
        })
    }
}

/// The cross-page extension entries characters contributes to OTHER modules' pages.
/// ONE source of truth: the LOCAL `Item` attaches these via `with_extensions` and the
/// REMOTE `ItemData` carries the SAME vec, so a split admin sees the identical menu.
/// characters extends the accounts Players row `⋯` menu with a "View Characters"
/// drill-down (`characters?owner=player:<uuid>&owner_name=<display name>`,
/// interpolated from the row `context` — the name rides the link so the scoped page
/// can title itself without knowing the accounts module).
pub(crate) fn extension_entries() -> Vec<adminapi::ExtensionEntry> {
    vec![adminapi::ExtensionEntry {
        point: accountsapi::admin::PLAYERS_ROW_MENU.id.into(),
        label: "View Characters".into(),
        icon: "characters".into(),
        link: format!("{ADMIN_ITEM_ID}?owner={{id}}&owner_name={{name}}"),
        present: adminapi::Present::Navigate,
        priority: 0,
    }]
}

/// Dispatches the Characters page on the `?owner=` drill-down param (the same
/// dispatch LOCAL and REMOTE, now params ride the wire):
///
/// - no `owner` → the all-characters table (unchanged data);
/// - `owner=player:<uuid>` → that player's characters as a card grid + a context
///   header (characters doesn't know account names, so the header title is the uuid
///   short form);
/// - `owner=character:<uuid>` → the character-detail content the modal fetches
///   (header + KPI stats + `modal_point`/`context` for the footer actions).
///
/// A malformed or FOREIGN `owner` renders error-content, NEVER an `Err` (the
/// foreign-params tolerance contract on `AdminData` — the portal forwards every
/// page's params to every provider, so an `Err` here would poison an unrelated page).
pub(crate) async fn admin_content(
    store: &Store,
    params: &adminapi::Params,
) -> anyhow::Result<adminapi::Content> {
    let owner = adminapi::param(params, "owner");
    if owner.is_empty() {
        return admin_all_characters(store).await;
    }
    if let Some(uuid) = owner.strip_prefix("player:") {
        if !is_uuid(uuid) {
            return Ok(error_content("Invalid owner id — not a uuid."));
        }
        let chars = store.list_by_player(uuid).await?;
        // Display-only: the owner's name rides the drill-down link (the accounts row
        // context) — characters itself never learns account display names.
        let owner_name = adminapi::param(params, "owner_name");
        return Ok(build_player_scoped(uuid, owner_name, &chars));
    }
    if let Some(uuid) = owner.strip_prefix("character:") {
        if !is_uuid(uuid) {
            return Ok(error_content("Invalid character id — not a uuid."));
        }
        return match store.get(uuid).await? {
            Some(c) => Ok(build_character_detail(&c)),
            None => Ok(error_content("No such character.")),
        };
    }
    // Foreign/unrecognized owner shape — tolerate, don't Err.
    Ok(error_content(
        "Invalid owner — expected player:<uuid> or character:<uuid>.",
    ))
}

/// The live "Characters" block: a count KPI + a table of the newest 50 characters.
/// Reads only its own data and returns the admin's declarative widgets (the admin
/// owns the look). Async because it queries the store.
async fn admin_all_characters(store: &Store) -> anyhow::Result<adminapi::Content> {
    let n = store.count().await?;
    let rows = store.list_all(50).await?;

    let mut table = adminapi::Table {
        columns: vec!["NAME".into(), "CLASS".into(), "PLAYER".into(), "CREATED".into()],
        rows: Vec::with_capacity(rows.len()),
        ..Default::default()
    };
    for c in rows {
        table.rows.push(vec![
            adminapi::Cell::text(&c.name),
            adminapi::Cell {
                text: c.class,
                badge: "blue".into(),
                ..Default::default()
            },
            adminapi::Cell::mono(&c.player_id),
            adminapi::Cell::text(short_ts(&c.created_at)),
        ]);
    }

    Ok(adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Characters".into(),
            value: n.to_string(),
            sub: String::new(),
        }],
        table: Some(table),
        form: None,
        ..Default::default()
    })
}

/// The player-scoped view (`?owner=player:<uuid>`): a context header (title = the
/// player's display name when the drill-down link carried `owner_name`, else the uuid
/// short form — characters itself doesn't know account names) plus one card per
/// character, the card grid bound to the characters-owned card `⋯` point. Pure over an
/// already-fetched character list so it is unit-testable without the DB.
pub(crate) fn build_player_scoped(
    player_uuid: &str,
    owner_name: &str,
    chars: &[Character],
) -> adminapi::Content {
    let cards = chars
        .iter()
        .enumerate()
        .map(|(i, c)| character_card(i, c))
        .collect();

    let title = if owner_name.is_empty() {
        short_uuid(player_uuid).to_string()
    } else {
        owner_name.to_string()
    };
    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&title),
            avatar_color_key: palette(player_uuid),
            title,
            subtitle_mono: format!("player:{player_uuid}"),
            right_note: format!("{} character(s)", chars.len()),
        }),
        cards: Some(adminapi::CardGrid {
            menu_point: charactersapi::admin::CHARACTERS_CARD_MENU.id.into(),
            cards,
        }),
        ..Default::default()
    }
}

/// One character card for the player-scoped grid. REAL fields only (name, class,
/// created_at — there is no level column), context `{"id": "character:<uuid>"}` for
/// interpolation, and a native menu: View (opens the character modal) + inert
/// Edit/Delete (per the mockup — not yet op-wired).
fn character_card(idx: usize, c: &Character) -> adminapi::Card {
    adminapi::Card {
        icon_text: initial(&c.name),
        color_key: cycle(idx),
        title: c.name.clone(),
        subtitle: c.class.clone(),
        badge: String::new(),
        stats: vec![
            adminapi::CardStat {
                label: "Class".into(),
                value: c.class.clone(),
            },
            adminapi::CardStat {
                label: "Created".into(),
                value: short_ts(&c.created_at).to_string(),
            },
        ],
        context: HashMap::from([
            // `id` is already the full entity ref (`character:<uuid>`) — link templates
            // use `owner={id}` verbatim, never re-prefix (a double `character:character:`
            // fails the uuid guard).
            ("id".into(), format!("character:{}", c.id)),
            ("name".into(), c.name.clone()),
        ]),
        menu: vec![
            adminapi::MenuEntry {
                label: "View".into(),
                icon: "view".into(),
                link: Some(format!("{ADMIN_ITEM_ID}?owner={{id}}")),
                present: adminapi::Present::Modal,
                ..Default::default()
            },
            adminapi::MenuEntry {
                label: "Edit".into(),
                icon: "edit".into(),
                disabled: true,
                ..Default::default()
            },
            adminapi::MenuEntry {
                label: "Delete".into(),
                icon: "delete".into(),
                danger: true,
                disabled: true,
                ..Default::default()
            },
        ],
    }
}

/// The character-detail content the modal fetches (`?owner=character:<uuid>`): an
/// owner-rendered header + KPI stats from REAL fields, plus `modal_point`/`context`
/// binding the modal footer to the characters-owned ModalActions point (the footer's
/// `{id}` interpolation source). Pure over one fetched character for unit tests.
pub(crate) fn build_character_detail(c: &Character) -> adminapi::Content {
    adminapi::Content {
        header: Some(adminapi::ContextHeader {
            avatar_text: initial(&c.name),
            avatar_color_key: palette(&c.id),
            title: c.name.clone(),
            // The full guid + created stamp ride the mono subtitle (mockup: neither the
            // guid nor created is a KPI cell — the six KPIs are the stat grid below).
            subtitle_mono: format!("character:{} · created {}", c.id, short_ts(&c.created_at)),
            right_note: c.class.clone(),
        }),
        // The mockup's six-stat grid. DECORATIVE FAKE (user-mandated) — see
        // [`character_stats`]; the class rides the header's `right_note`.
        kpis: character_stats(&c.id),
        modal_point: charactersapi::admin::CHARACTER_MODAL_ACTIONS.id.into(),
        context: HashMap::from([
            ("id".into(), format!("character:{}", c.id)),
            ("name".into(), c.name.clone()),
        ]),
        ..Default::default()
    }
}

/// DECORATIVE FAKE STATS (user-mandated) — the backend has NO combat stats, but the
/// mockup's character modal shows six. Each is derived deterministically from the
/// character uuid via a per-stat hash (`fnv(uuid#k)`), so two characters differ while
/// ONE character is stable across every render and BOTH topologies (pure, no
/// randomness, no clock). Labels + value shapes match the mockup exactly. NOT domain
/// data — cosmetic only, never a gameplay input.
pub(crate) fn character_stats(id: &str) -> Vec<adminapi::Kpi> {
    // Each stat draws from its OWN hash of the id, so the six are decorrelated.
    let span = |k: u64, lo: u64, hi: u64| lo + (fnv(&format!("{id}#{k}")) % (hi - lo + 1));
    let kpi = |label: &str, value: String| adminapi::Kpi {
        label: label.into(),
        value,
        sub: String::new(),
    };
    vec![
        kpi("POWER", thousands(span(1, 2_000, 13_000))),
        kpi("GEAR SCORE", span(2, 400, 900).to_string()),
        kpi("HEALTH", thousands(span(3, 15_000, 50_000))),
        kpi("MANA", thousands(span(4, 1_000, 10_000))),
        kpi("CRIT RATE", format!("{}%", span(5, 5, 65))),
        kpi("PLAYTIME", format!("{} h", span(6, 20, 1_300))),
    ]
}

/// A stable 64-bit FNV-1a fold of a seed — the deterministic basis for the decorative
/// fake stats (a per-stat suffix decorrelates the six).
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Thousands-separated decimal (`12480` → `"12,480"`) — presentation for the decorative
/// fake stats, matching the mockup's `12,480` / `48,200` shapes.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Renders a single message as an error card (a lone KPI), so a bad/foreign owner
/// param is a clean card, never a 500.
fn error_content(msg: &str) -> adminapi::Content {
    adminapi::Content {
        kpis: vec![adminapi::Kpi {
            label: "Error".into(),
            value: msg.into(),
            sub: String::new(),
        }],
        ..Default::default()
    }
}

/// A canonical 8-4-4-4-12 hex uuid check — guards the drill-down param before it
/// reaches the store's `$id::uuid` cast, so a malformed id renders an error card
/// instead of a Postgres cast error. Twin of inventory's `is_uuid`.
fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (i, c) in s.chars().enumerate() {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            if c != '-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// The store's raw `created_at::text` (`2026-07-08 16:17:45.286964+00`) truncated to
/// minutes for display (`2026-07-08 16:17`) — presentation only; `Character.created_at`
/// is a contract field consumed by HTTP clients, so the STORE never reformats it.
pub(crate) fn short_ts(ts: &str) -> &str {
    ts.get(..16).unwrap_or(ts)
}

/// The first hex group of a uuid (`b3f1a2c4`), the short form the header/KPI show
/// (characters doesn't know account display names).
fn short_uuid(uuid: &str) -> &str {
    uuid.split('-').next().unwrap_or(uuid)
}

/// The first character (uppercased) of a label as avatar text; a fallback glyph when
/// empty.
fn initial(s: &str) -> String {
    s.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Deterministic cycling-palette key (`av-0`..`av-5`) from a stable seed, so the same
/// entity always draws the same avatar colour (presentation only — no invented data).
fn palette(seed: &str) -> String {
    cycle(seed.bytes().map(|b| b as usize).sum())
}

/// The cycling-palette key for grid index `i` (`av-0`..`av-5`).
fn cycle(i: usize) -> String {
    format!("av-{}", i % 6)
}

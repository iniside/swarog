# Admin extension points + mockup restyle — plan

> After approval this plan is copied verbatim to
> `docs/plans/2026-07-14-<HHMM>-admin-extension-points-plan.md` (repo is the source
> of truth; work happens on master per standing agreement).

## Context

The admin portal today has exactly one extension seam: a module contributes a whole
sidebar page (`adminapi::SLOT` → `Item`). There is no way for module B to add an
action to module A's page — the scenario we need (Players row `⋯` menu gaining
"View Characters" / "View Inventory" from the characters/inventory modules, a
player-scoped Characters sub-page whose cards open a character modal with an
inventory action) is unbuildable without the page owner hardcoding knowledge of its
extenders, inverting the Open/Closed principle the whole repo is built on.

Research (6 subagents, synthesized in
`docs/research/2026-07-14-1804-admin-extension-points-research-summary.md`) confirmed:

- Contribution slots are strictly per-process — extension declarations MUST travel
  as pure data on the existing `admin.adminData` wire (additive
  `ItemData.extensions` field with `#[serde(default)]` flows through generated glue
  untouched).
- `AdminData::admin_data()` takes NO params today: remote drill-down GETs render
  unscoped. **User decision: extend the existing method** to
  `admin_data(params: Params)` (breaking `#[rpc]` change, `--bless-public-api`,
  all 7 provider impls) — no side-channel second method.
- Portal is zero-JS with CSP `default-src 'self'` — which already permits
  same-origin script FILES (only inline is blocked). **User decision: vendored
  htmx (single min.js, no npm/build step) + ~50 lines vanilla JS** for `⋯` menu
  toggles. Avoid `hx-on:` (needs eval).
- **User decision: normalize menu ordering** everywhere: owner-native entries →
  separator → extension entries (the mockup is internally inconsistent; we don't
  copy that).
- **User decision: scope = the mockup's active pages** (Players / Characters /
  Inventory) + full restyle to the new `UILayout/GameOps Admin.dc.html` color
  scheme; other sidebar entries stay static/decorative. Mockup is the exact visual
  spec (no creative freedom); data columns adapt to real backend fields where the
  mockup invents them (LEVEL/REGION/PLAYTIME don't exist in accounts — visual
  fidelity is per-component styling, not fictional data).
- Entity-ref convention: adopt inventory's existing `"<type>:<uuid>"` composite
  (`"player:<uuid>"`, `"character:<uuid>"`) as the standard context value.
- Edit/Delete menu items are inert in the mockup too — they render as visible inert
  items now; wiring them is a later op-backed phase (out of scope here).
- match/rating/leaderboard have no admin surface — untouched.

Why not extend an existing seam instead: the bus is async fire-and-forget (menus
are a sync render concern), the registry is 1:1 capability lookup (this is
many-contributors-to-one-point), and `contrib` slots don't cross processes. The
existing `Item`/`ItemData` channel is the only seam that already reaches every
topology — extensions ride it as data. (Research-before-planning rationale in the
research doc §8.)

## Design overview

Three parties, each knowing only its part (same triangle as the bus):

- **Page owner** declares extension points as typed consts in its own api crate
  (e.g. `accountsapi::admin::PLAYERS_ROW_MENU`), tags its tables/cards with a
  `menu_point` + per-row `context` (`{"id": "player:<uuid>"}`), and renders its own
  native menu entries.
- **Contributor** ships `ExtensionEntry` values (pure data: target point id, label,
  icon key, link template `"inventory?owner={id}"`, `present: Navigate|Modal`,
  priority) on its `Item` / `ItemData` — it imports only the owner's api crate for
  the point const.
- **Admin portal** merges: collects every resolved item's extensions per request,
  indexes by point id, interpolates `{key}` from row/card context, appends
  `from=<current-page>` for back-navigation chrome, renders Navigate entries as
  anchors and Modal entries as htmx fragment fetches (`?partial=modal`), and knows
  no domain.

New `adminapi` vocabulary (all additive except the `AdminData` signature):

```rust
pub enum ExtensionKind { EntityMenu, ModalActions }            // closed taxonomy
pub struct ExtensionPoint {                                     // owner-declared const
    pub id: &'static str,                                       // "accounts.players.row-menu"
    pub kind: ExtensionKind,
    pub context_keys: &'static [&'static str],                  // ["id"]
}
pub enum Present { Navigate, Modal }                            // Default = Navigate
pub struct ExtensionEntry {                                     // contributor data (serde)
    pub point: String, pub label: String, pub icon: String,     // icon = named key
    pub link: String,                                           // "slug?query" template, {key} interpolation
    #[serde(default)] pub present: Present,
    #[serde(default)] pub priority: i32,
}
pub struct MenuEntry {                                          // owner-native menu item
    pub label: String, pub icon: String, pub link: Option<String>,
    #[serde(default)] pub present: Present,
    #[serde(default)] pub danger: bool,                         // red styling (Delete)
    #[serde(default)] pub disabled: bool,                       // true = inert (mockup Edit/Delete); default = active
}
pub struct RowMeta { pub context: HashMap<String,String>, pub menu: Vec<MenuEntry> }
// Table gains: #[serde(default)] menu_point: String, #[serde(default)] row_meta: Vec<RowMeta>
pub struct Card { icon_text, color_key, title, subtitle, badge, stats: Vec<CardStat>,
                  context: HashMap<String,String>, menu: Vec<MenuEntry> }
pub struct CardGrid { pub menu_point: String, pub cards: Vec<Card> }
pub struct ContextHeader { avatar_text, avatar_color_key, title, subtitle_mono, right_note }
// Content gains: #[serde(default)] cards: Option<CardGrid>,
//                #[serde(default)] header: Option<ContextHeader>,
//                #[serde(default)] modal_point: String,         // ModalActions binding
//                #[serde(default)] context: HashMap<String,String>
//   `Content.context` is the interpolation source for modal_point entries — the
//   owner's detail render fills it ({"id": "character:<uuid>"}), same convention
//   as RowMeta.context/Card.context. Without it the modal footer entry
//   ("inventory?owner={id}") has no {id} source (the portal can't derive it from
//   the request's `owner` param — it's domain-blind).
// Item gains:     extensions: Vec<ExtensionEntry>  (Item::local sets vec![]; new
//                 builder Item::with_extensions(mut self, v) for contributors)
// ItemData gains: #[serde(default)] extensions: Vec<ExtensionEntry>
// AdminData:      async fn admin_data(&self, params: Params) -> Result<ItemData, Error>
```

Points declared for the baseline:

- `accountsapi::admin::PLAYERS_ROW_MENU` — `"accounts.players.row-menu"`,
  `EntityMenu`, keys `["id"]` (value `player:<uuid>`).
- `charactersapi::admin::CHARACTERS_CARD_MENU` — `"characters.characters.card-menu"`,
  `EntityMenu`, keys `["id"]` (value `character:<uuid>`).
- `charactersapi::admin::CHARACTER_MODAL_ACTIONS` —
  `"characters.character-modal.actions"`, `ModalActions`, keys `["id"]`.

Contributions for the baseline: characters → PLAYERS_ROW_MENU ("View Characters",
Navigate, `characters?owner={id}`); inventory → PLAYERS_ROW_MENU ("View Inventory",
Navigate, `inventory?owner={id}`); inventory → CHARACTERS_CARD_MENU ("View
Inventory", Modal, `inventory?owner={id}`); inventory → CHARACTER_MODAL_ACTIONS
(same entry, Modal).

Navigation/breadcrumb: back-chip ("‹ Players") is ADMIN chrome derived from a
`from=<slug>` query param the portal auto-appends when rendering extension links;
the owner never knows its navigational parent. The entity header (avatar + name +
mono subtitle) is owner-rendered `ContextHeader` (the owner looked the entity up
anyway). Crumb becomes `"<section> · <header.title>"` when a ContextHeader is
present (mockup: "Players · VoidR4nger").

Modal: an entry with `present: Modal` renders as
`hx-get="/admin/<interpolated>&partial=modal" hx-target="#modal-root"`. The portal's
`GET /admin/{slug}?partial=modal&...` returns a fragment (modal chrome: header from
`Content.header`, body from kpis/table/cards, footer = ModalActions extensions
matching `Content.modal_point` + Close). Works identically local and remote because
params now cross the wire. Without JS, Modal entries degrade to plain Navigate
anchors (progressive enhancement).

**API-dependency legality (verified by the plan reviewer):** owner api crates
(`accountsapi`, `charactersapi`) gain a types-only dependency on `adminapi` for the
`ExtensionPoint` consts. archcheck's `FORBIDDEN_API_DEPS` covers transport crates
only (tools/archcheck/src/main.rs:92-94) — no api→api ban; rule 19 is irrelevant
(consts, not new `Slot::new`). Still run `cargo run -p archcheck` at the end of
Step 1 as routine.

**Foreign-params tolerance (new contract obligation):** `resolve_items` already
forwards the request's params to EVERY item's fetch (modules/admin/src/lib.rs:1393)
— once params cross the wire, `/admin/characters?owner=player:X` delivers
`owner=player:X` to inventory-svc, apikeys-svc, etc. on every request. Step 1 adds
to the `AdminData::admin_data` doc comment: *implementations MUST tolerate
arbitrary/foreign params and never `Err` on them* (an `Err` becomes an error card
on an unrelated page, split-only). Malformed `owner` in characters/inventory
renders error-content or the default view (inventory already does this —
modules/inventory/src/admin.rs:81-90; characters' new dispatch mirrors it).

## Dispatch & effort (fixed with the plan)

- Implementation subagents (steps 1–6): effort **think hard**, embedded in every
  prompt (effort does not inherit). Lanes name concrete models per user directive:
  `[opus]` = Opus subagent (all steps 1–6 — Fable-tier steps degraded to Opus for
  this rollout; UI step 3 is explicitly the "opus starczy" lane), `[sonnet]` =
  Sonnet subagent (no step uses it; UI is NEVER dispatched to sonnet).
- Step 7 runs `[inline]`.
- One commit per step; Conventional Commits; `Co-Authored-By` trailer = executing
  model (Opus 4.8 for steps 1–6); trailer audit after the rollout.
- Review each step's diff against the plan before dispatching the next.

## Steps

### Step 1 — Contract: adminapi + adminrpc + owner consts + impl sweep  `[opus]`

**(a) What:** `api/admin/api/src/lib.rs` (all new types above; `AdminData` trait
change; extend the serde roundtrip test), `api/admin/rpc/src/lib.rs`
(`admin_remote_factory` stops discarding `_params` — forwards to
`Client::admin_data(params)`; `fetch_remote_admin` signature), owner consts in
`api/accounts/api` and `api/characters/api` (new `pub mod admin`), signature sweep
over all 7 `AdminData` impls (accounts, characters, inventory, config, apikeys,
audit, scheduler — `modules/<m>/src/admin.rs` or lib.rs; all but inventory ignore
the new param for now), plus the non-impl call/construction sites the sweep must
touch: `modules/apikeys/src/admin_tests.rs:224` (`svc.admin_data().await`),
`modules/admin/src/tests.rs:103,138` and `api/admin/rpc/src/lib.rs:89` (`Item`
struct literals break when `extensions` is added). Bless BOTH value gates:
`cargo run -p verifyctl -- --bless-public-api` (re-bless `adminapi.txt`,
`accountsapi.txt`, `charactersapi.txt`) AND
`cargo run -p verifyctl -- --bless-contract-golden` (the `AdminDataRequest` body
shape changes → `docs/reference/contract-golden/contracts.txt` diff would FAIL the
BLOCKING contract-golden stage).

**(b) Why now:** every later step compiles against this contract; the trait change
must land with the impl sweep in one step to keep the workspace green.

**(c) How:** archcheck api→api probe first (see risk above). `Item` gains a public
field — update `Item::local` to set `extensions: vec![]`, add
`with_extensions(self, Vec<ExtensionEntry>) -> Item`. `ItemData.extensions` and all
new Content fields get `#[serde(default)]` (old peers' payloads keep deserializing).
`Present`/`ExtensionKind` derive Clone/Serialize/Deserialize + `Default` on
`Present::Navigate`. Run `cargo test -p adminapi -p admin` (one test invocation at
a time — events-plane advisory lock).

**(d)** `[opus]`

### Step 2 — Portal engine: merge, interpolation, partial mode  `[opus]`

**(a) What:** `modules/admin/src/lib.rs` + `modules/admin/src/tests.rs`.

**(b) Why now:** the rendering data model (what the template sees) must exist
before the visual step 3 rewrites the template against it.

**(c) How:**
- `resolve_items(st, params)` already forwards params into every fetch closure
  (lib.rs:1393 — the wire gap was the factory, fixed in Step 1; no change here).
  NEW: collect `Vec<ExtensionEntry>` from every resolved item (local
  `item.extensions` + remote `ItemData.extensions`) into a per-request
  `HashMap<String point_id, Vec<ExtensionEntry>>`, sorted by `(priority, label)`.
- `page_view`: for a `Table` with non-empty `menu_point` + `row_meta`, build
  per-row menu view-models: native `MenuEntry`s → separator → matching extensions.
  **Interpolation is uniform across the merged menu** — natives and extensions
  alike go through the same `interpolate(template, ctx)` helper against
  `row_meta.context`/`Card.context`/`Content.context` (modal footer) — unknown
  `{key}` renders the entry SKIPPED with a warn log, not a panic. Append
  `from=<current page>` to every menu link. Entries with `present: Modal` get an
  `hx_url` (`...&partial=modal`) alongside the plain href fallback.
- `from` format (exact): value = current slug + optional query, e.g.
  `characters?owner=player:X` urlencoded as one param value. On read: split at the
  first `?`; validate the slug half against resolved slugs (unknown ⇒ no back
  chip, never reflect raw input); re-serialize the query half through
  `form_urlencoded` before emitting the href. `back: Option<BackNav{label,href}>`
  in `PageData`; label = the slug's item label.
- Crumb: `"<section> · <header.title>"` when `Content.header` is present.
- Partial mode: `GET /admin/{slug}?partial=modal` returns a fragment template
  (`modal.html` — new minijinja template: modal chrome, no shell) with the same
  gate/session checks as the full page; non-htmx (no-JS) requests to the same URL
  still work — htmx sets `HX-Request` header; without it, fall back to the FULL
  page render so a degraded anchor click is never a naked fragment. Expired
  session on an `HX-Request` fragment: respond `HX-Redirect: /admin/login`
  (htmx performs a full-page redirect) instead of the normal 303 — otherwise the
  login page would get swapped INTO `#modal-root`.
- Unit tests (in `tests.rs`, never inline): merge ordering (native→sep→extensions,
  priority sort), interpolation (hit, missing-key skip; uniform for natives and
  extensions; modal footer from `Content.context`), `from` append + slug
  validation + query re-encoding, modal fragment vs full-page fallback,
  `HX-Redirect` on expired session for fragments, remote extensions merged from
  `ItemData`, serde default (old ItemData JSON without `extensions` resolves).

**(d)** `[opus]`

### Step 3 — Visual layer: restyle to mockup + htmx + admin.js  `[opus]`

**(a) What:** `modules/admin/src/theme.css` (rewrite), `admin.html.tmpl` (rewrite),
new `modal.html.tmpl`, new `modules/admin/src/admin.js`, vendored
`modules/admin/src/htmx.min.js`, router additions (`/admin/admin.js`,
`/admin/htmx.min.js` served like `theme.css`), `login.html.tmpl` (add the Google
Fonts link — known gap).

**(b) Why now:** template renders the step-2 view-models; doing visuals after the
engine avoids rework.

**(c) How:** **Mockup is the exact spec — lift every value 1:1 from
`UILayout/GameOps Admin.dc.html`** (token/component extraction already in the
research doc §6–7: page bg `#060708`, sidebar `#0a0b0d`, header `#08090b`, cards
`#0e0f12`, borders `#191c21`/`#1b1e24`, row hairline `#15171c`, menu container
`#12141a` border `#262b35` radius 10 shadow `0 16px 40px rgba(0,0,0,0.6)`, modal
overlay `rgba(4,5,6,0.72)`+blur(3px) container 560px radius 16, rarity colors,
avatar cycling palette, badge rgba(…,0.12) backgrounds, fonts/sizes per role —
full tables in the research doc; read the mockup file directly when in doubt, never
quote colors from memory). New CSS components: dropdown menu (+danger/inert item,
separator), modal, card grid, context header ("‹" chip, avatar, mono subtitle),
toolbar, styled checkbox-group (existing gap), button variants (amber primary,
ghost secondary, danger). Keep `Cell.badge` class names (`green/amber/red/blue/grey`)
— only values change. `admin.js` (~50 lines, vanilla): `⋯` toggle via `data-menu`
delegation, outside-click close, Escape closes menu/modal, `#modal-root` clear on
overlay click; htmx handles fragment swaps. htmx: download pinned 2.x
`htmx.min.js` once, commit the file (no npm). CSP unchanged (`default-src 'self'`
allows same-origin files); NO inline `<script>`/handlers anywhere. The hardcoded
"COMING SOON" nav block is restyled per mockup as static decorative entries
(Analytics, Live Ops, Economy, Leaderboards, Moderation) — kept, per scope decision.

**(d)** `[opus]` — UI translation lane; per standing user rule this is
NEVER dispatched to the mechanical lane.

### Step 4 — Module content: accounts, characters, inventory  `[opus]`

**(a) What:** `modules/accounts/src/admin.rs` (+lib wiring),
`modules/characters/src/admin.rs` + `store.rs` (+lib), `modules/inventory/src/
admin.rs` (+lib).

**(b) Why now:** needs the contract (1) and portal semantics (2); step 3 is
independent of it but both must exist before split-proof (6).

**(c) How:**
- **accounts** (Players page): table gains `menu_point =
  PLAYERS_ROW_MENU.id`, `row_meta` per player (`context: {"id":
  "player:<uuid>"}`, native menu: Edit `enabled:false`, Delete `danger,
  enabled:false` — inert per mockup). Keep real columns (PLAYER, PLAYER ID,
  PROVIDERS, STATUS, CREATED) styled per mockup patterns (avatar chip from the
  cycling palette, mono id, status badge).
- **characters**: `admin_content` becomes param-dispatching like inventory's:
  no params → current all-characters table (unchanged data); `?owner=player:<uuid>`
  → `ContextHeader` (player short id — characters doesn't know account names; title
  = the uuid short form) + `CardGrid` (`menu_point = CHARACTERS_CARD_MENU.id`,
  card per character: class/level, native menu View (`present: Modal`, link
  `characters?owner=character:{id}`) + inert Edit/Delete, context `{"id":
  "character:<uuid>"}`); `?owner=character:<uuid>` → character-detail Content
  (header + kpi stats, `modal_point = CHARACTER_MODAL_ACTIONS.id`, `context =
  {"id": "character:<uuid>"}` — the modal footer's interpolation source) — this
  is what the modal fetches. Malformed `owner` → error-content, never `Err`
  (foreign-params tolerance). Store: `list_by_player` already exists
  (`modules/characters/src/store.rs:63` — reuse); add `get(id)` only if missing. Contribute the "View Characters" entry via
  `Item::with_extensions` AND in `admin_data` (`ItemData.extensions`) — one shared
  `fn extension_entries() -> Vec<ExtensionEntry>` so local/remote can't drift.
- **inventory**: contribute the three entries (players menu Navigate, card menu
  Modal, modal actions Modal) via the same shared-fn pattern; owner-detail view
  gains `ContextHeader`; rarity/qty cell styling data per mockup where real fields
  exist. No new store methods expected (owners list + per-owner list exist).
- Rarity/POWER/GEAR: characters/inventory schemas may not have these fields — use
  ONLY real fields (class, level, created, item name/qty); card stat row shows
  what exists. No fictional data.
- Tests per module in `src/tests.rs`: extension entries present on Item and
  ItemData, scoped render returns cards/header, character-detail sets modal_point.

**(d)** `[opus]`

### Step 5 — Validator: admincheck (topiccheck-shaped)  `[opus]`

**(a) What:** new `tools/admincheck` (workspace member) + advisory verify stage in
`tools/verifyctl` (stage manifest, tools/verifyctl/src/stages/mod.rs), CLAUDE.md
command list line (`cargo run -p admincheck` — NEVER the retired `verify.sh`/
`verify.ps1` names; the blocking `docs_current` stage bans them in root docs).

**(b) Why now:** after 1–4 there are real declarations to validate; before
split-proof so drift fails fast in verify.

**(c) How:** copy topiccheck's harness shape (`tools/topiccheck/src/main.rs`
`observe()` L348–395): for each `checkmodules::DeploymentProfile::{Monolith,Split}`
process, build `register`→`init` with lazy pool, then read
`ctx.slots().contributions(adminapi::SLOT)`. Declared points = compile-coupled
enumeration (direct imports of `accountsapi::admin::PLAYERS_ROW_MENU`, etc. — a
renamed const breaks the tool at compile time, per the `defined_topics()` pattern).
Checks (pure data only — renders do DB I/O and are NOT invoked): (1) every
`ExtensionEntry.point` targets a declared point id; (2) `present` compatible with
point kind (`ModalActions` ⇒ Modal); (3) every `{key}` in `link` ⊆ point's
`context_keys`; (4) duplicate (point, label) collisions across contributors.
**Known, accepted gap (recorded here deliberately):** `menu_point`/`modal_point`
values live inside render OUTPUT, which admincheck can't see without DB I/O, and
the domain-blind portal can't validate them either (it never imports owner consts)
— a typo'd binding means extensions silently don't appear. Coverage for the
baseline points comes from split-proof `[ADX1/ADX3]` asserting the entries ARE
rendered; any future point gets its own split-proof assertion per the existing
"extend split-proof when you add a flow" rule. Report table + exit non-zero on
findings under `--strict` (advisory default), mirroring topiccheck's contract.
Register as ADVISORY stage next to topiccheck in verifyctl.

**(d)** `[opus]`

### Step 6 — split-proof named assertions  `[opus]`

**(a) What:** `tools/splitproof/src/main.rs` (new `[ADX*]` block near `[AD3a]`
L1338), plus fixture reuse of the existing `[AD0]` proof character.

**(b) Why now:** last code step — proves the at-risk topology (split) end-to-end,
per the never-monolith-only rule.

**(c) How:** through gateway-svc with the authenticated admin session:
- `[ADX1]` GET `/admin/players` (accounts page slug) → 200, body contains BOTH
  cross-process extension labels ("View Characters", "View Inventory") — proves
  extensions ride `admin.adminData` from characters-svc and inventory-svc into
  admin-svc's render.
- `[ADX2]` register a SECOND player + character in the ADX block (every character
  splitproof seeds today belongs to the one test player — main.rs:1191-1226, so
  the negative has no fixture without this). Then GET
  `/admin/characters?owner=player:<AD0 player uuid>` → 200, body contains the AD0
  proof character name AND NOT the second player's character name — proves Params
  cross the wire (scoped remote render).
- `[ADX3]` GET `/admin/characters?owner=character:<AD0 char uuid>&partial=modal`
  with `HX-Request: true` header → 200, fragment (no `<aside class="sidebar"`),
  contains modal chrome + "View Inventory" (ModalActions extension from
  inventory-svc); same URL WITHOUT the header → full page (degradation proof).
- Adjust `[AD3a]` expectations if the characters page markup changed (it asserts a
  marker name — should survive; verify).
- Monolith parity re-run covers the same assertions by construction (split-proof
  re-runs the same checks against the monolith front).

**(d)** `[opus]`

### Step 7 — Full verification + visual smoke  `[inline]`

**(a) What:** run the safety net + eyeball the real portal.

**(c) How:** `cargo clippy --workspace --all-targets -- -D warnings`;
`cargo test --workspace` (ONE invocation, never concurrent — advisory lock);
`cargo run -p archcheck`; `cargo run -p topiccheck`; `cargo run -p admincheck`;
`cargo run -p verifyctl -- --all` (public-api + contract-golden must PASS against
the re-blessed baselines; NOT the retired `verify.ps1` script name); split-proof
via the verify stage. Then boot the monolith (`cargo run -p devctl -- up
monolith`), open
`/admin` in a browser: players row menu opens/closes, View Characters navigates
with back-chip, card menu opens character modal, modal footer View Inventory swaps
to inventory list, no console/CSP errors. Screenshot for the record. Commit
boundaries: one commit per step (Conventional Commits, trailer per executing
model), review each diff against its step before dispatching the next.

## Verification summary

- Unit: adminapi serde defaults; portal merge/interpolation/partial tests (step 2);
  per-module content tests (step 4).
- Static: archcheck (api→api edge sanity), topiccheck unchanged, admincheck (new),
  public-api against re-blessed `adminapi/accountsapi/charactersapi` baselines,
  contract-golden re-blessed (AdminDataRequest body shape), docs_current clean
  (no retired command names introduced).
- Live: split-proof `[ADX1–3]` (cross-process extensions, scoped remote render,
  modal fragment + degradation) + monolith parity + manual browser smoke.

## Explicitly out of scope

- Op-backed mutating menu entries (Edit/Delete stay inert) — later phase.
- Dashboard/Servers pages, functional search/filters/Export — static/decorative.
- match/rating/leaderboard admin surfaces.
- Entity-kind (per-entity, cross-page) extension addressing — revisit if entries
  duplicate across pages.

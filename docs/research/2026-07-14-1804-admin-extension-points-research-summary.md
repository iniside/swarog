# Admin extension points + restyle — research summary

Date: 2026-07-14. Method: 6 parallel research subagents (non-overlapping angles:
contract+portal render, remote fan-out, contributor survey, theme/CSS, mockup spec,
slots+validators), synthesized in the main model; one subagent claim
(match/rating/leaderboard "don't exist") was verified and corrected inline. This doc
feeds the upcoming plan for: cross-module admin extension points (row menus, modals,
contextual sub-pages) + full restyle to the new `UILayout/GameOps Admin.dc.html`
color scheme.

## 1. The contract today (`api/admin/api/src/lib.rs`)

- `SLOT: contrib::Slot<Item> = Slot::new("admin.item")` (L29) — the only admin
  contribution point. `Item` (L173–192): `id`, `section`, `label`,
  `render: Option<RenderFn>`, `remote_fetch: Option<RemoteFetchFn>`,
  `remote_submit: Option<RemoteSubmitFn>` — local vs remote discriminated by which
  closures are set.
- `Content` (L227–235) is a **singleton struct, not a block list**: `kpis: Vec<Kpi>`,
  `table: Option<Table>`, `form: Option<Form>`. At most one table + one form per page.
- `Cell` (L336–345): `text`/`badge`/`mono`/`link` — `link` is the only drill-down
  seam (`<a href="/admin/{slug}?query">`), page-owner-authored.
- `Params = HashMap<String,String>` + `param()` (L82–88); local `render(&Params)`
  receives the request's query params (drill-down works locally).
- `AdminData`/`AdminSubmit` `#[rpc(prefix="admin")]` traits (L44–76): the wire
  contract; glue in `adminrpc`. `SubmitOutcome`/`RevealItem` (L139–151) — show-once
  secrets after submit.

## 2. Portal render pipeline (`modules/admin/src/lib.rs`)

- GET `/admin/{slug}` → `gate` (session) → `resolve_items` (L1386–1423): reads
  `slots.contributions(adminapi::SLOT)` **per request** (live, no snapshot; captured
  `ctx.slots()` at init L386), fetches every remote item fresh per request
  ("fine, /admin is low-traffic" L1385; N-contributor fan-out multiplies this).
  `ItemError::Absent` ⇒ item dropped; other error ⇒ error card.
- Slugs minted only here: `slugify(label)` + `-2/-3` dedupe (L1401–1412). Sidebar
  order = first-seen section order = module registration order (`build_groups`
  L1497–1520). **No ordering/priority field exists anywhere.**
- Local render is lazy, with query params: `page_view` (L1427–1493) calls
  `item.render(&params)` — this is how inventory's `?owner=` drill-down works.
- POST flow (L1118–1214): CSRF before editability decision; local submit via
  allowlisted `collect_submit_params`; remote submit via `remote_submit` (params DO
  cross the wire); uniform durable `admin.action` audit; PRG with one-shot reveal.
- Template: ONE minijinja shell `admin.html.tmpl` (113 lines) + `login.html.tmpl`.
  Content area is a fixed stack: err → reveal → kpis → table → form. `render_cell`
  macro handles link/badge/mono. A hardcoded "COMING SOON" static nav block
  (tmpl L36–64) lists modules by hand — to be replaced/deleted by the redesign.
- **Zero JavaScript** anywhere; CSP `default-src 'self'` via `security_headers`
  (L510–524). Dropdown menus + modals need either CSS-only patterns
  (`<details>`, `:focus-within`) or a deliberate CSP/JS decision.

## 3. Split topology — the wire (`api/admin/rpc/src/lib.rs`)

- `admin_remote_factory(provider)` (L72–99) contributes a remote `Item` whose fetch
  dials `admin.adminData`. **`AdminData::admin_data()` takes NO arguments** — the
  portal passes `Params` into `RemoteFetchFn`, but the factory closure discards them
  (`_params`, L76). **Drill-down params do not cross the wire on GET today**; a
  remote item renders identically regardless of query string. `AdminSubmit::
  admin_submit(id, params)` DOES carry params (POST path) — precedent for adding a
  params-carrying fetch.
- `cmd/admin-svc/src/lib.rs:14-36`: admin-only stubs (`admin_stub`) for exactly 7
  providers: characters, inventory, config, accounts, audit, scheduler, apikeys.
  Peer addrs from `<PROVIDER>_EDGE_ADDR` env in main. Monolith `cmd/server`: all
  local, `AdminData` glue unused.
- `ItemData` derives Serialize/Deserialize — an additive `extensions: Vec<...>`
  field with `#[serde(default)]` flows through generated glue with no glue-crate
  change. It must be **static provider-side data** (returned unconditionally),
  unless we also extend `admin_data()` to take `Params` (breaking `#[rpc]` change +
  every provider impl + bless).
- split-proof (Rust binary `tools/splitproof/src/main.rs`, no .sh): `[AD3a]`
  gateway→admin-svc→characters-svc QUIC render assertion; `[AD3b]` apikeys remote
  form; `[AD6a–g]` remote submit path. New extension-point flows need analogous
  named assertions.
- public-api gate: `adminapi.txt` baseline exists and any `adminapi` change requires
  `--bless-public-api`; `adminrpc` is categorically NOT baselined (only
  `api/<d>/{api,events}` discovered).

## 4. Contributors today (who must not break; who plays in the first scenario)

7 contributors: accounts (`Identity/Players`, 3 KPIs + table, no links, no form),
characters (`Game Content/Characters`, 1 KPI + table, PLAYER column is inert
`Cell::mono(player_id)`), inventory (`Game Content/Inventory`, **the only Params
consumer + only Cell.link user**: self-link `inventory?owner=<type>:<uuid>`,
two-view owners-list/detail pattern, `modules/inventory/src/admin.rs:32-113`),
config (form + CAS `_expected_revision`, snapshot from RwLock cache — the only
render without block_in_place), apikeys (richest form: Select/CheckboxGroup,
actions dispatch, RevealItem), audit, scheduler (plain tables).
**match/rating/leaderboard modules EXIST but have no admin surface at all**
(verified: zero `adminapi` references; subagent claim that they don't exist was
wrong). All renders except config bridge async via
`tokio::task::block_in_place(|| Handle::current().block_on(...))` — fresh DB read
per render.

- Entity-id conventions: inventory's `"player:<uuid>"` / `"character:<uuid>"`
  composite is the ONLY typed entity-ref precedent (matches our planned context
  refs). accounts/characters render bare uuids.
- There are **zero cross-module drill-downs today** — the OCP violation is
  prospective, not committed. The extension-point contract should own the entity-ref
  convention centrally before ad-hoc links diverge.

## 5. Slot mechanics + validator template

- `contrib::Slot<T>` (core/contrib/src/lib.rs:13-122): typed by `T`, keyed by
  `&'static str`; `Mutex<HashMap<&str, Bucket{TypeId, Box<dyn Any>}>>`; contribute
  panics on type mismatch; `contributions` clones the Vec out. **Per-process only —
  proven**: no serde anywhere in contrib, every `Context` builds a fresh
  `Slots::new()`, and `AdminData` RPC exists precisely because slots don't cross
  the wire. No phase guard on read timing (convention: contribute in init, read at
  serve time).
- archcheck rule 19 (`slot_constructor_violations`, tools/archcheck/src/main.rs:
  1205-1249): token-grep tripwire — `contrib::Slot::new(` allowed only in
  `SLOT_OWNER_FILES` (main.rs:54-61). New extension-point slot constants must be
  added there (they'll live in owner api crates ⇒ extend the exemption list or add
  a rule-20 analog).
- topiccheck template (tools/topiccheck/src/main.rs): compile-coupled enumeration of
  defined statics (`defined_topics()` L183-200) + **runtime harness** — builds each
  profile's processes (from `checkmodules::DeploymentProfile`, 12 split processes)
  through `register`→`init` with a RecordingTransport, then validates observed vs
  declared. An extension-point checker reuses this shape: run init per process, read
  `ctx.slots().contributions(...)` after build, diff targeted-point ids/kinds/
  context-keys against owner declarations. Also: `contract-golden --bless`
  (VALUE-level baseline, docs/reference/contract-golden/contracts.txt) is the
  pattern for pinning contribution VALUES — nothing baselines slot values today
  (public-api sees only signatures).

## 6. Theme / restyle groundwork

- Entire visual layer = `modules/admin/src/theme.css` (108 lines, tokens in `:root`
  L3-8, served at `/admin/theme.css`, `include_str!` lib.rs:88) + inline SVGs in
  `admin.html.tmpl`. Restyle touches only these two files, zero per-module Rust.
- Current tokens vs mockup: page bg `#0f1116` → `#060708`, sidebar `#13151b` →
  `#0a0b0d`, header `#111319` → `#08090b`, cards `#161920` → `#0e0f12`, borders
  `#20242c/#232832` → `#191c21/#1b1e24`, row divider `#1d2129` → `#15171c`. Accents
  keep: amber `#f5a524`, green `#34d399`, red `#f5604d`, blue `#7aa2f7`. Badge
  translucent bgs `rgba(accent, 0.10-0.12)` (mockup: 0.12). Hardcoded non-token
  colors to hunt: `#d97706` gradient stop, `#3d8be0` avatar, `#2b303a` scrollbar
  (mockup `#22262e`), search/hbtn `#181b22/#262b35`, etc.
- Fonts unchanged: Public Sans + IBM Plex Mono via Google Fonts link (tmpl L7-9);
  login.html doesn't re-link fonts (falls back to system-ui on cold visit) — known
  gap. `.checkbox-group` classes used but unstyled — known gap. Only one `.btn`
  variant (solid green) — mockup needs amber primary + ghost secondary + danger.
- demos/webui is fully separate (own inline styles) — restyle does not touch it.

## 7. Mockup spec (extracted, exhaustive version in plan Context later)

Full token/component/state extraction done from `UILayout/GameOps Admin.dc.html`
(753 lines) — key deltas beyond §6 colors:

- New components with exact specs: dropdown row menu (`#12141a` bg, border
  `#262b35`, radius 10, shadow `0 16px 40px rgba(0,0,0,0.6)`, item 13px `#cfd3da`,
  hover `rgba(255,255,255,0.05)`, danger `#f5604d`, separator `#22262e`); modal
  (overlay `rgba(4,5,6,0.72)` + blur(3px), 560px container radius 16, shadow
  `0 30px 80px`); character cards grid (3-col, rarity-colored avatar chips);
  rarity badges (Legendary `#f5a524`, Epic `#a563d6`, Rare `#5fa8d6`, Common
  `#9aa0ac`, bg `rgba(c,0.14)`); sub-page context header ("‹ Players" chip +
  44px avatar + name + mono `#uid · Level N · Region` + right count); toolbar
  (search + filter chips + Export amber button); per-row avatar chips with cycling
  palette `['#e0823d','#3d8be0','#4caf8e','#a563d6','#d6635f','#5fa8d6']`.
- Menus verbatim: player row = [Edit, Delete, — , View Characters, View Inventory];
  character card = [View, View Inventory, — , Edit, Delete]. Ordering differs
  between the two IN THE MOCKUP (intentional per source) — decision needed whether
  to normalize (owner-first + separator + extensions) or follow mockup exactly.
- State model: pages {dashboard, players, servers, characters, inventory};
  `viewPlayer` context (full player object) carried into sub-pages; crumb becomes
  `"Players · <name>"`; modal state `{type: view|inv, char}`; modal 'view' footer
  "View Inventory" switches modal type in place. Sidebar has decorative
  non-functional entries (Analytics, Moderation...) — visual-only placeholders.
- Mockup simplification to NOT copy: characters/inventory data arrays are global,
  not per-player — backend obviously scopes by the clicked player.

## 8. Synthesis — what the plan must decide/do

Gaps confirmed (each maps to a plan work item):
1. **Extension entries** — new declarative data on the contribution path
   (`ItemData.extensions` additive `#[serde(default)]`; local Items get the same
   field or a parallel slot). Point ids as typed constants in owner api crates
   (archcheck rule-19 list extension). Entity-ref convention: adopt inventory's
   `"<type>:<uuid>"`.
2. **Params over the wire on GET** — remote drill-down/sub-pages NEED it (a
   player-scoped characters sub-page rendered by characters-svc requires
   `?owner=player:X` to reach `admin_data`). Decision: extend `AdminData` to
   `admin_data(params)` (breaking rpc change + bless + all 7+ impls) vs add a
   second versioned method. Without this, extension points ship but remote
   sub-pages silently render unscoped — unacceptable (never monolith-only).
3. **Content model growth** — cards/grid block, per-row/card menus (owner-native +
   extension-merged), modal presentation, contextual sub-page header, block list
   vs singleton Content, toolbar. All additive on `Content`/new structs +
   template/theme work.
4. **Menu/modal interactivity under CSP** — decision: CSS-only (`<details>`/
   `:focus-within`) vs minimal JS + CSP loosening (script-src 'self' with a static
   .js file is compatible with `default-src 'self'` — worth noting inline JS is
   NOT).
5. **Ordering** — no priority field exists; extension entries need section +
   priority; sidebar ordering may stay contribution-order.
6. **Validation** — extension-point checker: topiccheck-style runtime harness over
   `checkmodules::DeploymentProfile` + owner-declared points; optionally a
   contract-golden-style value baseline for contributed entries. Loud fail on
   entry→missing point, kind mismatch, undeclared context keys.
7. **Restyle** — theme.css token swap + new component CSS + template rework; the
   "COMING SOON" hardcoded block dies; contributors' Content data (columns, KPIs)
   largely unchanged.
8. **Verify surface** — adminapi public-api re-bless; new split-proof named
   assertions (row-menu entries present cross-process, remote sub-page scoped by
   params, modal fetch); match/rating/leaderboard remain without admin surface
   (out of scope unless we add pages for them).

Decisions (user, 2026-07-14):
- `AdminData` params: EXTEND the existing `admin_data` method to take `Params` —
  no side-channel second method ("nie ma powodów tworzyć hacków i funkcji na
  boku"). Breaking `#[rpc]` change + `--bless-public-api` + all provider impls.
- Menus/modals: **htmx (vendored single min.js file, no npm/build step) + ~50
  lines vanilla JS for the ⋯ menu toggles**. Served like theme.css via
  `include_str!`; NO CSP change needed (`default-src 'self'` already allows
  same-origin script files, blocks only inline). Avoid `hx-on:` (Function()
  eval). Modal = htmx fragment fetch of another module's page render
  (`?owner=<type>:<id>` + a partial flag) swapped into modal chrome.
  Progressive enhancement: without JS everything degrades to links/POSTs.
- Menu ordering: NORMALIZE across all menus — owner-native entries first,
  separator, then extension entries (do not copy the mockup's per-menu
  inconsistency).
- Scope: the mockup's ACTIVE pages only — Players / Characters / Inventory (+
  theme/restyle). Dashboard, Servers, and decorative sidebar entries stay
  static/decorative if present at all.

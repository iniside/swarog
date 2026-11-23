# Step 23 (config): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1, on the layout of `docs/plans/2026-11-23-step22-scheduler-checklist.md`. HEAD at extraction: `2d76012f`. Mockup page: `UILayout/redesign/config.html` (`P.config`). Scope: `plan:2413-2441` (Step 23), the Na0/Na1/Na2 template (`plan:1935-1968`), owner decisions D1-D7 (2026-11-23, §11).

Anchor marks: **(V)** = read at the cited lines in this extraction; **(H)** = hypothesis (inferred, cited from another doc, or predicted).

Builder lines config owns (all V):
- Page `P.config` `pp:88-109`. Crumbs Platform / Config & flags (`pp:89`). Head (`pp:107`): title "Config & flags", subtitle "Typed runtime configuration. Every save creates a revision.", head action `btn primary` `plus` "Key" with **no onclick**. Layout: `div.grid.g-21`, with Keys on the left and a `div.stack` of Edit, then Revisions, on the right (`pp:107`).
- Default selection: `P.cfg || "store.featured_rotation"` (`pp:91`), a client-side variable the key link sets (`pp:95`).
- Keys panel (`pp:92-95`): `pg("config","DataTable")`, `flush`, title "Keys", meta = key count. Table: `pager: false`, one filter chip "Type: any", search placeholder "Key prefix". Columns:
  - Key: `a.mono` link that selects the key;
  - Type: `span.tag`;
  - Value (`wrap`): `span.toggle(.on)` for bool, else `span.mono`;
  - Rev (`num`);
  - Updated: `span.row` holding `span.dim` time plus `P.actor(by)`.
  - The selected row carries `sel`.
- Edit panel (`pp:96-100`): `pg("config","Form")`, title `Edit <span class="mono">key</span>`, meta `rev N`, submit "Save revision", note "Compare-and-swap: saves only if still rev N.". Fields:
  - Value: a textarea for json, a toggle for bool, otherwise a mono text input; `typeName` = the type;
  - Change note: placeholder "Why? (stored with the revision)".
- Revisions panel (`pp:101-106`): `pg("config","Timeline")`, `flush`.
  - Preamble: muted "Pending change vs rev N", then a `div.code` with `span.diff-del` / `span.diff-add` (`pp:101-102`).
  - Three entries:
    - (1) the current value: `t` = sel.at, "rev N by actor", meta `span.mono` value, icon `edit`, tone `accent` (`pp:103`);
    - (2) and (3) rev N-1 and N-2 by marta.k / jonas.l, meta `span.mono` old value plus an `a` "restore" (`pp:104-105`).
- Registry: nav `reg:29` (platform, id config, "Config & flags", icon `sliders`, no counter) (V).
- Fake rows `dj:197-203` (V), seven keys covering every type: bool, int ×3, json, float, string. One row is "by scheduler" (`dj:201`).
- Note: the research doc cites the Change note at `pp:96` and the preamble at `pp:99-100` (`gaps:74-75`). The lines read here are `pp:99` and `pp:101-102`. The doc's numbers are off by about 3 lines; the restore link `pp:104-105` agrees (`todo:420`).

Abbreviations:
- Mockup and research: `pp` = `UILayout/redesign/assets/pages-platform.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `gaps` = `docs/research/2026-11-21-0848-admin-ports-21-24-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.
- config: `cl` = `modules/config/src/lib.rs`, `ct` = `modules/config/src/tests.rs`, `capi` = `api/config/api/src/lib.rs`, `cev` = `api/config/events/src/lib.rs`.
- controlplane and portal: `cp` = `core/controlplane/src/lib.rs`, `ord` = `core/controlplane/src/order.rs`, `act` = `core/admin/src/engine/actions.rs`, `rt` = `core/admin/src/routes.rs`, `frm` = `core/admin/src/render/form.rs`, `ptpl` = `core/admin/src/templates/primitives.html.tmpl`, `mtpl` = `core/admin/src/templates/modal.html.tmpl`.
- apikeys precedent: `aad` = `modules/apikeys/src/admin.rs`, `ast` = `modules/apikeys/src/store.rs`.
- tools: `pages` = `tools/admindiff/src/pages.rs`, `pol` = `tools/conformance/src/policy.rs`, `sp` = `tools/splitproof/src/main.rs`, `spa` = `tools/splitproof/src/apikeys_admin_checks.rs`.
- docs: `todo` = `docs/todo/admin-second-pass.md`, `ctodo` = `docs/todo/config.md`, `pab` = `docs/reference/public-api-baseline/`, `cg` = `docs/reference/contract-golden/contracts.txt`.

Method: targeted reads with line numbers and scoped ripgrep (each a lower bound). No LSP, cargo, crawl, diff, admincheck or splitproof.

## 0. Required checks

**0.1 What the store holds (V).**
- `config.settings(namespace text, key text, value text NOT NULL, updated_at timestamptz DEFAULT now(), PK (namespace, key))` (`cl:65-71`). There is **no type, no per-key revision, no updated_by and no history**.
- `config.revision` is a singleton holding a bigint seeded at 0 (`cl:72-77`).
- The trigger function `config.notify_changed()` (`cl:78-122`), in the writing transaction, in order:
  - (a) `UPDATE config.revision SET revision = revision + 1 … RETURNING` (`cl:96-98`);
  - (b) `pg_notify('config_changed', …)` without the value (`cl:114`);
  - (c) `asyncevents.append_event('config.changed', 1, payload)` with namespace, key, value, operation and revision (`cl:100-106,118`);
  - then `RETURN NULL`.
- The trigger is attached as `AFTER INSERT OR UPDATE OR DELETE … FOR EACH ROW` (`cl:125-128`) through `Ddl::trigger` (`cl:517`).
- Ids: namespace and key each match `^[a-z0-9_]+$` (`cl:200-217`), so **'.' can never occur in either**. `namespace.key` is therefore an unambiguous display key that splits at its only '.'.
- Writers:
  - `Service::set` / `set_tx` (`cl:245-283`) are an **upsert**: `INSERT … ON CONFLICT DO UPDATE SET value, updated_at = now()`.
  - Splitproof writes raw SQL (`sp:987,993,1053,3416,4373,4614,4640`), inserting without `updated_at` or any future column.
- No data on a fresh fleet: no module seeds `config.settings`. A scoped grep for `config.settings` in `tools`, `modules`, `cmd` and `demos` finds only config itself, its tests, inventory tests, archcheck tests and splitproof. demoseed has no config write (H: crawl fleet = 0 keys unless splitproof has run).

**0.2 Reads, RPCs and contracts today (V).**
- The only read is `load_snapshot_from`, a single statement `SELECT r.revision, s.namespace, s.key, s.value FROM config.revision r LEFT JOIN config.settings s ON true` (`cl:360-385`).
- Wire: `configapi::ConfigSnapshot::snapshot` serves `Snapshot{revision: i64, settings: Vec<Setting{namespace,key,value}>}` (`capi:34-49`, impl `cl:423-446`).
- Event: `configevents::Changed{namespace,key,value: Option,operation,revision: i64}` (`cev:31-43`), topic `config.changed` v1, `MinRetention{7}`, plus golden samples (`cev:67-95`).
- Gates pinning these:
  - `cg:43,68-73` (payload fields), `cg:341` (`config.snapshot` wire);
  - `pab/configapi.txt:38-41,63-64`; `pab/configevents.txt:7`;
  - `tools/topiccheck/src/golden.rs:154-158,283`; `golden_tests.rs:245-252`.
- No CAS write and no admin code: config has no `admin.rs` and no `controlplane` dependency; `lib.rs` has no `mod admin` (V: `cl:478-574`).

**0.3 Wiring, both topologies.**
- Monolith: config is hosted by `cmd/server` (H, not re-read). The in-process portal collects `PROVIDER_SLOT` (H, as Step 22 §0.3).
- Split:
  - `cmd/config-svc/src/main.rs:25-32` (V) stands up an edge server (it already serves `config.snapshot`).
  - `app` registers the `cp.*` face from `PROVIDER_SLOT` unconditionally on every edge process: `core/app/src/lib.rs:1032-1035`, cited from the Step 22 checklist and not re-read (H).
  - admin-svc already dials config: `peer_face("config", wiring, "127.0.0.1:9002")` (`cmd/admin-svc/src/lib.rs:28`, V) and `with_peer("config", env_addr("CONFIG_EDGE_ADDR", …))` (`cmd/admin-svc/src/main.rs:33`, V).
- So 23a1 changes no `cmd/*` file: `modules/config/Cargo.toml` (+`controlplane`), `cl` (`mod admin; mod store;` plus the `PROVIDER_SLOT` contribution in `init`, next to the edge contribution `cl:527-560`), new `admin.rs`, `store.rs`, `admin_tests.rs`.
- Order constant: `platform_nav::CONFIG = 40` (`ord:9`, V).

**0.4 The portal's form plumbing (V), basis of D5.**
- `act:40` `FIELD_REASON = "_reason"`; `check_form_keys` already admits `_reason` on every spec, `Form` included (`act:386-397`).
- `build_submission` reads `_reason` for any spec (`act:479`), sets `expected_revision = form.revision` (`act:504`), and enforces `ReasonReq::Required` only for `Confirm` (`act:507-508`).
- `rt:1196-1203` builds `InvokeCall{expected_revision, reason}` from the submission; `act:586,593-611` writes `detail … — reason: <r>` into `admin.action`.
- The only input drawn today is the Confirm modal's `name="_reason"` (`mtpl:19`).
- So a FormSpec reason needs: the spec field, drawing it in the panel form and the modal form, and the `Required` check extended to `Form`.
- A panel form posts as its contribution's single `PanelForm` action (`rt:805-845`, `rt:270-272`). Hidden values come from the stored spec (`act:503`), so a `hidden` key is server-trusted.
- Conflict rendering:
  - `InvokeOutcome::Conflict{current_revision: u64}` (`cp:513-521`) renders "This changed since you opened it (now revision N). Reload the page and try again." (`rt:1323-1325`);
  - `Failed` with `Status::Conflict` renders `CONFLICT_BANNER` (`rt:1326`);
  - both answer HTTP 409 (`act:559-560`).

**0.5 CAS precedent (V).**
- apikeys: `UPDATE apikeys.roles SET …, revision = revision + 1 WHERE name = $2 AND revision = $3`; a row count other than 1 is `WriteError::Conflict` (`ast:366-398`).
- u64 to i64: `form_revision` = `req.expected_revision.and_then(|r| i64::try_from(r).ok())`, else `Error::invalid` (`aad:280-284`).

**0.6 Carriers (all V in `cp`).**
- Requests: `InvokeCall` `:479-495`; `PanelQuery.params` `:446`; `PAGE_PARAM_SEARCH = "q"` `:381`.
- Manifest and page: `Manifest.writes` `:536`; `PageDecl{head_actions, layout, params}` `:598-610`; `Layout::Grid21(Vec,Vec)` `:619`; `ContributionDecl` `:644-656`.
- Actions: `ActionDecl` `:690-712`; `ActionPoint::PanelForm` / `PageHead` `:728-741`; `Then` `:765-770`.
- Forms: `FormSpec{fields, hidden, submit_label, note, revision: Option<u64>, title, modes}` `:784-797`; `Field` `:810-820`; `FieldKind` `:825-842`; `decode_field` `:873-929`; `ReasonReq` `:1061-1066`.
- Tables: `CellKind` `:1104-1125` (`Mono`, `Ref`, `Number`, `Dim`, `User`, `Toggle`); `Cell{link,more,row}` `:1130-1147`; `Column{align,wrap}` `:1149-1160`; `Row.highlight` `:1174`; `FilterKind::Select` `:1198`; `DataTable{filters, search_placeholder, item_noun}` `:1210-1225`.
- Timelines: `TimelineEntry` `:1293-1308`; `Timeline{date_only}` `:1311-1320`.
- Panels: `PanelBody{meta, heading, note, banner, foot, tag}` `:1475-1490`; `Primitive::Form` / `Empty` `:1507-1523`.
- Shared: `fake_hash` `:213`; `MAX_INPUT_VALUE_BYTES = 4096` `:205`.
- Render mapping:
  - `Row.highlight` → `tr.sel` (`ptpl:235`), matching the mockup's `sel` (`bj:161`);
  - `FieldKind` → control and type name (`frm:97-111`): String→input/"string", Int→int/"int", Float→input/"float", Bool→toggle/"bool", Json→textarea mono/"json", Text→textarea/"text". The type names equal the mockup's `typeName` values (`bj:216`).

## 1. Element table

### Page head and shell

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Nav "Config & flags", sliders, Platform | `reg:29` | `NavEntry{section "platform", id "config", page PAGE, order platform_nav::CONFIG (40), counter None}` | ok |
| Crumbs Platform / Config & flags | `pp:89` | portal crumbs | hand-check H1 |
| Title, subtitle | `pp:107` | `PageDecl.title/subtitle` | ok; subtitle text H1 |
| Head "+ Key" `btn primary` | `pp:107` | `setting.create`, `PageHead(PAGE)`, `Emphasis::Primary`, `then: Form` | REAL (D3); mockup has no onclick, so `EXCLUDED_MODALS` (§7) |
| Grid 2:1, Edit and Revisions stacked on the right | `pp:107` | `Layout::Grid21(["keys"], ["edit","revisions"])` | ok (H: the right column draws `div.stack`) |

### Keys panel

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Keys", `flush` | `pp:92` | `ContributionDecl.title` | ok |
| Meta "7" | `pp:92` | `PanelBody.meta` = exact `count(*)` of all keys | REAL; "0" at zero |
| Type filter chip "any" | `pp:93` | `FilterDef{key "type", label "Type", FilterKind::Select(["bool","int","float","string","json"])}` | REAL derived type (D4); select vs chip is a template choice (§7) |
| Search "Key prefix" | `pp:93` | `search_placeholder "Key prefix"`, `PanelQuery.search` = `q` | REAL, `starts_with(namespace \|\| '.' \|\| key, $q)` (no LIKE: `_` is a legal id byte) |
| Key `a.mono` (selects) | `pp:95` | `Cell{Mono, value "ns.key", link Link::Page{config, PAGE, {key, plus the current q and type when set}}}` | REAL; link markup may differ (§7, H) |
| Type `span.tag` | `pp:95` | `CellKind::Ref` (as Step 22's Target) | REAL derived (D4) |
| Value: bool toggle / else mono, `wrap` | `pp:94-95` | bool: `CellKind::Toggle` value "true"/"false"; else `CellKind::Mono`; `Column.wrap: true` | REAL |
| Rev, `num` | `pp:94-95` | `CellKind::Number`, `Align::Right`, value = row `revision` | REAL after 23s (D1) |
| Updated: dim time + actor in `span.row` | `pp:95` | `Cell{Dim, time, row: true, more: [Cell{User, by}]}` | time REAL (SQL: today → `HH24:MI`, else `Mon DD`, UTC); **by = data: FAKE** |
| Selected row `sel` | `pp:95` | `Row.highlight` = row is the selection | ok |
| No pager | `pp:93` | `PageInfo{next_cursor None, total None}`; capped read (C9) | ok |
| Row order | data order | `namespace COLLATE "C", key COLLATE "C"` | real stable order; sort arrow on Key only if the owner wants it (mockup has none: no `sorted`) |

### Edit panel (`Primitive::Form`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Edit" + mono key | `pp:96` | `ContributionDecl.title "Edit"` + `PanelBody.heading = "ns.key"` (drawn "Edit: ns.key") | the mono span is a **template choice** plus a todo item (fidelity bar); separator H5 |
| Meta "rev N" | `pp:96` | `PanelBody.meta = "rev N"` | REAL (D1) |
| Value field, `typeName` | `pp:98` | `Field{key "value", label "Value", kind per derived type (D4: bool→Bool, int→Int, float→Float, string→String, json→Json), default = current value, mono, required}` | REAL; control differs from the mockup's json default (D2, §7) |
| Change note | `pp:99` | `FormSpec` reason (23r0, D5), `Optional` | REAL to `admin.action` detail; placeholder text C4 |
| Submit "Save revision" | `pp:97` | `FormSpec.submit_label` | ok |
| CAS note | `pp:97` | `FormSpec.note = "Compare-and-swap: saves only if still rev N."` | ok, true after D1 |
| Hidden key / revision | n/a | `hidden {"key": "ns.key"}`, `revision Some(N as u64)` | ok |

### Revisions panel (`Primitive::Timeline`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Revisions", `flush` | `pp:102` | `ContributionDecl.title` | ok |
| Preamble "Pending change vs rev N" + diff block | `pp:101-102` | none | **contract gap** (D6, P2-7): accept line + todo item |
| Entry 1: time, "rev N by actor", icon edit, tone accent | `pp:103` | `TimelineEntry{at = updated_at, verb "rev N by", actor Cell User (FAKE), icon "edit", tone Accent}` | rev, time and value REAL; actor **FAKE** |
| Entry 1 meta: `span.mono` value | `pp:103` | `detail = value` (plain) | value REAL; mono span **contract gap** (ADMIN2-39 via P2-6) |
| Entries 2-3: rev N-1, N-2, old value, "restore" | `pp:104-105` | `TimelineEntry` ×2 | **data: FAKE** (D6; C2); restore link **contract gap** ADMIN2-39 |
| Date labels "Oct 14" | `pp:104-105` | `Timeline.date_only: true` | H6 |

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error | narrowed to zero |
|---|---|---|---|---|---|---|
| Keys | rows, meta, toolbar | `Primitive::Empty` portal default; meta "0" | n/a (no action on the panel) | `cache_ttl_s: None` | error block | toolbar kept; golden `table_empty_search` (H: covers filter + search) |
| Edit | form for the selection | no key at all, or an unknown `key` param: `Primitive::Empty` (C1) | `writes: true`; a Viewer sees the readonly form (`rt:805-845`, `panel_form_save` returns None) | n/a | error block, or "Form unavailable" (`rt:821-835`) | n/a |
| Revisions | 3 entries | as Edit | n/a | n/a | error block | n/a |

URLs:
- `PageDecl.params = ["key", "type"]`; the search is `q` (`PanelQuery.search`).
- The search and filter forms are built from `Ctx.query` (skill §1), so they keep `key`; selecting a key keeps the narrowing only because the provider copies `q`/`type` into the key link (row above).
- No pager keys. A `key` filtered out of the table still drives Edit and Revisions.
- Default selection (D2): with no `key` param, the first key by `namespace COLLATE "C", key COLLATE "C"`, the table's first unfiltered row.

## 3. `writes` and every ActionDecl

`writes: true`. Neither action has a native idempotency key (`request_id` unused). Both are replay-safe by construction: a replayed edit is a stale-revision Conflict, a replayed create an exists-Conflict. Name both under Leftovers for owner (Na2 rule, `plan:1959-1966`).

| id | Point | Label / icon | order | Emphasis | danger | implemented | then |
|---|---|---|---|---|---|---|---|
| `setting.create` | `PageHead(PAGE)` | "Key" / `plus` | 10 | Primary | no | true | `Form{fields: key (String, required, mono, placeholder "namespace.key"), type (Enum of the five, required, default "string"), value (Text, required, mono); submit "Create key"; title "New key"; reason Optional}` |
| `setting.edit` | `PanelForm("edit")` | "Save revision" | 10 | Default | no | true | `Invoke` (spec served by the panel body) |

`min_role: Operator` on both.

`invoke` rules (invariants for 23a2):
1. The first statement refuses any input value over `controlplane::MAX_INPUT_VALUE_BYTES` (`cp:205`); this is the conformance probe.
2. Key: split at the one '.', both halves `valid_ident` (`cl:200-217`), else a field error on `key`.
3. Value: decoded with the declared kind's `decode_field` (edit: the key's field kind; create: the picked type's kind, since the create form declares `value` as Text), then normalised:
   - a float text without `.`, `e` or `E` gets `.0`;
   - a json value is trimmed.
    - The value must then derive (D4) to the target type, else a field error naming both types. Edit keeps a key's type: the target is the current derived type. Create's target is the picked type.
   - The `.0` rule is required, not cosmetic: the portal re-encodes an edited value before `invoke` (`act:497`, `encode_field` `cp:931-941`), and `f64::to_string` turns 2.0 into "2".
4. Edit:
   - `expected_revision` is required and converted with `i64::try_from`, else Invalid (`aad:280-284` precedent);
   - `UPDATE config.settings SET value = $v WHERE namespace = $ns AND key = $k AND revision = $exp`;
   - 0 rows and the key exists → `Ok(InvokeOutcome::Conflict{current_revision})`; 0 rows and no key → `Error::conflict` (`rt:1323-1326`).
5. Create: an insert-only write (C3); 0 rows → `InvokeOutcome::FieldErrors{key: "already exists"}` (precedent `aad:326`): 422, the form is shown again and not used up, nothing is written. Not `Error::conflict`: that renders the generic "This changed since you opened it" banner (`rt:1297,1326`) and uses the modal up (`act:549`).
6. Both writes go through the trigger, so propagation, `config.changed` and audit are unchanged.

New `todo` items:
- 23a1:
  - FAKE `updated_by` (table actor and revision actor);
  - FAKE prior revisions;
  - Edit heading mono span (template choice);
  - "Pending change" preamble (contract gap, D6);
  - ADMIN2-39 widened to the `span.mono` meta (P2-6; `todo:418-424`).
- 23a2: none expected (both actions REAL). A known limit: values over 4096 bytes cannot be edited from the portal (C8).

## 4. Contributions, kinds and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `keys` | `Page(PAGE)` | 10 | Viewer | None | None |
| `edit` | `Page(PAGE)` | 20 | Viewer | None | None |
| `revisions` | `Page(PAGE)` | 30 | Viewer | None | None |

- No entity kind, row menu, search provider, tile, attention or nav counter. Config subscribes to nothing and purges nothing.
- Nav: `platform_nav::CONFIG` (`ord:9`).

## 5. Hand-check section

| # | Element | Check |
|---|---|---|
| H1 | Crumbs, subtitle text | live look |
| H2 | Keys meta = all keys, unaffected by filter or search | provider unit test |
| H3 | Rev column right-aligned, no sort arrow | live look |
| H4 | Selected row `tr.sel`; the default selection is the first key by C order (D2) | unit test + live look |
| H5 | Edit heading "Edit: ns.key" (separator, no mono span) | declared in the step report |
| H6 | Updated time format (today HH:MM, else "Mon DD", UTC, from SQL); Revisions `date_only` | unit test (local PG) + live look |
| H7 | Type derivation table (D4), including "007", "1e3", `"x"`, `null`, " 4", "TRUE" | unit test |
| H8 | Value field control and `typeName` per type; a float stays float after saving "2" | unit test + live look |
| H9 | Change note placeholder text (C4); the note lands in the `admin.action` detail | splitproof (CF2) |
| H10 | Conflict banner text on a stale revision | splitproof (CF3) + live look |
| H11 | Key link keeps `q` and `type` | unit test |
| H12 | FAKE actor and prior revisions stable across reloads (`fake_hash`) | unit test |
| H13 | Narrowed-to-zero keeps the toolbar | golden `table_empty_search` (H) |

## 6. Gaps and decisions

Owner decisions D1-D7 are in §11; proposals the owner has not settled are C1-C9 under "Gaps as proposed". The plan's Step 23 block cites this numbering.

## 7. Accept-file plan (predicted; exact text needs a crawl)

`pages.rs` rows:
- 23a1: `pages:430` `Page{page "config", route Route::Pending, …}` becomes `Route::Mapped("/admin/p/config/<PAGE>")` (C1).
- 23a2: `EXCLUDED_MODALS` (`pages:494`) gains `("config", "config/setting.create", ModalWhy{why: "the mockup's + Key head action has no onclick, so the mockup has no modal (pages-platform.js:107)"})`, as apikeys `role.create` (`pages:505`).
- No `EXCLUDED_MENUS` line (no row menu); no `EXCLUDED_MOCK_MODALS` line (the mockup opens no modal).

`config.txt` (new, part b):
- Frame lines copied from `scheduler.txt:5,8,9,11,12,13`: Sign out form; Lore Repositories `#3056ab`; counted Mail outbox / Locks `#848c49`; Economy section `#5694ed`; D3 `#ba7b3e`; theme toggle `#255bc4`.
- `mock-only ×3 a.nav-item#ecbb9a`: all from Lore paired with Economy. Platform is fully paired: mock plain apikeys/scheduler/audit against portal apikeys/Mail outbox (empty count)/scheduler, with active config against active config. No `a.active` lines are expected (H).
- Keys toolbar:
  - `mock-only span.input-icon#1fe724` / `portal-only form.input-icon#958dce` (template choice, copy `apikeys.txt:12-13`);
  - `mock-only span.filter-chip#9ca797` / `portal-only select.select#93a6c3` ×1 each (template choice, as `apikeys.txt:14-15` with "the Type filter").
- Keys rows, if the fleet has keys:
  - data difference (fresh-fleet keys are not the mockup's seven);
  - Updated actor `mock-only a#f59d71` / `portal-only span.user-cell#112ba6` (data difference, FAKE actor; copy `apikeys.txt:16-17`);
  - a `span.tag` actor ("scheduler", `dj:201`) mock-only only if the mock draws it on a compared row;
  - the key link markup vs `a.mono`, if it differs (template choice).
- Keys rows, if the fleet has no keys (C5): `portal-only div.state`, `mock-only div.tbl-wrap` and any `div.tbl-foot` (data difference, as `economy.txt:21-23`).
- Edit:
  - `mock-only div.form#67a233` / `portal-only form#9f822e` (template choice, copy `economy.txt:13,15`);
  - the heading `span.mono` (template choice, H5);
  - the Value control vs the mockup's json textarea (D2): `data difference: the default selection is the first key by key order (D2), a <type> key; the mockup's default store.featured_rotation is json`. Its exact shape is mock `textarea` vs portal `input`/int control. The kind follows the crawled key; predicted only when keys exist;
  - the Change note control vs the mockup's second field (template choice; markup unknown until 23r0).
- Revisions:
  - `mock-only` preamble `div.muted` + `div.code` with diff spans: `contract gap` (P2-7/D6, todo item);
  - `mock-only ×2 a` restore and `×3 span.mono` in `div.meta`: `contract gap` ADMIN2-39 (P2-6);
  - with no keys: `portal-only div.state` and `mock-only ul.tl` (data difference).

Every existing accept file with the `#ecbb9a` line (15 files, grep V):
- `index.txt:6`, `events.txt:6`, `apikeys.txt:11`, `players.txt:6`, `player.txt:6`, `player.characters.txt:7`, `player.inventory.txt:8`, `player.wallet.txt:8`, `player.friends.txt:10`, `player.groups.txt:10`, `player.inbox.txt:10`, `player.saves.txt:10`: `×{3,1}` becomes `×3` (H). Platform's last unported plain entry (Audit log) now pairs with the portal's empty-count Mail outbox, so Platform leaves 0. The reason drops "Config & flags" and the "so Platform leaves 1" clause.
- `mail.txt:6`: `×{3,2}` becomes `×{3,1}` (H); reason lists Audit log only.
- `economy.txt:9`: `×{4,1}` becomes `×4` (H).
- `scheduler.txt`:
  - `:10` stays `×3`; reason rewritten (Platform's only leftover is Audit log);
  - `:6` (`mock a.nav-item portal a.active.nav-item`, which pairs active Scheduler with Config/Audit) and `:7` (`mock-only a.active.nav-item#20de8e`) likely go **STALE** or change, and a `portal-only a.nav-item` for the Mail outbox may appear (H; crawl decides).
- Part b sweep first: `rg -n "ecbb9a|Config & flags" tools/admindiff/accept`.

## 8. Gate files the step touches

| File | Part | Change |
|---|---|---|
| `pab/controlplane.txt` (FormSpec `:1443-1450`) | 23r0 | **regenerate** (`cargo +nightly public-api -p controlplane -s --color=never`) in the 23r0 commit |
| `pab/configapi.txt:38-41,63-64` | — | unchanged (D1 wire decision); re-check with `rg -n "revision" pab/configapi.txt` after 23s |
| `pab/configevents.txt:7` | — | unchanged |
| `cg:43,68-73,341` | — | unchanged; `config.changed` keeps v1 and its fields |
| `tools/topiccheck/src/golden.rs:154-158,283`, `golden_tests.rs:245-252` | — | unchanged |
| `core/admin/src/golden_tests/{cases,build}.rs` | 23r0 | FormSpec literals + a form-with-reason golden case |
| FormSpec literal sites (about 28 in 10 files, a lower bound) | 23r0 | `modules/{wallet,apikeys,notifications,mail}/src/admin.rs`, `modules/wallet/src/admin_tests.rs`, `core/admin/src/tests.rs`, `cp` tests, `tools/admincheck/src/checks_tests.rs`, golden files above |
| `tools/admincheck/src/testdata/drawn_orders.txt` | 23a1 | H: a `platform_nav` config line if the file lists nav orders; check `rg -n "platform_nav" …` |
| `tools/conformance/src/policy.rs:514-521` (`all_na` becomes an Entry with an InputByteCaps fixture), `:180` basis sentence | 23a2 | `cp.invoke config input value` CapCase (`pol:60`) |
| `tools/admindiff/src/pages.rs:430`, `:494+` | 23a1, 23a2 | route; EXCLUDED_MODALS |
| `tools/admindiff/accept/*.txt` (16) | 23b | §7 |
| `tools/splitproof/expected-red.txt` | — | no config line (grep V) |
| `docs/todo/admin-second-pass.md`, `config.md`, `config-done.md` | 23a1 | §3; D7 |

## 9. Splitproof (23t)

New `tools/splitproof/src/config_admin_checks.rs`, on the `apikeys_admin_checks.rs` pattern (`spa:57-108` `assertions`/`cleanup`, `open :406`, `post :420`, `fetch :538`, `audit_mark/since :496-518`). Run in both topologies, plus the break run (admin-svc without its config peer: the page renders the panel error state, not a 500).

- Seed namespace `proof_cfgadmin`: `DELETE … WHERE namespace = 'proof_cfgadmin'` before and after each pass. Every check re-reads its row's revision, never a global one, so other checks' writes cannot race it (D1).
- Checks:
  - **CF1** GET `/admin/p/config/<PAGE>?key=proof_cfgadmin.flag` returns 200 with "Keys", "Edit", "Revisions" and the "rev N" meta equal to the row's revision.
  - **CF2** Edit with the right revision returns the success redirect. Afterwards:
    - the row value changed;
    - `row.revision` > old and = the `config.changed` event revision for that key;
    - exactly one new event;
    - the `admin.action` detail ends with ` — reason: <note>` (D5 in the split).
  - **CF3** Open two Edit forms at rev N (A and B); post A (Ok), then post B: 409 from the compare-and-swap; the row keeps A's value and no new event is written. Re-posting an already used form instance never reaches `invoke`: `take_form` falls through to `repeated()` (`rt:1096-1101,1119-1121`, 303 or a 200 toast under htmx); assert that as its own expected result, not as the CAS proof.
  - **CF4** Create a new key: row present, type derived as picked. Create the same key again from a fresh modal instance: 422 with the `key` field error, row unchanged, no new event.
  - **CF5** Refusals, each writing nothing:
    - invalid id (`Bad.Key`);
    - a value that changes the type and that the field kind accepts (a json key ← `5`; an int key ← "abc" is refused by the portal's Int `decode_field` before `invoke`, `cp:878`);
    - a value over 4096 bytes;
    - a Viewer post (H: a role check reachable in splitproof).
- proof-auditor audits.

## 10. Part sizes (estimate)

| Part | Lane | Content | Files | Tool calls |
|---|---|---|---|---|
| 23s | core-implementer opus; core-reviewer | `revision` column + stamping trigger (§11 D1), DDL docs, `ct` tests (stamp, one event per upsert-conflict, raw insert, multi-row statement, DELETE), `CONFIG_DB_TEST_GUARD` comment (`ct:127-129`), header docs `cl:9-23` | 2-3 | ~35 |
| 23r0 | core-implementer opus; core-reviewer + proof-auditor | FormSpec reason; panel and modal form drawing; `Required` check for Form (`act:507`); literal sites; golden case; baseline | ~14 | ~55 |
| 23a1 | core-implementer opus (Na1) | `store.rs` reads, `admin.rs` (manifest, 3 panels, D2, D4, FAKE), slot + `Cargo.toml`, `admin_tests.rs`, `pages.rs` route, todo items, D7 | 7-8 | ~60 |
| 23a2 | core-implementer opus (Na2) | `invoke` edit/create, store writes, `conformance.rs`, `pol`, `pages.rs` EXCLUDED_MODALS, tests | 6-7 | ~50 |
| 23t | test-author opus; proof-auditor | §9 | 2-3 | ~40 |
| 23b | mockup-implementer | crawls (orchestrator), `config.txt`, 15-file nav sweep, templates only if needed | 16 | ~35 |

## Not verified

- No cargo, crawl, mock, diff, admincheck or splitproof. Every §7 line and nav count is a prediction.
- Postgres trigger order for `INSERT … ON CONFLICT DO UPDATE`: BEFORE INSERT fires, then BEFORE UPDATE + AFTER UPDATE on conflict. This is from Postgres semantics, not tested here; it drives the 23s invariants.
- `app:1032-1035` (cp face on every edge process) and `cmd/server` hosting config: cited from Step 22, not re-read.
- Whether `table_empty_search` covers a filter plus search; whether a linked `Mono` cell draws `a.mono`; how `Layout::Grid21`'s right column wraps (`div.stack`?).
- Whether a crawl fleet holds any config keys (C5); the D2 entry depends on it.
- Whether `drawn_orders.txt` lists `platform_nav` orders.
- PG version for `starts_with` (PG ≥ 11) (H).
- Whether a Viewer post can be driven from splitproof (CF5).
- LSP not used; anchors come from reads and scoped grep.

## 11. Owner decisions

Owner 2026-11-23:

- **D1 (option B) — per-key revision, part 23s, done before every other part.** `config.settings` gains `revision bigint NOT NULL`, set to the global `config.revision` value of the write that last wrote the row (etcd-style). Invariants (code is the implementer's):
  - (a) Every INSERT/UPDATE on `config.settings` from any writer (service `set`/`set_tx`, raw SQL that names no `revision`, multi-row statements, upsert-on-conflict) stamps the row atomically in the writing transaction with a global revision strictly greater than any earlier one. Writers never supply it.
  - (b) The `config.changed` v1 event keeps its contract (`cev:31-43`, `cg:68-73`). Its `revision` equals the stamped row revision (DELETE: the bumped global value). Exactly one event and one NOTIFY per row mutation, including an upsert that hits ON CONFLICT (pinned by `ct:204-247`). Gaps in the global sequence are allowed; strict increase is what readers rely on (`capi:40-49`, `cl:303-315`).
  - (c) Snapshot coherence is unchanged: `load_snapshot_from`'s single statement (`cl:360-385`) still names the set.
  - (d) The current AFTER/RETURN NULL function (`cl:78-128`) cannot set `NEW.revision`, so the stamp needs a BEFORE ROW trigger that bumps `config.revision` per row. On `INSERT … ON CONFLICT DO UPDATE` Postgres fires BEFORE INSERT and then BEFORE UPDATE, so the conflict path uses two global values, and `ON CONFLICT DO NOTHING` (C3) uses one and writes nothing. Both are gaps, allowed by (b). The emit (NOTIFY + `config.changed`) runs only in the AFTER trigger, from `NEW.revision` (DELETE: its own bump), so each row mutation emits exactly once. A "bump once" scheme (BEFORE peeks, AFTER sets) is wrong: AFTER ROW triggers run at end of statement, so every row of a multi-row statement (`sp:1053`) would get the same revision, breaking (a). New `ct` cases: a DO NOTHING insert moves `config.revision` and emits no event; a multi-row insert stamps distinct revisions. The DDL docs note the lock-order change: the BEFORE bump takes the `config.revision` row lock while the statement still has rows to lock, so two concurrent multi-row statements can deadlock (multi-statement `set_tx` batches already could).
  - (e) Rollout: DROP SCHEMA config CASCADE + boot fresh. `CREATE TABLE IF NOT EXISTS` (`cl:65`) would leave an old table without the column, so the migrate notes this. No migration bridge.
  - (f) Wire shape: **no change** to `configapi::Setting`/`Snapshot` or `configevents::Changed`. The admin provider reads the store inside the module, not the snapshot cache (it holds no per-key revision), so 23a1 deletes `cached_snapshot` and `all` (`cl:322-356`), whose `expect(dead_code)` reason ("Step 23 … renders it") becomes false. No golden or baseline changes (§8).
  - (g) CAS: `UPDATE … WHERE namespace = $ns AND key = $k AND revision = $expected`; 0 rows → Conflict. `FormSpec.revision` (u64) is set from the i64 row revision (≥ 1); `InvokeCall.expected_revision` goes back through `i64::try_from`, and an overflow is Invalid.
  - (h) History, updated_by and the restore link stay FAKE or contract gap.
- **D2 — no 23a0 / T6 prelude.** Default selection is the first key by `namespace COLLATE "C", key COLLATE "C"`. The field-kind difference against the mockup's json default is a `data difference` accept entry (§7), plus H4.
- **D3 — "+ Key" is a real create:** a modal form (key, type, value, optional reason) per §3.
  - Validation: `valid_ident` on both halves of `ns.key`; the value decodes under the type's kind and derives to it.
  - An existing key is a Conflict with nothing written. The write must be insert-only for the Conflict to be atomic (C3).
- **D4 — type rule.** On the exact stored text `v`:
  - `v` empty or with leading/trailing whitespace → `string`;
  - `serde_json` parse of `v`: Bool → `bool`; Number with `as_i64()` Some → `int`; other Number → `float`; Array, Object, String (quoted) or Null → `json`; parse error → `string`.
  - Consequences: "007", "TRUE", "1" for a get_bool-style flag are `string`/`int` as the rule says.
  - Type filter: `FilterKind::Select(["bool","int","float","string","json"])`, empty option = all. Applied in Rust on the derived type before the draw cap (C9).
  - Search "Key prefix": `starts_with(namespace || '.' || key, q)`.
- **D5 — 23r0 FormSpec reason.** It reuses `ReasonReq` (`cp:1061-1066`); proposed shape: `reason: ReasonReq` + `reason_hint: Option<String>` (+ `reason_label` if the owner wants the mockup's "Change note" wording; C4).
  - Drawn in the panel form and the modal form as `name="_reason"`. `Required` is enforced for `Form` in `build_submission` (`act:507`).
  - The plumbing exists (`act:386-397,479,586-611`; `rt:1196-1203`). Public surface: regenerate `pab/controlplane.txt` in the same commit.
- **D6 —** "Pending change vs rev N" and its diff block are a `contract gap` accept entry plus a todo item. The Revisions timeline is FAKE except the current value and revision. ADMIN2-39 (`todo:418-424`) covers the restore link and, per P2-6, the `span.mono` value.
- **D7 — CONFIG-04 (`ctodo:23-28`) is stale.** Its seams no longer exist (the adminapi page was razed in plan step 1, `plan:500-501`; `cl:522-524` is now the edge contribution). 23a1 moves it to `config-done.md` as "(2026-11-2x, admin-control-plane-redesign step 23)". Note that split writes go through `cp.invoke` (23a2) and are proven by 23t CF2-CF4. `config-done.md` is `_None._` today (V).

Parts in order: **23s** → **23r0** → **23a1** → **23a2** → **23t** → **23b**, with lanes and files as in §10.

## Gaps as proposed (not yet decided)

- **C1. Page id.** `PAGE = "settings"`, route `/admin/p/config/settings` (noun per page, as `keys`, `outbox`, `schedules`). An unknown `key` param shows Empty in Edit and Revisions; it does not fall back to the default.
- **C2. FAKE prior revisions.** Two entries below the real one:
  - revisions `N - 1 - fake_hash("config.rev", key) % 3` and one more below that, dropped when < 1;
  - actor from config's own fixed login list via `fake_hash("config.by", key)`;
  - time = `updated_at - make_interval(days => h)` in SQL;
  - value = the current value, marked `// FAKE: no history is kept`. Alternative: "—".
- **C3. Create write.** A new insert-only store fn (`INSERT … ON CONFLICT DO NOTHING`, rows = 0 → Conflict). It shares `validate_ident` and the trigger path. "Existing set" is an upsert and would overwrite.
- **C4. Change note text.** The mockup's placeholder says "stored with the revision", but the note lands in the audit log (`act:593-611`). Proposal: label "Change note", placeholder "Why? (recorded in the audit log)", plus hand-check H9.
- **C5. Crawl data.** A fresh fleet probably has 0 keys, and no public API can seed config (seed rule, skill §5). Either accept three empty-state data differences for good, or let the orchestrator create keys through the portal's "+ Key" before the crawl (that is a seed, which needs the owner's leave).
- **C6. Edit keeps type.** A value that derives to another type is a field error; changing a key's type is out of scope (no delete action in the mockup).
- **C7. `updated_at`.** Let the 23s BEFORE trigger also set `updated_at = now()`, so raw-SQL updates (`sp:1053` upsert sets no `updated_at`) stay truthful in the Updated column.
- **C8. Large values.** Values over 4096 bytes (`cp:205`; splitproof writes one, `sp:4373`) cannot be edited from the portal. Known-limit todo item.
- **C9. Row cap.** Read all keys matching the prefix up to 500, derive types, filter, draw 100, with a `foot` when more exist. Exact `count(*)` for the meta.

Not checked: skill §2 (class → carrier table, beyond Step 22's mappings) and §3-§4, §7-§8; `core/app` cp-face wiring; the `drawn_orders.txt` content; the portal's actual drawing of a linked Mono cell and of `Grid21`.

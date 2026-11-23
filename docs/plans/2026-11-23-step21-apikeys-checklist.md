# Step 21 (apikeys): checklist

Read-only extraction per `.agents/skills/mockup-implementation/SKILL.md` §1, on the layout of `docs/plans/2026-11-22-step20-notifications-checklist.md`. HEAD at extraction: `40291286`. Mockup page: `UILayout/redesign/apikeys.html`. Scope: `plan:2363-2387` (21a1, 21a2, 21t, 21b) and the owner decisions of 2026-11-23 (§11).

Builder lines apikeys owns:
- Page `P.apikeys` `pp:31-44`. Crumbs Platform / API keys; `meta.after` opens the reveal when `?reveal` is set (`pp:32`). The head is title "API keys", subtitle "Machine credentials for game servers, CI and integrations.", and a `btn primary` "+ Create key" that runs `PAGES.createKey()` (`pp:41`). Layout `div.stack` holds Keys, then Roles (`pp:42`).
- Roles panel (`pp:34-36`): title "Roles", meta = role count, `flush`, tool `btn sm` "+ Role" with no onclick. Table with `pager: false`; columns Role (bold), Scopes (wrap, `span.tag` list), Keys (num). Each row has a `btn sm icon ghost` "more" button with no handler. Foot "Scopes are contributed by modules (e.g. lore.presign by lore)."
- Keys panel (`pp:37-40`): title "Keys", meta "N active", `flush`. Filter chips Status ("active + revoked") and Role ("any"); search "Name or key id". Columns Name (bold), Key (`span.mono.muted` prefix), Role (`span.tag`), Created (`span.row` = `span.dim` date + userCell), Last used (badge "now" ok, else `span.dim`), Status (`status()`). Row menu `rowMenuBtn("apikey", id)`; total "N keys".
- Menu `apikey` (`pp:9-16`):
  - group apikeys (`entity.apikey.row_action`): "Rotate secret…" (refresh) → `PAGES.reveal(id)`; "Change role…" (shield) → toast "Role picker…"; "Revoke…" (ban, danger) → `PAGES.revokeKey(id)`;
  - group audit: "Requests by this key" (list) → `audit.html`;
  - a revoked key draws only a disabled "Already revoked" (`pp:12`).
- Revoke confirm (`pp:18-23`): title "Revoke {name}?", icon ban; body "Requests signed with this key fail immediately on every process."; impact Key (`span.mono` id · role), Last used, Requests (24 h); reason hint "e.g. leaked in CI log"; typeToConfirm = name; confirm "Revoke key"; done "Key revoked".
- Create modal (`pp:24-28`): icon key (ok), h2 "Create API key", muted line "The secret is shown once after creation." Fields:
  - Name (required, mono, value "horde-agents-2", help "Lowercase, used in audit entries.");
  - Role (select "ref ApiRole" over role names, value "ci-build", help = that role's scopes as `span.tag`);
  - Expires (select enum Never / 30 days / 90 days / 1 year, value "90 days").
  Foot: Cancel, `btn primary` "Create key" → `PAGES.reveal()`.
- Reveal (`pp:29`): icon check, h2 "Key created — {name}" or "Secret rotated — {name}". `UI.secret` value plus an `extra` line "Use as `Authorization: Bearer <key>`. Role {tag} · expires …". Checkbox "I stored the secret somewhere safe" enables a disabled `btn primary` "Done".
- Registry: nav `reg:26` (platform, id apikeys, "API keys", icon key); search provider `reg:40` (kind apikey, label "API keys", icon key, title name, sub "{id} · role {role}", right = status, href apikeys.html, top 3).
- Out of scope, with reasons:
  - `reg:120` bell entry "Key ak_c4a8 created": the portal FAKE stays (P2-3, ADMIN2-04 at `docs/todo/admin-second-pass.md:39`).
  - `dj:114` services row `apikeys-svc`: another page.

Abbreviations: `pp` = `UILayout/redesign/assets/pages-platform.js`, `reg` = `…/registry.js`, `dj` = `…/data.js`, `bj` = `UILayout/design-system/components/bundle.js`, `cp` = `core/controlplane/src/lib.rs`, `ord` = `core/controlplane/src/order.rs`, `al` = `modules/apikeys/src/lib.rs`, `as` = `modules/apikeys/src/store.rs`, `ast` = `modules/apikeys/src/store_tests.rs`, `ac` = `modules/apikeys/src/conformance.rs`, `act` = `core/admin/src/engine/actions.rs`, `mtpl` = `core/admin/src/templates/modal.html.tmpl`, `ptpl` = `core/admin/src/templates/primitives.html.tmpl`, `tbl` = `core/admin/src/render/table.rs`, `shell` = `core/admin/src/templates/shell.html.tmpl`, `aad` = `modules/accounts/src/admin.rs`, `wad` = `modules/wallet/src/admin.rs`, `gad` = `modules/groups/src/admin.rs`, `iad` = `modules/inventory/src/admin.rs`, `mad` = `modules/mail/src/admin.rs`, `pol` = `tools/conformance/src/policy.rs`, `pages` = `tools/admindiff/src/pages.rs`, `sp` = `tools/splitproof/src/main.rs`, `er` = `tools/splitproof/expected-red.txt`, `gaps` = `docs/research/2026-11-21-0848-admin-ports-21-24-contract-gaps.md`, `plan` = `docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md`.

Method: targeted reads with line numbers, and `git show b8435c9a^:` for the removed admin. Ripgrep sweeps are marked **[grep]** (a lower bound). No cargo, `admindiff mock`, crawl, diff or splitproof.

## 0. Required checks

**0.1 What the store holds.**
- `apikeys.roles(name pk ≤128B, policy ≤4096B, revision, created_at, updated_at)` and `apikeys.keys(name pk ≤128B, secret_hash unique, prefix, role → roles(name), revision, created_at, updated_at, revoked_at)` (`al:61-87`).
- No created-by, last-used, expiry or request-count column. The key's id is its `name`.
- Secret = `ak_` + base64url(32 random bytes); `prefix` = its first 12 chars (`as:40-66`). The mockup's `swg_live_…` ids and prefixes are a data difference.
- Policy = `full` or a comma list of methods, checked loosely (`as:138-146`).

**0.2 Store functions and dead wrappers.**
- `list_roles` by name, no key count (`as:265-276`).
- `list_keys` newest first; no filter, search, paging or count; `created_at` rendered `Mon DD, HH24:MI` (`as:353-375`). The mockup's format is `YYYY-MM-DD`.
- `create_role` (`as:278-290`): a duplicate is a Conflict.
- `set_role_policy` with CAS (`as:294-327`).
- `delete_role` with CAS (`as:331-348`): FK `23503` → Conflict.
- `create_key` → `(secret, prefix)` (`as:379-399`): a duplicate name or a missing role is a Conflict.
- `set_key_role` with CAS (`as:403-428`).
- `revoke_key` with CAS (`as:433-456`): already revoked is a Conflict, not NoOp.
- The `Service` wrappers carry `expect(dead_code, reason = "Step 21a1/21a2 … plan.md:2346")` (`al:162-210`). The plan line is now 2363.
- Tests use the CAS `revoke_key` and `delete_role` (`ast:118,202,224,240,398,511`; `modules/apikeys/src/tests.rs:164`).

**0.3 Wiring today.**
- No `admin.rs`, no `controlplane` dependency, no `PROVIDER_SLOT` (`al:244-312`, `modules/apikeys/Cargo.toml`).
- `opscatalog` is a dev-dependency only. The removed admin used it as a normal dependency for the method list (`b8435c9a^:modules/apikeys/src/admin.rs:130-160`; `b8435c9a^:modules/apikeys/Cargo.toml`).
- admin-svc already dials apikeys: `peer_face("apikeys", wiring, "127.0.0.1:9009")` (`cmd/admin-svc/src/lib.rs:32`), `with_peer("apikeys", …)` (`cmd/admin-svc/src/main.rs:37`).
- The monolith hosts `apikeys::ApiKeys` (`cmd/server/src/lib.rs:60`); `cmd/apikeys-svc` hosts it in the split.
- The order constant exists: `platform_nav::APIKEYS = 10` (`ord:6`).
- No contract change, so no Na0.

**0.4 Show-once reveal plumbing (confirmed).**
- `InvokeOutcome::Ok{message, reveal, redirect}`, `NoOp{message}`, `Conflict{current_revision}` (`cp:512-519`). `SecretReveal{label, value, hint, expires_at, extra}` (`cp:1439-1446`).
- The portal stashes the flash in `admin.reveals` and redirects with `?reveal=<token>` (`act:658-730`). The shell draws the reveal modal and, when a reveal is set, no toast (`shell:84-85`).
- The reveal modal (`mtpl:31-41`):
  - h2 = `message` or `label`; a `div.muted` under the h2 holds `label` when `message` is set;
  - hint defaults to "It is shown exactly once."; `expires_at` and `extra` are plain `div.dim` lines;
  - the checkbox `data-ack` and a disabled `data-ack-done` "Done" button are present.

**0.5 Confirm submit and reason (confirmed).**
- A Confirm submit checks `ReasonReq::Required` and `type_to_confirm` before invoke (`act:506-516`).
- The reason reaches the `admin.action` detail as ` — reason: …` (`act:583-608`).
- `Err(opsapi::Error)` with `Status::Conflict` answers 409 and audits `conflict: <msg>` (`act:561,600`). `InvokeOutcome::Conflict` carries only a revision, no message (`cp:516`).

**0.6 Gateway header (confirmed).**
- HTTP reads `X-Api-Key` (`modules/gateway/src/lib.rs:2037-2040`). The socket reads the field `api_key` (`modules/gateway/src/keys.rs:80`).

**0.7 Row kind without an entity header (confirmed).**
- inventory declares kind `holding` with `RowAction("holding")` and no `EntityHeader` (`iad:320-350`).
- admincheck only requires the kind to be declared (`tools/admincheck/src/checks.rs:93-119`).
- A row becomes `tr.clickable` only when its kind has a header (`tbl:220`). It gets a menu button when its kind has row actions (`tbl:233`, `ptpl:239`).
- So `apikey` and `apirole` rows are not clickable, as in the mockup.

**0.8 Search precedent.**
- Only accounts declares search (`aad:454-459`, body `aad:743-760`): it returns empty when the kind does not match or `q` is empty or over the cap. It reads with `limit = min(req.limit, PAGE_SIZE)`.

## 1. Element table

### Page head and shell

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Nav entry "API keys", key icon, Platform section | `reg:26` | `NavEntry{section "platform", id "apikeys", page PAGE, order platform_nav::APIKEYS, counter None}` (as `mad:280-288`) | ok |
| Crumbs Platform / API keys | `pp:32` | portal crumbs from the nav section | hand-check H1 |
| Title "API keys", subtitle | `pp:41` | `PageDecl.title/subtitle` | ok |
| "+ Create key" `btn primary` | `pp:41` | `PageHead(PAGE)` `key.create`, `Emphasis::Primary`, `Then::Form`, `prepare: true` (K7) | REAL |
| Layout: Keys then Roles in `div.stack` | `pp:42` | `Layout::Stack([keys, roles])` | ok |
| Reveal on `?reveal=` (`meta.after`) | `pp:32` | the portal's `?reveal=<token>` flash (§0.4) | ok; the mock harness never runs `after` |

### Keys panel

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Keys", `flush` | `pp:37` | `ContributionDecl.title`; flush per SKILL §3 | ok |
| Meta "6 active" | `pp:37` | `PanelBody.meta` = `"{n} active"` | REAL count of `revoked_at IS NULL`; `0 active` at zero |
| Filter Status "active + revoked" | `pp:38` | `FilterDef{key "status", FilterKind::Select(["active","revoked"])}`, empty = both | REAL (K3) |
| Filter Role "any" | `pp:38` | `FilterDef{key "role", FilterKind::Select(<role names>)}` | REAL (K3) |
| Search "Name or key id" | `pp:38` | `search_placeholder "Name or key prefix"` | REAL over name and prefix (K3); hand-check H3 |
| Column Name, bold | `pp:39` | `CellKind::Strong` | REAL `name` |
| Column Key `span.mono.muted` | `pp:39` | `CellKind::MonoMuted` = `prefix` + "…" | REAL; `ak_` vs `swg_live_` is a data difference |
| Column Role `span.tag` | `pp:39` | `CellKind::Ref` | REAL; D4 real roles |
| Column Created: `span.dim` date + userCell | `pp:39` | `Cell{Dim, value YYYY-MM-DD, row: true, more: [User]}` | date REAL (SQL `to_char(created_at,'YYYY-MM-DD')`); by = **data: FAKE** (K4) |
| Column Last used: badge "now" ok, else `span.dim` | `pp:39` | `Badge` + `Tone::Ok` or `Dim` | **data: FAKE** (K4) |
| Column Status | `pp:39` | `Badge` "active" (`Tone::Ok`) / "revoked" | REAL `revoked_at`; tone hand-check H4 |
| Row menu button | `pp:40` | `Row.entity = apikey:<name>` + `RowAction("apikey")` actions | ok (§0.7) |
| Total "7 keys" | `pp:40` | `PageInfo.total` + `item_noun Noun("key","keys")` | REAL; the portal text is "7 keys · showing 1–7" (`tbl:204-205`), H5 |
| Pager | `pp:38` (default on) | `PageInfo{next_cursor, size 25}` keyset `(created_at, name)` | K3 |
| Row order | data order | `created_at DESC, name DESC`; no sort arrow (`sortable: false`) | rule: real stable order |

### Roles panel

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Roles", `flush` | `pp:34` | `ContributionDecl.title` | ok |
| Meta "4" | `pp:34` | `PanelBody.meta` = role count | REAL; "0" at zero |
| Tool "+ Role" `btn sm` | `pp:34` | `PanelTool("roles")` `role.create`, `Then::Form`, `when_empty: true`, `prepare: true` | REAL (K5) |
| Column Role, bold | `pp:35` | `CellKind::Strong` | REAL |
| Column Scopes, wrap, `span.tag` list | `pp:35` | `Column.wrap: true`; `Cell{Ref, first method, more: [Ref …]}` | REAL policy methods (D4: real op methods replace scopes); `full` = one tag "full" |
| Column Keys, num | `pp:35` | `CellKind::Number`, `Align::Right` | REAL count per role, revoked keys included (as the mockup's support-bot = 1 revoked) |
| No pager | `pp:35` | `PageInfo{next_cursor: None, total: None}` → no foot (`tbl:267`) | ok |
| Row "more" button | `pp:35` | `Row.entity = apirole:<name>` + `RowAction("apirole")` (D3) | REAL; the mockup button has no handler (accept, §7) |
| Foot text | `pp:36` | `PanelBody.foot` | text differs (K16), H6 |

### Key row menu (`apikey`)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Group label "apikeys" | `pp:10` | menu group per module | ok |
| "Rotate secret…" refresh | `pp:13` | `key.rotate`, `implemented: false` | plan-settled; todo item |
| "Change role…" shield | `pp:14` | `key.change-role`, `Then::Form`, `prepare: true` | REAL (K8); the mockup only toasts |
| "Revoke…" ban, danger | `pp:15` | `key.revoke`, `Then::Confirm`, `prepare: true`, `danger: true` | REAL (D1) |
| Group "audit": "Requests by this key" list | `pp:16` | apikeys `key.requests`, `Then::Link(Link::Page{module "audit", page K13, params {}})` | D2; drawn in the apikeys group, not an audit group (§7) |
| Revoked key: one disabled "Already revoked" | `pp:12` | none: the same menu for every key | P2-2 decided; NoOp on repeat; todo item; H9 |

### Role row menu (`apirole`, no mockup menu)

| Element | Source | Carrier | Status |
|---|---|---|---|
| "Edit policy…" | D3 | `role.edit`, `Then::Form`, `prepare: true`, `FormSpec.revision` = role revision | REAL; closes APIKEYS-01 |
| "Delete role…" danger | D3 | `role.delete`, `Then::Confirm`, `prepare: true`, `danger: true` | REAL (D1) |

### Create key modal

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Icon key, tone ok | `pp:24` | modal head from the action icon `key` | hand-check H7 |
| h2 "Create API key" | `pp:24` | `FormSpec.title` | ok |
| Muted "The secret is shown once after creation." | `pp:24` | `FormSpec.note` (`mtpl:10`) | ok |
| Name: required, mono, placeholder "horde-agents-2", help | `pp:25` | `Field{key "name", String, required, mono, placeholder}` | help text K14 |
| Role: select over role names, help = scopes | `pp:26` | `Field{key "role", Enum(<role names>), default first role, help = that role's policy as plain text}` | P2-5: plain text; accept or hand-check |
| Expires: select | `pp:27` | `Field{key "expires", Enum(["Never"]), default "Never"}` | D4 decided; todo item |
| Foot: empty `span.muted`, Cancel, "Create key" | `pp:28` | `submit_label "Create key"`; P2-9 renderer change | 21a2 |
| No Request id field | `pp:25-27` | none | plan-settled Leftover (K11) |

### Revoke confirm

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Title "Revoke {name}?", icon ban | `pp:20` | `ConfirmSpec.title`, action icon `ban`, `ConfirmTone::Danger` | REAL |
| Body sentence | `pp:21` | `ConfirmSpec.body` (same text) | true: the gateway cache is 5 s; H8 |
| Impact Key: mono id · role | `pp:22` | `("Key", Cell{Mono, prefix + "…", row: true, more: [Text "· {role}"]})` | REAL |
| Impact Last used | `pp:22` | Cell from the same FAKE as the column | **data: FAKE** (K4) |
| Impact Requests (24 h) | `pp:22` | Cell from `fake_hash` | **data: FAKE** (K4) |
| Reason, hint "e.g. leaked in CI log" | `pp:22` | `ReasonReq::Optional`, `reason_hint` | K15 |
| Type to confirm = name | `pp:22` | `type_to_confirm: Some(name)` | ok |
| Confirm "Revoke key"; done "Key revoked" | `pp:22` | `confirm_label`; outcome `Ok{message "Key revoked"}` | ok |

### Reveal (after Create key)

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Icon check, h2 "Key created — {name}" | `pp:29` | `mtpl:34`: h2 = `message` or `label` | K12 |
| Secret value + Copy | `pp:29` | `SecretReveal.value` | REAL `ak_…` |
| Hint line | `pp:29` | `hint: None` → "It is shown exactly once." | hand-check H10 |
| Extra: "Use as Authorization: Bearer …. Role {tag} · expires …" | `pp:29` | `extra` plain text: "Send it as the X-Api-Key header (HTTP) or the api_key field (socket). Role {role}." | plan-settled header; P2-4 plain text; no expiry (D4) |
| Checkbox + disabled Done | `pp:29` | `mtpl:37-38` | ok |

### Search provider `apikey`

| Element | Mockup line | Carrier | Status |
|---|---|---|---|
| Scope chip "API keys", key icon | `reg:40` | `SearchDecl{kind "apikey", label "API keys", icon "key", deadline_ms DEFAULT_SEARCH_DEADLINE_MS}` | ok |
| Hit title / sub / right / href | `reg:40` | `SearchHit{title name, subtitle "{prefix}… · role {role}", badge Some("active"/"revoked"), link Link::Page{apikeys, PAGE, {}}}` | REAL; prefix stands for the mockup's id |

Shell frame on `apikeys` and its menu and modal targets: copy the `frame` lines of `tools/admindiff/accept/mail.txt:5-11`, which match the `index.txt` entries (Sign out form, theme toggle, D3 Design-system section, Lore sections, not-yet-ported nav entries). Re-derive the `×` counts: API keys is now drawn.

## 2. States per panel, and URLs

| Panel | ok | empty | readonly | stale | error | narrowed to zero |
|---|---|---|---|---|---|---|
| Keys | rows + meta + toolbar + foot | `Primitive::Empty` portal default; meta `0 active` | n/a: `writes: true`, no `readonly` | `cache_ttl_s: None`: n/a | error block | toolbar kept; golden `table_empty_search` |
| Roles | rows + meta + tool | `Primitive::Empty`; tool kept (`when_empty`) | n/a | n/a | error block | n/a: no search or filter |

URLs:
- Keys: search and the two filter selects build from `ctx.query`. The pager builds the `cursor` URL. `PageDecl.params = ["status", "role"]` so both reach `PanelQuery.params`.
- Roles: none.
- Modal open/submit: the portal's action routes. Reveal: `?reveal=<token>` (`act:727-730`).

## 3. `writes` and every ActionDecl

`writes: true`. `min_role: Operator` on every write; `key.requests` (a Link) is `Viewer`.

| id | Point | Label / icon | order | Emphasis | danger | implemented | then |
|---|---|---|---|---|---|---|---|
| `key.create` | `PageHead(PAGE)` | "Create key" / `plus` | 10 | **Primary** | no | true, `prepare: true` | `Form(create_form)`: Name, Role, Expires |
| `key.rotate` | `RowAction("apikey")` | "Rotate secret…" / `refresh` | 10 | Default | no | **false** | `Invoke` |
| `key.change-role` | `RowAction("apikey")` | "Change role…" / `shield` | 20 | Default | no | true, `prepare: true` | `Form`: Role Enum, default = current role, `revision` = key revision |
| `key.revoke` | `RowAction("apikey")` | "Revoke…" / `ban` | 30 | Default | **yes** | true, `prepare: true` | `Confirm` (§1) |
| `key.requests` | `RowAction("apikey")` | "Requests by this key" / `list` | 40 | Default | no | true | `Link(Link::Page{"audit", K13, {}})` (D2) |
| `role.create` | `PanelTool("roles")` | "Role" / `plus` | 10 | Default | no | true, `prepare: true`, `when_empty: true` | `Form` (K5) |
| `role.edit` | `RowAction("apirole")` | "Edit policy…" / `edit` | 10 | Default | no | true, `prepare: true` | `Form` (K5), `revision` = role revision |
| `role.delete` | `RowAction("apirole")` | "Delete role…" / `trash` | 20 | Default | **yes** | true, `prepare: true` | `Confirm`: title "Delete role {name}?", `Danger`, no reason, `confirm_label "Delete role"` |

New `docs/todo/admin-second-pass.md` items in part `a`:
- `key.rotate` not implemented;
- Expires has only "Never";
- the per-entity "Already revoked" item (P2-2);
- the row-bound "Requests by this key" link (P2-1);
- the FAKE created-by, last used and requests 24 h;
- the role help as tags (P2-5) and the reveal `extra` as cells (P2-4), under **Accepted contract gaps** if they become `contract gap` accept lines.

## 4. Contributions, kinds and `order`

| Contribution id | Point | order | min_role | readonly | cache_ttl_s |
|---|---|---|---|---|---|
| `keys` | `Page(PAGE)` | 10 | Viewer | None | None |
| `roles` | `Page(PAGE)` | 20 | Viewer | None | None |

- Entity kinds: `apikey` ("API key", `key`) and `apirole` ("API role", `shield`), with no `EntityHeader` (§0.7).
- Search: `apikey`.
- No nav counter, no dashboard point, no purges (apikeys subscribes to nothing).
- Wiring, as notifications (`modules/notifications/src/lib.rs:252-253`): `controlplane` and `opscatalog` as normal dependencies, a new `admin.rs`, and `PROVIDER_SLOT` in `ApiKeys::init` (`al:300`).
- admin-svc is unchanged (§0.3).

## 5. Hand-check section

| # | Element | Check |
|---|---|---|
| H1 | Crumbs "Platform / API keys" | live look |
| H2 | Keys meta "N active"; Roles meta = role count | provider unit test |
| H3 | Search placeholder "Name or key prefix" (mockup "Name or key id") | declared in the step report |
| H4 | Status badge tone: active ok, revoked plain | live look |
| H5 | Foot "7 keys · showing 1–7" vs the mockup's "7 keys" (portal format) | declared |
| H6 | Roles foot text (K16) | live look |
| H7 | Create modal head icon `key`, tone ok | live look |
| H8 | Revoke modal: title, body, impact rows, optional reason, type-to-confirm gate | live look |
| H9 | Revoke of an already-revoked key answers NoOp "already revoked" (P2-2) | unit test + live look |
| H10 | Reveal: h2 per K12, hint, `extra` names X-Api-Key and api_key, Done disabled until checked | live look after a real create; admindiff cannot reach it (only after a POST) |
| H11 | Role help is the default role's policy as plain text (P2-5) | live look |
| H12 | Created by, Last used and Requests (24 h) are stable across reloads (FAKE via `fake_hash`) | unit test |
| H13 | Edit policy opens with the stored methods checked; a stale save is a Conflict | unit test (APIKEYS-01) |
| H14 | Delete role in use: 409 toast with the message; absent role: NoOp | unit test |
| H15 | "Requests by this key" lands on the audit URL (404 until Step 24, K13) | live look |

## 6. Gaps and decisions

**K1. Page id and route.** **Recommended:** `PAGE = "keys"`, so the route is `/admin/p/apikeys/keys` (mail is `/admin/p/mail/outbox`, `pages:393`). Open for the owner only if another id is wanted.

**K2. Reads.** **Recommended:** new store reads in 21a1:
- `keys_page(needle, status, role, after, limit)`: keyset on `(created_at, name)` DESC, `limit` 26 for a page of 25.
- One stats query: total and active (no narrowed total: K3 draws no total when narrowed). These are exact `count(*)`; the comment names the missing index (none on `created_at` or `role`).
- `roles_with_counts()`: `LEFT JOIN keys` and `count(k.name)` per role, ordered by name.
- `key(name)` and `role(name)` for `prepare` (moved to 21a2, its only caller).
- Delete `list_keys` and `list_roles` once nothing calls them.

**K3. Filters and search.** **Recommended:**
- Status `Select(["active","revoked"])`; Role `Select(<role names at render>)`; empty narrows nothing.
- The needle matches name or prefix with `strpos(lower(col), lower(needle)) > 0`, as the accounts store does, so `%` and `_` are literal; capped in `admin.rs` as accounts caps (`aad:744`). (Changed from `ILIKE` in 21a1 part 1.)
- The total is drawn only when not narrowed (as `mad:457`).

**K4. FAKE cells.** All come from `controlplane::fake_hash(salt, name)`, marked `// FAKE:`, each with a todo item:
- created-by: one of a fixed list of operator logins;
- last used: one of "now" (badge ok), "4 min ago", "2 h ago", "9 h ago", "1 d ago";
- requests 24 h: a number formatted with thousands separators.

A truthful constant does not exist for these: the gateway logs no use.

**K5. Role form (no mockup design; open).** **Recommended**, on the removed editor (`b8435c9a^:…/admin.rs:121-160,341-375`):
- Fields: Name (`String`, required, mono; create only, read-only on edit) and "Allowed methods" (`Multi(["full"] + opscatalog::OPERATIONS methods + methods some role uses that the catalog lacks)`, required).
- An optional "Additional methods" `String` (comma list) keeps the pre-authorize path.
- `full` picked → policy `full`; otherwise the de-duplicated union. Nothing picked → `FieldErrors`.
- Edit: `Multi` default = the stored methods as a JSON array (`form.rs:73-74` shows the decoded value), `revision` = role revision, `set_role_policy` CAS. A CAS miss → `InvokeOutcome::Conflict{current_revision}` from a re-read.
- **Owner:** keep or drop the free-text field.

**K6. Create key outcome.** **Recommended:**
- `create_key(name, role)` → `Ok{message: "", reveal: Some(SecretReveal{label "Key created — {name}", value secret, hint None, expires_at None, extra (§1)})}` (K12).
- Duplicate name → `FieldErrors{"name": "a key with this name exists"}`.
- Missing role (deleted after the form opened) → `Err(Error::conflict)`: 409, no row.
- Expires other than "Never" → `FieldErrors`.

**K7. `prepare` on PageHead and PanelTool (open, verify first).**
- `key.create` and `role.create` need runtime options (role names, in-use methods). A manifest is static, so they need `prepare: true`.
- No precedent prepares a PageHead or PanelTool action: wallet's create is static (`wad:506`). `cp` holds no rule against it [grep `prepare` in `cp`].
- 21a2 first confirms that `actions.rs` sends a `PrepareCall` with `entity: None, item: None` and admincheck accepts it.
- **Fallback for the owner:** static options read once at boot (stale after a change).

**K8. Change role.** **Recommended:** `set_key_role` CAS with `FormSpec.revision`. A miss → `Conflict{current_revision}` from a re-read; a gone key → `Error::not_found`; an unchanged role → `NoOp`.

**D1 (decided). Revoke and delete role without CAS.**
- `revoke_key(name)`: one `UPDATE … WHERE name = $1 AND revoked_at IS NULL`.
  - 0 rows and the key exists → `NoOp{"Key {name} is already revoked"}`.
  - 0 rows and the key is absent → `Error::not_found`.
- `delete_role(name)`: one `DELETE … WHERE name = $1`.
  - 0 rows → `NoOp`.
  - FK `23503` → `Err(Error::conflict("role {name} is still used by {n} key(s), revoked keys included"))`.
- The CAS variants and their tests go (`ast:202,224,240`). Uses of the old signature at `ast:118,398,511` and `tests.rs:164` move to the new one.
- No `ConfirmSpec` change.

**K9. A revoked key still blocks role delete (open).** `keys.role` keeps the FK after revoke (`al:83`). **Recommended:** keep it, so the audit of old keys keeps its role, and say so in the message. Alternative: purge revoked keys first (a new behaviour; not recommended).

**K10. Where the "clear message" lives.** `InvokeOutcome::Conflict` has no message (§0.5). **Decided (D1), detail recommended:** the in-use refusal is `Err(opsapi::Error)` with `Status::Conflict`. Verify that `opsapi::Error` has a conflict constructor; else build it with `Status::Conflict`.

**K11. Create without `request_id`.** Plan-settled Leftover. Leftover text: a create whose response is lost cannot be replayed. The retry meets "name exists", and the secret is gone, so the operator revokes and recreates. The name PK stops a double row.

**K12. Reveal heading (open).** **Recommended:** `message: ""` and `label: "Key created — {name}"`, so the h2 is the label and no muted line appears (`mtpl:34`). Verify that an empty `Ok` message is accepted (the audit detail is just `ok`, `act:595`). Alternative: message "Key created — {name}" plus a muted line, as an accept entry.

**K13. "Requests by this key" target (open).** No audit page exists until Step 24 (`modules/audit/src` has no admin [grep]). **Recommended:** fix the page id now (`Link::Page{module "audit", page "log", params {}}`); Step 24 must use it. The link answers 404 until then (H15). Alternative: hold the item for Step 24 (drops an element; not allowed).

**K14. Name help (open).** "Lowercase" is not enforced (`as:150-163`). **Recommended:** help "Used in audit entries." Alternative: enforce lowercase in `validate_name` for keys (a new rule).

**K15. Revoke reason.** **Recommended:** `ReasonReq::Optional`, because the mockup's reason is optional (`bj:314`). Alternative: Required.

**K16. Roles foot (open).** The mockup text names lore scopes, which do not exist. **Recommended:** "Policies list the op methods modules serve (e.g. leaderboard.topScores)." Data difference, hand-check H6.

**K17. Probe.** **Recommended:**
- The first statement of `ApiKeysAdmin::invoke` is `controlplane::MAX_INPUT_VALUE_BYTES` (as mail, wallet, groups, notifications; `pol:180`).
- A new `ac::conformance_cp_invoke_input_rejected` runs it through `ApiKeysAdmin::invoke` over a dead pool, on the notifications probe (`modules/notifications/src/conformance.rs:121-150`).
- A CapCase "cp.invoke apikeys input value" goes in `pol` `apikeys()`, and an apikeys sentence goes in the `cp.invoke req.input.<value>` basis (`pol:180`).

**K18. P2-9 renderer change (21a2).** The form modal foot draws no empty `span.muted`; a confirm keeps it (`mtpl:22`, `bj:315`). Update the golden cases it changes.

## 7. Accept-file plan (predicted; exact text needs a crawl)

`pages.rs` rows to change (part `a`, 21a1/21a2):
- `pages:392`: `Route::Pending` → `Route::Mapped("/admin/p/apikeys/keys")` (K1).
- Add `apikeys.menu.apikey`: `Page{page "apikeys", route same, menu Some(Menu{mock "apikey", kind "apikey"})}`.
- Add `apikeys.modal.create-key`: `Modal{id "create-key", opener "createKey", args &[], module "apikeys", action "key.create"}`.
- Add `apikeys.menu.apikey.modal.revoke`: `Modal{id "revoke", opener "revokeKey", args &["ak_9f2c"], module "apikeys", action "key.revoke"}`. The first menu-hosted modal entry: verify that args reach the opener.
- `EXCLUDED_MOCK_MENUS` (`pages:676`): remove `("apikey", …pending)`.
- `EXCLUDED_MENUS`: add `("apikeys", "apirole", "the mockup's Roles row button has no handler (pages-platform.js:35)")`.
- `EXCLUDED_MODALS`:
  - `("apikeys", "apikeys/role.create", "the mockup's + Role tool has no onclick (pages-platform.js:34)")`;
  - `("apikeys.excluded-menu.apirole", "apikeys/role.edit" | "apikeys/role.delete", "the mockup defines no apirole menu, so it has no modal")`;
  - `("apikeys.menu.apikey", "apikeys/key.change-role", "the mockup's Change role… only toasts 'Role picker…' (pages-platform.js:14)")`.
- `EXCLUDED_MOCK_MODALS`: `mock_modal("apikeys.menu.apikey", MockOpener::Pages("reveal"), PortalModal::Disabled("apikeys/key.rotate"), "no rotation exists: the portal draws Rotate secret… disabled as not implemented")`.

Accept files (part `b`):
- **`apikeys.txt`** (new):
  - frame lines from `mail.txt:5-11` with the counts re-derived;
  - the toolbar pair `span.input-icon` / `form.input-icon` as `mail.txt:12-13` (template choice);
  - the filter chips vs `FilterKind::Select` (template choice, per the crawl);
  - row count differences (data difference: a fresh fleet has the dev-seed or demoseed keys, not 7);
  - the Created `userCell` if drawn differently (template choice).
- **`apikeys.menu.apikey.txt`** (new): mock-only `div.menu-sep` and audit `div.menu-group`, with the portal's fourth item in the apikeys group. Reason: D2, apikeys draws "Requests by this key" until Step 24 moves it to audit.
- **`apikeys.modal.create-key.txt`** (new): the role help `span.tag` list vs plain text (contract gap P2-5); Expires options (D4); a footer `span.muted` only if P2-9 is not landed.
- **`apikeys.menu.apikey.modal.revoke.txt`** (new): impact `b > span.mono` vs the portal cell, if they differ (template choice); the reason input placeholder is an attribute (not compared).
- **Every existing accept file** (`index.txt`, `mail.txt`, `economy.txt`, `events.txt`, `events.modal.skip-poison.txt`, `players*.txt`, `player*.txt`): re-derive the `nav.nav > div.nav-section: mock-only ×N a.nav-item#ecbb9a` line and its reason. API keys now pairs; drop it from the "not ported yet" list.
- Part `b` sweep first: `rg -n "ecbb9a|API keys|apikey" tools/admindiff/accept`.

## 9. Splitproof (21t, D5; on `notifications_admin_checks.rs`)

New `tools/splitproof/src/apikeys_admin_checks.rs`, called from both topologies; it re-points the old IDs. Break run: admin-svc without its `apikeys` peer.
- **M3a** (monolith) and **AD3b** (split): GET `/admin/p/apikeys/keys` → 200 with the Keys and Roles panels and the Create key opener. The split hop runs in apikeys-svc.
- **AD4**: POST an apikeys action with a session and no `_csrf` → 403.
- **M3b / AD6b**: `role.create` through the portal form → 303, and an `apikeys.roles` row with the exact policy at rev 1.
- **AD6c**: `key.create` → 303 to `?reveal=<token>`; GET it shows the secret once, and a second GET shows none.
- **AD6d**: `secret_hash == base64url(sha256(secret))`, the role FK is right, no cleartext column (keep the reflected-columns check, `sp:5370-5409`).
- **AD6e**: the minted key authenticates through the gateway: 200 on an allowed op, 403 on another.
- **AD6f**: open the create form, delete its role by SQL, submit → 409, no row.
- **AD6g**: ≥2 new `admin.action` rows with `action` in (`apikeys.role.create`, `apikeys.key.create`). The old filter `target='api-keys' AND action='form-submit'` (`sp:5253,5490`) no longer matches (`act:577-581`).
- New: revoke twice → Ok, then NoOp (D1); delete an in-use role → 409.
- Remove `er:17` (M3a) and the AD3b, AD4, AD6b-AD6g lines. Move SPLITPROOF-06 to `docs/todo/splitproof-done.md`.

## 10. Part sizes (estimate)

| Part | Content | Files | Tool calls |
|---|---|---|---|
| 21a1 | `admin.rs` reads (manifest, Keys, Roles, counts, FAKE cells, search, kinds, nav), store reads (K2), `lib.rs` slot, `Cargo.toml`, `pages.rs` page/menu rows, tests | 5-6 | ~45 |
| 21a2 | forms, `prepare`, invoke arms, D1 store rewrite + tests, reveal, P2-9 template + goldens, probe + CapCase + `pol` basis, remove `expect(dead_code)`, `pages.rs` modal rows, todo items | 7-9 | ~45 |
| 21t | splitproof checks §9 | 3 | ~35 |
| 21b | crawls (monolith, split), new accept files, nav-line sweep | 25+ | ~40 |

## Not verified

- No cargo, crawl, `admindiff mock`, diff, splitproof or admincheck. Every §7 line is a prediction.
- K7: `prepare` on PageHead and PanelTool actions.
- Whether the harness passes `Modal.args` to a menu-hosted opener (no precedent entry).
- Whether `opsapi::Error` has a conflict constructor (K10).
- Whether an empty `Ok` message is accepted anywhere besides the audit detail (K12).
- Whether a `Link::Page` to an undeclared page fails admincheck (`tools/admincheck/src/checks.rs` [grep `Link::Page`]: no hit).
- The status badge tones the mockup's `status()` helper draws.
- Which keys and roles a fresh fleet holds (`APIKEYS_DEV_SEED`, demoseed).

## 11. Owner decisions

Owner, 2026-11-21 (`plan:2374-2387`): P2-1, P2-2, P2-3, P2-4, P2-5 and P2-9 as recommended.

Owner, 2026-11-23:
- **D1.** Revoke key and delete role run without CAS; no `ConfirmSpec` change. Revoke is one statement guarded by `revoked_at IS NULL`; already revoked → `NoOp`. Delete role: absent → `NoOp`; still used by a key → Conflict with a clear message (K10).
- **D2.** "Requests by this key" is drawn by apikeys now, as a static `Link::Page` to the unfiltered audit page. It moves to audit in Step 24.
- **D3.** Roles row menu (kind `apirole`, O-6): "Edit policy…" (Form; `Multi` pre-checked from the stored policy; `FormSpec.revision` → `set_role_policy` CAS; closes APIKEYS-01) and "Delete role…" (danger Confirm, D1).
- **D4.** The create form's Expires is a select with the one truthful option "Never", plus a todo item.
- **D5.** 21t re-points splitproof M3a, AD3b, AD4 and AD6b-g to the new page and actions in both topologies.
- **Plan-settled:**
  - Rotate is `implemented: false` with a todo item.
  - The reveal hint names X-Api-Key and the api_key field.
  - A create without request_id storage is a Leftover.
  - Created-by, last used and requests 24 h are FAKE via `controlplane::fake_hash`.
  - Mockup ids, prefixes and scopes that differ from ours are data-difference accept lines.

Owner, 2026-11-23 ("ok zaczynaj"), every open item as recommended:
- **K1.** `PAGE = "keys"`, route `/admin/p/apikeys/keys`.
- **K5.** The role form keeps the optional free-text "Additional methods" field beside the `Multi`.
- **K7.** If `prepare` on a PageHead or PanelTool action does not work, the implementer stops and reports; no silent fallback to options read once at boot.
- **K9.** A revoked key still blocks role delete; the 409 message says "revoked keys included".
- **K12.** `message: ""` and `label: "Key created — {name}"`.
- **K13.** `Link::Page{module "audit", page "log"}`; 404 until Step 24, which must use that id.
- **K14.** Name help "Used in audit entries."; lowercase is not enforced.
- **K15.** `ReasonReq::Optional`.
- **K16.** Roles foot "Policies list the op methods modules serve (e.g. leaderboard.topScores)."; data-difference accept line.

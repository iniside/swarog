# Admin ports 21–24: contract gaps against the control-plane contract (2026-11-21)

**Question (owner, 2026-11-21).** Before the admin ports start, which mockup elements of
Steps 21 (apikeys), 22 (scheduler), 23 (config) and 24 (audit) of
`docs/plans/2026-10-22-1759-admin-control-plane-redesign-plan.md` can the control-plane
contract plus renderer NOT draw in their exact shape, counting Steps 13e1–13e6 as landed?
Does any of it also touch Step 14 or ports 15–20? Companion to
`docs/research/2026-11-20-2230-admin-ports-contract-gaps.md` (ports 14–20).

**Method.** One read-only research pass: targeted reads and scoped greps (grep counts are
lower bounds). No rust-analyzer and nothing was run. Mockup anchors: `pp` =
`pages-platform.js` (apikeys :1-44, scheduler :74-85, config :88-109, audit :112-126),
`reg` = `registry.js`, `bundle` = `bundle.js`. `cp` = `core/controlplane/src/lib.rs`,
`tmpl` = `core/admin/src/templates/primitives.html.tmpl`.

## Answer

- Most of what the four pages need already has a carrier once 13e lands: composite cells
  (`Cell.more`, `Cell.row`), `Strong`, `Toggle`, select filters, `PageDecl.params`, the
  show-once reveal, the confirm modal (impact, reason, typed confirmation), panel `tag` and
  `foot`, and the timeline's actor, verb and badges.
- Nine gaps, P2-1 … P2-9. Six belong to one port (apikeys or config). Three cross ports:
  P2-1 (menu link bound to its row), P2-3 (notification bell), P2-9 (empty `span.muted` in a
  form modal's footer).
- **Step 14 (characters) is not affected** by any of them.
- None of the plan's claims for 21–24 is false. Several pages need more FAKE data than the plan
  says, and four mockup elements are in no step (see Plan claims).

## Map

`[grep]` rows are confirmed from grep hits only; the rest by a read.

| Element | Anchor | What it is |
|---|---|---|
| `SearchDecl` / `SearchHit` | cp:938, cp:428-434 | apikey search provider; hit has title, subtitle, badge |
| `InvokeOutcome::Ok{reveal}`, `SecretReveal` | cp:473, cp:1303-1310 | show-once reveal; `extra` is plain text |
| Reveal template | `core/admin/src/templates/modal.html.tmpl:32-41` | `extra` as plain text in `div.dim`; `expires_at` in its own `div` |
| Form modal | `core/admin/src/render/modal.rs:81-87`, modal.html.tmpl:22 | icon tone `ok`; footer always draws an empty `span.muted` |
| Row menu | `core/admin/src/routes.rs:843-857`, :196-236 | items come from `admit` only; a `Link` href is static |
| `bind` | `core/admin/src/render/action.rs:68-81` | binds the entity into `url`/`vals`, never into `href` |
| Filter → provider | `core/admin/src/render/table.rs:95-114`, routes.rs:632-639 | select value is a page URL param; reaches the provider only through `PageDecl.params` |
| Timeline | cp:1156-1181; tmpl:247-253; `core/admin/src/render/tiles.rs:152-164` | `meta` = target link + plain `detail` |
| Form reason | cp:889-891; `core/admin/src/engine/actions.rs:395` | a field key starting with `_` is refused; a form cannot send `_reason` |
| `admin.action` record | actions.rs:494-499 | `actor`, `action` (`module.action`), `target`, `detail` |
| Notification bell | routes.rs:141; `core/admin/src/fakes.rs:94-101`; `docs/todo/admin-second-pass.md:39-42` | portal FAKE already holding the apikeys entry; no `Point` (cp:609-634) |
| Storage `gc` attention fake | fakes.rs:178-186; `core/admin/src/portal_provider.rs:19,63,79`; todo :52-55 | order 50 |
| Schemas | `modules/apikeys/src/lib.rs:64-87`; `modules/config/src/lib.rs:65-77`; `modules/scheduler/src/lib.rs:152-176`; `modules/audit/src/lib.rs:107-113,246` | source data per page |
| admin-svc peers | `cmd/admin-svc/src/main.rs:33-37` | dials config, audit, scheduler, apikeys |
| admincheck peers | `tools/admincheck/src/checks.rs:394-442` | current peer findings |

## Flows

- **Create key, then reveal** (both topologies, traced at the portal only): `PageHead` action
  with `Then::Form` and `prepare` (current role list) opens the form; submit
  (actions.rs:388-432) → invoke → `Ok{reveal}` → reveal modal; `admin.action` is emitted from
  admin-svc (`core/admin/src/lib.rs:627-633`). In the split `cp.invoke` crosses the edge to
  apikeys-svc (main.rs:37); the stub itself was not traced.
- **Operator activity** (both): portal submit → durable `admin.action` → audit's
  subscription → `audit.log` → audit's `DashboardPanel` and page read it locally.
- **Audit actor filter** (both): select `actor` → page URL param (table.rs:99) →
  `PanelQuery.params` when the page lists it (routes.rs:632-639). The X5 operator link is
  `Link::Page{audit, audit, {actor}}`.

## Gaps

| Id | Mockup | Why the contract cannot draw it | Options (recommended first) | Other ports |
|---|---|---|---|---|
| **P2-1** Menu link bound to its row (apikey menu → audit "Requests by this key") | pp:15 | A `Then::Link` href is fixed when the item is built (routes.rs:228-231); `bind` never touches it (action.rs:68-81). | (a) static link to the unfiltered audit page (only the href differs; admindiff does not see it) plus a todo. (b) `bind` adds `entity=<kind:id>` to a `Link::Page` and the audit page lists `entity` in its params. Either way audit has no per-key request data. | accounts "Open detail" (reg:96) is row-bound; one grep found no such action in `modules/accounts/src` (hypothesis) |
| **P2-2** Revoked key's menu: one disabled "Already revoked" item with hint | pp:12 | Menu admits by role, `implemented`, `writes` only; nothing per entity (routes.rs:205-208, 843-857). | (a) same menu for every key; revoking a revoked key answers `NoOp` (cp:478), hand-checked; plus a todo. (b) per-entity menu call to the provider (new remote face). | none in 14–20 |
| **P2-3** Notification popover entry "Key … created" | reg:120 | No contribution point (cp:609-634); the portal draws the FAKE (routes.rs:141). | (a) keep the FAKE (todo admin-second-pass:39 tracks it) and name it in Step 21. (b) a new notification `Point`. | wallet (15), mail (11), core.events |
| **P2-4** Reveal extra line `div.dim > span.mono + span.tag`, expiry on the same line | pp:28 | `extra` is plain text; `expires_at` is a separate `div` (modal.html.tmpl:37). | (a) plain text plus a hand-check. (b) cell-valued `extra` (public API change). admindiff probably cannot open a reveal that only follows a POST (hypothesis). | none |
| **P2-5** Role field help as a list of `span.tag` | pp:26 | `Field.help` is plain text (tmpl:267). | (a) plain text plus an accept line. (b) `Field.help` as cells. | none |
| **P2-6** Revision meta: `span.mono` value, then the restore link | pp:104-105 | Timeline meta is the target link plus plain text (tmpl:249, cp:1156-1171). | Widen the existing restore-link `contract gap` entry (ADMIN2-39, todo :299-302) to cover the mono value. | none |
| **P2-7** Revisions preamble "Pending change vs rev N" with a diff block (`div.code > span.diff-del/.diff-add`) | pp:99-100 | No primitive draws a preamble or diff spans; `PropertyList.code` is plain (tmpl:233); it is also a live client-side diff of the unsaved form. | (a) `contract gap` accept line plus a todo. (b) a diff primitive plus admin.js. | none |
| **P2-8** "Change note (stored with the revision)" | pp:96 | A form cannot fill `InvokeCall.reason` (cp:889); config has nowhere to store a note. | (b) `FormSpec` gets a reason field that becomes `InvokeCall.reason`, so the note lands in the `admin.action` detail (actions.rs:504+) and on the audit page — as Step 23's Na0. (a) drop the field as a deviation. (c) storage in config. | none |
| **P2-9** Create-key form modal footer has no `span.muted` | pp:27 | modal.html.tmpl:22 always draws it; the mockup draws it for `confirm` (bundle:315), not for the form. | Renderer only: draw it for a confirm, not a form. Not a contract change. | every later form modal admindiff compares |

Need no contract change: audit "Export" (St1/O-3 pattern: `implemented:false` plus a todo);
scheduler and roles row menus (O-6 pattern, kinds `schedule` and `apirole`); the 13e6
wrappers fit all four layouts (apikeys and scheduler: two-panel `stack`; config:
`Grid21([keys],[edit, revisions])`; audit: one bare panel).

## Plan claims

- **Gateway header (21):** true — `X-Api-Key` (`modules/gateway/src/lib.rs:2037-2040`),
  `api_key` on the socket (`modules/gateway/src/keys.rs:80`).
- **Config global revision and `expected_revision` (23):** the singleton revision exists
  (`modules/config/src/lib.rs:72-77`, trigger :96-98); the portal already sends
  `FormSpec.revision` as `expected_revision` (actions.rs:417 → routes.rs:1142). No
  compare-and-swap write exists in config (`[grep]` of `api/config` found only snapshot
  revisions): the provider must do the check itself under the revision row lock. With one
  global revision, an edit to any other key is a Conflict. The mockup's per-key "rev N" (Rev
  column, panel meta, CAS note) has no source — the plan's open question on research P.
- **Scheduler `storage.gc` fake (22):** true, anchors above, order 50.
- **Audit actor filter (24):** feasible. The actor is in the stored `admin.action` payload;
  `audit.log` has only an `at` index, so filter and total are an unindexed `count(*)` that
  needs a comment naming the missing index. No source for the mockup's `scheduler` and
  `apikey:…` actors: `scheduler.fired` is not audited and gateway requests are not logged.
- **admincheck peer check (24.a3):** correctly still to do. Today it reports only a dialled
  peer with no `-svc` process (checks.rs:437-440) and a provider host that is not dialled
  (:425-435).
- **Mockup elements in no step:** audit's Export head action (pp:117); config's "+ Key" head
  action (pp:107); apikeys' "+ Role" tool, the Roles row menu and "Change role…"; audit's
  "Requests by this key" row action.

## Data the modules cannot supply (FAKE or derived)

- **apikeys:** created-by, last used, expiry, requests in 24 h; the key id is the `name`
  primary key.
- **scheduler:** stores only an interval (cron column follows the D4 rule); target, last
  result, per-schedule enabled, fire history with duration and note do not exist. "Next fire"
  derives as `last_fired + interval` in SQL.
- **config:** type, per-key revision, updated-by, revision history, change note.
- **audit:** non-operator actors.

## Owner decisions

1. P2-1: static link (recommended) or entity binding.
2. P2-2: same menu plus `NoOp` (recommended).
3. P2-3: keep the bell FAKE and name it in Step 21 (recommended).
4. P2-4, P2-5: plain text with an accept line or hand-check (recommended).
5. P2-6, P2-7: accept entries (recommended).
6. P2-8: form reason field as Step 23's Na0 (recommended), or drop the field.
7. Config per-key revision: still open in the plan (research P).
8. Where the mockup elements in no step go.

None blocks Step 14.

## Not verified

- Whether admindiff can reach a reveal or a prepared modal.
- The format of `roles.policy` (scopes).
- Whether the apikeys store can change a key's role.
- What the `config.changed` payload carries (any actor).
- The remote stubs in the split for these four providers.
- Whether accounts actually lacks "Open detail" (one grep only).
- `admin.js` and `golden_tests`.

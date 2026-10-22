(() => {
  const { icon, esc, panel, table, kpi, frag, badge, status, userCell, timeline, props } = UI;
  const D = DATA;
  const P = window.PAGES;
  const code = s => `<pre class="code type-def">${esc(s.trim())}</pre>`;
  const list = items => `<ul>${items.map(i => `<li>${i}</li>`).join("")}</ul>`;
  const stateTags = (on) => ["ok", "loading", "empty", "stale", "error", "readonly", "denied"].map(s => `<span class="badge ${on.includes(s) ? (s === "error" ? "err" : s === "denied" || s === "stale" ? "warn" : s === "readonly" ? "info" : "ok") : "plain"}" style="${on.includes(s) ? "" : "opacity:.35;text-decoration:line-through"}">${s}</span>`).join(" ");

  const sampleRows = D.locks.slice(0, 3).map(l => [`<span class="mono">${esc(l.file.split("/").pop())}</span>`, userCell(l.owner), l.stale ? badge(l.age, "warn") : l.age]);
  const C = (m, p, prim) => ({ module: m, point: p, prim });

  const prims = [
    {
      id: "DataTable", name: "Data table", used: ["players.html", "lore-repos.html", "lore-locks.html", "apikeys.html", "audit.html", "events.html", "economy.html"],
      desc: "Rows of one entity kind with server-side search, typed filters, sort, pagination, selection and per-row actions. Columns may come from other modules (<span class='tag'>entity.user.list_column</span>); a failing column degrades to an icon, not a failed table.",
      data: `
DataTable {
  columns: Column[]            // key, label, kind, sortable, align
  rows: Row[]                  // { ref: EntityRef, cells: Cell[] }
  page: { cursor, size, total?: u64 }
  filters: FilterDef[]         // key, label, kind: enum|text|range|bool|ref
  search?: { placeholder }
  selectable: bool
}
Column.kind = text | mono | badge | ref | number | bytes | time | user
Cell        = { kind, value, tone?: ok|warn|err|info, link?: EntityRef }
EntityRef   = "user:7f3a9c21" | "repository:ashfall-game" | "apikey:ak_9f2c"`,
      actions: ["Row action → <span class='tag'>*.row_action</span> menu, grouped by module", "Bulk action on selection (same ActionDef, <code>multi: true</code>)", "Row click → entity detail via EntityRef", "Search / filter / sort / page → refetch with Params"],
      states: ["ok", "loading", "empty", "stale", "error", "readonly", "denied"],
      points: ["page", "entity tab", "dashboard.panel"],
      demo: () => panel({ c: C("lore", "page", "DataTable"), title: "Open locks", meta: "ashfall-game", flush: true, noForce: true, body: table({ selectable: true, filters: [{ k: "Owner", v: "any" }], cols: [{ label: "File" }, { label: "Owner" }, { label: "Held" }], rows: sampleRows, rowMenu: () => `<button class="btn sm icon ghost" onclick="UI.openMenu(event,'lock','L-2291')">${icon("more")}</button>`, total: "3 of 14", page: "1 / 5" }) }),
    },
    {
      id: "EntityDetail", name: "Entity detail", used: ["player.html", "lore-repo.html"],
      desc: "Host for one entity: header (title, status, facts), header actions and a tab strip. The portal owns the frame; <b>every tab, fact and action is a contribution</b> keyed by entity kind, so a new module adds a Wallet or Lore tab without touching the host.",
      data: `
EntityKind  { kind: "user" | "repository" | "apikey" | …, owner: Module,
              resolve(ref) -> { title, subtitle, status, avatar? } }
EntityTab   { kind, id, label, order, count?: Fetch<u64>, body: Panel[] }
SummaryFact { kind, order, render: Fetch<Cell> }
HeaderAction{ kind, ActionDef }`,
      actions: ["Header actions (<span class='tag'>entity.user.header_action</span>)", "Overflow menu = the row-action set for that kind", "Tab switch keeps the URL (<code>?tab=lore</code>), loads only that tab"],
      states: ["ok", "loading", "error", "denied"],
      points: ["entity.&lt;kind&gt;.tab", "entity.&lt;kind&gt;.summary_field", "entity.&lt;kind&gt;.header_action"],
      demo: () => frag(C("portal", "entity.user (host)", "EntityDetail"), `<section class="panel"><div class="entity-head">${UI.avatar(D.byHandle["aiko.tanaka"], true)}<div class="grow"><div class="row"><h1 style="font-size:16px">Aiko Tanaka</h1>${status("active")}</div><div class="entity-facts">${frag(C("accounts", "entity.user.summary_field", "SummaryFact"), `<span>${icon("hash")} <span class="mono">7f3a9c21</span></span>`, { inline: true, tag: "span" })}${frag(C("lore", "entity.user.summary_field", "SummaryFact"), `<span>${icon("shield")} Lore Lead</span>`, { inline: true, tag: "span" })}${frag(C("wallet", "entity.user.summary_field", "SummaryFact"), `<span style="color:var(--err)">${icon("alert")} wallet unavailable</span>`, { inline: true, tag: "span" })}</div></div></div><div class="tabs">${["Profile & identities", "Wallet", "Lore"].map((t, i) => frag(C(["accounts", "wallet", "lore"][i], "entity.user.tab", "EntityTab"), `<button class="tab ${i ? "" : "on"}">${t}${i === 1 ? '<span class="dot err"></span>' : ""}</button>`, { inline: true, tag: "span" })).join("")}</div></section>`, { left: true }),
    },
    {
      id: "Form", name: "Form / configurator", used: ["lore-repo.html", "config.html", "economy.html", "apikeys.html"],
      desc: "Typed fields rendered by the portal from a schema; the module validates on submit and returns field errors. Supports compare-and-swap (<code>expected_revision</code>) and idempotency keys so retries are safe.",
      data: `
Form {
  fields: Field[]
  submit: { label, action: ActionId, idempotent?: bool }
  revision?: u64               // CAS: submit fails if changed
}
Field { key, label, type, required?, help?, default?, readonly? }
type = string | text | int | float | bool | enum(values) | multi(values)
     | json(schema?) | glob | ref(kind) | money(currency[]) | duration
SubmitOutcome = Ok { message, reveal?: SecretReveal, redirect?: EntityRef }
              | FieldErrors { [key]: string } | Conflict { current_revision }`,
      actions: ["Submit → SubmitOutcome", "Cancel / reset", "Field-level async validation (optional, debounced)"],
      states: ["ok", "loading", "stale", "error", "readonly", "denied"],
      points: ["page", "entity tab", "modal (from ActionDef)"],
      demo: () => panel({ c: C("config", "page", "Form"), title: "Edit <span class='mono'>match.max_party_size</span>", meta: "rev 12", noForce: true, body: UI.form({ submit: "Save revision", note: "CAS on rev 12", fields: [{ label: "Value", type: "text", typeName: "int (1..8)", value: "9", error: "Must be between 1 and 8", mono: true, w: "120px" }, { label: "Change note", placeholder: "Why?" }] }) }),
    },
    {
      id: "ConfirmAction", name: "Confirmation action modal", used: ["lore-locks.html", "lore-server.html", "events.html", "apikeys.html"],
      desc: "Gate for any destructive or expensive action. The module supplies the impact list (computed server-side at open time), whether a reason is required (goes to audit) and an optional typed confirmation token.",
      data: `
ConfirmAction {
  title, body, tone: danger | warn | info
  impact: (label, value)[]     // fetched when the modal opens
  reason: required | optional | none
  type_to_confirm?: string     // e.g. repo name, key name
  confirm_label
}`,
      actions: ["Confirm → invokes ActionDef with { reason }", "Cancel"],
      states: ["ok", "loading", "error", "denied"],
      points: ["any ActionDef with <code>confirm</code>"],
      demo: () => `<button class="btn danger" onclick="PAGES.forceUnlock('L-2241')">${icon("unlock")} Open example: force unlock</button> <button class="btn" onclick="PAGES.skipPoison()">${icon("skip")} Typed confirmation</button>`,
    },
    {
      id: "SecretReveal", name: "Show-once secret reveal", used: ["apikeys.html?reveal=1"],
      desc: "Returned inside a SubmitOutcome, rendered exactly once, never refetchable. The portal never stores it; reload loses it by design. Requires explicit acknowledgement before closing.",
      data: `
SecretReveal { label, value: Secret, hint?: string, expires_at?: time }
// Secret is a distinct type: never logged, never cached, never in Params`,
      actions: ["Copy", "Acknowledge (closes)"],
      states: ["ok"],
      points: ["SubmitOutcome.reveal"],
      demo: () => frag(C("apikeys", "SubmitOutcome.reveal", "SecretReveal"), UI.secret({ value: "swg_live_6be1_Qm9yZWFsaXMtNGM3ZS1hMWQ5" }), { left: true }),
    },
    {
      id: "KpiTile", name: "KPI tile", used: ["index.html", "lore-repos.html", "lore-billing.html", "events.html"],
      desc: "One number with unit, comparison and optional sparkline or capacity bar. Each tile is fetched separately with its own timeout, so one slow module never blanks the dashboard.",
      data: `
KpiTile {
  label, value: Number, unit?: string
  delta?: { value, period, good: up | down | none }
  series?: Number[]            // sparkline, oldest first
  capacity?: { used, limit }   // renders bar
  link?: PageRef | EntityRef
}`,
      actions: ["Click → link"],
      states: ["ok", "loading", "empty", "stale", "error", "denied"],
      points: ["dashboard.tile", "page"],
      demo: () => `<div class="grid g-2">${kpi({ c: C("lore", "dashboard.tile", "KpiTile"), label: "Lore seats", value: "8", delta: "users with Lore access", spark: [5, 6, 6, 7, 8, 8], noForce: true })}${kpi({ c: C("wallet", "dashboard.tile", "KpiTile"), label: "Embers spent (24 h)", value: "1.94 M", state: "error", noForce: true })}</div>`,
    },
    {
      id: "Timeline", name: "Event timeline / log", used: ["lore-events.html", "index.html", "scheduler.html", "config.html", "player.html?tab=inbox"],
      desc: "Time-ordered entries grouped by day, newest first, cursor-paginated backwards. Entries carry actor and target refs so they link across modules. Can be live (appends) or static.",
      data: `
Timeline { entries: Entry[], cursor_before?: Cursor, live?: bool }
Entry {
  at: time, actor?: EntityRef, verb: string, target?: EntityRef
  summary: RichText, detail?: string
  icon: push|lock|unlock|branch|shield|…, tone?
}`,
      actions: ["Load older", "Filter by type / actor / target", "Entry click → target"],
      states: ["ok", "loading", "empty", "stale", "error", "denied"],
      points: ["page", "entity tab", "dashboard.panel"],
      demo: () => panel({ c: C("lore", "page", "Timeline"), title: "Events", flush: true, noForce: true, body: timeline(D.loreEvents.slice(0, 3).map(e => ({ t: e.t, day: e.day, icon: e.icon, tone: e.tone, html: `${userCell(e.who)} ${esc(e.text)}`, meta: esc(e.detail) }))) }),
    },
    {
      id: "WorkQueue", name: "Work queue", used: ["mail.html", "events.html"],
      desc: "Items moving through states with attempt history. Counts per state act as tabs. Per-item and bulk actions: retry now, cancel (drop), park (keep, stop retrying), resume.",
      data: `
WorkQueue {
  states: { key, label, count, tone }[]
  items: Item[]                     // filtered by selected state
}
Item { id, ref?: EntityRef, state, attempts: u32, max_attempts: u32,
       next_attempt_at?: time, last_error?: string, payload_preview? }
Allowed actions per item state are declared by the module:
  pending → cancel ; retrying → retry | park | cancel ; failed → retry | cancel ; parked → resume | cancel`,
      actions: ["Retry now", "Cancel (confirm)", "Park / Resume", "Bulk versions of each", "Pause the whole queue"],
      states: ["ok", "loading", "empty", "stale", "error", "readonly", "denied"],
      points: ["page", "dashboard.attention (count)"],
      demo: () => panel({ c: C("mail", "page", "WorkQueue"), flush: true, noForce: true, title: "Outbox", body: `<div class="q-counts"><div class="q-count on"><div class="n">4</div><div class="l">Needs attention</div></div><div class="q-count"><div class="n">309</div><div class="l">Pending</div></div><div class="q-count"><div class="n">1</div><div class="l">Parked</div></div></div>` + table({ pager: false, cols: [{ label: "Message" }, { label: "State" }, { label: "Attempts" }, { label: "" }], rows: [[`<span class="mono">m-88180</span>`, status("failed"), `<span class="attempts">${"<i class='f'></i>".repeat(5)}</span>`, `<button class="btn sm">${icon("refresh")} Retry</button>`]] }) }),
    },
    {
      id: "PermissionMatrix", name: "Permission matrix editor", used: ["lore-permissions.html"],
      desc: "Two-axis grid where each cell is a value from a small enum. Two modes: <b>boolean</b> (roles × actions) and <b>override</b> (subjects × resources with inherit / value / none). Edits are staged and applied as one change set; an effective-permissions preview resolves one cell.",
      data: `
PermissionMatrix {
  rows: { ref: EntityRef, label, base?: Value }[]   // users or roles
  cols: { key, label, help? }[]                      // actions or repos
  cells: { row, col, value: Value, source: inherit | explicit }[]
  values: Value[]                                    // e.g. roles + "none"
  locked_rows?: key[]                                // built-in roles
}
ChangeSet { changes: { row, col, from, to }[], reason? }  // CAS on revision
Explain(row, col) -> { resolved, steps: (source, value)[] }`,
      actions: ["Stage cell change", "Apply / discard change set", "Explain effective permission"],
      states: ["ok", "loading", "stale", "error", "readonly", "denied"],
      points: ["page", "entity tab"],
      demo: () => panel({ c: C("lore", "page", "PermissionMatrix"), flush: true, noForce: true, title: "Overrides", body: `<div class="tbl-wrap"><table class="tbl matrix"><thead><tr><th>User</th><th class="rot mono">ashfall-art</th><th class="rot mono">engine-fork</th></tr></thead><tbody><tr><td>${userCell("elena.p")}</td><td class="cell"><span class="pcell none">none</span></td><td class="cell"><span class="pcell none">none</span></td></tr><tr><td>${userCell("priya.n")}</td><td class="cell"><span class="pcell override pending">Lead</span></td><td class="cell"><span class="pcell inherit">Developer</span></td></tr></tbody></table></div>` }),
    },
    {
      id: "ServiceStatus", name: "Process / service status", used: ["index.html#health", "lore-server.html", "lore-events.html"],
      desc: "Health of processes or streams: state, version, latency, recent history bar. <b>ProcessStatus</b> is the single-process variant with ports, restarts, health-check log and lifecycle actions (restart, logs).",
      data: `
ServiceStatus { items: { name, state, version?, p99?, uptime?, note?,
                         history?: (time, state)[] }[] }
state = running | degraded | restarting | crashed | down
ProcessStatus extends item { pid, host, ports: (name, port)[],
                restarts: { total, last_24h }, last_check: { at, ok, latency } }`,
      actions: ["Restart (ConfirmAction)", "Open logs", "Drill into module page"],
      states: ["ok", "loading", "stale", "error", "denied"],
      points: ["dashboard.health", "shell.health", "page"],
      demo: () => panel({ c: C("portal", "dashboard.health", "ServiceStatus"), flush: true, noForce: true, title: "Service health", body: UI.services(D.services.filter(s => ["wallet-svc", "mail-svc", "lore-svc"].includes(s.name))) }),
    },
    {
      id: "PropertyList", name: "Property list", used: ["player.html", "lore-server.html", "lore-events.html"],
      desc: "Label → value pairs for one object. Values are Cells, so refs, badges and mono values look the same as in tables.",
      data: `PropertyList { items: (label, Cell)[], columns?: 1 | 2 }`,
      actions: ["Optional panel tool: Edit → Form"],
      states: ["ok", "loading", "stale", "error", "denied"],
      points: ["entity tab", "page"],
      demo: () => panel({ c: C("accounts", "entity.user.tab", "PropertyList"), title: "Profile", noForce: true, body: props([["User id", `<span class="mono">7f3a9c21</span>`], ["Status", status("active")], ["Created", "2025-03-11"]]) }),
    },
    {
      id: "TimeSeriesChart", name: "Time-series chart", used: ["lore-billing.html"],
      desc: "One or two series over shared time buckets (second series may use a right axis). Deliberately minimal: line only, with a table next to it for exact values.",
      data: `TimeSeries { buckets: time[], series: { label, unit, values: Number[], axis: left|right }[] }`,
      actions: ["Range switch (re-fetch with Params)", "Export CSV"],
      states: ["ok", "loading", "empty", "stale", "error", "denied"],
      points: ["page", "dashboard.panel"],
      demo: () => panel({ c: C("lore", "page", "TimeSeriesChart"), title: "Seats & TB", noForce: true, body: UI.chart({ labels: D.billing.months, right: true, series: [{ values: D.billing.seats, max: 10, color: "var(--chart-1)", unit: "seats" }, { values: D.billing.tb, max: 15, color: "var(--chart-2)", unit: "TB" }] }) }),
    },
    {
      id: "MenuAction", name: "Action (row / header / bulk)", used: ["players.html", "lore-locks.html", "apikeys.html"],
      desc: "The unit of behaviour other modules hang on someone else's entity. The portal groups actions by module, shows disabled ones with a reason (module down, missing permission) and routes the click to a Form, ConfirmAction or direct invoke.",
      data: `
ActionDef {
  id, kind: EntityKind, label, icon, order, danger?: bool
  multi?: bool                       // bulk-capable
  when?: Predicate                   // e.g. user.has_lore_role
  requires: Permission
  then: invoke | form(Form) | confirm(ConfirmAction) | link(PageRef)
}`,
      actions: ["Invoke → SubmitOutcome"],
      states: ["ok", "error", "denied"],
      points: ["entity.&lt;kind&gt;.row_action", "entity.&lt;kind&gt;.header_action", "bulk"],
      demo: () => `<button class="btn" onclick="UI.openMenu(event,'user','aiko.tanaka')">${icon("more")} Open user row menu (6 modules)</button>`,
    },
    {
      id: "AttentionItem", name: "Attention item", used: ["index.html"],
      desc: "A module's claim that something needs an operator now. The dashboard sorts by severity across modules. Each item links to where it is resolved.",
      data: `AttentionItem { severity: err | warn, title, text, link: PageRef | EntityRef, action_label, since }`,
      actions: ["Open link", "Snooze (portal-side, per operator)"],
      states: ["ok", "loading", "error"],
      points: ["dashboard.attention"],
      demo: () => frag(C("lore", "dashboard.attention", "AttentionItem"), `<div class="panel"><div class="pop-row" style="align-items:center"><span class="modal-icon warn" style="width:28px;height:28px">${icon("lock")}</span><div class="grow"><div class="t">2 stale Lore locks</div><div class="d">Held &gt; 14 days by sam.o (suspended).</div></div><a class="btn sm" href="lore-locks.html?stale=1">Review</a></div></div>`),
    },
    {
      id: "SearchResult", name: "Search provider", used: ["index.html?search=ash"],
      desc: "Each module answers global search for its own entity kinds. Providers are queried in parallel with a short deadline; late or failing providers show inline without blocking others.",
      data: `
SearchProvider { kinds: EntityKind[], label, deadline_ms: 800,
                 search(q, limit) -> { ref, title, subtitle, badge? }[] }`,
      actions: ["Open result (EntityRef)", "Scope to one kind"],
      states: ["ok", "loading", "empty", "error"],
      points: ["search.provider"],
      demo: () => `<button class="btn" onclick="REG.openSearch('ash')">${icon("search")} Open search for “ash”</button> <span class="muted" style="font-size:12px">wallet provider times out on purpose</span>`,
    },
    {
      id: "NavEntry", name: "Navigation entry", used: ["every page"],
      desc: "A module adds pages under a section. Sections are declared once with an order; entries carry an optional live counter. The shell greys an entry and adds a red dot when its module is down.",
      data: `
NavSection { id, label, order }
NavEntry   { section, id, label, icon, order, page: PageId,
             counter?: Fetch<{ value, tone }>, requires: Permission }`,
      actions: ["Navigate"],
      states: ["ok", "error", "denied"],
      points: ["nav.section", "nav.entry"],
      demo: () => `<div style="width:232px" class="panel"><div style="padding:6px">${frag(C("lore", "nav.entry", "NavEntry"), `<a class="nav-item">${icon("lock")}<span>Locks</span><span class="count warn">26</span></a>`, { inline: true })}${frag(C("wallet", "nav.entry", "NavEntry"), `<a class="nav-item down">${icon("coins")}<span>Currencies &amp; ledger</span></a>`, { inline: true })}</div></div>`,
    },
  ];

  const compositions = [
    ["PoisonEventResolver", "events.html", "PropertyList + code block + three ActionDefs (resume, skip → ConfirmAction, copy)"],
    ["EffectivePermissions", "lore-permissions.html", "PermissionMatrix <code>Explain(row, col)</code> rendered as a PropertyList + checklist"],
    ["AttentionList", "index.html", "Portal host that sorts AttentionItems from all modules"],
    ["FacetBar", "lore-locks.html", "DataTable filter values with counts, shown above the table"],
    ["ProcessStatus", "lore-server.html", "ServiceStatus single-item variant"],
    ["EntityTab", "player.html", "EntityDetail tab contribution: label, order, optional count, Panel[]"],
    ["SummaryFact", "player.html", "EntityDetail header fact: one Cell"],
    ["Column", "players.html", "DataTable column contributed to another module's table: Column + batched Cell[]"],
    ["NavSection", "index.html", "NavEntry group: id, label, order"],
    ["NotificationItem", "index.html", "AttentionItem shape without an action label, shown under the shell bell"],
    ["Breadcrumbs", "index.html", "Shell-derived from NavSection, NavEntry and EntityKind.resolve; no module input"],
  ];

  const points = [
    ["nav.section / nav.entry", "portal", "—", "NavEntry", "order"],
    ["search.provider", "portal", "query", "SearchResult", "parallel, 800 ms deadline"],
    ["dashboard.tile", "portal", "time range", "KpiTile", "order; each tile fetched alone"],
    ["dashboard.attention", "portal", "—", "AttentionItem", "by severity"],
    ["dashboard.health", "portal", "—", "ServiceStatus", "probed by portal, not the module"],
    ["dashboard.panel", "portal", "time range", "Timeline, DataTable, Chart", "order"],
    ["notification.source", "portal", "operator", "(title, text, tone, link)", "time"],
    ["entity.user.tab", "accounts", "EntityRef user", "any Panel[]", "order; lazy per tab"],
    ["entity.user.summary_field", "accounts", "EntityRef user", "Cell", "order"],
    ["entity.user.header_action / row_action", "accounts", "EntityRef user", "ActionDef", "grouped by module"],
    ["entity.user.list_column", "accounts", "EntityRef[] (page of rows)", "Column + Cell[]", "batched per page"],
    ["entity.repository.tab / row_action", "lore", "EntityRef repository", "Panel[] / ActionDef", "order"],
    ["entity.apikey.row_action", "apikeys", "EntityRef apikey", "ActionDef", "grouped by module"],
    ["entity.lock.row_action", "lore", "EntityRef lock", "ActionDef", "grouped by module"],
    ["entity.&lt;kind&gt; (host), dashboard.attention (host)", "portal", "—", "EntityDetail, AttentionList", "portal-owned frames, outlined so the host is visible"],
    ["shell.breadcrumbs / shell.health", "portal", "—", "Breadcrumbs, ServiceStatus", "shell-owned, no module input"],
    ["SubmitOutcome.reveal", "any module", "—", "SecretReveal", "returned from a submit, not a contribution"],
    ["page", "contributing module", "Params (query string)", "any Panel[] in a layout", "nav order"],
  ];

  P.catalog = {
    meta: { nav: "catalog", crumbs: [["Design system", "catalog.html"], ["Primitive catalog"]] },
    render: () => {
      const toc = `<nav class="cat-toc panel" style="padding:6px"><div class="nav-title">Contract</div><a href="#envelope">Panel envelope</a><a href="#points">Extension points</a><div class="nav-title" style="margin-top:6px">Primitives</div>${prims.map(p => `<a href="#${p.id}">${esc(p.name)}<span class="code-name">${p.id}</span></a>`).join("")}<div class="nav-title" style="margin-top:6px">More</div><a href="#compositions">Compositions</a><a href="#tokens">Tokens & atoms</a><a href="#questions">Open API questions</a></nav>`;

      const envelope = `<section class="cat-entry" id="envelope"><h2>${icon("layers")} Panel envelope <span class="tag">every contribution</span></h2><p class="dim" style="max-width:820px">A module never draws chrome or failure UI. It contributes a <b>Panel</b> = envelope + one primitive payload. The portal fetches each panel independently, applies the deadline, picks the state and renders the chrome. That is what lets one module lag or fail while the rest of the page works.</p><div class="grid g-2">${panel({ title: "Contract", body: code(`
Contribution {
  module: ModuleId               // "lore"
  point: ExtensionPointId        // "entity.user.tab"
  id, order: i32, title, icon?
  requires: Permission           // checked by portal before fetch → denied
  capabilities: { write: bool }  // false → readonly chrome, writes disabled
  fetch(ctx: Ctx, params: Params) -> Result<Primitive, PanelError>
  cache: none | last_good(ttl)   // enables "stale" when module is slow/down
  deadline_ms: u32               // default 2000
}
PanelError = Unavailable{ detail } | Denied{ perm } | Invalid{ detail }
Ctx = { operator, entity?: EntityRef, time_range? }`) })}${panel({ title: "State rules (owned by the portal)", body: `<table class="tbl"><tbody>${[["loading", "fetch in flight > 150 ms; skeleton shaped like the primitive"], ["ok", "payload returned"], ["empty", "payload has zero items; module supplies empty text + optional action"], ["stale", "deadline missed or Unavailable, and a last-good copy exists (shows age)"], ["error", "Unavailable, no cached copy; Retry refetches only this panel"], ["readonly", "capabilities.write = false; read actions stay enabled"], ["denied", "operator lacks requires; nothing is fetched"]].map(([s, d]) => `<tr><td style="width:90px">${badge(s, s === "error" ? "err" : s === "denied" || s === "stale" ? "warn" : s === "readonly" ? "info" : "ok", true)}</td><td class="wrap dim">${d}</td></tr>`).join("")}</tbody></table><div class="muted" style="font-size:12px;margin-top:8px">Writes are never queued or served from cache: if the module is unreachable, write actions are disabled with a reason. <a href="states.html">See all states →</a></div>` })}</div></section>`;

      const pts = `<section class="cat-entry" id="points"><h2>${icon("puzzle")} Extension points</h2><p class="dim" style="max-width:820px">Every outlined fragment on every screen names one of these. Entity points are declared by the module that owns the entity kind; any module may contribute to them.</p>${panel({ flush: true, body: table({ pager: false, cols: [{ label: "Point" }, { label: "Declared by" }, { label: "Context passed" }, { label: "Accepts" }, { label: "Ordering / fetch" }], rows: points.map(r => [`<span class="mono" style="font-weight:500">${r[0]}</span>`, `<span class="tag">${r[1]}</span>`, `<span class="dim">${r[2]}</span>`, r[3], `<span class="dim">${r[4]}</span>`]) }) })}</section>`;

      const entries = prims.map((p, i) => `<section class="cat-entry" id="${p.id}"><h2><span class="muted mono" style="font-size:12px">${String(i + 1).padStart(2, "0")}</span>${esc(p.name)} <span class="tag">${p.id}</span></h2><p class="dim" style="max-width:860px;margin:6px 0 0">${p.desc}</p>
        <div class="cat-spec">${panel({ title: "Data it needs", body: code(p.data) , style: "grid-column: span 2" })}${panel({ title: "Actions", body: list(p.actions) })}${panel({ title: "States", body: `<div class="row" style="flex-wrap:wrap;gap:4px">${stateTags(p.states)}</div>` })}${panel({ title: "Extension points", body: `<div class="row" style="flex-wrap:wrap;gap:4px">${p.points.map(x => `<span class="tag">${x}</span>`).join("")}</div>` })}${panel({ title: "Used on", body: `<div class="row" style="flex-wrap:wrap;gap:4px">${p.used.map(u => u.endsWith(".html") || u.includes(".html") ? `<a class="tag" href="${u}">${u.replace(/\.html.*/, "")}</a>` : `<span class="tag">${u}</span>`).join("")}</div>` })}</div>
        <div class="cat-state-label">Live example</div>${p.demo()}</section>`).join("");

      const comp = `<section class="cat-entry" id="compositions"><h2>${icon("grid")} Compositions (not primitives)</h2><p class="dim">Names that appear in the annotation layer but are built from the primitives above. They do not need their own API type.</p>${panel({ flush: true, body: table({ pager: false, cols: [{ label: "Name" }, { label: "Screen" }, { label: "Built from" }], rows: compositions.map(c => [`<span class="mono">${c[0]}</span>`, `<a href="${c[1]}">${c[1].replace(".html", "")}</a>`, `<span class="dim">${c[2]}</span>`]) }) })}</section>`;

      const tokens = `<section class="cat-entry" id="tokens"><h2>${icon("sliders")} Tokens & atoms</h2><div class="grid g-2">${panel({ title: "Status badges (one style everywhere)", body: `<div class="row" style="flex-wrap:wrap;gap:6px">${["running", "active", "sent", "pending", "retrying", "lagging", "degraded", "suspended", "parked", "revoked", "failed", "paused", "down", "banned"].map(s => status(s)).join("")}</div><div class="muted" style="font-size:12px;margin-top:10px">Tone is derived from the value, never chosen per screen: ok · info · warn · err · neutral.</div>` })}${panel({ title: "Cells", body: props([["ref", `${UI.ref("user", "7f3a9c21")} ${UI.ref("repository", "ashfall-game")}`], ["user", userCell("aiko.tanaka")], ["mono", `<span class="mono">c41f9e2</span>`], ["tag", `<span class="tag">main</span>`], ["badge", badge("Lead", "", true)], ["buttons", `<button class="btn primary sm">Primary</button> <button class="btn sm">Default</button> <button class="btn sm danger">Danger</button> <button class="btn sm ghost">Ghost</button>`]]) })}</div></section>`;

      const qs = [
        ["Fetch needs a Ctx, not only Params.", "<span class='mono'>AdminData::admin_data(params)</span> already carries the query params over the wire, and every provider must tolerate foreign params. It carries no operator identity and no entity context, so a module cannot check <code>requires</code> per operator or know which entity tab it is rendering except by parsing params."],
        ["One Content per item → a Panel list.", "<span class='mono'>Content</span> holds kpis, one table, one form, one card grid, a context header, a modal point and context. Screens here need N independently fetched panels per page, each with its own state, deadline and cache."],
        ["EntityRef is owned centrally.", "<code>kind:id</code>. Today the precedent is <code>player:&lt;uuid&gt;</code> in row/card <span class='mono'>context</span>; these mockups write <code>user:</code> on the assumption of the planned player_id → user_id rename. Kinds are declared by their owning module; links, actions and tabs key on them."],
        ["Ordering exists for menus only.", "<span class='mono'>ExtensionEntry.priority</span> already sorts menu entries (ties on label). Nav groups and items still follow first-seen registration order (<span class='mono'>build_groups</span>). Sections, nav entries, tabs and tiles need an explicit <code>order</code>."],
        ["Per-panel deadline + last-good cache.", "Needed to tell stale from error. Decide where the cache lives; portal memory is enough because the portal serves one deployment."],
        ["Write capability: explicit or inferred?", "A remote provider that does not register <span class='mono'>AdminSubmit</span> already degrades to read-only through <span class='mono'>UnknownMethod</span>. The envelope's <code>capabilities.write</code> would make that explicit per panel, so the shell can say why (as on Cloud saves) without probing."],
        ["Batch fetch for list columns.", "<span class='tag'>entity.user.list_column</span> must be one call per page of rows, not per row."],
        ["Script policy.", "The portal ships htmx and admin.js under a CSP. These mockups use inline handlers everywhere; the real menus, tabs, modals and search palette must fit the CSP (no inline script) or the CSP must change deliberately."],
      ];
      const replaced = [
        ["ExtensionKind::EntityMenu, ExtensionPoint, ExtensionEntry, MenuEntry, RowMeta", "MenuAction on <span class='mono'>entity.&lt;kind&gt;.row_action</span>"],
        ["ExtensionKind::ModalActions, Content.modal_point, Present::Modal", "MenuAction on <span class='mono'>entity.&lt;kind&gt;.header_action</span>; modal = Form / ConfirmAction"],
        ["Content.header (ContextHeader), Content.context", "EntityDetail host + <span class='mono'>entity.&lt;kind&gt;.summary_field</span>"],
        ["Content.kpis", "KpiTile (page or <span class='mono'>dashboard.tile</span>)"],
        ["Content.table (+ Cell)", "DataTable"],
        ["Content.form, SubmitOutcome, RevealItem", "Form, SubmitOutcome, SecretReveal"],
        ["Content.cards (CardGrid)", "No primitive yet: use DataTable or PropertyList, or add a CardGrid primitive"],
        ["Item.section / label (nav)", "NavSection + NavEntry with order"],
      ];
      const questions = `<section class="cat-entry" id="questions"><h2>${icon("info")} Open API questions these mockups raise</h2>${panel({ title: "What the current contract types become", flush: true, body: table({ pager: false, cols: [{ label: "Today (api/admin/api)" }, { label: "In this design" }], rows: replaced.map(r => [`<span class="mono" style="white-space:normal">${r[0]}</span>`, `<span class="dim">${r[1]}</span>`]) }) })}<div style="height:12px"></div>${panel({ flush: true, body: `<table class="tbl"><tbody>${qs.map(([a, b], i) => `<tr><td style="width:28px" class="muted mono">${i + 1}</td><td style="width:280px;font-weight:500" class="wrap">${a}</td><td class="wrap dim">${b}</td></tr>`).join("")}</tbody></table>` })}</section>`;

      return P.head("Primitive catalog", `${prims.length} primitives · ${points.length} extension points. Every screen in this mockup is built only from these.`, `<a class="btn" href="states.html">${icon("grid")} Panel states</a><button class="btn" onclick="REG.toggleAnnot()">${icon("layers")} Toggle contributions</button>`) +
        `<div class="cat-grid">${toc}<div>${envelope}${pts}${entries}${comp}${tokens}${questions}</div></div>`;
    },
  };
})();

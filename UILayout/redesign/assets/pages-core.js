window.PAGES = window.PAGES || {};
(() => {
  const { icon, esc, panel, table, kpi, frag, badge, status, userCell, timeline, props, rowMenuBtn } = UI;
  const D = DATA;
  const P = window.PAGES;

  P.boot = name => REG.mount(P[name].meta, P[name].render);
  const head = (title, sub, actions = "") => `<div class="page-head"><div><h1>${title}</h1>${sub ? `<div class="sub">${sub}</div>` : ""}</div><div class="page-actions">${actions}</div></div>`;
  P.head = head;
  P.actor = a => D.byHandle[a] ? userCell(a) : `<span class="tag">${esc(a)}</span>`;

  // ---------- shared confirmations used by contributed actions ----------
  P.suspend = (h = P.currentUser?.handle) => UI.confirm({
    c: { module: "accounts", point: "entity.user.row_action", prim: "ConfirmAction" },
    title: `Suspend ${h}?`, tone: "warn", icon: "ban",
    body: "The user is signed out everywhere and cannot sign in until reinstated. Game data is kept.",
    impact: [["Active sessions", "2 (revoked now)"], ["Lore access", D.byHandle[h]?.loreRole ? "blocked; still counted as a seat" : "none"], ["Reversible", "yes — Reinstate"]],
    reason: "e.g. offboarding, chargeback, CS ticket #", confirm: "Suspend", done: `${h} suspended`,
  });
  P.deleteAccount = h => UI.confirm({
    c: { module: "accounts", point: "entity.user.row_action", prim: "ConfirmAction" },
    title: `Delete account ${h}?`, body: "Starts the deletion saga: every module purges its data for this user (accounts.deleted). This cannot be undone.",
    impact: [["Modules that purge", "11"], ["Cloud saves", "3 slots · 41 MB"], ["Wallet balance", "unknown (wallet unavailable)"]],
    reason: "GDPR request id", typeToConfirm: h, confirm: "Delete account", done: "Deletion saga started",
  });
  P.changeRole = (h = P.currentUser?.handle) => {
    const u = D.byHandle[h];
    UI.modal(`${frag({ module: "lore", point: "entity.user.header_action", prim: "Form" }, `<div class="modal-head"><div class="modal-icon ok">${icon("shield")}</div><div><h2>Change Lore role — ${esc(h)}</h2><div class="muted" style="font-size:12px">Global role; per-repository overrides stay in place.</div></div></div><div class="modal-body">${UI.form({ actions: false, fields: [
      { label: "Global role", type: "select", typeName: "enum LoreRole", options: D.roles.map(r => r.name), value: u.loreRole },
      { label: "Effective from", type: "select", typeName: "enum", options: ["Now (new tokens)", "Now + revoke live sessions"], value: "Now (new tokens)", help: "Lore checks permissions online on each request, so the change applies within seconds." },
    ] })}<div class="impact"><div><span>Overrides kept</span><b>${Object.keys(D.overrides[h] || {}).length}</b></div><div><span>Seats billed</span><b>unchanged (8)</b></div></div></div>`, { left: true })}<div class="modal-foot"><button class="btn" onclick="UI.closeModal()">Cancel</button><button class="btn primary" onclick="UI.closeModal();UI.toast('Lore role updated')">Save role</button></div>`);
  };
  P.releaseAll = h => {
    const mine = D.locks.filter(l => l.owner === h);
    UI.confirm({
      c: { module: "lore", point: "entity.user.row_action", prim: "ConfirmAction" },
      title: `Release all ${mine.length} locks held by ${h}?`, icon: "unlock",
      body: "Each lock is force-unlocked on its repository. The owner's local edits to those files are not lost, but they may conflict on their next push.",
      impact: mine.map(l => [l.repo, `<span class="mono">${esc(l.file.split("/").pop())}</span>`]),
      reason: "e.g. owner offboarded", confirm: `Force unlock ${mine.length}`, done: `${mine.length} locks released`,
    });
  };

  // ---------- dashboard ----------
  P.dashboard = {
    meta: { nav: "dashboard", crumbs: [["Dashboard"]] },
    render: () => {
      const tiles = REG.tiles.map(t => kpi({ ...t, c: { module: t.module, point: "dashboard.tile", prim: "KpiTile" } })).join("");
      const attention = panel({
        c: { module: "portal", point: "dashboard.attention (host)", prim: "AttentionList" }, title: "Needs attention", meta: `${REG.attention.length} items from ${new Set(REG.attention.map(a => a.module)).size} modules`, flush: true, noForce: true,
        body: REG.attention.map(a => frag({ module: a.module, point: "dashboard.attention", prim: "AttentionItem" }, `<div class="pop-row" style="align-items:center"><span class="modal-icon ${a.tone === "err" ? "danger" : "warn"}" style="width:28px;height:28px">${icon(a.icon)}</span><div class="grow"><div class="t">${esc(a.title)}</div><div class="d">${esc(a.text)}</div></div><a class="btn sm" href="${a.href}">${esc(a.action)}</a></div>`)).join(""),
      });
      const activity = panel({
        c: { module: "audit", point: "dashboard.panel", prim: "Timeline" }, title: "Recent operator activity", meta: "audit", flush: true,
        tools: `<a class="btn sm ghost" href="audit.html">Audit log ${icon("right")}</a>`,
        body: timeline(D.audit.slice(0, 7).map(a => ({ t: a.t.slice(0, 5), html: `${P.actor(a.actor)} <span class="mono">${esc(a.action)}</span>`, meta: `${esc(a.target)} · ${esc(a.detail)}`, icon: a.topic.startsWith("lore") ? "repo" : a.topic === "wallet" ? "coins" : a.topic === "events" ? "zap" : "activity", tone: a.action.includes("force") || a.action.includes("paused") ? "err" : "" }))),
      });
      const svcs = D.services;
      const health = panel({
        c: { module: "portal", point: "dashboard.health", prim: "ServiceStatus" }, title: "Service health", meta: `${svcs.filter(s => s.state === "running").length}/${svcs.length} healthy · probed every 10 s`, flush: true, noForce: true,
        tools: `<span class="muted hide-tablet" style="font-size:11.5px">last 4 h</span><span class="muted" style="font-size:11.5px;width:64px;text-align:right">p99</span>`,
        body: `<div class="grid g-2" style="gap:0">${[svcs.slice(0, 10), svcs.slice(10)].map(c => `<div style="border-right:1px solid var(--border)">${UI.services(c)}</div>`).join("")}</div>`,
      });
      return head("Dashboard", "emberline-prod · Swarog 0.42.1 · 20 processes (split topology)", `<div class="seg"><button>1 h</button><button class="on">24 h</button><button>7 d</button></div><button class="btn">${icon("refresh")} Refresh</button>`) +
        `<div class="grid g-tiles" style="margin-bottom:12px">${tiles}</div>` +
        `<div class="grid g-21" style="margin-bottom:12px">${attention}${activity}</div>` +
        `<div id="health">${health}</div>`;
    },
  };

  // ---------- players list ----------
  const listCols = [
    { label: "User" }, { label: "Kind" }, { label: "Status" }, { label: "Identities" },
    { label: "Characters", num: true, c: { module: "characters", point: "entity.user.list_column", prim: "Column" } },
    { label: "Lore role", c: { module: "lore", point: "entity.user.list_column", prim: "Column" } },
    { label: "Embers", num: true, c: { module: "wallet", point: "entity.user.list_column", prim: "Column" } },
    { label: "Last seen", sorted: "desc" }, { label: "Created" },
  ];
  P.players = {
    meta: { nav: "players", crumbs: [["Players", "players.html"], ["Users & players"]] },
    render: () => {
      const cols = listCols.map(c => c.c ? { ...c, label: c.label } : c);
      const rows = D.users.map(u => ({
        href: `player.html?u=${u.handle}`,
        cells: [
          `<span class="user-cell">${UI.avatar(u)}<span><a href="player.html?u=${u.handle}" style="color:var(--text);font-weight:500">${esc(u.handle)}</a>${u.kind === "staff" ? ` <span class="muted">${esc(u.name)}</span>` : ""}</span></span>`,
          badge(u.kind, u.kind === "staff" ? "accent" : "", true),
          status(u.status),
          u.providers.map(p => `<span class="tag">${p}</span>`).join(" "),
          u.characters,
          u.loreRole ? badge(u.loreRole, "", true) : `<span class="muted">—</span>`,
          `<span class="muted" title="wallet module unavailable">${icon("alert")}</span>`,
          `<span class="dim">${u.lastSeen}</span>`,
          `<span class="muted">${u.created}</span>`,
        ],
      }));
      const tbl = table({
        cols, rows, selectable: true, searchPlaceholder: "Handle, name, email or user id", searchWidth: 280,
        filters: [{ k: "Kind", v: "any" }, { k: "Status", v: "any" }, { k: "Provider", v: "any" }],
        toolbarRight: `<span class="muted" style="font-size:12px">Columns:</span><button class="btn sm">${icon("sliders")} 9 of 12</button>`,
        rowMenu: r => rowMenuBtn("user", r.href.split("=")[1]),
        total: "214,903 accounts · showing 1–14", page: "1 / 15,351",
      });
      return head("Users & players", "Staff and players share one account space; Lore access is a role on the account.", `<button class="btn">${icon("download")} Export CSV</button><button class="btn primary">${icon("plus")} Create account</button>`) +
        panel({ c: { module: "accounts", point: "page", prim: "DataTable" }, flush: true, body: tbl, cls: "players-table",
          banner: `<div class="banner warn" data-module="wallet">${icon("alert")} Embers column unavailable — wallet module did not answer. Other columns are live.<button class="btn sm" onclick="UI.retry(this)">${icon("refresh")} Retry</button></div>` });
    },
  };
  P.players.meta.after = () => {
    const ths = document.querySelectorAll(".players-table thead th");
    listCols.forEach((c, i) => { if (c.c) { const th = ths[i + 1]; th.innerHTML = frag(c.c, `<span>${th.innerHTML}</span>`, { inline: true }); } });
  };

  // ---------- player detail ----------
  const tabRender = {};
  const sessions = [["Windows · Ashfall client 0.9.214", "83.4.112.9", "today 08:12", "in 6 h"], ["Admin portal · Chrome", "83.4.112.9", "today 08:12", "in 11 h"]];

  tabRender.profile = u => `<div class="grid g-2">
    ${panel({ c: { module: "accounts", point: "entity.user.tab", prim: "PropertyList" }, title: "Profile", tools: `<button class="btn sm">${icon("edit")} Edit</button>`, body: props([["User id", `<span class="mono">${u.id}</span> ${UI.ref("user", u.id)}`], ["Handle", esc(u.handle)], ["Display name", esc(u.name)], ["Kind", badge(u.kind, u.kind === "staff" ? "accent" : "", true)], ["Status", status(u.status)], ["Country", u.country], ["Created", u.created], ["Last seen", u.lastSeen]]) })}
    ${panel({ c: { module: "accounts", point: "entity.user.tab", prim: "PropertyList" }, title: "Personal data", state: "denied", perm: "accounts.pii.read" })}
    ${panel({ c: { module: "accounts", point: "entity.user.tab", prim: "DataTable" }, title: "Linked identities", meta: `${u.providers.length}`, flush: true, tools: `<button class="btn sm">${icon("plus")} Link</button>`,
      body: table({ pager: false, cols: [{ label: "Provider" }, { label: "Subject" }, { label: "Linked" }, { label: "Last used" }, { label: "" }], rows: u.providers.map((p, i) => [`<span class="tag">${p}</span>`, `<span class="mono">${p === "google" ? "1094…" + u.id.slice(0, 4) : p === "epic" ? "epic|" + u.id.slice(2, 8) : u.id}</span>`, u.created, i ? "3 d ago" : u.lastSeen, `<button class="btn sm ghost danger-ghost" ${u.providers.length < 2 ? "disabled title='Last identity cannot be unlinked'" : ""}>Unlink</button>`]) }) })}
    ${panel({ c: { module: "accounts", point: "entity.user.tab", prim: "DataTable" }, title: "Active sessions", meta: "2", flush: true, tools: `<button class="btn sm danger-ghost">Revoke all</button>`,
      body: table({ pager: false, cols: [{ label: "Client" }, { label: "IP" }, { label: "Started" }, { label: "Expires" }, { label: "" }], rows: sessions.map(s => [...s.map(x => esc(x)), `<button class="btn sm ghost">Revoke</button>`]) }) })}
  </div>`;
  tabRender.characters = u => panel({ c: { module: "characters", point: "entity.user.tab", prim: "DataTable" }, title: "Characters", meta: `${u.characters}`, flush: true,
    body: table({ pager: false, cols: [{ label: "Name" }, { label: "Class" }, { label: "Level", num: true }, { label: "Region" }, { label: "Last played" }, { label: "Id" }], rows: [["Yuki", "Warden", 48, "eu-west", "4 min ago", "c_91a2"], ["Hoshi", "Ashcaller", 31, "eu-west", "6 d ago", "c_91a3"], ["Tsubaki", "Ranger", 12, "ap-ne", "41 d ago", "c_a002"]].slice(0, Math.max(1, u.characters)).map(r => [r[0], r[1], r[2], `<span class="tag">${r[3]}</span>`, r[4], `<span class="mono muted">${r[5]}</span>`]) }) });
  tabRender.wallet = () => `<div class="grid g-tiles" style="margin-bottom:12px">${["Embers (EMB)", "Gold (GLD)", "Reroll tokens (TOK)"].map(l => kpi({ c: { module: "wallet", point: "entity.user.tab", prim: "KpiTile" }, label: l, value: "—", state: "error" })).join("")}</div>` +
    panel({ c: { module: "wallet", point: "entity.user.tab", prim: "DataTable" }, title: "Transactions", state: "error" });
  tabRender.inventory = u => panel({ c: { module: "inventory", point: "entity.user.tab", prim: "DataTable" }, title: "Inventory", meta: "37 items · owner user:" + u.id, flush: true, tools: `<button class="btn sm">${icon("gift")} Grant item</button>`,
    body: table({ filters: [{ k: "Owner", v: "all characters" }, { k: "Type", v: "any" }], searchPlaceholder: "Item id or name", cols: [{ label: "Item" }, { label: "Type" }, { label: "Qty", num: true }, { label: "Owner" }, { label: "Source" }, { label: "Acquired" }],
      rows: [["Frost Warden skin", "cosmetic", 1, "user", "store.purchase", "2026-10-22"], ["Ashblade +3", "weapon", 1, "character:Yuki", "quest.reward", "2026-10-19"], ["Ember shard", "material", 412, "character:Yuki", "match.reward", "2026-10-22"], ["Reroll token", "consumable", 12, "user", "event.s4", "2026-10-20"], ["Warden banner", "cosmetic", 1, "user", "admin.grant", "2026-09-02"]].map(r => [r[0], `<span class="tag">${r[1]}</span>`, r[2], `<span class="mono">${r[3]}</span>`, `<span class="mono muted">${r[4]}</span>`, r[5]]),
      rowMenu: () => `<button class="btn sm icon ghost">${icon("more")}</button>`, total: "37 items", page: "1 / 2" }) });
  tabRender.friends = () => panel({ c: { module: "friends", point: "entity.user.tab", prim: "DataTable" }, title: "Friends", meta: "12 · 1 pending", flush: true,
    body: table({ cols: [{ label: "User" }, { label: "State" }, { label: "Since" }], rows: [["jonas.l", "accepted", "2025-03-14"], ["MiraVoss", "accepted", "2026-02-01"], ["NightOwl_77", "accepted", "2026-05-22"], ["kestrel", "pending (outgoing)", "2026-10-21"], ["diego.r", "accepted", "2025-03-11"]].map(r => [userCell(r[0]), r[1].startsWith("pending") ? badge(r[1], "info") : badge(r[1], "ok"), r[2]]), rowMenu: () => `<button class="btn sm icon ghost">${icon("more")}</button>`, total: "12 friends", page: "1 / 1" }) });
  tabRender.groups = () => panel({ c: { module: "groups", point: "entity.user.tab", prim: "DataTable" }, title: "Groups", meta: "2", flush: true,
    body: table({ pager: false, cols: [{ label: "Group" }, { label: "Role" }, { label: "Members", num: true }, { label: "Joined" }], rows: [["Frostreach Playtesters", "officer", 48, "2025-04-02"], ["Emberline Staff", "member", 9, "2025-03-11"]].map(r => [`<a href="#">${r[0]}</a>`, badge(r[1], "", true), r[2], r[3]]) }) });
  tabRender.saves = () => panel({ c: { module: "storage", point: "entity.user.tab", prim: "DataTable" }, title: "Cloud saves", meta: "3 slots · 41.2 MB of 64 MB", readonly: true, flush: true,
    readonlyText: "Read-only: storage exposes saves for inspection and download; edits and deletes are not remotely supported.",
    tools: ({ readonly }) => `<button class="btn sm">${icon("download")} Download all</button><button class="btn sm danger-ghost" ${readonly ? "disabled" : ""}>${icon("trash")} Delete</button>`,
    body: table({ pager: false, cols: [{ label: "Slot" }, { label: "Size", num: true }, { label: "Updated" }, { label: "Version" }, { label: "Checksum" }, { label: "" }], rows: [["autosave", "18.4 MB", "4 min ago", "v214", "sha256:9c1f…e02a"], ["manual-1", "17.9 MB", "2 d ago", "v211", "sha256:0b7d…41c9"], ["settings", "4.9 MB", "12 d ago", "v190", "sha256:77ea…d310"]].map(r => [`<span class="mono">${r[0]}</span>`, r[1], r[2], `<span class="tag">${r[3]}</span>`, `<span class="mono muted">${r[4]}</span>`, `<button class="btn sm ghost">${icon("download")}</button>`]) }) });
  tabRender.inbox = () => panel({ c: { module: "notifications", point: "entity.user.tab", prim: "Timeline" }, title: "Inbox", meta: "4 · 1 unread", flush: true, tools: `<button class="btn sm">${icon("send")} Send notification</button>`,
    body: timeline([
      { t: "12:00", day: "Today", html: "<b>Season 4 rewards</b> are waiting in your mail", meta: "template season_reward · unread", icon: "gift", tone: "accent" },
      { t: "09:31", day: "Today", html: "Frostreach is live!", meta: "broadcast motd · read", icon: "bell" },
      { t: "17:44", day: "Oct 20", html: "jonas.l accepted your friend request", meta: "friends.accepted · read", icon: "heart" },
      { t: "11:02", day: "Oct 18", html: "Playtest Fri 24 Oct — RSVP", meta: "template playtest_invite · read", icon: "mail" },
    ]) });
  tabRender.lore = u => {
    const ov = D.overrides[u.handle] || {};
    return `<div class="grid g-2">${panel({ c: { module: "lore", point: "entity.user.tab", prim: "PropertyList + DataTable" }, title: "Lore access", tools: `<a class="btn sm" href="lore-permissions.html?u=${u.handle}#effective">${icon("shield")} Permissions</a>`,
      body: props([["Global role", badge(u.loreRole, "accent", true)], ["Seat", "since " + u.seatSince], ["Actions", (D.roles.find(r => r.name === u.loreRole)?.actions || []).map(a => `<span class="tag">${a}</span>`).join(" ")], ["Overrides", Object.keys(ov).length ? Object.entries(ov).map(([r, v]) => `${UI.repoLink(r)} → ${badge(v, v === "none" ? "err" : "", true)}`).join("<br>") : `<span class="muted">none — global role applies to all 6 repositories</span>`], ["Locks held", `${D.locks.filter(l => l.owner === u.handle).length} · <a href="lore-locks.html?owner=${u.handle}">view</a>`]]) })}
      ${panel({ c: { module: "lore", point: "entity.user.tab", prim: "Timeline" }, title: "Recent pushes & locks", flush: true, body: timeline(D.loreEvents.filter(e => e.who === u.handle).map(e => ({ t: e.t, day: e.day, html: `${esc(e.text)} in ${UI.repoLink(e.repo)}`, meta: `${esc(e.branch)}${e.detail ? " · " + esc(e.detail) : ""}`, icon: e.icon, tone: e.tone }))) })}</div>`;
  };

  P.player = {
    meta: { nav: "players", crumbs: [] },
    render: () => {
      const q = new URLSearchParams(location.search);
      const u = D.byHandle[q.get("u")] || D.byHandle["aiko.tanaka"];
      P.currentUser = u;
      const tabs = REG.userTabs.filter(t => !t.when || t.when(u));
      const active = tabs.find(t => t.id === (P.tab || q.get("tab"))) ? (P.tab || q.get("tab")) : "profile";
      P.player.meta.crumbs = [["Players", "players.html"], ["Users & players", "players.html"], [esc(u.handle)]];
      const facts = REG.userFacts.map(f => { const h = f.render(u); return h ? frag({ module: f.module, point: "entity.user.summary_field", prim: "SummaryFact" }, `<span>${h}</span>`, { inline: true, tag: "span" }) : ""; }).join("");
      const actions = REG.userHeaderActions.filter(a => !a.when || a.when(u)).map(a => frag({ module: a.module, point: "entity.user.header_action", prim: "MenuAction" }, `<button class="btn ${a.danger ? "danger-ghost" : ""}" onclick="${a.run}">${icon(a.icon)} ${esc(a.label)}</button>`, { inline: true, tag: "span" })).join("");
      const tabBar = tabs.map(t => frag({ module: t.module, point: "entity.user.tab", prim: t.prim }, `<button class="tab ${t.id === active ? "on" : ""}" onclick="PAGES.tab='${t.id}';REG.render()">${esc(t.label)}${t.count ? ` <span class="count">${t.count(u)}</span>` : ""}${t.state === "error" ? `<span class="dot err" title="module unavailable"></span>` : ""}${t.readonly ? `<span class="muted" title="read-only">${icon("lock")}</span>` : ""}</button>`, { inline: true, tag: "span" })).join("");
      const header = frag({ module: "portal", point: "entity.user (host)", prim: "EntityDetail" }, `<section class="panel" style="margin-bottom:12px"><div class="entity-head">${UI.avatar(u, true)}<div class="grow"><div class="row"><h1>${esc(u.name)}</h1>${u.name !== u.handle ? `<span class="muted">@${esc(u.handle)}</span>` : ""}${status(u.status)}${badge(u.kind, u.kind === "staff" ? "accent" : "", true)}</div><div class="entity-facts">${facts}</div></div><div class="page-actions">${actions}<button class="btn icon" onclick="UI.openMenu(event,'user','${u.handle}')">${icon("more")}</button></div></div><div class="tabs">${tabBar}</div></section>`, { left: true });
      return header + tabRender[active](u);
    },
  };

  // ---------- states gallery ----------
  P.states = {
    meta: { nav: "states", crumbs: [["Design system", "catalog.html"], ["Panel states"]] },
    render: () => {
      const sample = { cols: [{ label: "File" }, { label: "Owner" }, { label: "Since" }], rows: D.locks.slice(0, 3).map(l => [`<span class="mono">${esc(l.file.split("/").pop())}</span>`, userCell(l.owner), l.age]), pager: false };
      const st = [
        ["ok", "Live data", {}],
        ["loading", "First fetch in flight; skeleton matches the primitive's shape. Shown after 150 ms to avoid flicker.", { state: "loading" }],
        ["empty", "Fetch succeeded with zero rows. The module supplies the message and an optional call to action.", { state: "empty", emptyTitle: "No open locks", emptyDetail: "Nobody holds a lock in this repository.", emptyIcon: "unlock" }],
        ["stale", "Module is slow or flapping; the portal shows its last good response with age. Writes stay enabled only if the module is reachable.", { state: "stale", staleText: "Showing cached data from 3 min ago — lore-svc is lagging (p99 4.2 s)." }],
        ["error", "Module did not answer. Only this panel fails; Retry re-fetches this panel alone.", { state: "error", errorDetail: "lore-svc: connection refused (10.0.4.17:7301). Last good response 3 min ago." }],
        ["readonly", "Module declared no remote write support. Data visible, write actions disabled with explanation.", { readonly: true }],
        ["denied", "Operator lacks the permission the panel declares. Nothing is fetched.", { state: "denied", perm: "lore.locks.read" }],
      ];
      const tableCards = st.map(([k, d, o]) => `<div><div class="cat-state-label">${k}</div>${panel({ c: { module: "lore", point: "page", prim: "DataTable" }, title: "Open locks", meta: "ashfall-audio", flush: true, noForce: true, tools: ({ readonly }) => `<button class="btn sm danger-ghost" ${readonly ? "disabled" : ""}>Force unlock</button>`, body: table(sample), ...o })}<div class="muted" style="font-size:12px;margin-top:6px">${d}</div></div>`).join("");
      const kpis = ["ok", "loading", "empty", "stale", "error", "denied"].map(k => `<div><div class="cat-state-label">${k}</div>${kpi({ c: { module: "lore", point: "dashboard.tile", prim: "KpiTile" }, label: "Lore store used", value: "13.29", unit: "TB", delta: "▲ 1.19 TB this month", spark: D.billing.tb, state: k, noForce: true })}</div>`).join("");
      return head("Panel states", "Every primitive renders inside the same panel chrome, which owns these states. Modules return data or a typed failure; they never draw their own error UI.") +
        `<h2 style="font-size:14px;margin:4px 0 10px">DataTable in a panel</h2><div class="cat-states" style="margin-bottom:24px">${tableCards}</div><h2 style="font-size:14px;margin:4px 0 10px">KpiTile</h2><div class="cat-states">${kpis}</div>`;
    },
  };
})();

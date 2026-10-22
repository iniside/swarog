// Mock module manifests: what each module contributes to which extension point.
// The shell and pages read ONLY from here; nothing in the shell knows a domain.
window.REG = (() => {
  const { icon, esc } = UI;
  const D = DATA;

  // nav.section — sections are declared once (by any module) and ordered by `order`.
  const sections = [
    { id: "overview", label: "Overview", order: 0, owner: "portal" },
    { id: "players", label: "Players", order: 10, owner: "accounts" },
    { id: "lore", label: "Lore", order: 20, owner: "lore" },
    { id: "economy", label: "Economy", order: 30, owner: "wallet" },
    { id: "platform", label: "Platform", order: 40, owner: "portal" },
    { id: "design", label: "Design system", order: 90, owner: "portal" },
  ];
  // nav.entry
  const nav = [
    { module: "portal", section: "overview", id: "dashboard", label: "Dashboard", icon: "home", href: "index.html" },
    { module: "accounts", section: "players", id: "players", label: "Users & players", icon: "users", href: "players.html", count: "214,903" },
    { module: "lore", section: "lore", id: "lore.repos", label: "Repositories", icon: "repo", href: "lore-repos.html", count: "6" },
    { module: "lore", section: "lore", id: "lore.permissions", label: "Permissions", icon: "shield", href: "lore-permissions.html" },
    { module: "lore", section: "lore", id: "lore.locks", label: "Locks", icon: "lock", href: "lore-locks.html", count: "26", countTone: "warn" },
    { module: "lore", section: "lore", id: "lore.server", label: "Server instance", icon: "server", href: "lore-server.html" },
    { module: "lore", section: "lore", id: "lore.events", label: "Events", icon: "activity", href: "lore-events.html" },
    { module: "lore", section: "lore", id: "lore.billing", label: "Billing & usage", icon: "chart", href: "lore-billing.html" },
    { module: "wallet", section: "economy", id: "economy", label: "Currencies & ledger", icon: "coins", href: "economy.html", down: true },
    { module: "apikeys", section: "platform", id: "apikeys", label: "API keys", icon: "key", href: "apikeys.html" },
    { module: "mail", section: "platform", id: "mail", label: "Mail outbox", icon: "mail", href: "mail.html", count: "312", countTone: "warn" },
    { module: "scheduler", section: "platform", id: "scheduler", label: "Scheduler", icon: "clock", href: "scheduler.html" },
    { module: "config", section: "platform", id: "config", label: "Config & flags", icon: "sliders", href: "config.html" },
    { module: "audit", section: "platform", id: "audit", label: "Audit log", icon: "list", href: "audit.html" },
    { module: "core.events", section: "platform", id: "events", label: "Event plane", icon: "zap", href: "events.html", count: "1 paused", countTone: "alert" },
    { module: "portal", section: "design", id: "catalog", label: "Primitive catalog", icon: "book", href: "catalog.html" },
    { module: "portal", section: "design", id: "states", label: "Panel states", icon: "grid", href: "states.html" },
  ];

  // search.provider — each provider answers independently; one may time out.
  const search = [
    { module: "accounts", kind: "user", label: "Users & players", icon: "user", results: q => D.users.filter(u => !q || u.handle.toLowerCase().includes(q) || u.name.toLowerCase().includes(q)).slice(0, 5).map(u => ({ title: u.handle, sub: `${u.kind} · ${u.id}`, right: u.status, href: `player.html?u=${u.handle}` })) },
    { module: "lore", kind: "repository", label: "Lore repositories", icon: "repo", results: q => D.repos.filter(r => !q || r.name.includes(q)).slice(0, 4).map(r => ({ title: r.name, sub: `${r.size.toFixed(2)} TB · ${r.branches} branches`, right: `${r.locks} locks`, href: `lore-repo.html?r=${r.name}` })) },
    { module: "apikeys", kind: "apikey", label: "API keys", icon: "key", results: q => D.apiKeys.filter(k => !q || k.name.includes(q) || k.id.includes(q)).slice(0, 3).map(k => ({ title: k.name, sub: `${k.id} · role ${k.role}`, right: k.status, href: "apikeys.html" })) },
    { module: "wallet", kind: "transaction", label: "Wallet transactions", icon: "coins", timeout: true },
  ];

  // dashboard.tile
  const tiles = [
    { module: "accounts", label: "Active players (24 h)", value: "38,412", delta: '<span class="up">▲ 6.1%</span> vs prev. day', spark: [31, 30, 33, 32, 35, 34, 36, 35, 37, 38.4], href: "players.html" },
    { module: "lore", label: "Lore seats", value: "8", unit: "/ 10 licensed", bar: 80, delta: "1 seat suspended (sam.o)", href: "lore-billing.html" },
    { module: "lore", label: "Lore store used", value: "13.29", unit: "TB", delta: '<span class="bad-up">▲ 1.19 TB</span> this month', spark: [6.2, 7.4, 8.9, 10.6, 12.1, 13.29], sparkColor: "var(--chart-2)", href: "lore-billing.html" },
    { module: "storage", label: "Cloud saves used", value: "1.82", unit: "TB", delta: "64 MB quota / player", spark: [1.5, 1.55, 1.6, 1.64, 1.7, 1.76, 1.82], sparkColor: "var(--chart-2)" },
    { module: "mail", label: "Mail queue", value: "312", delta: '<span class="bad-up">▲ relay throttled</span> · 1 failed', spark: [4, 6, 3, 8, 20, 140, 310, 312], sparkColor: "var(--warn)", href: "mail.html" },
    { module: "core.events", label: "Paused subscriptions", value: "1", unit: "/ 7", delta: '<span class="down">inventory.on_purchase.v1</span>', href: "events.html" },
    { module: "wallet", label: "Embers spent (24 h)", value: "1.94 M", state: "error" },
    { module: "match", label: "Matches in progress", value: "1,204", delta: '<span class="flat">— steady</span>', spark: [900, 1010, 1100, 1180, 1220, 1190, 1204] },
  ];

  // dashboard.attention — modules surface things needing an operator.
  const attention = [
    { module: "wallet", tone: "err", icon: "alert", title: "wallet-svc unavailable", text: "7 restarts in 15 min; grants and ledger are offline.", href: "economy.html", action: "Open" },
    { module: "core.events", tone: "err", icon: "zap", title: "Subscription paused by poison event", text: "inventory.on_purchase.v1 stopped at seq 12,905 · 118 behind.", href: "events.html", action: "Resolve" },
    { module: "lore", tone: "warn", icon: "lock", title: "2 stale Lore locks", text: "Held > 14 days by sam.o (suspended) in ashfall-game, ashfall-art.", href: "lore-locks.html?stale=1", action: "Review" },
    { module: "mail", tone: "warn", icon: "mail", title: "Mail relay throttling", text: "312 queued, 2 retrying, 1 failed permanently.", href: "mail.html", action: "Outbox" },
    { module: "scheduler", tone: "warn", icon: "clock", title: "storage.gc failed at 03:30", text: "timeout: storage-svc deadline exceeded.", href: "scheduler.html", action: "Logs" },
  ];

  // user.tab — the entity detail for kind "user" is built from these.
  const userTabs = [
    { module: "accounts", id: "profile", label: "Profile & identities", prim: "PropertyList + DataTable" },
    { module: "characters", id: "characters", label: "Characters", prim: "DataTable", count: u => u.characters },
    { module: "wallet", id: "wallet", label: "Wallet", prim: "KpiTile + DataTable", state: "error" },
    { module: "inventory", id: "inventory", label: "Inventory", prim: "DataTable", count: () => 37 },
    { module: "friends", id: "friends", label: "Friends", prim: "DataTable", count: () => 12 },
    { module: "groups", id: "groups", label: "Groups", prim: "DataTable", count: () => 2 },
    { module: "storage", id: "saves", label: "Cloud saves", prim: "DataTable", readonly: true },
    { module: "notifications", id: "inbox", label: "Inbox", prim: "Timeline", count: () => 4 },
    { module: "lore", id: "lore", label: "Lore", prim: "PropertyList + Timeline", when: u => !!u.loreRole },
  ];

  // user.summary_field — facts shown in the entity header.
  const userFacts = [
    { module: "accounts", render: u => `${icon("hash")} <span class="mono">${u.id}</span>` },
    { module: "accounts", render: u => `${icon("clock")} joined ${u.created}` },
    { module: "characters", render: u => `${icon("swords")} ${u.characters} characters` },
    { module: "lore", render: u => u.loreRole ? `${icon("shield")} Lore ${u.loreRole}` : "" },
    { module: "wallet", render: () => `<span style="color:var(--err)">${icon("alert")} wallet unavailable</span>` },
  ];

  // user.header_action
  const userHeaderActions = [
    { module: "lore", label: "Change Lore role", icon: "shield", run: "PAGES.changeRole()", when: u => !!u.loreRole },
    { module: "notifications", label: "Notify", icon: "send", run: "UI.toast('Compose notification…','info')" },
    { module: "accounts", label: "Suspend", icon: "ban", danger: true, run: "PAGES.suspend()" },
  ];

  // user.row_action — grouped per module in the row menu.
  const userRowActions = [
    { module: "accounts", items: [
      { label: "Open detail", icon: "external", run: "location.href='player.html?u=$'" },
      { label: "Suspend account…", icon: "ban", run: "PAGES.suspend('$')" },
      { label: "Delete account…", icon: "trash", danger: true, run: "PAGES.deleteAccount('$')" },
    ] },
    { module: "wallet", note: "unavailable", items: [
      { label: "Grant currency…", icon: "gift", disabled: "wallet module unavailable" },
      { label: "Revoke currency…", icon: "coins", disabled: "wallet module unavailable" },
    ] },
    { module: "inventory", items: [{ label: "Grant item…", icon: "box", run: "UI.toast('Grant item form…','info')" }] },
    { module: "notifications", items: [{ label: "Send notification…", icon: "send", run: "UI.toast('Compose notification…','info')" }] },
    { module: "lore", when: u => !!u.loreRole, items: [
      { label: "Change Lore role…", icon: "shield", run: "PAGES.changeRole('$')" },
      { label: "Release all locks…", icon: "unlock", danger: true, run: "PAGES.releaseAll('$')" },
    ] },
    { module: "storage", items: [{ label: "Download cloud saves", icon: "download", hint: "read-only", run: "UI.toast('Preparing archive…','info')" }] },
  ];

  // notification.source
  const notifications = [
    { module: "wallet", tone: "err", title: "wallet-svc is down", text: "Health probe failed 7× in 15 min.", when: "3 min" },
    { module: "core.events", tone: "err", title: "inventory.on_purchase.v1 paused", text: "Poison event at seq 12,905.", when: "4 h" },
    { module: "lore", tone: "warn", title: "Lore store passed 13 TB", text: "Plan includes 15 TB; 88% used.", when: "5 h" },
    { module: "mail", tone: "warn", title: "SMTP relay throttling", text: "Retry backoff active.", when: "2 h" },
    { module: "apikeys", tone: "info", title: "Key ak_c4a8 created", text: "by marta.k · role ci-build", when: "6 h" },
  ];

  UI.defineMenu("user", handle => {
    const u = D.byHandle[handle];
    return userRowActions.filter(g => !g.when || g.when(u)).map(g => ({
      c: { module: g.module, point: "user.row_action", prim: "MenuAction" },
      label: g.module, note: g.note ? `<span style="color:var(--err)">${g.note}</span>` : "",
      items: g.items.map(it => ({ ...it, run: (it.run || "").replace(/\$/g, handle) })),
    }));
  });

  // ---------- shell ----------
  const healthDown = D.services.filter(s => s.state !== "running");

  function sidebar(active) {
    const groups = sections.slice().sort((a, b) => a.order - b.order).map(s => {
      const items = nav.filter(n => n.section === s.id);
      if (!items.length) return "";
      const entries = items.map(n => UI.frag({ module: n.module, point: "nav.entry", prim: "NavEntry" },
        `<a class="nav-item ${n.id === active ? "active" : ""} ${n.down ? "down" : ""}" href="${n.href}" title="${n.down ? "wallet module unavailable" : ""}">${icon(n.icon)}<span>${esc(n.label)}</span>${n.count && !n.down ? `<span class="count ${n.countTone || ""}">${esc(n.count)}</span>` : ""}</a>`, { inline: true })).join("");
      return UI.frag({ module: s.owner, point: "nav.section", prim: "" }, `<div class="nav-section"><div class="nav-title">${esc(s.label)}</div>${entries}</div>`);
    }).join("");
    return `<aside class="sidebar"><div class="brand"><div class="brand-mark">S</div><div><div class="brand-name">Swarog Admin</div><div class="brand-sub">emberline-prod</div></div></div><nav class="nav">${groups}</nav><div class="sidebar-foot"><div class="me">${UI.avatar(D.byHandle["marta.k"])}<div class="grow"><div style="font-weight:500">Marta Kowalczyk</div><div class="muted" style="font-size:11.5px">operator · admin.users</div></div></div></div></aside>`;
  }

  function topbar(crumbs) {
    const cr = crumbs.map((c, i) => i === crumbs.length - 1 ? `<span class="cur">${c[0]}</span>` : `<a href="${c[1]}">${c[0]}</a><span class="sep">/</span>`).join("");
    const states = ["", "loading", "empty", "error", "stale", "readonly", "denied"];
    return `<header class="topbar">
      <button class="top-btn menu-toggle" onclick="document.body.classList.toggle('nav-open')" aria-label="Menu">${icon("menu")}</button>
      ${UI.frag({ module: "portal", point: "shell.breadcrumbs", prim: "Breadcrumbs" }, `<div class="crumbs">${cr}</div>`, { inline: true, style: "min-width:0" })}
      <button class="search-trigger" onclick="REG.openSearch()">${icon("search")}<span class="grow" style="text-align:left">Search users, repositories, keys…</span><span class="kbd">Ctrl K</span></button>
      <button class="top-btn" id="health-btn" onclick="REG.healthPop(event)" title="System health"><span class="health-pill"><span class="dot err"></span><span class="hide-tablet">${D.services.length - healthDown.length}/${D.services.length} healthy</span></span></button>
      <button class="top-btn" id="notif-btn" onclick="REG.notifPop(event)" aria-label="Notifications">${icon("bell")}<span class="pip"></span></button>
      <span class="devbar">
        <select class="select" title="Preview panel state (mockup only)" onchange="REG.setForce(this.value)">${states.map(s => `<option value="${s}" ${UI.state.force === s ? "selected" : ""}>${s ? "State: " + s : "State: as designed"}</option>`).join("")}</select>
        <button class="top-btn ${UI.state.annot ? "on" : ""}" id="annot-btn" onclick="REG.toggleAnnot()" title="Outline every contributed fragment">${icon("layers")}<span class="hide-tablet">Show contributions</span></button>
        <button class="top-btn" onclick="REG.toggleTheme()" aria-label="Toggle theme">${icon(UI.state.theme === "dark" ? "sun" : "moon")}</button>
      </span>
    </header>`;
  }

  function legend() {
    const mods = [...new Set([...document.querySelectorAll(".page .frag, .sidebar .frag")].map(f => f.dataset.module))];
    const points = [...new Set([...document.querySelectorAll(".page .frag")].map(f => f.dataset.point))];
    return `<b style="color:var(--text)">Contributions on this screen:</b> ${mods.map(m => `<span><span class="sw" style="background:${UI.MODULE_COLORS[m] || "#475569"}"></span>${esc(m)}</span>`).join("")}<span class="muted">· extension points: ${points.map(p => `<span class="tag">${esc(p)}</span>`).join(" ")}</span>`;
  }

  let pageFn = null, pageMeta = null;
  function mount(meta, fn) {
    pageMeta = meta; pageFn = fn;
    render();
    document.addEventListener("keydown", e => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); openSearch(); }
      if (e.key === "Escape") { UI.closeModal(); UI.closePop(); }
    });
    if (new URLSearchParams(location.search).get("search")) openSearch(new URLSearchParams(location.search).get("search"));
  }
  function render() {
    const scroll = document.querySelector(".main")?.scrollTop || 0;
    document.body.classList.toggle("annot", UI.state.annot);
    const content = pageFn();
    document.body.innerHTML = `<div class="app">${sidebar(pageMeta.nav)}${topbar(pageMeta.crumbs)}<main class="main"><div class="page"><div class="annot-legend"></div>${content}</div></main></div>`;
    document.querySelector(".annot-legend").innerHTML = legend();
    document.querySelector(".main").scrollTop = scroll;
    document.title = `${pageMeta.crumbs[pageMeta.crumbs.length - 1][0].replace(/<[^>]+>/g, "")} · Swarog Admin`;
    pageMeta.after?.();
  }

  function setForce(v) { UI.state.force = v; render(); }
  function toggleAnnot() { UI.state.annot = !UI.state.annot; try { localStorage.setItem("swg.annot", UI.state.annot ? "1" : "0"); } catch (e) { /* ignore */ } render(); }
  function toggleTheme() { UI.state.theme = UI.state.theme === "dark" ? "light" : "dark"; document.documentElement.dataset.theme = UI.state.theme; try { localStorage.setItem("swg.theme", UI.state.theme); } catch (e) { /* ignore */ } render(); }

  function healthPop(ev) {
    const html = `<div class="popover-head">${icon("activity")} System health<span class="grow"></span><a href="index.html#health" style="font-weight:500;font-size:12px">All services</a></div>${UI.frag({ module: "portal", point: "shell.health", prim: "ServiceStatus" }, `<div class="pop-list">${UI.services(healthDown, { bars: false })}<div class="pop-row"><span class="dot ok" style="margin-top:4px"></span><div><div class="t">${D.services.length - healthDown.length} services healthy</div><div class="d">Probed every 10 s by the portal · last probe 2 s ago</div></div></div></div>`)}`;
    UI.popover(ev, html);
  }
  function notifPop(ev) {
    const rows = notifications.map(n => UI.frag({ module: n.module, point: "notification.source", prim: "" }, `<div class="pop-row"><span class="dot ${n.tone === "info" ? "muted" : n.tone}" style="margin-top:5px"></span><div class="grow"><div class="t">${esc(n.title)}</div><div class="d">${esc(n.text)}</div></div><span class="muted" style="font-size:11.5px">${n.when}</span></div>`, { left: true })).join("");
    UI.popover(ev, `<div class="popover-head">${icon("bell")} Notifications<span class="grow"></span><button class="btn sm ghost">Mark all read</button></div><div class="pop-list">${rows}</div>`);
  }

  function openSearch(initial = "") {
    UI.closeModal();
    const s = document.createElement("div");
    s.className = "scrim";
    s.innerHTML = `<div class="palette" role="dialog" aria-label="Search"><div class="palette-in">${icon("search")}<input placeholder="Search users, repositories, API keys…" value="${esc(initial)}"><span class="kbd">Esc</span></div><div class="palette-scopes"><span class="filter-chip" style="background:var(--selected);color:var(--accent-fg)">All</span>${search.map(p => `<span class="filter-chip">${icon(p.icon)} ${esc(p.kind)}</span>`).join("")}</div><div class="palette-list"></div><div class="palette-foot"><span><span class="kbd">↑↓</span> navigate</span><span><span class="kbd">↵</span> open</span><span><span class="kbd">Tab</span> scope</span><span class="grow"></span><span>${search.length} search providers</span></div></div>`;
    s.addEventListener("click", e => { if (e.target === s) s.remove(); });
    document.body.appendChild(s);
    const input = s.querySelector("input");
    const list = s.querySelector(".palette-list");
    const draw = () => {
      const q = input.value.trim().toLowerCase();
      let first = true;
      list.innerHTML = search.map(p => {
        const c = { module: p.module, point: "search.provider", prim: "SearchResult" };
        if (p.timeout) return q ? UI.frag(c, `<div class="palette-group"><span>${esc(p.label)}</span><span>${esc(p.module)}</span></div><div class="palette-item" style="color:var(--text-3);cursor:default">${icon("alert")} Provider timed out after 800 ms — other results are unaffected <button class="btn sm" style="margin-left:auto" onclick="event.stopPropagation();UI.toast('Retrying wallet search…','info')">Retry</button></div>`) : "";
        const rs = p.results(q);
        if (!rs.length) return "";
        return UI.frag(c, `<div class="palette-group"><span>${esc(p.label)}</span><span>${esc(p.module)}</span></div>${rs.map(r => { const h = first; first = false; return `<a class="palette-item ${h ? "hl" : ""}" href="${r.href}" style="color:inherit;text-decoration:none">${icon(p.icon)}<div><div>${esc(r.title)}</div><div class="sub">${esc(r.sub)}</div></div><span class="right">${esc(r.right)}</span></a>`; }).join("")}`);
      }).join("") || `<div class="state compact"><div class="state-title">No matches</div><div class="state-detail">Try a handle, user id, repository or key name.</div></div>`;
    };
    input.addEventListener("input", draw);
    draw();
    input.focus();
  }

  return { sections, nav, search, tiles, attention, userTabs, userFacts, userHeaderActions, userRowActions, notifications, mount, render, setForce, toggleAnnot, toggleTheme, healthPop, notifPop, openSearch };
})();

/* @ds-bundle: {"format":4,"namespace":"Swarog","components":[{"name":"Panel"},{"name":"DataTable"},{"name":"KpiTile"},{"name":"PropertyList"},{"name":"Timeline"},{"name":"Form"},{"name":"ConfirmAction"},{"name":"SecretReveal"},{"name":"ServiceStatus"},{"name":"TimeSeriesChart"},{"name":"MenuAction"},{"name":"Badge"},{"name":"Annotation"}]} */
window.UI = (() => {
  const ICONS = {
    home: '<path d="M3 10.5 12 3l9 7.5"/><path d="M5 9.5V21h14V9.5"/>',
    users: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20c.8-3.6 3.4-5.5 6.5-5.5s5.7 1.9 6.5 5.5"/><path d="M16 4.6a3.5 3.5 0 0 1 0 6.8"/><path d="M18 14.8c1.8.8 3 2.6 3.5 5.2"/>',
    user: '<circle cx="12" cy="8" r="4"/><path d="M4 21c1-4.2 4.2-6.5 8-6.5s7 2.3 8 6.5"/>',
    repo: '<path d="M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5z"/><path d="M5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3"/><path d="M9 7h6"/>',
    lock: '<rect x="4.5" y="10.5" width="15" height="10" rx="2"/><path d="M8 10.5V7a4 4 0 0 1 8 0v3.5"/>',
    unlock: '<rect x="4.5" y="10.5" width="15" height="10" rx="2"/><path d="M8 10.5V7a4 4 0 0 1 7.6-1.7"/>',
    shield: '<path d="M12 3 4.5 6v5.5c0 4.6 3.1 8.2 7.5 9.5 4.4-1.3 7.5-4.9 7.5-9.5V6z"/>',
    server: '<rect x="3.5" y="4" width="17" height="7" rx="1.5"/><rect x="3.5" y="13" width="17" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/>',
    activity: '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
    chart: '<path d="M4 20V4"/><path d="M4 20h16"/><path d="m7 15 4-5 3 3 5-7"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="m10.8 12.2 8.7-8.7"/><path d="m16.5 6.5 2.5 2.5"/><path d="m14 9 2 2"/>',
    mail: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3.5 6.5 8.5 6.5 8.5-6.5"/>',
    clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
    sliders: '<path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
    list: '<path d="M9 6h11M9 12h11M9 18h11"/><path d="M4.5 6h.01M4.5 12h.01M4.5 18h.01"/>',
    zap: '<path d="M13 3 5 13.5h6L10 21l8-10.5h-6z"/>',
    coins: '<ellipse cx="9" cy="7" rx="5.5" ry="2.5"/><path d="M3.5 7v4c0 1.4 2.5 2.5 5.5 2.5"/><path d="M3.5 11v4c0 1.4 2.5 2.5 5.5 2.5"/><ellipse cx="15" cy="13" rx="5.5" ry="2.5"/><path d="M9.5 13v4c0 1.4 2.5 2.5 5.5 2.5s5.5-1.1 5.5-2.5v-4"/>',
    search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>',
    bell: '<path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15z"/><path d="M10 20.5a2 2 0 0 0 4 0"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4"/>',
    moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>',
    layers: '<path d="m12 3 9 5-9 5-9-5z"/><path d="m3 13 9 5 9-5"/>',
    menu: '<path d="M4 6h16M4 12h16M4 18h16"/>',
    more: '<circle cx="5" cy="12" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="19" cy="12" r="1.2"/>',
    right: '<path d="m9 5 7 7-7 7"/>',
    left: '<path d="m15 5-7 7 7 7"/>',
    down: '<path d="m5 9 7 7 7-7"/>',
    refresh: '<path d="M20 11a8 8 0 0 0-14.3-4.9L4 8"/><path d="M4 3.5V8h4.5"/><path d="M4 13a8 8 0 0 0 14.3 4.9L20 16"/><path d="M20 20.5V16h-4.5"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    check: '<path d="m5 12.5 4.5 4.5L19 7"/>',
    alert: '<path d="M12 3.5 2.5 20h19z"/><path d="M12 10v4.5M12 17.5h.01"/>',
    info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.5h.01"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    filter: '<path d="M4 5h16l-6 7.5V19l-4 1.5v-8z"/>',
    download: '<path d="M12 4v11"/><path d="m7 10.5 5 5 5-5"/><path d="M5 20h14"/>',
    external: '<path d="M14 4h6v6"/><path d="M20 4 11 13"/><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
    copy: '<rect x="8.5" y="8.5" width="11" height="11" rx="1.5"/><path d="M15.5 8.5V5.5A1.5 1.5 0 0 0 14 4H5.5A1.5 1.5 0 0 0 4 5.5V14a1.5 1.5 0 0 0 1.5 1.5h3"/>',
    eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>',
    branch: '<circle cx="6" cy="5.5" r="2.2"/><circle cx="6" cy="18.5" r="2.2"/><circle cx="18" cy="8" r="2.2"/><path d="M6 7.7v8.6"/><path d="M18 10.2c0 4-6 3-11 6.5"/>',
    up: '<path d="M12 20V5"/><path d="m6 11 6-6 6 6"/>',
    box: '<path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z"/><path d="m4 7.5 8 4.5 8-4.5M12 12v9"/>',
    heart: '<path d="M12 20s-7.5-4.6-7.5-10A4.3 4.3 0 0 1 12 7.4 4.3 4.3 0 0 1 19.5 10c0 5.4-7.5 10-7.5 10z"/>',
    group: '<circle cx="12" cy="8" r="3"/><circle cx="5" cy="10" r="2.2"/><circle cx="19" cy="10" r="2.2"/><path d="M7 20c.6-3.4 2.6-5.5 5-5.5s4.4 2.1 5 5.5"/><path d="M2 18.5c.3-2.2 1.4-3.6 3-4"/><path d="M22 18.5c-.3-2.2-1.4-3.6-3-4"/>',
    cloud: '<path d="M7 18.5a4.5 4.5 0 0 1-.6-9 6 6 0 0 1 11.6 1.5 3.8 3.8 0 0 1-.5 7.5z"/>',
    inbox: '<path d="M3.5 13 6 5h12l2.5 8v6h-17z"/><path d="M3.5 13H9l1 2.5h4L15 13h5.5"/>',
    book: '<path d="M4 4.5h6a2 2 0 0 1 2 2V20a1.5 1.5 0 0 0-1.5-1.5H4z"/><path d="M20 4.5h-6a2 2 0 0 0-2 2V20a1.5 1.5 0 0 1 1.5-1.5H20z"/>',
    grid: '<rect x="4" y="4" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1"/>',
    play: '<path d="M7 4.5v15l12-7.5z"/>',
    pause: '<path d="M8 5v14M16 5v14"/>',
    skip: '<path d="M5 5v14l10-7z"/><path d="M19 5v14"/>',
    park: '<rect x="4" y="4" width="16" height="16" rx="3"/><path d="M10 16.5v-9h3a2.5 2.5 0 0 1 0 5h-3"/>',
    trash: '<path d="M4 7h16"/><path d="M9 7V4.5h6V7"/><path d="M6 7l1 13h10l1-13"/>',
    edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="m13.5 6.5 4 4"/>',
    ban: '<circle cx="12" cy="12" r="8.5"/><path d="m6 6 12 12"/>',
    gift: '<rect x="3.5" y="8" width="17" height="4" rx="1"/><path d="M5 12v8.5h14V12M12 8v12.5"/><path d="M12 8c-1.5-3.5-5.5-4-5.5-1.5S10 8 12 8zM12 8c1.5-3.5 5.5-4 5.5-1.5S14 8 12 8z"/>',
    send: '<path d="M21 3 10 14"/><path d="M21 3 14.5 21 10 14 3 9.5z"/>',
    swords: '<path d="M14.5 17.5 3 6V3h3l11.5 11.5"/><path d="m13 19 6-6M16 16l4 4M19 21l2-2"/><path d="M9.5 6.5 14 2h3v3l-4.5 4.5"/><path d="M5 14l-2 2 4 4 2-2"/>',
    db: '<ellipse cx="12" cy="5.5" rx="7.5" ry="2.5"/><path d="M4.5 5.5v13c0 1.4 3.4 2.5 7.5 2.5s7.5-1.1 7.5-2.5v-13"/><path d="M4.5 12c0 1.4 3.4 2.5 7.5 2.5s7.5-1.1 7.5-2.5"/>',
    puzzle: '<path d="M9 4h3a1.8 1.8 0 1 1 3.5 0H19v4a1.8 1.8 0 1 0 0 3.5V15h-4a1.8 1.8 0 1 1-3.5 0H8v-4a1.8 1.8 0 1 0 0-3.5z"/>',
    hash: '<path d="M5 9h15M4 15h15M10 4 8 20M16 4l-2 16"/>',
  };
  const icon = (name, cls = "") => `<svg class="i ${cls}" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name] || ""}</svg>`;

  const MODULE_COLORS = {
    portal: "#475569", admin: "#475569", accounts: "#2563eb", characters: "#0e7490", inventory: "#b45309", wallet: "#c026d3",
    friends: "#db2777", groups: "#7c3aed", party: "#9333ea", notifications: "#0891b2", mail: "#ea580c",
    storage: "#0d9488", match: "#4d7c0f", rating: "#65a30d", leaderboard: "#15803d", config: "#a16207",
    scheduler: "#be185d", audit: "#334155", apikeys: "#4338ca", lore: "#dc2626", "core.events": "#0369a1",
  };

  const esc = s => String(s ?? "").replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const fmt = n => typeof n === "number" ? n.toLocaleString("en-US") : n;

  const state = {
    theme: "light",
    annot: false,
    force: "",
    navOpen: false,
  };

  // ---------- annotation wrapper ----------
  function frag(c, html, opts = {}) {
    if (!c) return html;
    const color = MODULE_COLORS[c.module] || "#475569";
    const tag = opts.tag || "div";
    const cls = ["frag", opts.inline ? "inline-frag" : "", opts.left ? "left-label" : "", opts.cls || ""].join(" ");
    const title = `module: ${c.module}\nextension point: ${c.point}\nprimitive: ${c.prim || "—"}`;
    return `<${tag} class="${cls}" style="--fc:${color};${opts.style || ""}" data-module="${esc(c.module)}" data-point="${esc(c.point)}" data-prim="${esc(c.prim || "")}">` +
      `<span class="frag-label" title="${esc(title)}"><span class="m">${esc(c.module)}</span><span class="k">${esc(c.point)}</span>${c.prim ? `<span class="p">${esc(c.prim)}</span>` : ""}</span>` +
      html + `</${tag}>`;
  }

  // ---------- shared atoms ----------
  const badge = (text, tone = "", plain = false) => `<span class="badge ${tone} ${plain ? "plain" : ""}">${esc(text)}</span>`;
  const STATUS_TONE = { active: "ok", running: "ok", ok: "ok", sent: "ok", enabled: "ok", healthy: "ok", pending: "info", retrying: "warn", lagging: "warn", degraded: "warn", restarting: "warn", suspended: "warn", parked: "", disabled: "", revoked: "", failed: "err", fail: "err", paused: "err", down: "err", crashed: "err", banned: "err" };
  const status = s => badge(s, STATUS_TONE[s] ?? "");
  const initials = n => n.replace(/[^A-Za-z ._]/g, "").split(/[ ._]/).filter(Boolean).slice(0, 2).map(p => p[0].toUpperCase()).join("") || "?";
  const avatar = (u, lg) => `<span class="avatar ${lg ? "lg" : ""}" style="background:${u.color}">${initials(u.name)}</span>`;
  const userCell = (handle, link = true) => {
    const u = window.DATA?.byHandle?.[handle];
    if (!u) return `<span class="mono">${esc(handle)}</span>`;
    const inner = `<span class="user-cell">${avatar(u)}<span>${esc(u.handle)}</span></span>`;
    return link ? `<a href="player.html?u=${u.handle}" style="color:inherit">${inner}</a>` : inner;
  };
  const repoLink = name => `<a href="lore-repo.html?r=${name}" class="mono">${esc(name)}</a>`;
  const ref = (kind, id) => `<span class="tag">${esc(kind)}:${esc(id)}</span>`;

  // ---------- panel: the chrome every primitive lives in, owns all panel states ----------
  const STATE_DEFS = {
    loading: null,
    empty: { icon: "inbox", title: "Nothing here yet", cls: "" },
    error: { icon: "alert", title: "module unavailable", cls: "err" },
    denied: { icon: "lock", title: "Permission denied", cls: "denied" },
  };
  function stateBlock(kind, o = {}) {
    if (kind === "loading") {
      const rows = o.rows || 4;
      return `<div style="padding:14px 12px;display:flex;flex-direction:column;gap:12px" aria-busy="true">${Array.from({ length: rows }, (_, i) => `<div class="row"><div class="sk" style="width:${22 + (i * 17) % 30}%"></div><div class="sk" style="width:${12 + (i * 23) % 20}%"></div><div class="sk grow"></div></div>`).join("")}</div>`;
    }
    const d = STATE_DEFS[kind];
    const title = o.title || (kind === "error" ? `${o.module || "module"} module unavailable` : d.title);
    const detail = o.detail || (kind === "error" ? `${o.module || "module"}-svc did not answer within 2 s. The rest of this page is unaffected.` : kind === "denied" ? `Requires <span class="tag">${esc(o.perm || "admin.access")}</span>. Ask a deployment owner to grant it.` : "");
    const actions = o.actions ?? (kind === "error" ? `<button class="btn sm" onclick="UI.retry(this)">${icon("refresh")} Retry</button><a class="btn sm ghost" href="index.html#health">Service health</a>` : "");
    return `<div class="state ${d.cls} ${o.compact ? "compact" : ""}"><div class="state-icon">${icon(o.icon || d.icon)}</div><div class="state-title">${title}</div>${detail ? `<div class="state-detail">${detail}</div>` : ""}${actions ? `<div class="row">${actions}</div>` : ""}</div>`;
  }

  function panel(o) {
    const st = state.force && o.c && !o.noForce ? state.force : (o.state || "ok");
    const ro = st === "readonly" || o.readonly;
    let body;
    if (st === "loading") body = stateBlock("loading", { rows: o.skeletonRows });
    else if (st === "empty") body = stateBlock("empty", { title: o.emptyTitle, detail: o.emptyDetail, icon: o.emptyIcon, actions: o.emptyActions ?? "" });
    else if (st === "error") body = stateBlock("error", { module: o.c?.module, detail: o.errorDetail });
    else if (st === "denied") body = stateBlock("denied", { perm: o.perm });
    else body = typeof o.body === "function" ? o.body({ readonly: ro }) : o.body;

    const banner = st === "stale" ? `<div class="banner warn">${icon("clock")} ${o.staleText || `Showing cached data from 3 min ago — ${o.c?.module || "module"}-svc is lagging.`}<button class="btn sm" onclick="UI.retry(this)">${icon("refresh")} Refresh</button></div>`
      : ro ? `<div class="banner info">${icon("info")} ${o.readonlyText || `Read-only: the <b>${o.c?.module}</b> module does not accept remote edits from the portal.`}</div>`
      : (o.banner || "");
    const tools = st === "ok" || st === "stale" || ro ? (typeof o.tools === "function" ? o.tools({ readonly: ro }) : (o.tools || "")) : "";
    const head = o.title !== undefined ? `<div class="panel-head">${o.icon ? `<span class="muted">${icon(o.icon)}</span>` : ""}<span class="panel-title">${o.title}</span>${o.meta ? `<span class="panel-meta">${o.meta}</span>` : ""}${ro ? `<span class="ro-chip">${icon("lock")} read-only</span>` : ""}<div class="panel-tools">${tools}</div></div>` : "";
    const foot = o.foot && (st === "ok" || st === "stale" || ro) ? `<div class="panel-foot">${o.foot}</div>` : "";
    const flush = o.flush && (st === "ok" || st === "stale" || ro) ? "flush" : "";
    const html = `<section class="panel ${o.cls || ""}" style="${o.style || ""}">${head}${banner}<div class="panel-body ${flush}">${body}</div>${foot}</section>`;
    return o.c ? frag(o.c, html, { style: o.wrapStyle || "" }) : html;
  }

  // ---------- data table ----------
  function table(o) {
    const cols = o.cols;
    const sel = o.selectable;
    const filters = o.filters ? `<div class="tbl-toolbar">${o.search !== false ? `<span class="input-icon">${icon("search")}<input class="input" style="width:${o.searchWidth || 220}px" placeholder="${esc(o.searchPlaceholder || "Search…")}"></span>` : ""}${o.filters.map(f => typeof f === "string" ? f : `<span class="filter-chip">${esc(f.k)}: <b>${esc(f.v)}</b><span class="x">${icon("x")}</span></span>`).join("")}<span class="filter-chip add">${icon("plus")} Filter</span><span class="grow"></span>${o.toolbarRight || ""}</div>` : "";
    const bulk = o.bulk ? `<div class="bulkbar">${o.bulk}</div>` : "";
    const head = `<tr>${sel ? `<th class="cb"><input type="checkbox"></th>` : ""}${cols.map(c => `<th class="${c.num ? "num" : ""} ${c.sorted ? "sorted" : ""}" style="${c.w ? `width:${c.w}` : ""}">${esc(c.label)}${c.sorted ? ` <span>${c.sorted === "asc" ? "↑" : "↓"}</span>` : ""}</th>`).join("")}${o.rowMenu ? `<th class="act"></th>` : ""}</tr>`;
    const body = o.rows.map((r, i) => {
      const cells = r.cells || r;
      const href = r.href ? ` onclick="if(!event.target.closest('a,button,input'))location.href='${r.href}'"` : "";
      return `<tr class="${r.href ? "clickable" : ""} ${r.sel ? "sel" : ""}"${href}>${sel ? `<td class="cb"><input type="checkbox" ${r.sel ? "checked" : ""}></td>` : ""}${cells.map((v, j) => `<td class="${cols[j]?.num ? "num" : ""} ${cols[j]?.wrap ? "wrap" : ""}">${v}</td>`).join("")}${o.rowMenu ? `<td class="act">${o.rowMenu(r, i)}</td>` : ""}</tr>`;
    }).join("");
    const foot = o.pager !== false ? `<div class="tbl-foot"><span>${o.total ? `${esc(o.total)}` : `${o.rows.length} rows`}</span>${o.footNote ? `<span>· ${o.footNote}</span>` : ""}<div class="pager"><select class="select" style="height:24px;font-size:12px"><option>25 / page</option><option>50 / page</option><option>100 / page</option></select><button class="btn sm icon" disabled>${icon("left")}</button><span style="padding:0 6px">${o.page || "1 / 1"}</span><button class="btn sm icon" ${o.page && o.page !== "1 / 1" ? "" : "disabled"}>${icon("right")}</button></div></div>` : "";
    return `${filters}${bulk}<div class="tbl-wrap"><table class="tbl ${o.cls || ""}"><thead>${head}</thead><tbody>${body}</tbody></table></div>${foot}`;
  }

  const rowMenuBtn = (menuId, arg) => `<button class="btn sm icon ghost" aria-label="Row actions" onclick="UI.openMenu(event,'${menuId}','${esc(arg)}')">${icon("more")}</button>`;

  // ---------- KPI tile ----------
  function spark(values, color = "var(--chart-1)") {
    const w = 160, h = 26, max = Math.max(...values), min = Math.min(...values);
    const pts = values.map((v, i) => [i * (w / (values.length - 1)), h - 2 - ((v - min) / (max - min || 1)) * (h - 4)]);
    const d = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join("");
    return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><path d="${d}L${w},${h}L0,${h}Z" fill="${color}" opacity=".1"/><path d="${d}" fill="none" stroke="${color}" stroke-width="1.5" vector-effect="non-scaling-stroke"/></svg>`;
  }
  function kpi(o) {
    const st = state.force && !o.noForce ? state.force : (o.state || "ok");
    let inner;
    if (st === "loading") inner = `<div class="kpi"><div class="kpi-label">${esc(o.label)}</div><div class="sk" style="width:50%;height:20px;margin:6px 0"></div><div class="sk" style="width:70%"></div></div>`;
    else if (st === "error") inner = `<div class="kpi state-mini"><div class="kpi-label">${esc(o.label)}</div><div class="row" style="color:var(--err);font-weight:500;margin:6px 0 2px">${icon("alert")} ${esc(o.c.module)} module unavailable</div><div class="row"><button class="btn sm" onclick="UI.retry(this)">${icon("refresh")} Retry</button><span class="muted" style="font-size:11.5px">last value 3 min ago: ${o.value}</span></div></div>`;
    else if (st === "denied") inner = `<div class="kpi state-mini"><div class="kpi-label">${esc(o.label)}</div><div class="row" style="color:var(--warn);margin-top:6px">${icon("lock")} Requires ${esc(o.perm || `${o.c.module}.read`)}</div></div>`;
    else if (st === "empty") inner = `<div class="kpi"><div class="kpi-label">${esc(o.label)}</div><div class="kpi-value muted">—</div><div class="kpi-delta">No data reported yet</div></div>`;
    else inner = `<div class="kpi"><div class="kpi-label">${o.icon ? icon(o.icon) : ""}${esc(o.label)}${st === "stale" ? ` <span class="badge warn" style="margin-left:auto">stale 3m</span>` : ""}</div><div class="kpi-value">${o.value}${o.unit ? `<small>${o.unit}</small>` : ""}</div>${o.delta ? `<div class="kpi-delta">${o.delta}</div>` : ""}${o.bar != null ? `<div class="bar"><i style="width:${o.bar}%"></i></div>` : ""}${o.spark ? spark(o.spark, o.sparkColor) : ""}</div>`;
    const html = `<a class="panel" ${o.href ? `href="${o.href}"` : ""} style="color:inherit;text-decoration:none">${inner}</a>`;
    return frag(o.c, html);
  }

  // ---------- property list ----------
  const props = pairs => `<dl class="props">${pairs.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${v}</dd>`).join("")}</dl>`;

  // ---------- timeline ----------
  function timeline(items) {
    if (!items.length) return stateBlock("empty", { title: "No events", detail: "Nothing recorded in this range.", icon: "activity", actions: "", compact: true });
    let lastDay = null, out = "";
    for (const it of items) {
      if (it.day && it.day !== lastDay) { if (lastDay !== null) out += "</ul>"; out += `<div class="tl-day">${esc(it.day)}</div><ul class="tl">`; lastDay = it.day; }
      else if (lastDay === null && !it.day) { out += `<ul class="tl">`; lastDay = ""; }
      out += `<li><span class="when">${esc(it.t)}</span><span class="ico ${it.tone || ""}">${icon(it.icon || "activity")}</span><div class="body"><div>${it.html}</div>${it.meta ? `<div class="meta">${it.meta}</div>` : ""}</div></li>`;
    }
    return out + "</ul>";
  }

  // ---------- form ----------
  function field(f, ro) {
    const dis = ro || f.disabled ? "disabled" : "";
    let ctl;
    switch (f.type) {
      case "select": ctl = `<select class="select" style="width:${f.w || "260px"}" ${dis}>${f.options.map(o => `<option ${o === f.value ? "selected" : ""}>${esc(o)}</option>`).join("")}</select>`; break;
      case "bool": ctl = `<span class="toggle ${f.value ? "on" : ""}" role="switch" aria-checked="${!!f.value}" onclick="${dis ? "" : "this.classList.toggle('on')"}"></span>`; break;
      case "checks": ctl = `<div class="checks">${f.options.map(o => `<label><input type="checkbox" ${f.value?.includes(o) ? "checked" : ""} ${dis}> <span class="mono">${esc(o)}</span></label>`).join("")}</div>`; break;
      case "textarea": ctl = `<textarea class="input" rows="${f.rows || 4}" style="width:100%" ${dis}>${esc(f.value || "")}</textarea>`; break;
      case "user": ctl = `<span class="input-icon">${icon("user")}<input class="input" style="width:${f.w || "260px"}" value="${esc(f.value || "")}" placeholder="Search users…" ${dis}></span>`; break;
      case "money": ctl = `<div class="row"><input class="input num" style="width:140px" value="${esc(f.value || "")}" ${dis}><select class="select" style="width:110px" ${dis}>${f.options.map(o => `<option>${o}</option>`).join("")}</select></div>`; break;
      default: ctl = `<input class="input ${f.mono ? "mono" : ""} ${f.error ? "invalid" : ""}" style="width:${f.w || "100%"};max-width:520px" value="${esc(f.value || "")}" placeholder="${esc(f.placeholder || "")}" ${dis}>`;
    }
    return `<label class="fl">${esc(f.label)}${f.required ? ' <span style="color:var(--err)">*</span>' : ""}<span class="type">${esc(f.typeName || f.type || "string")}</span></label><div>${ctl}${f.error ? `<div class="err-msg">${esc(f.error)}</div>` : ""}${f.help ? `<div class="help">${f.help}</div>` : ""}</div>`;
  }
  function form(o, ro) {
    return `<div class="form">${o.fields.map(f => field(f, ro)).join("")}${o.actions !== false ? `<div class="form-actions">${o.note ? `<span class="muted" style="margin-right:auto;font-size:12px;align-self:center">${o.note}</span>` : ""}<button class="btn" ${ro ? "disabled" : ""}>Cancel</button><button class="btn primary" ${ro ? "disabled" : ""} ${o.onSubmit ? `onclick="${o.onSubmit}"` : ""}>${esc(o.submit || "Save")}</button></div>` : ""}</div>`;
  }

  // ---------- secret reveal ----------
  const secret = o => `<div class="secret"><div class="row" style="color:var(--warn);font-weight:600">${icon("key")} ${esc(o.title || "Copy this secret now")}</div><div style="font-size:12px;color:var(--text-2);margin-top:4px">${o.text || "It is shown exactly once. Swarog stores only a hash; if you lose it, revoke the key and create a new one."}</div><div class="secret-value"><span>${esc(o.value)}</span><button class="btn sm" onclick="UI.copied(this)">${icon("copy")} Copy</button></div>${o.extra || ""}</div>`;

  // ---------- service status ----------
  function uptimeBar(seed, bad) {
    let out = "";
    for (let i = 0; i < 48; i++) {
      const h = (seed * 31 + i * 17) % 97;
      let c = "";
      if (bad === "down" && i > 41) c = i % 2 ? "e" : "w";
      else if (bad === "degraded" && i > 43) c = "w";
      else if (h === 3) c = "w";
      out += `<i class="${c}"></i>`;
    }
    return `<div class="uptime" style="width:140px">${out}</div>`;
  }
  const SVC_TONE = { running: "ok", degraded: "warn", restarting: "warn", down: "err", crashed: "err" };
  function services(list, o = {}) {
    return list.map((s, i) => `<div class="svc"><span class="dot ${SVC_TONE[s.state] === "ok" ? "ok" : SVC_TONE[s.state]}"></span><span class="row"><span class="name">${esc(s.name)}</span>${s.state !== "running" ? status(s.state) : ""}</span><span class="row">${o.bars !== false ? `<span class="hide-tablet">${uptimeBar(i + 3, s.state)}</span>` : ""}<span class="muted mono" style="width:64px;text-align:right">${esc(s.p99)}</span></span>${s.note || o.detail ? `<span class="detail">${esc(s.note || "")}</span>` : ""}</div>`).join("");
  }

  // ---------- line chart ----------
  function chart(o) {
    const W = 640, H = 220, L = 38, R = o.right ? 40 : 12, T = 12, B = 24;
    const xs = o.labels.map((_, i) => L + i * ((W - L - R) / (o.labels.length - 1)));
    const series = o.series.map(s => {
      const max = s.max, y = v => T + (1 - v / max) * (H - T - B);
      const d = s.values.map((v, i) => `${i ? "L" : "M"}${xs[i].toFixed(1)},${y(v).toFixed(1)}`).join("");
      const dots = s.values.map((v, i) => `<circle cx="${xs[i]}" cy="${y(v)}" r="2.6" fill="var(--panel)" stroke="${s.color}" stroke-width="1.6"><title>${o.labels[i]}: ${v} ${s.unit}</title></circle>`).join("");
      return { ...s, d, dots, y };
    });
    const grid = [0, .25, .5, .75, 1].map(f => {
      const yy = T + (1 - f) * (H - T - B);
      const a = series[0], b = series[1];
      return `<line x1="${L}" x2="${W - R}" y1="${yy}" y2="${yy}" stroke="var(--chart-grid)"/><text x="${L - 6}" y="${yy + 3.5}" text-anchor="end">${+(a.max * f).toFixed(1)}</text>${b && o.right ? `<text x="${W - R + 6}" y="${yy + 3.5}">${+(b.max * f).toFixed(1)}</text>` : ""}`;
    }).join("");
    const xl = o.labels.map((l, i) => `<text x="${xs[i]}" y="${H - 6}" text-anchor="middle">${l}</text>`).join("");
    return `<svg class="chart" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" role="img" aria-label="${esc(o.aria || "")}">${grid}${xl}${series.map(s => `<path d="${s.d}" fill="none" stroke="${s.color}" stroke-width="2" vector-effect="non-scaling-stroke"/>${s.dots}`).join("")}</svg>`;
  }

  // ---------- menus ----------
  const menus = {};
  function defineMenu(id, fn) { menus[id] = fn; }
  function closePop() { document.querySelectorAll(".menu,.popover").forEach(m => m.remove()); }
  function placeAt(el, anchor, align = "right") {
    const r = anchor.getBoundingClientRect();
    document.body.appendChild(el);
    const w = el.offsetWidth, h = el.offsetHeight;
    let left = align === "right" ? r.right - w : r.left;
    left = Math.max(8, Math.min(left, innerWidth - w - 8));
    let top = r.bottom + 4;
    if (top + h > innerHeight - 8) top = Math.max(8, r.top - h - 4);
    el.style.left = left + "px"; el.style.top = top + "px";
  }
  function openMenu(ev, id, arg) {
    ev.stopPropagation();
    closePop();
    const groups = menus[id](arg);
    const el = document.createElement("div");
    el.className = "menu";
    el.innerHTML = groups.map((g, gi) => frag(g.c, `${gi ? '<div class="menu-sep"></div>' : ""}${g.label ? `<div class="menu-group"><span>${esc(g.label)}</span>${g.note ? `<span style="text-transform:none;letter-spacing:0;font-weight:500">${g.note}</span>` : ""}</div>` : ""}${g.items.map(it => `<div class="menu-item ${it.danger ? "danger" : ""} ${it.disabled ? "disabled" : ""}" ${it.disabled ? `title="${esc(it.disabled)}"` : `onclick="UI.closePop();${esc(it.run || "")}"`}>${icon(it.icon || "right")}<span>${esc(it.label)}</span>${it.hint ? `<span class="hint">${esc(it.hint)}</span>` : ""}</div>`).join("")}`, { cls: "", left: true })).join("");
    placeAt(el, ev.currentTarget);
  }
  function popover(ev, html, align = "right") {
    ev.stopPropagation();
    const was = document.querySelector(".popover");
    closePop();
    if (was && was.dataset.src === ev.currentTarget.id) return;
    const el = document.createElement("div");
    el.className = "popover";
    el.dataset.src = ev.currentTarget.id;
    el.innerHTML = html;
    el.addEventListener("click", e => e.stopPropagation());
    placeAt(el, ev.currentTarget, align);
  }
  document.addEventListener("click", closePop);

  // ---------- modals ----------
  function modal(html, o = {}) {
    closeModal();
    const s = document.createElement("div");
    s.className = "scrim";
    s.innerHTML = `<div class="modal ${o.wide ? "wide" : ""}" role="dialog" aria-modal="true">${html}</div>`;
    s.addEventListener("click", e => { if (e.target === s) closeModal(); });
    document.body.appendChild(s);
    s.querySelector("input:not([disabled]),button.primary,button.danger")?.focus();
  }
  function closeModal() { document.querySelectorAll(".scrim").forEach(s => s.remove()); }
  // ConfirmAction primitive: title, body, impact rows, optional typed confirmation and reason.
  function confirm(o) {
    const tone = o.tone || "danger";
    const typed = o.typeToConfirm ? `<div style="margin-top:10px"><div style="font-size:12px;margin-bottom:4px">Type <b class="mono">${esc(o.typeToConfirm)}</b> to confirm</div><input class="input mono" style="width:100%" oninput="this.closest('.modal').querySelector('.go').disabled = this.value !== '${esc(o.typeToConfirm)}'"></div>` : "";
    const reason = o.reason ? `<div style="margin-top:10px"><div style="font-size:12px;margin-bottom:4px">Reason <span class="muted">(recorded in audit log)</span></div><input class="input" style="width:100%" placeholder="${esc(o.reason)}"></div>` : "";
    const html = `${frag(o.c, `<div class="modal-head"><div class="modal-icon ${tone === "danger" ? "danger" : tone}">${icon(o.icon || (tone === "danger" ? "alert" : "info"))}</div><div><h2>${esc(o.title)}</h2></div></div><div class="modal-body">${o.body || ""}${o.impact ? `<div class="impact">${o.impact.map(([k, v]) => `<div><span>${esc(k)}</span><b>${v}</b></div>`).join("")}</div>` : ""}${reason}${typed}</div>`, { left: true })}<div class="modal-foot"><span class="muted" style="margin-right:auto;font-size:12px;align-self:center">${o.footNote || ""}</span><button class="btn" onclick="UI.closeModal()">Cancel</button><button class="btn ${tone === "danger" ? "danger" : "primary"} go" ${o.typeToConfirm ? "disabled" : ""} onclick="${o.onConfirm ? esc(o.onConfirm) : "UI.closeModal();UI.toast('" + esc(o.done || "Done") + "')"}">${esc(o.confirm || "Confirm")}</button></div>`;
    modal(html, o);
  }
  function toast(text, tone = "ok") {
    const t = document.createElement("div");
    t.style.cssText = "position:fixed;bottom:18px;left:50%;transform:translateX(-50%);z-index:90;background:var(--text);color:var(--bg);padding:8px 14px;border-radius:7px;box-shadow:var(--shadow-pop);font-weight:500;display:flex;gap:8px;align-items:center";
    t.innerHTML = `${icon(tone === "ok" ? "check" : "info")} ${esc(text)}`;
    document.body.appendChild(t);
    setTimeout(() => t.remove(), 2600);
  }
  function copied(btn) { btn.innerHTML = `${icon("check")} Copied`; }
  function retry(btn) {
    const m = btn.closest("[data-module]")?.dataset.module || "module";
    btn.disabled = true;
    btn.innerHTML = `${icon("refresh")} Retrying…`;
    setTimeout(() => { btn.disabled = false; btn.innerHTML = `${icon("refresh")} Retry`; toast(`Still unavailable — ${m}-svc refused connection`, "info"); }, 900);
  }

  return { icon, esc, fmt, frag, badge, status, avatar, userCell, repoLink, ref, panel, stateBlock, table, rowMenuBtn, kpi, spark, props, timeline, form, field, secret, services, uptimeBar, chart, defineMenu, openMenu, popover, closePop, modal, closeModal, confirm, toast, copied, retry, state, MODULE_COLORS };
})();

window.Swarog = Object.assign(window.UI, {
  Panel: UI.panel, DataTable: UI.table, KpiTile: UI.kpi, PropertyList: UI.props, Timeline: UI.timeline,
  Form: UI.form, ConfirmAction: UI.confirm, SecretReveal: UI.secret, ServiceStatus: UI.services,
  TimeSeriesChart: UI.chart, MenuAction: UI.openMenu, Badge: UI.badge, Annotation: UI.frag,
});

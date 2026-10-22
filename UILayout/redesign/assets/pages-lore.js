(() => {
  const { icon, esc, panel, table, kpi, frag, badge, status, userCell, timeline, props, rowMenuBtn, repoLink } = UI;
  const D = DATA;
  const P = window.PAGES;
  const L = c => ({ module: "lore", point: "page", prim: c });
  const loreCrumb = ["Lore", "lore-repos.html"];
  const totalTB = D.billing.storeTB;
  const branchesFor = r => {
    if (r.name === "ashfall-game") return D.branches;
    const extra = ["release/0.9", "feature/ci-cache", "feature/cleanup"].slice(0, Math.min(r.branches, 4) - 1);
    return [r.branch, ...extra].map((name, i) => ({ name, head: (0x9c1e0a7 + i * 0x2f1b3 + r.name.length * 0x51).toString(16).slice(0, 7), by: r.pushBy, when: i ? "4 d ago" : r.lastPush, ahead: i ? 3 + i : 0, behind: i ? 20 * i : 0, prot: i < r.protected, locks: i ? 0 : r.locks }));
  };

  UI.defineMenu("repo", name => [
    { c: { module: "lore", point: "entity.repository.row_action", prim: "MenuAction" }, label: "lore", items: [
      { label: "Open", icon: "external", run: `location.href='lore-repo.html?r=${name}'` },
      { label: "Protected branches", icon: "shield", run: `location.href='lore-repo.html?r=${name}'` },
      { label: "View locks", icon: "lock", run: `location.href='lore-locks.html?repo=${name}'` },
      { label: "Copy clone URL", icon: "copy", run: `UI.toast('Copied lore://lore.emberline.gg/${name}')` },
    ] },
    { c: { module: "audit", point: "entity.repository.row_action", prim: "MenuAction" }, label: "audit", items: [
      { label: "Audit trail", icon: "list", run: "location.href='audit.html'" },
    ] },
    { c: { module: "lore", point: "entity.repository.row_action", prim: "MenuAction" }, label: "danger", items: [
      { label: "Obliterate files…", icon: "trash", danger: true, run: `PAGES.obliterate('${name}')` },
    ] },
  ]);
  UI.defineMenu("lock", id => {
    const l = D.locks.find(x => x.id === id);
    return [{ c: { module: "lore", point: "entity.lock.row_action", prim: "MenuAction" }, label: "lore", items: [
      { label: "Force unlock…", icon: "unlock", danger: true, run: `PAGES.forceUnlock('${id}')` },
      { label: "Open repository", icon: "repo", run: `location.href='lore-repo.html?r=${l.repo}'` },
      { label: `Locks by ${l.owner}`, icon: "user", run: `location.href='lore-locks.html?owner=${l.owner}'` },
    ] }, { c: { module: "notifications", point: "entity.lock.row_action", prim: "MenuAction" }, label: "notifications", items: [
      { label: "Ask owner to release", icon: "send", run: `UI.toast('Notification sent to ${l.owner}')` },
    ] }];
  });

  P.obliterate = name => UI.confirm({
    c: { module: "lore", point: "entity.repository.row_action", prim: "ConfirmAction" }, title: `Obliterate files in ${name}`, icon: "trash",
    body: `<div style="margin-bottom:8px">Permanently purges file content from every revision. Clones that already fetched it keep their copy.</div><div class="form" style="grid-template-columns:110px 1fr">${UI.field({ label: "Path glob", typeName: "glob", value: "Content/Leaked/**", mono: true })}</div>`,
    impact: [["Matching files", "14 in 230 revisions"], ["Content to purge", "2.3 GB"], ["Requires action", '<span class="tag">obliterate</span>']],
    reason: "Why is this content being purged?", typeToConfirm: name, confirm: "Obliterate", done: "Obliterate job queued",
  });
  P.forceUnlock = (id, many) => {
    const ls = many ? D.locks.filter(l => l.stale) : [D.locks.find(l => l.id === id)];
    const l = ls[0];
    UI.confirm({
      c: { module: "lore", point: "entity.lock.row_action", prim: "ConfirmAction" }, icon: "unlock",
      title: many ? `Force unlock ${ls.length} stale locks?` : "Force unlock this file?",
      body: many ? "Both locks are held by sam.o, whose account is suspended." : `<div class="code" style="white-space:normal;overflow-wrap:anywhere">${esc(l.repo)}: ${esc(l.file)}</div>`,
      impact: many ? ls.map(x => [x.repo, `<span class="mono">${esc(x.file.split("/").pop())}</span> · ${x.age}`]) : [["Owner", userCell(l.owner, false)], ["Branch", `<span class="tag">${l.branch}</span>`], ["Held for", l.age + (l.stale ? ' <span class="badge warn">stale</span>' : "")], ["Owner is notified", "yes (notifications)"]],
      footNote: "The owner's local edits are kept but may conflict on push.",
      reason: "e.g. owner offboarded, blocking release", confirm: many ? `Force unlock ${ls.length}` : "Force unlock", done: many ? `${ls.length} locks released` : "Lock released",
    });
  };

  // ---------- repositories ----------
  P.loreRepos = {
    meta: { nav: "lore.repos", crumbs: [loreCrumb, ["Repositories"]] },
    render: () => {
      const tiles = [
        { label: "Repositories", value: "6", delta: "1 created this month" },
        { label: "Store used", value: totalTB.toFixed(2), unit: "TB", delta: `instance store · ${D.billing.volumeTB} TB volume`, bar: Math.round(totalTB / D.billing.volumeTB * 100) },
        { label: "Open locks", value: "26", delta: '<span class="bad-up">2 stale (&gt; 14 d)</span>', href: "lore-locks.html" },
        { label: "Pushes (24 h)", value: "41", delta: "8.6 GB uploaded", spark: [22, 30, 18, 41, 36, 12, 9, 41] },
      ].map(t => kpi({ ...t, c: { module: "lore", point: "page", prim: "KpiTile" } })).join("");
      const rows = D.repos.map(r => ({ href: `lore-repo.html?r=${r.name}`, cells: [
        `<div style="line-height:1.25"><a href="lore-repo.html?r=${r.name}" style="font-weight:500">${r.name}</a><div class="muted" style="font-size:11.5px">${esc(r.desc)}</div></div>`,
        `<span class="tag">${UI.icon("branch")} ${r.branch}</span>`,
        `<span class="row"><span class="dim">${r.lastPush}</span> ${userCell(r.pushBy)}</span>`,
        r.branches,
        r.protected ? `${r.protected}` : `<span class="muted">0</span>`,
        r.locks ? (r.name === "ashfall-game" || r.name === "ashfall-art" ? `<span class="row" style="justify-content:flex-end">${badge("1 stale", "warn")} ${r.locks}</span>` : r.locks) : `<span class="muted">0</span>`,
      ] }));
      return P.head("Repositories", `Hosted Lore · instance <span class="mono">lore-01</span> · ${D.repos.length} repositories`, `<button class="btn">${icon("copy")} Server URL</button><button class="btn primary">${icon("plus")} New repository</button>`) +
        `<div class="grid g-tiles" style="margin-bottom:12px">${tiles}</div>` +
        panel({ c: L("DataTable"), flush: true, body: table({
          cols: [{ label: "Repository" }, { label: "Default branch" }, { label: "Last push", sorted: "desc" }, { label: "Branches", num: true }, { label: "Protected", num: true }, { label: "Locks", num: true }],
          rows, filters: [{ k: "Has locks", v: "any" }], searchPlaceholder: "Repository name", rowMenu: r => rowMenuBtn("repo", r.href.split("=")[1]), total: "6 repositories",
        }) });
    },
  };

  // ---------- repository detail ----------
  const repoTabs = [
    { module: "lore", id: "branches", label: "Branches", prim: "DataTable + Form", count: r => r.branches },
    { module: "lore", id: "locks", label: "Locks", prim: "DataTable", count: r => r.locks },
    { module: "lore", id: "access", label: "Access", prim: "DataTable" },
    { module: "lore", id: "events", label: "Events", prim: "Timeline" },
    { module: "audit", id: "audit", label: "Audit", prim: "DataTable" },
  ];
  P.loreRepo = {
    meta: { nav: "lore.repos", crumbs: [] },
    render: () => {
      const q = new URLSearchParams(location.search);
      const r = D.repos.find(x => x.name === q.get("r")) || D.repos[0];
      const active = P.repoTab || q.get("tab") || "branches";
      P.loreRepo.meta.crumbs = [loreCrumb, ["Repositories", "lore-repos.html"], [r.name]];
      const tabs = repoTabs.map(t => frag({ module: t.module, point: "entity.repository.tab", prim: t.prim }, `<button class="tab ${t.id === active ? "on" : ""}" onclick="PAGES.repoTab='${t.id}';REG.render()">${t.label}${t.count ? ` <span class="count">${t.count(r)}</span>` : ""}</button>`, { inline: true, tag: "span" })).join("");
      const header = frag({ module: "portal", point: "entity.repository (host)", prim: "EntityDetail" }, `<section class="panel" style="margin-bottom:12px"><div class="entity-head"><span class="modal-icon" style="width:44px;height:44px;background:var(--accent-soft);color:var(--accent-fg)">${icon("repo")}</span><div class="grow"><div class="row"><h1>${r.name}</h1>${badge("healthy", "ok")}</div><div class="entity-facts"><span>${esc(r.desc)}</span><span>${icon("branch")} default <span class="tag">${r.branch}</span></span><span>${icon("clock")} last push ${r.lastPush} by ${esc(r.pushBy)}</span><span class="mono">lore://lore.emberline.gg/${r.name}</span></div></div><div class="page-actions"><button class="btn">${icon("copy")} Clone URL</button><button class="btn icon" onclick="UI.openMenu(event,'repo','${r.name}')">${icon("more")}</button></div></div><div class="tabs">${tabs}</div></section>`, { left: true });
      let body = "";
      if (active === "branches") {
        const list = branchesFor(r);
        const sel = list.find(b => b.name === P.branch) || list.find(b => b.name === "release/0.9") || list[0];
        const rows = list.map(b => ({ sel: b === sel, cells: [
          `<a href="#" onclick="PAGES.branch='${b.name}';REG.render();return false" style="font-weight:500">${b.name}</a>${b.name === r.branch ? ` ${badge("default", "", true)}` : ""}`,
          `<span class="mono">${b.head}</span>`, `<span class="row"><span class="dim">${b.when}</span> ${userCell(b.by)}</span>`,
          b.ahead || b.behind ? `<span class="mono"><span style="color:var(--ok)">+${b.ahead}</span> / <span style="color:var(--text-3)">−${b.behind}</span></span>` : `<span class="muted">—</span>`,
          b.prot ? badge("protected", "accent") : `<span class="muted">—</span>`,
          b.locks || `<span class="muted">0</span>`,
        ] }));
        const bt = panel({ c: { module: "lore", point: "entity.repository.tab", prim: "DataTable" }, title: "Branches", meta: `${list.length} of ${r.branches}`, flush: true,
          body: table({ cols: [{ label: "Branch" }, { label: "Head" }, { label: "Updated" }, { label: "vs default" }, { label: "Protected" }, { label: "Locks", num: true }], rows, filters: [{ k: "Protected", v: "any" }], searchPlaceholder: "Branch name", total: `${list.length} of ${r.branches} branches`, page: list.length < r.branches ? "1 / " + Math.ceil(r.branches / list.length) : "1 / 1", rowMenu: () => `<button class="btn sm icon ghost">${icon("more")}</button>` }) });
        const pf = panel({ c: { module: "lore", point: "entity.repository.tab", prim: "Form" }, title: `Protection · <span class="mono">${sel.name}</span>`, meta: sel.prot ? "protected" : "not protected",
          body: UI.form({ submit: "Save", note: "Calls BranchProtect / BranchUnprotect on loreserver.", fields: [
            { label: "Branch", typeName: "ref branch", value: sel.name, mono: true, disabled: true },
            { label: "Protected", type: "bool", typeName: "bool", value: sel.prot, help: "Protection is a per-branch PROTECT flag; loreserver has no per-ref patterns. Today only service accounts bypass it; the <span class='tag'>push-protected</span> action (Lead, Admin) becomes the bypass after LEP." },
          ] }) });
        body = `<div class="grid g-21">${bt}${pf}</div>`;
      } else if (active === "locks") {
        body = locksTable(D.locks.filter(l => l.repo === r.name), { point: "entity.repository.tab", hideRepo: true, of: r.locks });
      } else if (active === "access") {
        const rows = D.users.filter(u => u.loreRole).map(u => {
          const o = D.overrides[u.handle]?.[r.name];
          const eff = o || u.loreRole;
          return [userCell(u.handle), badge(u.loreRole, "", true), o ? badge(o, o === "none" ? "err" : "accent", true) : `<span class="muted">inherit</span>`, eff === "none" ? `<span style="color:var(--err)">no access</span>` : (D.roles.find(x => x.name === eff).actions.map(a => `<span class="tag">${a}</span>`).join(" "))];
        });
        body = panel({ c: { module: "lore", point: "entity.repository.tab", prim: "DataTable" }, title: "Who can access this repository", meta: "global role + override = effective", flush: true, tools: `<a class="btn sm" href="lore-permissions.html#overrides">Edit overrides</a>`, body: table({ pager: false, cols: [{ label: "User" }, { label: "Global role" }, { label: "Override" }, { label: "Effective actions" }], rows }) });
      } else if (active === "events") {
        body = panel({ c: { module: "lore", point: "entity.repository.tab", prim: "Timeline" }, title: "Events", flush: true, body: timeline(loreTimelineItems(D.loreEvents.filter(e => e.repo === r.name))) });
      } else {
        body = panel({ c: { module: "audit", point: "entity.repository.tab", prim: "DataTable" }, title: "Audit", meta: `target repository:${r.name}`, flush: true, body: table({ pager: false, cols: [{ label: "Time" }, { label: "Actor" }, { label: "Action" }, { label: "Detail" }], rows: D.audit.filter(a => a.target.includes(r.name)).map(a => [`<span class="mono">${a.t}</span>`, P.actor(a.actor.replace("apikey:", "")), `<span class="mono">${a.action}</span>`, esc(a.detail)]) }) });
      }
      return header + body;
    },
  };

  // ---------- permissions ----------
  P.lorePermissions = {
    meta: { nav: "lore.permissions", crumbs: [loreCrumb, ["Permissions"]] },
    render: () => {
      const roleMatrix = panel({ c: { module: "lore", point: "page", prim: "PermissionMatrix" }, title: "Role definitions", meta: "roles × actions", flush: true,
        tools: `<button class="btn sm">${icon("plus")} Custom role</button>`,
        body: `<div class="tbl-wrap"><table class="tbl matrix"><thead><tr><th>Role</th>${D.actions.map(a => `<th class="rot" title="${esc(a.desc)}"><span class="mono">${a.key}</span></th>`).join("")}<th class="num">Users</th></tr></thead><tbody>${D.roles.map(r => `<tr><td><div style="font-weight:500">${r.name} ${r.builtin ? `<span class="muted" title="built-in">${icon("lock")}</span>` : ""}</div><div class="muted" style="font-size:11.5px">${esc(r.desc)}</div></td>${D.actions.map(a => `<td class="cell"><span class="check-cell ${r.actions.includes(a.key) ? "on" : ""} ${r.builtin ? "locked" : ""}">${r.actions.includes(a.key) ? icon("check") : ""}</span></td>`).join("")}<td class="num">${r.users}</td></tr>`).join("")}</tbody></table></div>`,
        foot: `${icon("info")} Built-in roles are fixed. <span class="mono">presign … obliterate</span> use the Lore action vocabulary; <span class="mono">read</span>/<span class="mono">write</span> stand for repository access, and how they map to Lore grants is still open.` });

      const loreUsers = D.users.filter(u => u.loreRole);
      const assign = panel({ c: { module: "lore", point: "page", prim: "DataTable" }, title: "Global role assignment", meta: `${loreUsers.length} seats`, flush: true,
        tools: `<button class="btn sm">${icon("plus")} Grant access</button>`,
        body: table({ pager: false, cols: [{ label: "User" }, { label: "Role" }, { label: "Overrides", num: true }, { label: "Status" }], rows: loreUsers.map(u => [userCell(u.handle), `<select class="select" style="height:24px;font-size:12px">${D.roles.map(r => `<option ${r.name === u.loreRole ? "selected" : ""}>${r.name}</option>`).join("")}</select>`, Object.keys(D.overrides[u.handle] || {}).length || `<span class="muted">0</span>`, status(u.status)]), rowMenu: () => `<button class="btn sm icon ghost">${icon("more")}</button>` }) });

      const eu = [D.byHandle[P.effUser], D.byHandle[new URLSearchParams(location.search).get("u")]].find(u => u?.loreRole) || D.byHandle["elena.p"], er = P.effRepo || "ashfall-art";
      const ov = D.overrides[eu.handle]?.[er];
      const effRole = ov || eu.loreRole;
      const effActs = effRole === "none" ? [] : D.roles.find(r => r.name === effRole).actions;
      const preview = `<div id="effective">` + panel({ c: { module: "lore", point: "page", prim: "EffectivePermissions" }, title: "Effective permissions preview",
        body: `<div class="row" style="margin-bottom:12px;flex-wrap:wrap"><select class="select" onchange="PAGES.effUser=this.value;REG.render()">${loreUsers.map(u => `<option ${u === eu ? "selected" : ""}>${u.handle}</option>`).join("")}</select><span class="muted">on</span><select class="select" onchange="PAGES.effRepo=this.value;REG.render()">${D.repos.map(r => `<option ${r.name === er ? "selected" : ""}>${r.name}</option>`).join("")}</select></div>
        <div class="impact" style="margin-top:0"><div><span>Global role</span><b>${eu.loreRole}</b></div><div><span>Override on ${er}</span><b>${ov ? badge(ov, ov === "none" ? "err" : "accent", true) : "none"}</b></div><div><span>Resolved</span><b>${effRole === "none" ? '<span style="color:var(--err)">no access</span>' : effRole}</b></div></div>
        <table class="tbl" style="margin-top:8px">${D.actions.map(a => { const on = effActs.includes(a.key); return `<tr><td style="width:24px">${on ? `<span style="color:var(--ok)">${icon("check")}</span>` : `<span class="muted">${icon("x")}</span>`}</td><td><span class="mono">${a.key}</span></td><td class="muted wrap">${on ? (ov ? `via override → ${ov}` : `via global role ${eu.loreRole}`) : effRole === "none" ? "override removes all access" : `not in ${effRole}`}</td></tr>`; }).join("")}</table>` }) + `</div>`;

      const repos = D.repos.map(r => r.name);
      const pending = { u: "priya.n", r: "ashfall-art", to: "Lead" };
      const matrix = panel({ c: { module: "lore", point: "page", prim: "PermissionMatrix" }, title: "Per-repository overrides", meta: "users × repositories", flush: true,
        tools: `<span class="row" style="font-size:11.5px;color:var(--text-3);gap:10px"><span class="pcell inherit" style="min-width:0;padding:0 6px">inherit</span><span class="pcell override" style="min-width:0;padding:0 6px">override</span><span class="pcell none" style="min-width:0;padding:0 6px">no access</span></span>`,
        banner: `<div class="banner warn">${icon("edit")} 1 unsaved change: priya.n on ashfall-art Developer → Lead<button class="btn sm">Discard</button><button class="btn sm primary" style="margin-left:0" onclick="UI.toast('Override applied')">Apply</button></div>`,
        body: `<div class="tbl-wrap"><table class="tbl matrix"><thead><tr><th>User</th><th>Global</th>${repos.map(r => `<th class="rot"><span class="mono">${r}</span></th>`).join("")}</tr></thead><tbody>${loreUsers.map(u => `<tr><td>${userCell(u.handle)}</td><td>${badge(u.loreRole, "", true)}</td>${repos.map(r => { const o = D.overrides[u.handle]?.[r]; const p = pending.u === u.handle && pending.r === r; return p ? `<td class="cell"><span class="pcell override pending" title="unsaved">${pending.to}</span></td>` : `<td class="cell"><span class="pcell ${o === "none" ? "none" : o ? "override" : "inherit"}" title="click to change">${o === "none" ? "none" : o || u.loreRole}</span></td>`; }).join("")}</tr>`).join("")}</tbody></table></div>`,
        foot: "Click a cell to set an override. Dashed = inherits the global role." });

      return P.head("Permissions", "Roles, assignments and per-repository overrides for this deployment's Lore instance.", `<a class="btn" href="audit.html">${icon("list")} Change history</a>`) +
        `<div class="stack">${roleMatrix}<div class="grid g-2">${assign}${preview}</div><div id="overrides">${matrix}</div></div>`;
    },
  };

  // ---------- locks ----------
  function locksTable(list, o = {}) {
    const stale = list.filter(l => l.stale);
    const sel = o.selectStale;
    const cols = [{ label: "File" }, ...(o.hideRepo ? [] : [{ label: "Repository" }]), { label: "Owner" }, { label: "Branch" }, { label: "Held", sorted: "desc" }, { label: "Description", wrap: true }];
    const rows = list.map(l => ({ sel: sel && l.stale, cells: [
      `<div style="line-height:1.25;padding:3px 0" title="${esc(l.file)}"><div class="mono">${esc(l.file.split("/").pop())}</div><div class="mono muted" style="font-size:11px">${esc(l.file.slice(0, l.file.lastIndexOf("/") + 1))}</div></div>`,
      ...(o.hideRepo ? [] : [repoLink(l.repo)]), userCell(l.owner), `<span class="tag">${l.branch}</span>`,
      `<span title="${l.since}">${l.stale ? badge(l.age, "warn") : esc(l.age)}</span>`,
      l.desc ? `<span class="dim">${esc(l.desc)}</span>` : `<span class="muted">—</span>`,
    ] }));
    return panel({ c: { module: "lore", point: o.point || "page", prim: "DataTable" }, title: o.title, flush: true, emptyTitle: "No open locks", emptyIcon: "unlock", state: list.length ? "ok" : "empty", emptyDetail: "Nobody holds a lock here.",
      body: table({ selectable: true, cols, rows, filters: o.hideRepo ? [{ k: "Owner", v: "any" }] : [{ k: "Repository", v: "all" }, { k: "Owner", v: "any" }, { k: "Branch", v: "any" }, `<span class="filter-chip" style="${sel ? "background:var(--warn-soft);color:var(--warn);border-color:transparent" : ""}" onclick="PAGES.staleOnly=!PAGES.staleOnly;REG.render()">${icon("clock")} Older than 14 d${sel ? " ✓" : ""}</span>`],
        searchPlaceholder: "Path contains…", bulk: sel ? `${stale.length} selected <button class="btn sm danger" onclick="PAGES.forceUnlock(null,true)">${icon("unlock")} Force unlock ${stale.length}</button><button class="btn sm ghost">Notify owners</button>` : "",
        rowMenu: (r, i) => rowMenuBtn("lock", list[i].id), total: `${list.length} of ${o.of ?? 26} locks${sel ? " · filtered: older than 14 d" : ""}`, page: !sel && list.length < (o.of ?? 26) ? `1 / ${Math.ceil((o.of ?? 26) / list.length)}` : "1 / 1", footNote: "Locks live in loreserver memory and are lost on restart" }) });
  }
  P.loreLocks = {
    meta: { nav: "lore.locks", crumbs: [loreCrumb, ["Locks"]] },
    render: () => {
      const q = new URLSearchParams(location.search);
      if (P.staleOnly === undefined) P.staleOnly = q.get("stale") === "1";
      let list = D.locks.slice().sort((a, b) => a.since.localeCompare(b.since));
      if (q.get("owner")) list = list.filter(l => l.owner === q.get("owner"));
      if (q.get("repo")) list = list.filter(l => l.repo === q.get("repo"));
      if (P.staleOnly) list = list.filter(l => l.stale);
      const per = D.repos.map(r => `<span class="filter-chip">${r.name} <b>${r.locks}</b></span>`).join("");
      return P.head("Locks", "Every exclusive file lock across all repositories.", `<button class="btn">${icon("download")} Export</button>`) +
        frag(L("FacetBar"), `<div class="row" style="flex-wrap:wrap;margin-bottom:12px">${per}</div>`) +
        locksTable(list, { selectStale: P.staleOnly, title: undefined });
    },
  };

  // ---------- server instance ----------
  P.loreServer = {
    meta: { nav: "lore.server", crumbs: [loreCrumb, ["Server instance"]] },
    render: () => {
      const proc = panel({ c: { module: "lore", point: "page", prim: "ProcessStatus" }, title: "loreserver · lore-01",
        tools: `<button class="btn sm" onclick="PAGES.restartLore()">${icon("refresh")} Restart</button><button class="btn sm">${icon("download")} Logs</button>`,
        body: `<div class="row" style="gap:16px;align-items:flex-start;flex-wrap:wrap"><div style="min-width:180px"><div class="muted" style="font-size:12px">State</div><div class="row" style="font-size:18px;font-weight:600;margin:4px 0"><span class="dot ok" style="width:10px;height:10px"></span>Running</div><div class="seg" style="margin-top:6px"><button class="on">running</button><button>restarting</button><button>crashed</button></div><div class="muted" style="font-size:11px;margin-top:4px">supervisor states (weles)</div></div><div class="grow">${props([
          ["Version", `<span class="mono">loreserver 0.1.0 (cb5b536)</span> ${badge("stock binary", "", true)}`],
          ["PID · host", `<span class="mono">18244 · ember-build-01</span>`],
          ["Uptime", "1 d 2 h 14 min"],
          ["Restarts", "0 in 24 h · 3 total"],
          ["Ports", `<span class="tag">quic udp :41337</span> <span class="tag">grpc tcp :41337</span> <span class="tag">http :41339</span>`],
          ["Last health check", `${badge("ok", "ok")} <span class="mono">GET /health_check</span> · 2 s ago · 11 ms`],
          ["Auth mode", `UrcAuthApi adapter → swarog lore-svc <span class="muted">(online CheckUserPermission)</span>`],
        ])}</div></div>` });
      const store = panel({ c: { module: "lore", point: "page", prim: "PropertyList" }, title: "Store",
        body: `${props([["Path", `<span class="mono">D:\\lore\\store</span>`], ["Used", `${totalTB.toFixed(2)} TB of ${D.billing.volumeTB} TB volume`], ["Measured", "size of the store directory (hourly)"], ["Lock store", `${badge("in-memory", "warn")} LocalLockStore`]])}<div class="kpi" style="padding:10px 0 0;min-height:0"><div class="bar"><i style="width:66%"></i></div><div class="kpi-delta" style="margin-top:4px">66% of volume</div></div>`,
        foot: `${icon("alert")} 26 locks are held in RAM and will be lost on restart.` });
      const checks = panel({ c: { module: "lore", point: "page", prim: "DataTable" }, title: "Health checks", meta: "HTTP :41339 · every 10 s", flush: true,
        foot: `${icon("info")} weles probes <span class="mono">/readyz</span> by default; lore-01 needs a <span class="mono">health_path</span> setting (open gap).`,
        body: table({ pager: false, cols: [{ label: "Time" }, { label: "Probe" }, { label: "Result" }, { label: "Latency", num: true }], rows: [["14:12:30", "GET /health_check", "ok", "11 ms"], ["14:12:20", "GET /health_check", "ok", "9 ms"], ["14:12:10", "GET /health_check", "ok", "14 ms"], ["2026-10-21 12:01:44", "GET /health_check", "fail", "timeout"]].map(r => [`<span class="mono">${r[0]}</span>`, r[1], status(r[2]), r[3]]) }) });
      const streams = panel({ c: { module: "lore", point: "page", prim: "ServiceStatus" }, title: "Notification streams", meta: "NotificationService.Subscribe, one per repository", flush: true,
        body: UI.services(D.repos.map((r, i) => ({ name: r.name, state: i === 3 ? "degraded" : "running", p99: i === 3 ? "lag 4 s" : "lag 0", note: i === 3 ? "reconnected 2× in 1 h; the stream is at-most-once, so events during the gap are lost" : "" })), { bars: false }),
        foot: `${icon("info")} Events are copied onto durable <span class="mono">lore.*</span> topics. Repository create/obliterate are not streamed by stock loreserver.` });
      const hist = panel({ c: { module: "lore", point: "page", prim: "Timeline" }, title: "Process history", flush: true, body: timeline([
        { day: "Oct 21", t: "12:02", html: "Started <span class='mono'>0.1.0</span>", meta: "after restart by marta.k", icon: "play", tone: "ok" },
        { day: "Oct 21", t: "12:01", html: "Restart requested", meta: "marta.k · reason: config reload ([server.auth])", icon: "refresh", tone: "warn" },
        { day: "Oct 19", t: "03:14", html: "Crashed: out of file descriptors", meta: "supervisor restarted in 4 s · 11 locks lost", icon: "alert", tone: "err" },
        { day: "Oct 12", t: "09:00", html: "Upgraded 0.0.9 → 0.1.0", meta: "weles deploy", icon: "up", tone: "accent" },
      ]) });
      return P.head("Server instance", "One loreserver per deployment, supervised by weles.", "") +
        `<div class="grid g-21" style="margin-bottom:12px">${proc}${store}</div><div class="grid g-2" style="margin-bottom:12px">${streams}${checks}</div>${hist}`;
    },
  };
  P.restartLore = () => UI.confirm({ c: { module: "lore", point: "page", prim: "ConfirmAction" }, title: "Restart loreserver?", icon: "refresh", tone: "danger",
    body: "Clients see connection errors for ~5 s and retry automatically.",
    impact: [["Locks lost (in-memory store)", "<span style='color:var(--err)'>26 locks in 4 repositories</span>"], ["Active transfers", "3 pushes in flight will be aborted"], ["Lock owners notified", "8 users"]],
    reason: "Why restart?", typeToConfirm: "lore-01", confirm: "Restart", done: "Restart requested" });

  // ---------- events ----------
  function loreTimelineItems(list) {
    return list.map(e => ({ t: e.t, day: e.day, icon: e.icon, tone: e.tone, html: `${userCell(e.who)} ${esc(e.text)} <span class="muted">in</span> ${repoLink(e.repo)} <span class="tag">${esc(e.branch)}</span>`, meta: e.detail ? esc(e.detail) : "" }));
  }
  P.loreEvents = {
    meta: { nav: "lore.events", crumbs: [loreCrumb, ["Events"]] },
    render: () => {
      const types = ["push", "branch", "lock", "unlock", "protect"];
      const filt = P.evType;
      const list = D.loreEvents.filter(e => !filt || e.type === filt);
      const tl = panel({ c: { module: "lore", point: "page", prim: "Timeline" }, flush: true, title: "Timeline", meta: "durable lore.* topics",
        tools: `<span class="badge ok">live</span>`,
        body: `<div class="tbl-toolbar"><div class="seg"><button class="${!filt ? "on" : ""}" onclick="PAGES.evType='';REG.render()">All</button>${types.map(t => `<button class="${filt === t ? "on" : ""}" onclick="PAGES.evType='${t}';REG.render()">${t}</button>`).join("")}</div><span class="filter-chip">Repository: <b>all</b><span class="x">${icon("x")}</span></span><span class="filter-chip">User: <b>any</b><span class="x">${icon("x")}</span></span></div>${list.length ? timeline(loreTimelineItems(list)) : UI.stateBlock("empty", { title: "No events match", detail: "Clear a filter to see more.", actions: "" })}`,
        foot: `<button class="btn sm">Load older</button><span>Showing ${list.length} events since yesterday 10:00</span>` });
      const counts = panel({ c: { module: "lore", point: "page", prim: "PropertyList" }, title: "Last 24 h", body: props([["Pushes", "41 · 8.6 GB"], ["Branches created / deleted", "3 / 1"], ["Locks taken / released", "29 / 22"], ["Force unlocks", '<span style="color:var(--err)">1</span>'], ["Protection changes", "1"]]) });
      const sub = panel({ c: { module: "core.events", point: "page", prim: "ServiceStatus" }, title: "Consumers of lore.*", flush: true,
        body: UI.services(D.subs.filter(s => s.topic.startsWith("lore.")).map(s => ({ name: s.id, state: s.state, p99: `lag ${s.lag}` })), { bars: false }),
        foot: `<a href="events.html">Event plane ${icon("right")}</a>` });
      return P.head("Events", "Pushes, branch changes and locks across all repositories.", "") + `<div class="grid g-21">${tl}<div class="stack">${counts}${sub}</div></div>`;
    },
  };

  // ---------- billing ----------
  P.loreBilling = {
    meta: { nav: "lore.billing", crumbs: [loreCrumb, ["Billing & usage"]] },
    render: () => {
      const b = D.billing;
      const tiles = [
        { label: "Seats (current)", value: "8", delta: "users with Lore access · peak this month 8" },
        { label: "Store (current)", value: totalTB.toFixed(2), unit: "TB", bar: 66, delta: `of ${b.volumeTB} TB volume` },
        { label: "Growth (30 d)", value: "+1.19", unit: "TB", delta: "Sep → Oct" },
        { label: "Last snapshot", value: "14:00", delta: "scheduler · lore.billing_snapshot · hourly" },
      ].map(t => kpi({ ...t, c: { module: "lore", point: "page", prim: "KpiTile" } })).join("");
      const ch = panel({ c: { module: "lore", point: "page", prim: "TimeSeriesChart" }, title: "Seats and store over time", meta: "monthly peak",
        tools: `<div class="legend"><span><i style="background:var(--chart-1)"></i>Seats</span><span><i style="background:var(--chart-2)"></i>Store (TB)</span></div><div class="seg"><button class="on">6 mo</button><button>12 mo</button></div>`,
        body: UI.chart({ labels: b.months, right: true, aria: "Seats and terabytes per month", series: [{ values: b.seats, max: 10, color: "var(--chart-1)", unit: "seats" }, { values: b.tb, max: 15, color: "var(--chart-2)", unit: "TB" }] }) });
      const rows = b.months.map((m, i) => [m + " 2026", b.seats[i], b.tb[i].toFixed(2) + " TB", i ? `<span style="color:${b.tb[i] - b.tb[i - 1] > 1.4 ? "var(--warn)" : "var(--text-2)"}">+${(b.tb[i] - b.tb[i - 1]).toFixed(2)}</span>` : "—", i === 5 ? badge("open", "info") : badge("closed", "", true)]).reverse();
      const tbl = panel({ c: { module: "lore", point: "page", prim: "DataTable" }, title: "Monthly usage", flush: true, tools: `<button class="btn sm">${icon("download")} CSV</button>`,
        body: table({ pager: false, cols: [{ label: "Month" }, { label: "Peak seats", num: true }, { label: "Store", num: true }, { label: "Δ TB", num: true }, { label: "Period" }], rows }) });
      const seats = panel({ c: { module: "lore", point: "page", prim: "DataTable" }, title: "Seat holders", meta: "8", flush: true,
        body: table({ pager: false, cols: [{ label: "User" }, { label: "Role" }, { label: "Since" }, { label: "Last Lore activity" }], rows: D.users.filter(u => u.loreRole).map(u => [userCell(u.handle), badge(u.loreRole, "", true), u.seatSince, u.status === "suspended" ? `<span class="row">${badge("suspended · still counted", "warn")}</span>` : u.lastSeen]) }),
        foot: `${icon("info")} Whether a suspended user still counts as a seat needs an owner decision.` });
      return P.head("Billing & usage", "Per-deployment usage reported by the lore module: seats = users with Lore access, TB = size of this instance's store.", `<button class="btn">${icon("download")} Usage report</button>`) +
        `<div class="grid g-tiles" style="margin-bottom:12px">${tiles}</div>${ch}<div class="grid g-2" style="margin-top:12px">${tbl}${seats}</div>`;
    },
  };
})();

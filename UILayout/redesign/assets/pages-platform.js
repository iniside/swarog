(() => {
  const { icon, esc, panel, table, kpi, frag, badge, status, userCell, timeline, props, rowMenuBtn } = UI;
  const D = DATA;
  const P = window.PAGES;
  const plat = ["Platform", "index.html"];
  const pg = (module, prim) => ({ module, point: "page", prim });

  // ---------- API keys ----------
  UI.defineMenu("apikey", id => {
    const k = D.apiKeys.find(x => x.id === id);
    return [{ c: { module: "apikeys", point: "apikey.row_action", prim: "MenuAction" }, label: "apikeys", items: k.status === "revoked" ? [{ label: "Already revoked", icon: "ban", disabled: "revoked keys cannot be restored" }] : [
      { label: "Rotate secret…", icon: "refresh", run: "PAGES.reveal('" + id + "')" },
      { label: "Change role…", icon: "shield", run: "UI.toast('Role picker…','info')" },
      { label: "Revoke…", icon: "ban", danger: true, run: `PAGES.revokeKey('${id}')` },
    ] }, { c: { module: "audit", point: "apikey.row_action", prim: "MenuAction" }, label: "audit", items: [{ label: "Requests by this key", icon: "list", run: "location.href='audit.html'" }] }];
  });
  P.revokeKey = id => {
    const k = D.apiKeys.find(x => x.id === id);
    UI.confirm({ c: { module: "apikeys", point: "apikey.row_action", prim: "ConfirmAction" }, title: `Revoke ${k.name}?`, icon: "ban",
      body: "Requests signed with this key fail immediately on every process.",
      impact: [["Key", `<span class="mono">${k.id}</span> · ${k.role}`], ["Last used", k.lastUsed], ["Requests (24 h)", k.role === "game-server" ? "1.9 M" : "4,120"]],
      reason: "e.g. leaked in CI log", typeToConfirm: k.name, confirm: "Revoke key", done: "Key revoked" });
  };
  P.createKey = () => UI.modal(`${frag({ module: "apikeys", point: "page", prim: "Form" }, `<div class="modal-head"><div class="modal-icon ok">${icon("key")}</div><div><h2>Create API key</h2><div class="muted" style="font-size:12px">The secret is shown once after creation.</div></div></div><div class="modal-body">${UI.form({ actions: false, fields: [
    { label: "Name", required: true, value: "horde-agents-2", mono: true, help: "Lowercase, used in audit entries." },
    { label: "Role", type: "select", typeName: "ref ApiRole", options: D.apiRoles.map(r => r.name), value: "ci-build", help: D.apiRoles[1].scopes.map(s => `<span class="tag">${s}</span>`).join(" ") },
    { label: "Expires", type: "select", typeName: "enum", options: ["Never", "30 days", "90 days", "1 year"], value: "90 days" },
  ] })}</div>`, { left: true })}<div class="modal-foot"><button class="btn" onclick="UI.closeModal()">Cancel</button><button class="btn primary" onclick="PAGES.reveal()">Create key</button></div>`);
  P.reveal = () => UI.modal(`${frag({ module: "apikeys", point: "page", prim: "SecretReveal" }, `<div class="modal-head"><div class="modal-icon ok">${icon("check")}</div><div><h2>Key created — horde-agents-2</h2></div></div><div class="modal-body">${UI.secret({ value: "swg_live_6be1_Qm9yZWFsaXMtNGM3ZS1hMWQ5LWVtYmVybGluZS0wMDEx", extra: `<div style="margin-top:10px;font-size:12px" class="dim">Use as <span class="mono">Authorization: Bearer &lt;key&gt;</span>. Role <span class="tag">ci-build</span> · expires 2027-01-20.</div>` })}<label class="row" style="margin-top:12px;font-size:12.5px"><input type="checkbox" onchange="document.querySelector('.reveal-done').disabled=!this.checked"> I stored the secret somewhere safe</label></div>`, { left: true })}<div class="modal-foot"><button class="btn primary reveal-done" disabled onclick="UI.closeModal();UI.toast('Key active')">Done</button></div>`);

  P.apikeys = {
    meta: { nav: "apikeys", crumbs: [plat, ["API keys"]], after: () => { if (new URLSearchParams(location.search).get("reveal")) P.reveal(); } },
    render: () => {
      const roles = panel({ c: pg("apikeys", "DataTable"), title: "Roles", meta: `${D.apiRoles.length}`, flush: true, tools: `<button class="btn sm">${icon("plus")} Role</button>`,
        body: table({ pager: false, cols: [{ label: "Role" }, { label: "Scopes", wrap: true }, { label: "Keys", num: true }], rows: D.apiRoles.map(r => [`<span style="font-weight:500">${r.name}</span>`, r.scopes.map(s => `<span class="tag">${s}</span>`).join(" "), r.keys]), rowMenu: () => `<button class="btn sm icon ghost">${icon("more")}</button>` }),
        foot: "Scopes are contributed by modules (e.g. lore.presign by lore)." });
      const keys = panel({ c: pg("apikeys", "DataTable"), title: "Keys", meta: `${D.apiKeys.filter(k => k.status === "active").length} active`, flush: true,
        body: table({ filters: [{ k: "Status", v: "active + revoked" }, { k: "Role", v: "any" }], searchPlaceholder: "Name or key id", cols: [{ label: "Name" }, { label: "Key" }, { label: "Role" }, { label: "Created" }, { label: "Last used" }, { label: "Status" }],
          rows: D.apiKeys.map(k => [`<span style="font-weight:500">${k.name}</span>`, `<span class="mono muted">${k.prefix}</span>`, `<span class="tag">${k.role}</span>`, `<span class="row"><span class="dim">${k.created}</span> ${userCell(k.by)}</span>`, k.lastUsed === "now" ? badge("now", "ok") : `<span class="dim">${k.lastUsed}</span>`, status(k.status)]),
          rowMenu: (r, i) => rowMenuBtn("apikey", D.apiKeys[i].id), total: `${D.apiKeys.length} keys` }) });
      return P.head("API keys", "Machine credentials for game servers, CI and integrations.", `<button class="btn primary" onclick="PAGES.createKey()">${icon("plus")} Create key</button>`) +
        `<div class="stack">${keys}${roles}</div>`;
    },
  };

  // ---------- mail outbox (WorkQueue) ----------
  const att = (n, ok) => `<span class="attempts" title="${n} attempts">${Array.from({ length: 5 }, (_, i) => `<i class="${i < n ? (ok && i === n - 1 ? "ok" : "f") : ""}"></i>`).join("")}</span>`;
  P.mail = {
    meta: { nav: "mail", crumbs: [plat, ["Mail outbox"]] },
    render: () => {
      const tab = P.mailTab || "attention";
      const counts = [["attention", "Needs attention", 4], ["pending", "Pending", 309], ["retrying", "Retrying", 2], ["failed", "Failed", 1], ["parked", "Parked", 1], ["sent", "Sent (24 h)", "182,051"]];
      const list = D.mail.filter(m => tab === "attention" ? ["retrying", "failed", "parked"].includes(m.state) : m.state === tab);
      const q = panel({ c: pg("mail", "WorkQueue"), flush: true, title: "Outbox", meta: "relay smtp.emberline.gg",
        banner: `<div class="banner warn">${icon("alert")} SMTP relay is throttling (421). Retries back off exponentially; nothing is lost.</div>`,
        tools: `<button class="btn sm">${icon("pause")} Pause sending</button>`,
        body: `<div class="q-counts">${counts.map(([k, l, n]) => `<div class="q-count ${tab === k ? "on" : ""}" onclick="PAGES.mailTab='${k}';REG.render()"><div class="n">${n}</div><div class="l">${l}</div></div>`).join("")}</div>` +
          (list.length ? table({ selectable: true, searchPlaceholder: "Recipient, template or message id", filters: [{ k: "Template", v: "any" }],
            cols: [{ label: "Message" }, { label: "To" }, { label: "Template" }, { label: "State" }, { label: "Attempts" }, { label: "Next attempt" }, { label: "Last error", wrap: true }, { label: "" }],
            rows: list.map(m => [`<span class="mono">${m.id}</span>`, userCell(m.to), `<span class="tag">${m.template}</span>`, status(m.state), att(m.attempts, m.state === "sent"), `<span class="dim">${m.next}</span>`, m.err ? `<span class="mono" style="font-size:11.5px;color:${m.state === "failed" ? "var(--err)" : "var(--text-2)"}">${esc(m.err)}</span>` : `<span class="muted">—</span>`,
              `<span class="row" style="justify-content:flex-end;gap:4px">${m.state === "sent" ? "" : `<button class="btn sm" ${m.state === "pending" ? "disabled" : ""} onclick="UI.toast('Retry scheduled now')">${icon("refresh")} Retry</button><button class="btn sm ghost" title="Park: keep but stop retrying" onclick="UI.toast('Parked')">${icon("park")}</button><button class="btn sm ghost danger-ghost" title="Cancel" onclick="PAGES.cancelMail('${m.id}')">${icon("x")}</button>`}</span>`]),
            total: `${list.length} messages`, page: "1 / 1" }) : UI.stateBlock("empty", { title: "Queue is empty", detail: "Nothing in this state.", icon: "check", actions: "" })) });
      return P.head("Mail outbox", "Durable outbox; each message is retried until sent, cancelled or parked.", `<button class="btn">${icon("send")} Test send</button>`) + q;
    },
  };
  P.cancelMail = id => UI.confirm({ c: { module: "mail", point: "page", prim: "ConfirmAction" }, title: `Cancel ${id}?`, body: "The message is removed from the outbox and will never be delivered.", impact: [["Recipient", "kestrel"], ["Template", "season_reward"]], confirm: "Cancel message", done: "Message cancelled" });

  // ---------- scheduler ----------
  P.scheduler = {
    meta: { nav: "scheduler", crumbs: [plat, ["Scheduler"]] },
    render: () => {
      const s = panel({ c: pg("scheduler", "DataTable"), title: "Schedules", meta: `${D.schedules.length}`, flush: true, tools: `<button class="btn sm">${icon("plus")} Schedule</button>`,
        body: table({ pager: false, cols: [{ label: "Id" }, { label: "When" }, { label: "Target" }, { label: "Next fire" }, { label: "Last" }, { label: "Enabled" }],
          rows: D.schedules.map(x => [`<span class="mono" style="font-weight:500">${x.id}</span>`, `<span class="mono">${x.cron}</span>`, `<span class="tag">${x.target}</span>`, `<span class="dim">${x.next}</span>`, `<span class="row">${status(x.last)} <span class="muted">${x.lastAt}</span></span>`, `<span class="toggle ${x.enabled ? "on" : ""}"></span>`]),
          rowMenu: () => `<button class="btn sm icon ghost" onclick="UI.toast('Fire now · Edit · Delete','info')">${icon("more")}</button>` }) });
      const f = panel({ c: pg("scheduler", "Timeline"), title: "Recent fires", flush: true, tools: `<span class="badge plain">24 h</span>`,
        body: timeline(D.fires.map(x => ({ t: x.at.slice(0, 5), html: `<span class="mono">${x.id}</span> ${x.result === "ok" ? "" : badge("failed", "err")}`, meta: `${x.dur} · ${esc(x.note)}`, icon: x.result === "ok" ? "check" : "alert", tone: x.result === "ok" ? "ok" : "err" }))) });
      return P.head("Scheduler", "Cron and one-shot schedules that call module operations.", "") + `<div class="stack">${s}${f}</div>`;
    },
  };

  // ---------- config & flags ----------
  P.config = {
    meta: { nav: "config", crumbs: [plat, ["Config & flags"]] },
    render: () => {
      const sel = D.config.find(c => c.key === (P.cfg || "store.featured_rotation"));
      const t = panel({ c: pg("config", "DataTable"), flush: true, title: "Keys", meta: `${D.config.length}`,
        body: table({ pager: false, filters: [{ k: "Type", v: "any" }], searchPlaceholder: "Key prefix",
          cols: [{ label: "Key" }, { label: "Type" }, { label: "Value", wrap: true }, { label: "Rev", num: true }, { label: "Updated" }],
          rows: D.config.map(c => ({ sel: c === sel, cells: [`<a href="#" class="mono" onclick="PAGES.cfg='${c.key}';REG.render();return false">${c.key}</a>`, `<span class="tag">${c.type}</span>`, c.type === "bool" ? `<span class="toggle ${c.value === "true" ? "on" : ""}"></span>` : `<span class="mono">${esc(c.value)}</span>`, c.rev, `<span class="row"><span class="dim">${c.at}</span> ${P.actor(c.by)}</span>`] })) }) });
      const e = panel({ c: pg("config", "Form"), title: `Edit <span class="mono">${sel.key}</span>`, meta: `rev ${sel.rev}`,
        body: UI.form({ submit: "Save revision", note: `Compare-and-swap: saves only if still rev ${sel.rev}.`, fields: [
          { label: "Value", type: sel.type === "json" ? "textarea" : sel.type === "bool" ? "bool" : "text", typeName: sel.type, value: sel.type === "json" ? '[\n  "frost_warden",\n  "ash_blade",\n  "ember_cloak"\n]' : sel.type === "bool" ? sel.value === "true" : sel.value, mono: true, rows: 5 },
          { label: "Change note", value: "", placeholder: "Why? (stored with the revision)" },
        ] }) });
      const diff = `<div class="code"><span class="diff-del">-  "ash_blade"</span><span class="diff-add">+  "ash_blade",</span><span class="diff-add">+  "ember_cloak"</span></div>`;
      const h = panel({ c: pg("config", "Timeline"), title: "Revisions", flush: true, body: `<div style="padding:10px 12px 0"><div class="muted" style="font-size:12px;margin-bottom:6px">Pending change vs rev ${sel.rev}</div>${diff}</div>` + timeline([
        { t: sel.at, html: `rev ${sel.rev} by ${P.actor(sel.by)}`, meta: `<span class="mono">${esc(sel.value)}</span>`, icon: "edit", tone: "accent" },
        { t: "Oct 14", html: `rev ${sel.rev - 1} by ${userCell("marta.k")}`, meta: `<span class="mono">["frost_warden"]</span> <a href="#">restore</a>`, icon: "edit" },
        { t: "Sep 30", html: `rev ${sel.rev - 2} by ${userCell("jonas.l")}`, meta: `<span class="mono">[]</span> <a href="#">restore</a>`, icon: "edit" },
      ]) });
      return P.head("Config & flags", "Typed runtime configuration. Every save creates a revision.", `<button class="btn primary">${icon("plus")} Key</button>`) + `<div class="grid g-21">${t}<div class="stack">${e}${h}</div></div>`;
    },
  };

  // ---------- audit ----------
  P.audit = {
    meta: { nav: "audit", crumbs: [plat, ["Audit log"]] },
    render: () => {
      const topics = [...new Set(D.audit.map(a => a.topic))];
      const tf = P.auditTopic;
      const list = D.audit.filter(a => !tf || a.topic === tf);
      return P.head("Audit log", "Append-only record of operator, key and system actions.", `<button class="btn">${icon("download")} Export</button>`) +
        panel({ c: pg("audit", "DataTable"), flush: true,
          body: table({ searchPlaceholder: "Target or detail contains…", searchWidth: 260,
            filters: [`<select class="select" style="height:24px;font-size:12px" onchange="PAGES.auditTopic=this.value;REG.render()"><option value="">Topic: all</option>${topics.map(t => `<option ${t === tf ? "selected" : ""}>${t}</option>`).join("")}</select>`, `<select class="select" style="height:24px;font-size:12px"><option>Actor: anyone</option>${["marta.k", "jonas.l", "aiko.tanaka", "system", "scheduler", "apikey:ak_c4a8"].map(a => `<option>${a}</option>`).join("")}</select>`, { k: "Time", v: "today" }],
            cols: [{ label: "Time", sorted: "desc" }, { label: "Actor" }, { label: "Topic" }, { label: "Action" }, { label: "Target" }, { label: "Detail", wrap: true }],
            rows: list.map(a => [`<span class="mono">${a.t}</span>`, P.actor(a.actor), `<span class="tag">${a.topic}</span>`, `<span class="mono" style="font-weight:500">${a.action}</span>`, `<span class="mono" style="font-size:11.5px">${esc(a.target)}</span>`, `<span class="dim">${esc(a.detail)}</span>`]),
            total: `${list.length} of 1,204 entries today`, page: "1 / 49" }) });
    },
  };

  // ---------- event plane ----------
  const lagBar = s => { const n = +s.lag; const pct = Math.min(100, n / 3); return `<span class="row"><span class="lag-bar"><i class="${s.state === "paused" ? "e" : n > 100 ? "w" : ""}" style="width:${Math.max(3, pct)}%"></i></span><span class="mono" style="font-size:11.5px">${s.lag}</span></span>`; };
  P.events = {
    meta: { nav: "events", crumbs: [plat, ["Event plane"]] },
    render: () => {
      const tiles = [
        { label: "Head sequence", value: "991,210", delta: "1.8 k events/min" },
        { label: "Subscriptions", value: "7", delta: "6 running · 1 paused" },
        { label: "Max lag", value: "273", delta: '<span class="bad-up">wallet.on_match.v1 (wallet down)</span>' },
        { label: "Dead-lettered (7 d)", value: "3", delta: "all skipped by operator" },
      ].map(t => kpi({ ...t, c: pg("core.events", "KpiTile") })).join("");
      const subsTbl = panel({ c: pg("core.events", "DataTable"), title: "Subscriptions", flush: true,
        body: table({ pager: false, filters: [{ k: "State", v: "any" }], searchPlaceholder: "Subscription id or topic",
          cols: [{ label: "Subscription" }, { label: "Topic" }, { label: "Consumer" }, { label: "Checkpoint", num: true }, { label: "Head", num: true }, { label: "Lag" }, { label: "State" }],
          rows: D.subs.map(s => ({ sel: s.state === "paused", cells: [`<span class="mono" style="font-weight:500">${s.id}</span>`, `<span class="tag">${s.topic}</span>`, s.module, `<span class="mono">${s.checkpoint.toLocaleString("en-US")}</span>`, `<span class="mono muted">${s.head.toLocaleString("en-US")}</span>`, lagBar(s), status(s.state)] })),
          rowMenu: () => `<button class="btn sm icon ghost" onclick="UI.toast('Pause · Rewind checkpoint · Inspect','info')">${icon("more")}</button>` }) });
      const s = D.subs.find(x => x.poison);
      const poison = panel({ c: pg("core.events", "PoisonEventResolver"), title: `Paused: <span class="mono">${s.id}</span>`, meta: `since ${s.poison.at}`,
        banner: `<div class="banner err">${icon("alert")} Consumer failed 5× on the same event and stopped to preserve ordering. ${s.lag} events are waiting behind it.</div>`,
        body: `${props([["Event", `<span class="mono">wallet.debited #${s.poison.seq.toLocaleString("en-US")}</span>`], ["Consumer", `${s.module} · handler on_purchase`], ["Error", `<span class="mono" style="color:var(--err)">${esc(s.poison.err)}</span>`]])}<div class="code" style="margin-top:10px">{
  "seq": 12905,
  "topic": "wallet.debited",
  "player_id": "d71b2f86",
  "amount": { "value": 1, "currency": "TOK2" },
  "reason": "store.purchase",
  "tx": "tx_89f7"
}</div><div class="row" style="margin-top:12px;flex-wrap:wrap"><button class="btn primary" onclick="UI.toast('Resumed — retrying #12,905')">${icon("play")} Resume (retry event)</button><button class="btn" onclick="PAGES.skipPoison()">${icon("skip")} Skip event…</button><button class="btn ghost">${icon("download")} Copy payload</button><span class="muted" style="font-size:12px">Fix the consumer first, then Resume. Skip moves the event to the dead-letter list.</span></div>` });
      return P.head("Event plane", "Durable topics, subscription checkpoints and lag.", "") + `<div class="grid g-tiles" style="margin-bottom:12px">${tiles}</div><div class="stack">${poison}${subsTbl}</div>`;
    },
  };
  P.skipPoison = () => UI.confirm({ c: { module: "core.events", point: "page", prim: "ConfirmAction" }, title: "Skip event #12,905?", icon: "skip",
    body: "The checkpoint advances past this event and the subscription resumes. The inventory grant for this purchase will NOT be applied unless done by hand.",
    impact: [["Subscription", '<span class="mono">inventory.on_purchase.v1</span>'], ["Event", '<span class="mono">wallet.debited #12,905</span>'], ["Goes to", "dead-letter list (kept 30 d)"]],
    reason: "e.g. bad currency code from store hotfix, compensated manually", typeToConfirm: "12905", confirm: "Skip and resume", done: "Skipped; subscription resumed" });

  // ---------- economy ----------
  P.economy = {
    meta: { nav: "economy", crumbs: [["Economy", "economy.html"], ["Currencies & ledger"]] },
    render: () => {
      const stale = "Showing cached data from 3 min ago — wallet-svc is unavailable (7 restarts in 15 min).";
      const cur = panel({ c: pg("wallet", "DataTable"), title: "Currencies", flush: true, state: "stale", staleText: stale,
        body: table({ pager: false, cols: [{ label: "Code" }, { label: "Name" }, { label: "Kind" }, { label: "Supply", num: true }, { label: "Holders", num: true }, { label: "Cap" }], rows: D.currencies.map(c => [`<span class="tag">${c.code}</span>`, c.name, badge(c.kind, c.kind === "premium" ? "accent" : "", true), c.supply, c.holders, `<span class="dim">${c.cap}</span>`]) }) });
      const grant = panel({ c: pg("wallet", "Form"), title: "Grant or revoke", state: "ok",
        banner: `<div class="banner err">${icon("alert")} Submissions are disabled while wallet-svc is unavailable. Writes are never queued by the portal.</div>`,
        body: () => `<div class="seg" style="margin-bottom:12px"><button class="on">Grant</button><button>Revoke</button></div>` + UI.form({ submit: "Grant", note: "Idempotent per request id.", fields: [
          { label: "Player", type: "user", typeName: "ref user", value: "MiraVoss", disabled: true },
          { label: "Amount", type: "money", typeName: "i64 + CurrencyCode", value: "500", options: ["EMB", "GLD", "TOK"], disabled: true },
          { label: "Reason", value: "CS ticket #4482", required: true, disabled: true, help: "Shown in the ledger and audit log." },
          { label: "Request id", value: "grant-7c1e0a", mono: true, disabled: true, typeName: "idempotency key" },
        ] }, true) });
      const led = panel({ c: pg("wallet", "DataTable"), title: "Ledger", flush: true, state: "stale", staleText: stale,
        body: table({ filters: [{ k: "Currency", v: "any" }, { k: "Reason", v: "any" }], searchPlaceholder: "Player or tx id",
          cols: [{ label: "Time" }, { label: "Player" }, { label: "Currency" }, { label: "Amount", num: true }, { label: "Balance", num: true }, { label: "Reason", wrap: true }, { label: "Tx" }],
          rows: D.ledger.map(l => [`<span class="mono" style="font-size:11.5px">${l.t}</span>`, userCell(l.who), `<span class="tag">${l.cur}</span>`, `<span class="mono" style="color:${l.amt < 0 ? "var(--err)" : "var(--ok)"}">${l.amt > 0 ? "+" : ""}${l.amt.toLocaleString("en-US")}</span>`, `<span class="mono">${l.bal.toLocaleString("en-US")}</span>`, `<span class="dim">${esc(l.reason)}</span>`, `<span class="mono muted">${l.ref}</span>`]),
          total: "7 of 2.4 M transactions", page: "1 / 98,112" }) });
      return P.head("Currencies & ledger", "wallet module", "") + `<div class="grid g-21" style="margin-bottom:12px">${cur}${grant}</div>${led}`;
    },
  };
})();

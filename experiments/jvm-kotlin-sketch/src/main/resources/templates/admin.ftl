<#-- admin.ftl — shell reproduced from UILayout/GameOps Admin.dc.html.
     Sidebar = dynamic contributed groups (items grouped by section) + a static COMING SOON block.
     Server-side page switching: each item links to /admin/<slug>. -->
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GameOps · ${title}</title>
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Public+Sans:wght@400;500;600;700;800&family=IBM+Plex+Mono:wght@400;500;600&display=swap" rel="stylesheet">
  <link rel="stylesheet" href="/admin/theme.css">
</head>
<body>
  <div class="layout">

    <!-- SIDEBAR -->
    <aside class="sidebar">
      <div class="sb-head">
        <div class="logo">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#10131a" stroke-width="2.2" stroke-linejoin="round"><path d="M12 3 21 8v8l-9 5-9-5V8z"/><path d="M12 3v18M3 8l9 5 9-5"/></svg>
        </div>
        <div>
          <div class="brand-name">GameOps</div>
          <div class="brand-sub">Backend Console</div>
        </div>
      </div>

      <div class="nav">
        <#list groups as g>
        <div class="nav-label">${g.section}</div>
        <div class="nav-list">
          <#list g.items as item>
          <a class="nav-item<#if item.active> active</#if>" href="/admin/${item.slug}">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" style="flex-shrink:0;"><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></svg>
            <span>${item.label}</span>
          </a>
          </#list>
        </div>
        </#list>

        <div class="nav-label">COMING SOON</div>
        <div class="nav-list">
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" style="flex-shrink:0;"><path d="M4 20V12M10 20V4M16 20V9M21 20H3"/></svg><span>Analytics</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" style="flex-shrink:0;"><path d="M13 3 4 14h6l-1 7 9-11h-6z"/></svg><span>Live Ops &amp; Events</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" style="flex-shrink:0;"><ellipse cx="12" cy="6" rx="7" ry="3"/><path d="M5 6v6c0 1.7 3 3 7 3s7-1.3 7-3V6"/><path d="M5 12c0 1.7 3 3 7 3s7-1.3 7-3"/></svg><span>Economy &amp; Store</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" style="flex-shrink:0;"><rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><circle cx="7" cy="7.5" r="0.9" fill="currentColor" stroke="none"/><circle cx="7" cy="16.5" r="0.9" fill="currentColor" stroke="none"/></svg><span>Matchmaking &amp; Servers</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" style="flex-shrink:0;"><rect x="4" y="11" width="4" height="9" rx="1"/><rect x="10" y="5" width="4" height="15" rx="1"/><rect x="16" y="14" width="4" height="6" rx="1"/></svg><span>Leaderboards</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" style="flex-shrink:0;"><path d="M12 3 5 6v5c0 4 3 7 7 9 4-2 7-5 7-9V6z"/><path d="M9 12l2 2 4-4"/></svg><span>Moderation</span></span>
          <span class="nav-item muted"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" style="flex-shrink:0;"><path d="M5 4v16"/><path d="M5 5h12l-2.5 3.5L17 12H5"/></svg><span>Game Config &amp; Flags</span></span>
        </div>
      </div>

      <div class="sb-foot">
        <div class="avatar">AR</div>
        <div style="flex:1;min-width:0;">
          <div class="u-name">A. Reyes</div>
          <div class="u-role">Backend Admin</div>
        </div>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="#6b7180" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>
      </div>
    </aside>

    <!-- MAIN -->
    <div class="main">
      <div class="header">
        <div style="min-width:0;">
          <div class="crumb">${crumb}</div>
          <div class="page-title">${title}</div>
        </div>
        <div class="spacer"></div>
        <div class="search">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="#6b7180" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M21 21l-4-4"/></svg>
          <span class="ph">Search players, matches, IDs…</span>
          <span class="kbd">⌘K</span>
        </div>
        <div class="pill-prod"><span class="pulse"></span><span>Production</span></div>
        <div class="icon-btn">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#aeb3bd" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9"/><path d="M13.7 21a2 2 0 0 1-3.4 0"/></svg>
          <span class="count">4</span>
        </div>
        <div class="avatar-sm">AR</div>
      </div>

      <div class="scroll">
        <#if page??>
          <#if page.err??>
          <div class="card"><p class="empty">${page.err}</p></div>
          <#else>
          <div class="stack">
            <#if page.kpis?has_content>
            <div class="kpis">
              <#list page.kpis as kpi>
              <div class="kpi">
                <div class="kpi-label">${kpi.label}</div>
                <div class="kpi-val">${kpi.value}</div>
              </div>
              </#list>
            </div>
            </#if>

            <#if page.table??>
            <div class="card">
              <h2 class="card-title">${page.title}</h2>
              <table>
                <thead>
                  <tr><#list page.table.headers as h><th>${h}</th></#list></tr>
                </thead>
                <tbody>
                  <#list page.table.rows as row>
                  <tr>
                    <#list row as cell>
                    <td class="<#if cell.mono>mono</#if>">
                      <#if cell.badge><span class="badge">${cell.text}</span><#else>${cell.text}</#if>
                    </td>
                    </#list>
                  </tr>
                  <#else>
                  <tr><td class="empty" colspan="${page.table.headers?size}">— no rows —</td></tr>
                  </#list>
                </tbody>
              </table>
            </div>
            </#if>
          </div>
          </#if>
        <#else>
        <p class="empty">No sections contributed.</p>
        </#if>
      </div>
    </div>

  </div>
</body>
</html>

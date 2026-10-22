Swarog Admin is the operator portal of one Swarog deployment: one studio, one isolated install, one operator team. It is an ops tool in the line of Linear, Grafana and Retool: dense, quiet, and built so that whatever needs attention is visible at a glance. There is no marketing surface, no tenant switcher and no decoration.

The portal knows nothing about game domains. Every screen is assembled from **panel primitives** that modules contribute through named **extension points**. Each panel loads, lags or fails on its own. Design every new screen from the primitives below; if one is missing, add it here first.

## Content fundamentals

- **Write from the operator's side.** Name things by what the operator manages: "Locks", "Mail outbox", "API keys", not "LockService" or "outbox rows".
- **Sentence case everywhere**: page titles, panel titles, buttons, menu items. Section labels (sidebar sections, timeline day dividers, menu group headers) are the only uppercase text, and they get it from CSS (`nav-title`, `tl-day`, `menu-group`; type style `ts-section-label`).
- **Buttons say exactly what happens**: "Force unlock", "Revoke key", "Skip and resume". The toast confirms in the past tense: "Lock released", "Key revoked".
- **Confirmation titles are questions**: "Force unlock this file?", "Revoke horde-agents?".
- **Errors name the module and the next step**: "wallet module unavailable", then the detail ("wallet-svc did not answer within 2 s. The rest of this page is unaffected.") and a Retry. Never "Something went wrong".
- **Identifiers stay verbatim and in mono**: `c41f9e2`, `ak_c4a8`, `lore.branch.pushed`, `0 7 * * *`. Entity references are written `kind:id`: `user:7f3a9c21`, `repository:ashfall-game`, `apikey:ak_9f2c`.
- **Numbers carry units**: "13.29 TB", "lag 273", "p99 38 ms", "held 22 d". Use tabular figures in every column of digits.
- **No emoji, no exclamation marks, no filler stats.**

## Visual foundations

**Color.** Neutral greys with a slight blue bias, one indigo `accent`, four status hues. Status color is never decoration: `ok` (running, sent, active), `warn` (degraded, lagging, retrying, stale), `err` (down, failed, paused, destructive), `info` (pending, read-only). Each status appears as text on its `*-soft` fill (badges, banners) or as a solid dot or bar. Status is always carried by a word or icon as well as color. Text uses `text` / `text-2` / `text-3` on `panel`, `panel-2` or `bg`; every pair meets 4.5:1 in both themes. `text-3` never goes on a soft status fill.

**Themes.** Light is first and the fallback; dark is a full second palette, not an inversion. In dark, the accent lightens, so labels on it switch to dark text (`accent-text`, `on-err`).

**Type.** One sans at 13px base (`ts-body`), tables at 12.5px (`ts-table`), metadata at 12px (`ts-meta`), labels at 11.5px (`ts-label`). The largest text on any screen is a KPI value at 22px; page titles are 18px. Mono (`ts-mono`, `ts-tag`) is for identifiers only.

**Spacing and density.** Controls are 28px tall (`control-height`), small controls 24px; table rows 36px (`row-height`). Panels pad 12px (`space-3`) and sit 12px apart. Page gutter 20px (`space-5`) on desktop, 12–16px on tablet and phone.

**Surfaces, borders, shadows.** Panels are `panel` with a 1px `border` and `radius-lg`; they never cast shadows. Only floating things (menus, popovers, modals, the search palette, toasts) use `shadow-pop`. Controls use `control-border`, which holds 3:1 against the panel. Keyboard focus is a solid 2px `accent` outline, offset 2px.

**Radii.** `radius-sm` 4px for tags and cells, `radius-item` 5px for nav and menu items, `radius` 6px for controls, `radius-lg` 8px for panels and menus, `radius-xl` 10px for modals and status pills.

**Motion.** Only the skeleton shimmer while loading, the 120 ms toggle knob and the drawer slide on tablet. No entrance animations.

**Layout.** A 232px sidebar (`sidebar-width`) with module-contributed nav sections, a 48px topbar (`topbar-height`) with breadcrumbs, global search, health, notifications. Content is a grid of panels (`g-tiles` for KPI rows, `g-21`, `g-2`, `stack`). Under 1080px the sidebar becomes a drawer and multi-column grids stack. Wide tables scroll inside their panel, never the page.

## Panel states (owned by the portal, never by a module)

Every primitive renders inside `Panel`, which owns seven states. Modules return data or a typed failure; they never draw their own error UI.

| state | when | looks like |
| --- | --- | --- |
| ok | data returned | the primitive |
| loading | fetch over 150 ms | skeleton bars shaped like the primitive |
| empty | zero items | icon well, title, one line, optional action |
| stale | module slow or flapping, cached copy exists | `warn-soft` banner with the data's age and Refresh; data stays visible; writes disabled if the module is unreachable |
| error | module unavailable, nothing cached | `err-soft` icon well, "<module> module unavailable", detail, Retry for this panel only |
| readonly | module declares no remote writes | `info-soft` banner and a dashed "read-only" chip; read actions stay enabled |
| denied | operator lacks the panel's permission | `warn-soft` lock well naming the permission; nothing is fetched |

## Using this system

Load `tokens.css`, then `components/bundle.css`, then `components/bundle.js`. The bundle needs no framework: it defines `window.Swarog` (also `window.UI`), whose primitives are functions that **return HTML strings**. Insert them with `innerHTML`:

```js
root.innerHTML = Swarog.Panel({ title: "Open locks", flush: true,
  c: { module: "lore", point: "page", prim: "DataTable" },
  body: Swarog.DataTable({ cols: [{ label: "File" }, { label: "Owner" }], rows: [["Hub.umap", "sam.o"]] }) });
```

Pass `c: { module, point, prim }` on every contributed fragment. It feeds the annotation layer (`Annotation`): with `document.body.classList.add("annot")` each fragment is outlined in its module's color and labelled with module, extension point and primitive. Set the panel state with `state` (`"loading"`, `"empty"`, `"stale"`, `"error"`, `"denied"`) or `readonly: true`.

Status badges come only from `Swarog.status(value)`, so one value always gets one tone. Icons come from `Swarog.icon(name)`: a 1.7px-stroke 24px line set drawn inline with `currentColor` (home, users, repo, lock, unlock, shield, server, activity, chart, key, mail, clock, sliders, list, zap, coins, search, bell, alert, info, check, x, more, refresh, branch, trash, edit, ban, gift, send and more). No icon font, no emoji.

## Brand mark

There is no logo file. The portal shows a typographic monogram: a 24px `radius` square in `text` with a bold "S" in `bg`, beside "Swarog Admin" and the deployment id in mono (`emberline-prod`).


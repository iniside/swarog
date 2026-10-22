# Panel

The chrome every contributed fragment renders in: title, meta, tools, banner, body, footer, and the seven panel states.

## When to use

Always. A module never returns bare markup; it returns a primitive, and the portal wraps it in a Panel. One Panel per independently fetched piece of data.

## What the consumer provides

- `title`, optional `meta` (one short phrase in `meta` style), `icon`, `tools` (small buttons, right-aligned), `foot`.
- `body`: a primitive's HTML, or a function `({ readonly }) => html` so write controls can disable themselves.
- `state`: `"ok"` (default), `"loading"`, `"empty"`, `"stale"`, `"error"`, `"denied"`; or `readonly: true`.
- State text: `emptyTitle`, `emptyDetail`, `emptyIcon`, `emptyActions`, `errorDetail`, `staleText`, `readonlyText`, `perm` (for denied).
- `flush: true` when the body is a table or list that runs edge to edge.
- `c: { module, point, prim }` for the annotation layer.

## Do and don't

- Do let the Panel draw failure. A module returns `Unavailable`, `Denied` or data; it never draws an error card itself.
- Do keep data visible in `stale`; disable writes only if the module is unreachable.
- Don't nest Panels. Don't add shadows to Panels.

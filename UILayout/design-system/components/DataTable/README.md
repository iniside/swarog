# DataTable

Rows of one entity kind with search, filter chips, sort, pagination, selection, bulk actions and a per-row action menu.

## When to use

Any list of entities: users, repositories, locks, keys, audit entries, ledger rows. Other modules may add columns to it through `entity.<kind>.list_column`; a failing contributed column shows an alert icon in its cells and a banner, never a failed table.

## What the consumer provides

- `cols`: `[{ label, num?, wrap?, sorted?: "asc" | "desc" }]`. `num` right-aligns with tabular figures; `wrap` lets long text wrap (min 220px).
- `rows`: arrays of cell HTML, or `{ cells, href?, sel? }` for clickable or selected rows.
- `filters`: `[{ k, v }]` chips or raw HTML; `search: false` hides the search box; `searchPlaceholder`.
- `selectable`, `bulk` (HTML of the bulk bar shown when rows are selected).
- `rowMenu(row, i)`: returns the trailing action button (use `Swarog.rowMenuBtn(menuId, arg)`).
- `total`, `page`, `footNote`; `pager: false` for short fixed lists.

## Do and don't

- Do put ids, hashes and paths in mono; statuses through `Swarog.status`.
- Do keep 36px rows; wrap a long path into file name plus a muted directory line instead of truncating it.
- Don't scroll the page sideways; the table scrolls inside its panel.

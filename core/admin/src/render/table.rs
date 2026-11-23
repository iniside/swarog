use controlplane::{Align, CellKind, FilterKind, Point, SortDir};
use serde::Serialize;
use crate::engine::columns::{ColumnOutcome, ListColumn};
use crate::engine::labels::Labels;
use super::*;

// ---- DataTable ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ColumnView {
    pub(crate) label: String,
    pub(crate) num: bool,
    /// The sort arrow of the column the rows are ordered by.
    pub(crate) sorted: Option<&'static str>,
    /// An `EntityListColumn`: its label is wrapped in a `span`.
    pub(crate) contributed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TableCell {
    pub(crate) cell: CellView,
    pub(crate) num: bool,
    /// `Column::wrap`.
    pub(crate) wrap: bool,
    /// A failed list column's marker title; the cell draws the marker instead of `cell`.
    pub(crate) failed: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RowView {
    pub(crate) cells: Vec<TableCell>,
    pub(crate) href: Option<String>,
    pub(crate) menu_url: Option<String>,
    pub(crate) highlight: bool,
    pub(crate) actions: Vec<ActionItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FilterView {
    pub(crate) label: String,
    pub(crate) value: String,
    /// A `FilterKind::Select` draws a select; every other kind a chip.
    pub(crate) select: Option<SelectView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SelectView {
    pub(crate) key: String,
    /// The panel URL without this filter's key; the select sends its own value.
    pub(crate) url: String,
    pub(crate) options: Vec<OptionView>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OptionView {
    pub(crate) value: String,
    pub(crate) selected: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TableView {
    pub(crate) columns: Vec<ColumnView>,
    pub(crate) rows: Vec<RowView>,
    pub(crate) selectable: bool,
    pub(crate) row_menu: bool,
    /// Some row has `TableRow` actions: every row gets a trailing actions cell.
    pub(crate) row_actions: bool,
    pub(crate) toolbar: bool,
    pub(crate) search_placeholder: Option<String>,
    pub(crate) q: String,
    pub(crate) search_url: String,
    pub(crate) filters: Vec<FilterView>,
    pub(crate) foot: bool,
    pub(crate) total: String,
    pub(crate) size: u32,
    pub(crate) next_url: Option<String>,
    /// The pager's `"1 / N"` between its buttons; `None` past the first page.
    pub(crate) page_label: Option<String>,
}

/// `"1 / N"` on the first page only: a cursor does not say how far into the list it is.
pub(super) fn page_label(page: &controlplane::PageInfo, ctx: &Ctx<'_>) -> Option<String> {
    if ctx.param(QUERY_CURSOR).is_some() {
        return None;
    }
    match page.total {
        Some(total) if page.size > 0 => Some(format!("1 / {}", total.div_ceil(u64::from(page.size)).max(1))),
        _ if page.next_cursor.is_none() => Some("1 / 1".to_string()),
        _ => None,
    }
}

/// An absent filter value reads as `"any"` and a Select draws its "all" placeholder (the panel
/// route drops an empty or `"any"` value before it gets here). A Select's current value
/// outside its declared options is drawn as one more selected option, so the select shows
/// what the table is filtered by rather than its "all" placeholder.
pub(super) fn filter_views(filters: &[controlplane::FilterDef], ctx: &Ctx<'_>) -> Vec<FilterView> {
    filters
        .iter()
        .map(|f| {
            seen::record(&f.kind);
            let current = ctx.param(&f.key);
            let select = match &f.kind {
                FilterKind::Select(options) => {
                    let mut views: Vec<OptionView> = options
                        .iter()
                        .map(|o| OptionView { value: o.clone(), selected: current == Some(o.as_str()) })
                        .collect();
                    if let Some(value) = current.filter(|v| !options.iter().any(|o| o == v)) {
                        views.push(OptionView { value: value.to_string(), selected: true });
                    }
                    Some(SelectView {
                        key: f.key.clone(),
                        url: with_param(ctx.path, ctx.query, &f.key, None),
                        options: views,
                    })
                }
                FilterKind::Enum(_) | FilterKind::Text | FilterKind::Range | FilterKind::Bool | FilterKind::Ref(_) => None,
            };
            FilterView { label: f.label.clone(), value: current.unwrap_or("any").to_string(), select }
        })
        .collect()
}

fn right_aligned(align: Align) -> bool {
    seen::record(&align);
    align == Align::Right
}

pub(super) fn column_views(columns: &[controlplane::Column]) -> Vec<ColumnView> {
    columns
        .iter()
        .map(|c| ColumnView {
            label: c.label.clone(),
            num: right_aligned(c.align),
            sorted: c.sorted.map(|dir| {
                seen::record(&dir);
                match dir {
                    SortDir::Asc => "↑",
                    SortDir::Desc => "↓",
                }
            }),
            contributed: false,
        })
        .collect()
}

/// `in_list`: the cells belong to a table row that opens an entity, so a linked user cell
/// carries its link inside.
pub(super) fn table_cells(
    cells: &[controlplane::Cell],
    columns: &[controlplane::Column],
    in_list: bool,
    labels: &Labels,
) -> Vec<TableCell> {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| TableCell {
            cell: list_cell(c, in_list, Some(labels)),
            num: columns.get(i).is_some_and(|col| right_aligned(col.align)),
            wrap: columns.get(i).is_some_and(|col| col.wrap),
            failed: None,
        })
        .collect()
}

fn list_cell(c: &controlplane::Cell, in_list: bool, labels: Option<&Labels>) -> CellView {
    let mut view = cell(c, labels);
    view.inner_link = in_list && view.kind == "user" && view.href.is_some();
    view
}

/// The cell of `kind`'s list column `column` in a row whose entity id is `id`: the column's
/// cell for it, an empty cell when it has none, or the failure marker. A column cell is no
/// part of the slot's body, so it draws no label.
fn list_column_cell(kind: &str, column: &ListColumn, id: Option<&str>, in_list: bool) -> TableCell {
    let point = Point::EntityListColumn { kind: kind.to_string(), label: column.label.clone(), align: column.align };
    seen::record(&point);
    let num = right_aligned(column.align);
    let blank = controlplane::Cell {
        kind: CellKind::Text,
        value: String::new(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    };
    match &column.outcome {
        ColumnOutcome::Cells(cells) => {
            let found = id.and_then(|id| cells.get(id)).unwrap_or(&blank);
            TableCell { cell: list_cell(found, in_list, None), num, wrap: false, failed: None }
        }
        ColumnOutcome::Failed(_) => TableCell {
            cell: cell(&blank, None),
            num,
            wrap: false,
            failed: Some(format!("{} module unavailable", column.module)),
        },
    }
}

/// The foot's count: with an `item_noun`, `"M accounts · showing 1–N"` on the first page
/// and `"M accounts"` past it (a cursor does not say how far in it is), `"N accounts"`
/// without a total; without one, `"N of M"` or `"N rows"`. The noun agrees with the count
/// before it.
fn table_total(t: &controlplane::DataTable, shown: u64, ctx: &Ctx<'_>) -> String {
    match (&t.item_noun, t.page.total) {
        (None, Some(total)) => format!("{} of {}", thousands(shown), thousands(total)),
        (None, None) => format!("{} {}", thousands(shown), if shown == 1 { "row" } else { "rows" }),
        (Some(noun), Some(total)) if ctx.param(QUERY_CURSOR).is_none() => {
            format!("{} {} · showing 1–{}", thousands(total), noun.of(total), thousands(shown))
        }
        (Some(noun), Some(total)) => format!("{} {}", thousands(total), noun.of(total)),
        (Some(noun), None) => format!("{} {}", thousands(shown), noun.of(shown)),
    }
}

pub(super) fn table(t: &controlplane::DataTable, ctx: &Ctx<'_>) -> TableView {
    let kind = t.list_columns_for.as_deref();
    let at = t.list_columns_at.min(t.columns.len());
    let list_columns = ctx.input.list_columns;
    let rows: Vec<RowView> = t
        .rows
        .iter()
        .map(|r| {
            let href = r.entity.as_ref().filter(|e| ctx.input.header_kinds.contains(&e.kind)).map(entity_url);
            let in_list = href.is_some();
            let mut cells = table_cells(&r.cells, &t.columns, in_list, ctx.input.labels);
            let id = r.entity.as_ref().filter(|e| Some(e.kind.as_str()) == kind).map(|e| e.id.as_str());
            let at = at.min(cells.len());
            let drawn = list_columns.iter().map(|c| list_column_cell(kind.unwrap_or_default(), c, id, in_list));
            cells.splice(at..at, drawn);
            RowView {
                cells,
                href,
                menu_url: r
                    .entity
                    .as_ref()
                    .filter(|e| ctx.input.row_menu_kinds.contains(&e.kind))
                    .map(|e| format!("/admin/_menu/{}/{}", encode(&e.kind), encode(&e.id))),
                highlight: r.highlight,
                actions: r
                    .actions
                    .iter()
                    .filter_map(|id| ctx.input.actions.iter().find(|a| a.table_row && &a.id == id))
                    .map(|a| bind(a, r.entity.as_ref().or(ctx.input.entity), r.item.as_deref()))
                    .collect(),
            }
        })
        .collect();
    let mut columns = column_views(&t.columns);
    columns.splice(
        at..at,
        list_columns.iter().map(|c| ColumnView {
            label: c.label.clone(),
            num: right_aligned(c.align),
            sorted: None,
            contributed: true,
        }),
    );
    let total = table_total(t, rows.len() as u64, ctx);
    TableView {
        columns,
        row_menu: rows.iter().any(|r| r.menu_url.is_some()),
        row_actions: rows.iter().any(|r| !r.actions.is_empty()),
        rows,
        selectable: t.selectable,
        toolbar: !t.filters.is_empty() || t.search_placeholder.is_some(),
        search_placeholder: t.search_placeholder.clone(),
        q: ctx.input.search.unwrap_or_default().to_string(),
        search_url: with_param(ctx.path, ctx.query, QUERY_SEARCH, None),
        filters: filter_views(&t.filters, ctx),
        foot: t.page.next_cursor.is_some() || t.page.total.is_some_and(|n| n > 0 || !t.rows.is_empty()),
        total,
        size: t.page.size,
        next_url: t.page.next_cursor.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_CURSOR, Some(c))),
        page_label: page_label(&t.page, ctx),
    }
}

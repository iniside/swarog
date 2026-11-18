use serde::Serialize;
use super::*;

// ---- WorkQueue ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueStateView {
    pub(crate) label: String,
    pub(crate) count: String,
    pub(crate) tone: &'static str,
    pub(crate) on: bool,
    pub(crate) url: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueItemView {
    pub(crate) id: String,
    pub(crate) entity: Option<CellView>,
    /// `WorkQueue::columns`' cells, drawn between the id and the state.
    pub(crate) cells: Vec<TableCell>,
    pub(crate) state: CellView,
    pub(crate) attempts: Vec<&'static str>,
    pub(crate) attempts_title: String,
    pub(crate) next_attempt: Option<String>,
    pub(crate) last_error: Option<String>,
    pub(crate) error_color: &'static str,
    pub(crate) payload: Option<String>,
    pub(crate) actions: Vec<ActionItem>,
}

/// The toolbar and foot fields carry `TableView`'s names, so `table_toolbar` and
/// `table_foot` draw a queue as they draw a table.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct QueueView {
    pub(crate) states: Vec<QueueStateView>,
    pub(crate) items: Vec<QueueItemView>,
    pub(crate) has_entity: bool,
    pub(crate) has_payload: bool,
    /// The id column's header.
    pub(crate) item_label: String,
    pub(crate) columns: Vec<ColumnView>,
    pub(crate) selectable: bool,
    pub(crate) toolbar: bool,
    pub(crate) search_placeholder: Option<String>,
    pub(crate) q: String,
    pub(crate) search_url: String,
    pub(crate) filters: Vec<FilterView>,
    pub(crate) foot: bool,
    /// `"7 messages"`, or `"25 of 309 messages"` when more exist.
    pub(crate) total: String,
    pub(crate) size: u32,
    pub(crate) next_url: Option<String>,
    pub(crate) page_label: Option<String>,
}

/// The attempts bar always draws this many squares (`pages-platform.js:47`); its title
/// carries the real count.
const ATTEMPT_SQUARES: u32 = 5;

pub(super) fn queue(q: &controlplane::WorkQueue, ctx: &Ctx<'_>) -> QueueView {
    let items: Vec<QueueItemView> = q
        .items
        .iter()
        .map(|it| {
            let state_tone = status_tone(&it.state);
            let attempts = (0..ATTEMPT_SQUARES)
                .map(|i| match (i < it.attempts, state_tone == "ok" && i + 1 == it.attempts) {
                    (true, true) => "ok",
                    (true, false) => "f",
                    (false, _) => "",
                })
                .collect();
            let actions = it
                .actions
                .iter()
                .filter_map(|id| ctx.input.actions.iter().find(|a| a.queue_row && &a.id == id))
                .map(|a| bind(a, it.entity.as_ref(), Some(&it.id)))
                .collect();
            QueueItemView {
                id: it.id.clone(),
                entity: it.entity.as_ref().map(ref_cell),
                cells: table_cells(&it.cells, &q.columns, false),
                state: CellView {
                    kind: "badge",
                    value: it.state.clone(),
                    tone: state_tone,
                    href: None,
                    color: "",
                    initials: String::new(),
                    bar: None,
                    bar_tone: "",
                    more: Vec::new(),
                    inner_link: false,
                    row: false,
                },
                attempts,
                attempts_title: format!("{} attempts", it.attempts),
                next_attempt: it.next_attempt_at.clone(),
                last_error: it.last_error.clone(),
                error_color: if state_tone == "err" { "var(--err)" } else { "var(--text-2)" },
                payload: it.payload_preview.clone(),
                actions,
            }
        })
        .collect();
    let shown = items.len() as u64;
    let total = match q.page.total {
        Some(total) if total != shown => {
            format!("{} of {} {}", thousands(shown), thousands(total), q.item_noun.of(total))
        }
        _ => format!("{} {}", thousands(shown), q.item_noun.of(shown)),
    };
    QueueView {
        states: q
            .states
            .iter()
            .map(|s| QueueStateView {
                label: s.label.clone(),
                count: thousands(s.count),
                tone: s.tone.map(tone).unwrap_or(""),
                on: s.key == q.selected,
                url: with_param(ctx.path, ctx.query, QUERY_STATE, Some(&s.key)),
            })
            .collect(),
        has_entity: items.iter().any(|i| i.entity.is_some()),
        has_payload: items.iter().any(|i| i.payload.is_some()),
        item_label: q.item_label.clone(),
        columns: column_views(&q.columns),
        selectable: q.selectable,
        toolbar: !q.filters.is_empty() || q.search_placeholder.is_some(),
        search_placeholder: q.search_placeholder.clone(),
        q: ctx.input.search.unwrap_or_default().to_string(),
        search_url: with_param(ctx.path, ctx.query, QUERY_SEARCH, None),
        filters: filter_views(&q.filters, ctx),
        foot: !items.is_empty() || q.page.next_cursor.is_some(),
        total: total.trim_end().to_string(),
        size: q.page.size,
        next_url: q.page.next_cursor.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_CURSOR, Some(c))),
        page_label: page_label(&q.page, ctx),
        items,
    }
}

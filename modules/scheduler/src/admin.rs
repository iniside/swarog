//! The `scheduler` control-plane provider: the Scheduler page (the Schedules table with each
//! schedule's interval, next fire and last fire, and the Recent fires timeline of the last 24
//! hours) and its disabled actions (create, fire now, edit, delete). Nothing here writes: the
//! module declares no writes and every action is `implemented: false`. It also draws the
//! dashboard's "Scheduler runs" attention item.

use async_trait::async_trait;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Banner, Cell, CellKind, Column, ContributionDecl, DataTable,
    Emphasis, EntityKindDecl, EntityRef, InvokeCall, InvokeOutcome, Layout, Link, Manifest, NavEntry, Noun,
    PageDecl, PageInfo, PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery,
    Severity, Then, Timeline, TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use sqlx::PgPool;

use crate::store::{self, FireRow, ScheduleRow};

const MODULE: &str = "scheduler";
pub(crate) const PAGE: &str = "schedules";
pub(crate) const SCHEDULE_KIND: &str = "schedule";

pub(crate) const SCHEDULES: &str = "schedules";
pub(crate) const FIRES: &str = "fires";

pub(crate) const SCHEDULE_CREATE: &str = "schedule.create";
pub(crate) const SCHEDULE_FIRE: &str = "schedule.fire";
pub(crate) const SCHEDULE_EDIT: &str = "schedule.edit";
pub(crate) const SCHEDULE_DELETE: &str = "schedule.delete";

pub(crate) const SUBTITLE: &str =
    "Interval schedules; each fire emits scheduler.fired for the modules that react to it.";

/// The schedules the table draws; one more is read to learn that more exist.
pub(crate) const MAX_ROWS: usize = 100;
/// The fires the timeline draws.
pub(crate) const MAX_FIRES: usize = 100;
/// The tag on the Recent fires panel: the window `store::recent_fires` reads.
pub(crate) const FIRES_WINDOW: &str = "24 h";
/// What a never-fired or out-of-range value shows.
pub(crate) const NEVER: &str = "—";
pub(crate) const DISABLED_BANNER: &str = "Scheduler disabled on this process: no schedule fires";

pub(crate) const OK: &str = "ok";
pub(crate) const FAIL: &str = "fail";

/// The operation names the FAKE target cell draws from.
pub(crate) const FAKE_TARGETS: [&str; 5] =
    ["leaderboard.reset", "mail.enqueue_batch", "storage.collect_orphans", "accounts.purge", "config.set_flag"];
/// One FAKE result in this many is a failure.
pub(crate) const FAKE_FAIL_ONE_IN: u64 = 8;
/// One FAKE enabled switch in this many is off.
pub(crate) const FAKE_OFF_ONE_IN: u64 = 6;
/// The FAKE duration of a failed fire: the timeout it hit.
pub(crate) const FAKE_FAIL_DURATION: &str = "30.0 s";
pub(crate) const FAKE_FAIL_NOTE: &str = "timeout: deadline exceeded";

pub(crate) struct SchedulerAdmin {
    pool: PgPool,
    /// This process's `SCHEDULER_ENABLED`: when off, no schedule it lists fires here.
    enabled: bool,
}

impl SchedulerAdmin {
    pub(crate) fn new(pool: PgPool, enabled: bool) -> SchedulerAdmin {
        SchedulerAdmin { pool, enabled }
    }

    async fn schedules(&self) -> Result<PanelBody, Error> {
        let (rows, total) = store::schedules(&self.pool, MAX_ROWS as i64 + 1).await.map_err(internal)?;
        Ok(schedules_body(&rows, total, self.enabled))
    }

    async fn fires(&self) -> Result<PanelBody, Error> {
        let rows = store::recent_fires(&self.pool, MAX_FIRES as i64).await.map_err(internal)?;
        Ok(fires_body(&rows))
    }
}

fn internal<E: std::fmt::Display>(e: E) -> Error {
    Error::internal(e.to_string())
}

fn cell(kind: CellKind, value: impl Into<String>) -> Cell {
    Cell {
        kind,
        value: value.into(),
        tone: None,
        link: None,
        bar: None,
        more: Vec::new(),
        row: false,
        label: None,
    }
}

fn column(key: &str, label: &str, kind: CellKind) -> Column {
    Column {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        sortable: false,
        align: Align::Left,
        sorted: None,
        wrap: false,
    }
}

/// The interval as the When column draws it: whole hours, else whole minutes, else seconds.
pub(crate) fn every(interval_seconds: i64) -> String {
    if interval_seconds > 0 && interval_seconds % 3600 == 0 {
        format!("every {} h", interval_seconds / 3600)
    } else if interval_seconds > 0 && interval_seconds % 60 == 0 {
        format!("every {} min", interval_seconds / 60)
    } else {
        format!("every {interval_seconds} s")
    }
}

/// The FAKE operation a schedule calls.
pub(crate) fn fake_target(name: &str) -> &'static str {
    // FAKE: a fire carries only the schedule name; no schedule records a target operation.
    FAKE_TARGETS[(controlplane::fake_hash("scheduler.target", name) % FAKE_TARGETS.len() as u64) as usize]
}

/// The FAKE result of a schedule's last fire, shared by the Last cell and the fire entry.
pub(crate) fn fake_result(name: &str) -> &'static str {
    // FAKE: the scheduler records no fire outcome; a fire only bumps `last_fired`.
    if controlplane::fake_hash("scheduler.last", name).is_multiple_of(FAKE_FAIL_ONE_IN) {
        FAIL
    } else {
        OK
    }
}

/// The FAKE per-schedule enabled switch.
pub(crate) fn fake_enabled(name: &str) -> bool {
    // FAKE: the store has no per-schedule enabled flag; only the process-wide SCHEDULER_ENABLED.
    !controlplane::fake_hash("scheduler.enabled", name).is_multiple_of(FAKE_OFF_ONE_IN)
}

/// The FAKE duration of a schedule's last fire.
pub(crate) fn fake_duration(name: &str) -> String {
    // FAKE: the scheduler records no fire duration.
    if fake_result(name) == FAIL {
        return FAKE_FAIL_DURATION.to_string();
    }
    let tenths = controlplane::fake_hash("scheduler.duration", name) % 99 + 1;
    format!("{}.{} s", tenths / 10, tenths % 10)
}

/// The FAKE note of a schedule's last fire.
pub(crate) fn fake_note(name: &str) -> String {
    // FAKE: a fire reports nothing back; the consumers that react to it record no summary.
    if fake_result(name) == FAIL {
        return FAKE_FAIL_NOTE.to_string();
    }
    let rows = controlplane::fake_hash("scheduler.note", name) % 500;
    format!("{rows} rows affected")
}

/// The dashboard's "Scheduler runs" attention contribution.
pub(crate) const ATTENTION_RUNS: &str = "attention.runs";

// FAKE: the scheduler stores no run failures.
pub(crate) fn attention_runs() -> Vec<AttentionItem> {
    vec![AttentionItem {
        severity: Severity::Warn,
        title: "storage.gc failed at 03:30".to_string(),
        text: "timeout: storage-svc deadline exceeded.".to_string(),
        link: Link::Page { module: MODULE.to_string(), page: PAGE.to_string(), params: Default::default() },
        action_label: "Logs".to_string(),
        since: "03:30".to_string(),
    }]
}

fn result_tone(result: &str) -> Tone {
    if result == OK {
        Tone::Ok
    } else {
        Tone::Err
    }
}

fn last_cell(row: &ScheduleRow) -> Cell {
    let Some(at) = &row.last_at else {
        return cell(CellKind::Muted, NEVER);
    };
    let result = fake_result(&row.name);
    Cell {
        tone: Some(result_tone(result)),
        row: true,
        more: vec![cell(CellKind::Muted, at.as_str())],
        ..cell(CellKind::Badge, result)
    }
}

fn schedule_row(row: &ScheduleRow) -> Row {
    Row {
        entity: Some(EntityRef {
            kind: SCHEDULE_KIND.to_string(),
            id: row.name.clone(),
        }),
        cells: vec![
            cell(CellKind::Mono, row.name.as_str()),
            cell(CellKind::Mono, every(row.interval_seconds)),
            cell(CellKind::Ref, fake_target(&row.name)),
            cell(CellKind::Dim, row.next_fire.as_deref().unwrap_or(NEVER)),
            last_cell(row),
            cell(CellKind::Toggle, fake_enabled(&row.name).to_string()),
        ],
        highlight: false,
        item: None,
        actions: Vec::new(),
    }
}

/// The Schedules body: at most [`MAX_ROWS`] of `rows` (read one past it), the count of all
/// schedules as its meta, a foot when more exist, and a warning while this process's loop is off.
pub(crate) fn schedules_body(rows: &[ScheduleRow], total: i64, enabled: bool) -> PanelBody {
    let drawn = &rows[..rows.len().min(MAX_ROWS)];
    let mut body = PanelBody::new(Primitive::DataTable(DataTable {
        columns: vec![
            column("id", "Id", CellKind::Mono),
            column("when", "When", CellKind::Mono),
            column("target", "Target", CellKind::Ref),
            column("next", "Next fire", CellKind::Dim),
            column("last", "Last", CellKind::Badge),
            column("enabled", "Enabled", CellKind::Toggle),
        ],
        rows: drawn.iter().map(schedule_row).collect(),
        page: PageInfo {
            next_cursor: None,
            size: MAX_ROWS as u32,
            total: None,
        },
        filters: Vec::new(),
        search_placeholder: None,
        selectable: false,
        list_columns_for: None,
        list_columns_at: 0,
        item_noun: Some(Noun::new("schedule", "schedules")),
    }));
    body.meta = Some(total.to_string());
    if rows.len() > MAX_ROWS {
        body.foot = Some(format!("first {MAX_ROWS} schedules"));
    }
    if !enabled {
        body.banner = Some(Banner {
            tone: Tone::Warn,
            text: DISABLED_BANNER.to_string(),
        });
    }
    body
}

fn fire_entry(row: &FireRow) -> TimelineEntry {
    let result = fake_result(&row.name);
    let failed = result == FAIL;
    TimelineEntry {
        at: row.at.clone(),
        actor: None,
        title: None,
        verb: row.name.clone(),
        target: None,
        summary: String::new(),
        badges: if failed {
            vec![Cell {
                tone: Some(Tone::Err),
                ..cell(CellKind::Badge, "failed")
            }]
        } else {
            Vec::new()
        },
        detail: Some(format!("{} · {}", fake_duration(&row.name), fake_note(&row.name))),
        icon: if failed { "alert" } else { "check" }.to_string(),
        tone: Some(result_tone(result)),
    }
}

/// The Recent fires body: one entry per fire of the last 24 hours, newest first.
pub(crate) fn fires_body(rows: &[FireRow]) -> PanelBody {
    let mut body = PanelBody::new(Primitive::Timeline(Timeline {
        entries: rows.iter().map(fire_entry).collect(),
        cursor_before: None,
        live: false,
        days: false,
        date_only: false,
    }));
    body.tag = Some(FIRES_WINDOW.to_string());
    body
}

fn contribution(id: &str, order: i32, title: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point: Point::Page(PAGE.to_string()),
        order,
        title: title.to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
        readonly: None,
    }
}

/// A disabled schedule action: none opens a form or confirm, so each is `Then::Invoke`.
fn disabled_action(id: &str, point: ActionPoint, label: &str, icon: &str, order: i32) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point,
        label: label.to_string(),
        icon: icon.to_string(),
        order,
        danger: false,
        multi: false,
        min_role: Role::Operator,
        then: Then::Invoke,
        implemented: false,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

pub(crate) fn manifest() -> Manifest {
    let row = || ActionPoint::RowAction(SCHEDULE_KIND.to_string());
    let tool = ActionPoint::PanelTool(SCHEDULES.to_string());
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: MODULE.to_string(),
            label: "Scheduler".to_string(),
            icon: "clock".to_string(),
            order: order::platform_nav::SCHEDULER,
            page: PAGE.to_string(),
            counter: None,
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Scheduler".to_string(),
            subtitle: SUBTITLE.to_string(),
            head_actions: Vec::new(),
            tiles: Vec::new(),
            layout: Layout::Stack(vec![SCHEDULES.to_string(), FIRES.to_string()]),
            params: Vec::new(),
        }],
        entity_kinds: vec![EntityKindDecl {
            kind: SCHEDULE_KIND.to_string(),
            label: "Schedule".to_string(),
            icon: "clock".to_string(),
        }],
        contributions: vec![
            contribution(SCHEDULES, 10, "Schedules"),
            contribution(FIRES, 20, "Recent fires"),
            ContributionDecl {
                point: Point::DashboardAttention,
                icon: "clock".to_string(),
                ..contribution(ATTENTION_RUNS, order::dashboard_attention::SCHEDULER, "Scheduler runs")
            },
        ],
        // NEW: the scheduler has no write path; schedules are seeded by DDL or inserted by hand.
        actions: vec![
            ActionDecl {
                when_empty: true,
                ..disabled_action(SCHEDULE_CREATE, tool, "Schedule", "plus", 10)
            },
            disabled_action(SCHEDULE_FIRE, row(), "Fire now", "zap", 10),
            disabled_action(SCHEDULE_EDIT, row(), "Edit…", "edit", 20),
            ActionDecl {
                danger: true,
                ..disabled_action(SCHEDULE_DELETE, row(), "Delete…", "trash", 30)
            },
        ],
        search: Vec::new(),
        writes: false,
    }
}

#[async_trait]
impl Provider for SchedulerAdmin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, Error> {
        match req.contribution.as_str() {
            SCHEDULES => self.schedules().await,
            FIRES => self.fires().await,
            ATTENTION_RUNS => Ok(PanelBody::new(Primitive::AttentionItems(attention_runs()))),
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        match req.action.as_str() {
            SCHEDULE_CREATE | SCHEDULE_FIRE | SCHEDULE_EDIT | SCHEDULE_DELETE => {
                Err(Error::forbidden("not implemented yet"))
            }
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

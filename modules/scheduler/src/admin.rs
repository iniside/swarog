//! The `scheduler` control-plane provider: the Scheduler page (the Schedules table with each
//! schedule's interval, next fire and last fire, and the Recent fires timeline of the last 24
//! hours) and its one write, Fire now: an immediate fire of the row's schedule through
//! [`crate::fire`] with [`Due::Forced`], so it stamps `last_fired` and emits `scheduler.fired`
//! even when the schedule is not due, and a repeat fires again.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bus::Bus;
use controlplane::order;
use controlplane::{
    ActionDecl, ActionPoint, Align, Banner, Cell, CellKind, Column, ContributionDecl, DataTable, Emphasis,
    EntityKindDecl, EntityRef, InvokeCall, InvokeOutcome, Layout, Manifest, NavEntry, Noun, PageDecl, PageInfo,
    PanelBody, PanelQuery, Point, Primitive, Provider, Role, Row, SearchHit, SearchQuery, Then, Timeline,
    TimelineEntry, Tone, DEFAULT_PANEL_DEADLINE_MS,
};
use opsapi::Error;
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::store::{self, FireRow, ScheduleRow};
use crate::{fire, Due, FireOutcome};

const MODULE: &str = "scheduler";
pub(crate) const PAGE: &str = "schedules";
pub(crate) const SCHEDULE_KIND: &str = "schedule";

pub(crate) const SCHEDULES: &str = "schedules";
pub(crate) const FIRES: &str = "fires";

pub(crate) const SCHEDULE_FIRE: &str = "schedule.fire";

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
pub(crate) const DISABLED_REFUSAL: &str = "Scheduler disabled on this process: Fire now is refused";
pub(crate) const FIRE_BUSY: &str = "Another Fire now is running on this process; try again";

/// The `statement_timeout` of a Fire now. It bounds each statement, not the fire.
pub(crate) const FIRE_NOW_DEADLINE: Duration = Duration::from_secs(3);
/// The room a Fire now leaves under [`controlplane::INVOKE_DEADLINE`] for the `cp.invoke`
/// round trip in the split.
pub(crate) const FIRE_NOW_MARGIN: Duration = Duration::from_secs(2);
/// The whole Fire now (connect, lock, update, emit, commit, unlock), so it answers before
/// the portal's [`controlplane::INVOKE_DEADLINE`]. Cancelling the fire at this bound is
/// abort-safe: its dedicated connection closes (see the crate docs).
pub(crate) const FIRE_NOW_TOTAL: Duration = controlplane::INVOKE_DEADLINE.saturating_sub(FIRE_NOW_MARGIN);
// The margin is at least the minimum room for the `cp.invoke` QUIC round trip in the split,
// and the whole Fire now leaves room for at least one statement.
const _: () = assert!(FIRE_NOW_MARGIN.as_nanos() >= Duration::from_secs(1).as_nanos());
const _: () = assert!(FIRE_NOW_TOTAL.as_nanos() > FIRE_NOW_DEADLINE.as_nanos());

pub(crate) struct SchedulerAdmin {
    pool: PgPool,
    bus: Arc<Bus>,
    /// This process's `SCHEDULER_ENABLED`: when off, no schedule it lists fires here.
    enabled: bool,
    /// Held for one Fire now, so at most one runs beside the tick
    /// ([`crate::DEDICATED_FIRE_SESSIONS`]).
    firing: Mutex<()>,
}

impl SchedulerAdmin {
    pub(crate) fn new(pool: PgPool, bus: Arc<Bus>, enabled: bool) -> SchedulerAdmin {
        SchedulerAdmin { pool, bus, enabled, firing: Mutex::new(()) }
    }

    async fn fire_now(&self, req: &InvokeCall) -> Result<InvokeOutcome, Error> {
        let name = bound_schedule(req)?;
        if !self.enabled {
            return Err(Error::forbidden(DISABLED_REFUSAL));
        }
        let Ok(_firing) = self.firing.try_lock() else {
            return Err(Error::conflict(FIRE_BUSY));
        };
        let fire_now = fire(&self.pool, &self.bus, name, FIRE_NOW_DEADLINE, Due::Forced);
        let fired = tokio::time::timeout(FIRE_NOW_TOTAL, fire_now)
            .await
            .map_err(|_| {
                Error::unavailable(format!(
                    "Fire now of {name} did not finish in {} s; check last_fired",
                    FIRE_NOW_TOTAL.as_secs()
                ))
            })?;
        match fired.map_err(internal)? {
            FireOutcome::Fired => {
                Ok(InvokeOutcome::Ok { message: format!("Fired {name}"), reveal: None, redirect: None })
            }
            FireOutcome::LockHeld => Err(Error::conflict(format!("{name} is firing now; try again"))),
            FireOutcome::Missing => Err(Error::not_found(format!("no schedule {name:?}"))),
            FireOutcome::NotDue => Ok(InvokeOutcome::NoOp { message: format!("{name} is not due") }),
        }
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

/// Every input value is at most [`controlplane::MAX_INPUT_VALUE_BYTES`].
fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= controlplane::MAX_INPUT_VALUE_BYTES)
}

/// The schedule name of a row action: a non-empty `schedule` entity id.
fn bound_schedule(req: &InvokeCall) -> Result<&str, Error> {
    match &req.entity {
        Some(e) if e.kind == SCHEDULE_KIND && !e.id.is_empty() => Ok(&e.id),
        _ => Err(Error::invalid("scheduler: this action needs a schedule")),
    }
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

fn schedule_row(row: &ScheduleRow) -> Row {
    Row {
        entity: Some(EntityRef {
            kind: SCHEDULE_KIND.to_string(),
            id: row.name.clone(),
        }),
        cells: vec![
            cell(CellKind::Mono, row.name.as_str()),
            cell(CellKind::Mono, every(row.interval_seconds)),
            cell(CellKind::Dim, row.next_fire.as_deref().unwrap_or(NEVER)),
            cell(CellKind::Dim, row.last_at.as_deref().unwrap_or(NEVER)),
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
            column("next", "Next fire", CellKind::Dim),
            column("last", "Last", CellKind::Dim),
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
    TimelineEntry {
        at: row.at.clone(),
        actor: None,
        title: None,
        verb: row.name.clone(),
        target: None,
        summary: String::new(),
        badges: Vec::new(),
        detail: None,
        icon: "zap".to_string(),
        tone: None,
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

/// A schedule action that opens no form or confirm, so it is `Then::Invoke`.
fn invoke_action(id: &str, point: ActionPoint, label: &str, icon: &str, order: i32) -> ActionDecl {
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
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

pub(crate) fn manifest() -> Manifest {
    let row = || ActionPoint::RowAction(SCHEDULE_KIND.to_string());
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
        ],
        actions: vec![invoke_action(SCHEDULE_FIRE, row(), "Fire now", "zap", 10)],
        search: Vec::new(),
        writes: true,
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
            other => Err(Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, Error> {
        if !input_within_cap(&req.input) {
            return Err(Error::invalid(format!(
                "an input value is longer than {} bytes",
                controlplane::MAX_INPUT_VALUE_BYTES
            )));
        }
        match req.action.as_str() {
            SCHEDULE_FIRE => self.fire_now(&req).await,
            other => Err(Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

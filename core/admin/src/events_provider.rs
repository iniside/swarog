//! The `core.events` control-plane provider: the Event plane page, its dashboard tile,
//! attention items and nav counter, over `asyncevents::admin`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use asyncevents::admin::{self as plane, SkipResult, StateSnapshot, SubInfo, SubState};
use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Banner, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    ContributionDecl, DataTable, Delta, Emphasis, EntityHeader, EntityKindDecl, EntityRef, FilterDef, FilterKind,
    InvokeCall, InvokeOutcome, KpiTile, Layout, Link, Manifest, NavEntry, PageDecl, PageInfo, Palette, PanelBody,
    PanelQuery, Point, PrepareCall, Primitive, PropertyList, Provider, ReasonReq, Role, Row, SearchHit, SearchQuery,
    Severity, Then, Tone, Trend, DEFAULT_PANEL_DEADLINE_MS,
};
use sqlx::PgPool;

use crate::fakes;
use crate::render::{day_and_time, thousands};

pub(crate) const MODULE: &str = "core.events";
const PAGE: &str = "events";
const KIND: &str = "subscription";

/// The byte cap on every `InvokeCall.input` value this provider accepts.
pub(crate) const MAX_INPUT_VALUE_BYTES: usize = 4096;
/// The byte cap on a skip reason, which is stored in the subscription's `last_error`.
pub(crate) const MAX_REASON_BYTES: usize = 1024;
/// The payload bytes the poison preview shows.
const PAYLOAD_PREVIEW_BYTES: usize = 4096;
/// Paused subscriptions listed as attention items.
const ATTENTION_LIMIT: usize = 5;
/// The mockup's lag above which a lag cell turns to a warning.
const LAG_WARN: i64 = 100;

const SKIP_NEEDS_ITEM: &str = "Skip needs the previewed event";
const SKIP_BODY: &str = "The checkpoint advances past this event and the subscription resumes. The consumer never \
                         applies this event unless it is done by hand.";
const SKIP_REASON_HINT: &str = "e.g. malformed payload from a producer bug, compensated manually";
const POISON_HINT: &str = "Fix the consumer first, then Resume. Skip advances the checkpoint past this event.";
const NOTHING_TO_SKIP: &str = "Nothing to skip — the subscription is healthy";
const ALREADY_RESOLVED: &str =
    "Already resolved — the event was delivered or another event blocks now; refresh to see the current state";

const NAV_PAUSED: &str = "nav.paused";
const TILE_PAUSED: &str = "tile.paused";
const ATTENTION_PAUSED: &str = "attention.paused";
const KPI_HEAD: &str = "kpi.head";
const KPI_SUBSCRIPTIONS: &str = "kpi.subscriptions";
const KPI_MAX_LAG: &str = "kpi.max-lag";
const KPI_DEAD_LETTERED: &str = "kpi.dead-lettered";
const POISON: &str = "poison";
const SUBSCRIPTIONS: &str = "subscriptions";
const HEADER: &str = "subscription.header";
const DETAILS: &str = "subscription.details";

const POISON_RESUME: &str = "poison.resume";
const POISON_SKIP: &str = "poison.skip";
const POISON_COPY: &str = "poison.copy";
const SUB_PAUSE: &str = "sub.pause";
const SUB_RESUME: &str = "sub.resume";
const SUB_RETRY: &str = "sub.retry";
const SUB_REWIND: &str = "sub.rewind";

/// The table filter key; `state` is the portal's work-queue key.
const FILTER_STATE: &str = "sub_state";

pub(crate) struct EventsProvider {
    pool: PgPool,
}

impl EventsProvider {
    pub(crate) fn new(pool: PgPool) -> EventsProvider {
        EventsProvider { pool }
    }
}

/// The poison preview's `item`: the subscription and the event it previewed. One event
/// can block several subscriptions of its topic, so the event id alone is not enough.
fn poison_item(subscription: &str, event_id: &str) -> String {
    format!("{subscription}#{event_id}")
}

/// `(subscription, event_id)`; event ids are uuids, so the last `#` separates them.
fn parse_poison_item(item: &str) -> Option<(&str, &str)> {
    item.rsplit_once('#').filter(|(subscription, _)| !subscription.is_empty())
}

/// The name an operator reads and types for an event: the first 8 hex characters of its
/// uuid. The plane exposes no event sequence number.
fn short_id(event_id: &str) -> Option<&str> {
    event_id.get(..8).filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn event_name(topic: &str, short: &str) -> String {
    format!("{topic} #{short}")
}

/// `"Nov 17 14:02"` from Postgres timestamptz text, or the text unchanged if it does not
/// parse. The date is always shown: the provider reads no clock, so it cannot tell a
/// timestamp from today from an older one.
fn day_time(at: &str) -> String {
    match day_and_time(at) {
        (_, day, time) if !day.is_empty() => format!("{day} {time}"),
        _ => at.to_string(),
    }
}

/// `"since Nov 17 14:02"`, as [`day_time`].
fn since(at: &str) -> String {
    format!("since {}", day_time(at))
}

/// Every input value is at most [`MAX_INPUT_VALUE_BYTES`]. This provider declares no form
/// fields, so an accepted value is never read.
pub(crate) fn input_within_cap(input: &BTreeMap<String, String>) -> bool {
    input.values().all(|v| v.len() <= MAX_INPUT_VALUE_BYTES)
}

fn internal(error: anyhow::Error) -> opsapi::Error {
    opsapi::Error::internal(format!("{error:#}"))
}

/// A verb's [`plane::Retired`] refusal is `Invalid`; any other failure is internal.
fn refused(error: anyhow::Error) -> opsapi::Error {
    match error.downcast_ref::<plane::Retired>() {
        Some(retired) => opsapi::Error::invalid(retired.to_string()),
        None => internal(error),
    }
}

fn events_page() -> Link {
    Link::Page { module: MODULE.to_string(), page: PAGE.to_string(), params: BTreeMap::new() }
}

fn text(kind: CellKind, value: impl Into<String>, tone: Option<Tone>) -> Cell {
    Cell { kind, value: value.into(), tone, link: None, bar: None }
}

/// The mockup's lag bar fill (`pages-platform.js:129`): a third of the lag, between 3 and
/// 100 percent.
fn lag_bar(lag: i64) -> u8 {
    (lag / 3).clamp(3, 100) as u8
}

fn state_tone(state: &str, failures: i32) -> Option<Tone> {
    match state {
        "paused" => Some(Tone::Err),
        "active" if failures > 0 => Some(Tone::Warn),
        "active" => Some(Tone::Ok),
        _ => Some(Tone::Info),
    }
}

fn lag_tone(sub: &SubInfo) -> Option<Tone> {
    match (sub.state.as_str(), sub.lag_events) {
        ("paused", _) => Some(Tone::Err),
        (_, lag) if lag > LAG_WARN => Some(Tone::Warn),
        _ => None,
    }
}

fn state_cell(state: &str, failures: i32) -> Cell {
    text(CellKind::Badge, state, state_tone(state, failures))
}

fn prop(label: &str, cell: Cell) -> (String, Cell) {
    (label.to_string(), cell)
}

fn done(message: String) -> InvokeOutcome {
    InvokeOutcome::Ok { message, reveal: None, redirect: None }
}

fn noop(message: impl Into<String>) -> InvokeOutcome {
    InvokeOutcome::NoOp { message: message.into() }
}

fn unchanged(id: &str, after: &StateSnapshot) -> InvokeOutcome {
    noop(format!("Nothing changed — {id} is already {}", after.state))
}

fn count(n: i64) -> String {
    thousands(n.max(0) as u64)
}

fn contribution(id: &str, point: Point, order: i32, title: &str, icon: &str) -> ContributionDecl {
    ContributionDecl {
        id: id.to_string(),
        point,
        order,
        title: title.to_string(),
        icon: icon.to_string(),
        min_role: Role::Viewer,
        cache_ttl_s: Some(60),
        deadline_ms: DEFAULT_PANEL_DEADLINE_MS,
    }
}

struct Act {
    id: &'static str,
    point: ActionPoint,
    label: &'static str,
    icon: &'static str,
    order: i32,
    then: Then,
}

fn act(id: &'static str, point: ActionPoint, label: &'static str, icon: &'static str, order: i32, then: Then) -> Act {
    Act { id, point, label, icon, order, then }
}

impl Act {
    fn decl(self, danger: bool, emphasis: Emphasis, implemented: bool) -> ActionDecl {
        ActionDecl {
            id: self.id.to_string(),
            point: self.point,
            label: self.label.to_string(),
            icon: self.icon.to_string(),
            order: self.order,
            danger,
            multi: false,
            min_role: Role::Operator,
            then: self.then,
            implemented,
            emphasis,
            trailing_icon: false,
            prepare: false,
        }
    }
}

fn manifest() -> Manifest {
    let page = || Point::Page(PAGE.to_string());
    let tool = || ActionPoint::PanelTool(POISON.to_string());
    let row = || ActionPoint::RowAction(KIND.to_string());
    // Fixes only the variant: the shown spec comes from `prepare` for the previewed event.
    let skip = ConfirmSpec {
        title: "Skip the previewed event?".to_string(),
        body: SKIP_BODY.to_string(),
        tone: ConfirmTone::Danger,
        impact: Vec::new(),
        reason: ReasonReq::Required,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Skip and resume".to_string(),
    };
    let pause = ConfirmSpec {
        title: "Pause this subscription?".to_string(),
        body: "Delivery stops in every process until it is resumed; events keep arriving behind the \
               checkpoint."
            .to_string(),
        tone: ConfirmTone::Warn,
        impact: Vec::new(),
        reason: ReasonReq::None,
        reason_hint: None,
        type_to_confirm: None,
        confirm_label: "Pause".to_string(),
    };
    Manifest {
        module: MODULE.to_string(),
        sections: Vec::new(),
        entries: vec![NavEntry {
            section: "platform".to_string(),
            id: PAGE.to_string(),
            label: "Event plane".to_string(),
            icon: "zap".to_string(),
            order: 60,
            page: PAGE.to_string(),
            counter: Some(NAV_PAUSED.to_string()),
        }],
        pages: vec![PageDecl {
            id: PAGE.to_string(),
            title: "Event plane".to_string(),
            subtitle: "Durable topics, subscription checkpoints and lag.".to_string(),
            head_actions: Vec::new(),
            tiles: [KPI_HEAD, KPI_SUBSCRIPTIONS, KPI_MAX_LAG, KPI_DEAD_LETTERED].map(str::to_string).to_vec(),
            layout: Layout::Stack([POISON, SUBSCRIPTIONS].map(str::to_string).to_vec()),
        }],
        entity_kinds: vec![EntityKindDecl {
            kind: KIND.to_string(),
            label: "Subscription".to_string(),
            icon: "zap".to_string(),
        }],
        contributions: vec![
            contribution(NAV_PAUSED, Point::NavCounter, 0, "Paused subscriptions", "zap"),
            contribution(TILE_PAUSED, Point::DashboardTile, 60, "Paused subscriptions", "zap"),
            contribution(ATTENTION_PAUSED, Point::DashboardAttention, 20, "Paused subscriptions", "zap"),
            contribution(KPI_HEAD, page(), 10, "Head sequence", "activity"),
            contribution(KPI_SUBSCRIPTIONS, page(), 20, "Subscriptions", "zap"),
            contribution(KPI_MAX_LAG, page(), 30, "Max lag", "chart"),
            contribution(KPI_DEAD_LETTERED, page(), 40, "Dead-lettered (7 d)", "inbox"),
            contribution(POISON, page(), 50, "Paused:", "alert"),
            contribution(SUBSCRIPTIONS, page(), 60, "Subscriptions", "zap"),
            contribution(HEADER, Point::EntityHeader(KIND.to_string()), 0, "Subscription", "zap"),
            contribution(
                DETAILS,
                Point::EntityTab {
                    kind: KIND.to_string(),
                    tab: "details".to_string(),
                    tab_label: "Details".to_string(),
                },
                0,
                "Delivery state",
                "zap",
            ),
        ],
        actions: vec![
            act(POISON_RESUME, tool(), "Resume (retry event)", "play", 10, Then::Invoke)
                .decl(false, Emphasis::Primary, true),
            ActionDecl {
                prepare: true,
                ..act(POISON_SKIP, tool(), "Skip event…", "skip", 20, Then::Confirm(skip))
                    .decl(false, Emphasis::Default, true)
            },
            // NEW: copying to the clipboard is a client action no backend call performs.
            act(POISON_COPY, tool(), "Copy payload", "download", 30, Then::Invoke).decl(false, Emphasis::Ghost, false),
            act(SUB_PAUSE, row(), "Pause…", "pause", 10, Then::Confirm(pause)).decl(false, Emphasis::Default, true),
            act(SUB_RESUME, row(), "Resume", "play", 20, Then::Invoke).decl(false, Emphasis::Default, true),
            act(SUB_RETRY, row(), "Retry now", "refresh", 30, Then::Invoke).decl(false, Emphasis::Default, true),
            // NEW: no checkpoint rewind exists.
            act(SUB_REWIND, row(), "Rewind checkpoint…", "left", 40, Then::Invoke)
                .decl(true, Emphasis::Default, false),
        ],
        search: Vec::new(),
        writes: true,
    }
}

impl EventsProvider {
    async fn infos(&self) -> Result<Vec<SubInfo>, opsapi::Error> {
        plane::info(&self.pool).await.map_err(internal)
    }

    async fn states(&self) -> Result<Vec<SubState>, opsapi::Error> {
        plane::states(&self.pool).await.map_err(internal)
    }

    /// The subscription's current state; `NotFound` for an unknown id, which the plane's
    /// verbs report only as an untyped error.
    async fn state_of(&self, id: &str) -> Result<SubState, opsapi::Error> {
        self.states()
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| opsapi::Error::not_found(format!("no subscription {id:?}")))
    }

    async fn nav_counter(&self) -> Result<Primitive, opsapi::Error> {
        let paused = self.states().await?.iter().filter(|s| s.state == "paused").count();
        if paused == 0 {
            return Ok(Primitive::Empty { title: "No paused subscriptions".to_string(), detail: String::new() });
        }
        Ok(Primitive::Cell(text(CellKind::Badge, format!("{paused} paused"), Some(Tone::Err))))
    }

    async fn paused_tile(&self) -> Result<Primitive, opsapi::Error> {
        let states = self.states().await?;
        let paused: Vec<&SubState> = states.iter().filter(|s| s.state == "paused").collect();
        Ok(Primitive::KpiTile(KpiTile {
            label: "Paused subscriptions".to_string(),
            value: paused.len().to_string(),
            unit: Some(format!("/ {}", states.len())),
            delta: paused.first().map(|s| Delta { value: s.id.clone(), period: String::new(), good: Trend::None }),
            series: Vec::new(),
            spark: Palette::Chart1,
            capacity: None,
            link: Some(events_page()),
        }))
    }

    async fn attention(&self) -> Result<Primitive, opsapi::Error> {
        let items = self
            .infos()
            .await?
            .into_iter()
            .filter(|s| s.state == "paused")
            .take(ATTENTION_LIMIT)
            .map(|s| AttentionItem {
                severity: Severity::Err,
                title: if s.consecutive_failures > 0 {
                    "Subscription paused by poison event".to_string()
                } else {
                    "Subscription paused".to_string()
                },
                text: format!("{} stopped · {} behind.", s.id, count(s.lag_events)),
                link: events_page(),
                action_label: "Resolve".to_string(),
                since: s.paused_since.as_deref().map(day_time).unwrap_or_default(),
            })
            .collect();
        Ok(Primitive::AttentionItems(items))
    }

    async fn subscriptions_kpi(&self) -> Result<Primitive, opsapi::Error> {
        let states = self.states().await?;
        let by_state = ["active", "paused", "retired"]
            .map(|st| format!("{} {st}", states.iter().filter(|s| s.state == st).count()))
            .join(" · ");
        Ok(Primitive::KpiTile(KpiTile {
            label: "Subscriptions".to_string(),
            value: states.len().to_string(),
            unit: None,
            delta: Some(Delta { value: String::new(), period: by_state, good: Trend::None }),
            series: Vec::new(),
            spark: Palette::Chart1,
            capacity: None,
            link: None,
        }))
    }

    async fn max_lag_kpi(&self) -> Result<Primitive, opsapi::Error> {
        let infos = self.infos().await?;
        let top = infos.iter().max_by_key(|s| s.lag_events);
        Ok(Primitive::KpiTile(KpiTile {
            label: "Max lag".to_string(),
            value: count(top.map_or(0, |s| s.lag_events)),
            unit: None,
            delta: top.filter(|s| s.lag_events > 0).map(|s| Delta {
                value: format!("{} ({})", s.id, s.state),
                period: String::new(),
                good: Trend::None,
            }),
            series: Vec::new(),
            spark: Palette::Chart1,
            capacity: None,
            link: None,
        }))
    }

    async fn table(&self, req: &PanelQuery) -> Result<Primitive, opsapi::Error> {
        let needle = req.search.as_deref().unwrap_or("").to_lowercase();
        let state = req.params.get(FILTER_STATE).filter(|s| !s.is_empty() && *s != "any");
        let rows: Vec<Row> = self
            .infos()
            .await?
            .into_iter()
            .filter(|s| state.is_none_or(|st| &s.state == st))
            .filter(|s| {
                needle.is_empty() || s.id.to_lowercase().contains(&needle) || s.topic.to_lowercase().contains(&needle)
            })
            .map(|s| {
                let checkpoint = fakes::checkpoint(&s.id);
                let head = checkpoint + s.lag_events.max(0) as u64;
                Row {
                    entity: Some(EntityRef { kind: KIND.to_string(), id: s.id.clone() }),
                    cells: vec![
                        text(CellKind::Mono, s.id.clone(), None),
                        text(CellKind::Mono, format!("{} v{}", s.topic, s.version), None),
                        text(CellKind::Text, fakes::consumer(&s.id), None),
                        text(CellKind::Number, thousands(checkpoint), None),
                        text(CellKind::Number, thousands(head), None),
                        Cell {
                            bar: Some(lag_bar(s.lag_events)),
                            ..text(CellKind::Number, count(s.lag_events), lag_tone(&s))
                        },
                        state_cell(&s.state, s.consecutive_failures),
                    ],
                    highlight: s.state == "paused",
                }
            })
            .collect();
        let column = |key: &str, label: &str, kind: CellKind, align: Align| Column {
            key: key.to_string(),
            label: label.to_string(),
            kind,
            sortable: false,
            align,
        };
        Ok(Primitive::DataTable(DataTable {
            columns: vec![
                column("subscription", "Subscription", CellKind::Mono, Align::Left),
                column("topic", "Topic", CellKind::Mono, Align::Left),
                column("consumer", "Consumer", CellKind::Text, Align::Left),
                column("checkpoint", "Checkpoint", CellKind::Number, Align::Right),
                column("head", "Head", CellKind::Number, Align::Right),
                column("lag", "Lag", CellKind::Number, Align::Left),
                column("state", "State", CellKind::Badge, Align::Left),
            ],
            page: PageInfo { next_cursor: None, size: rows.len() as u32, total: Some(rows.len() as u64) },
            rows,
            filters: vec![FilterDef {
                key: FILTER_STATE.to_string(),
                label: "State".to_string(),
                kind: FilterKind::Enum(["active", "paused", "retired"].map(str::to_string).to_vec()),
            }],
            search_placeholder: Some("Subscription id or topic".to_string()),
            selectable: false,
            list_columns_for: None,
        }))
    }

    async fn poison(&self) -> Result<PanelBody, opsapi::Error> {
        let infos = self.infos().await?;
        let Some(sub) = infos.iter().find(|s| s.state == "paused") else {
            return Ok(PanelBody::new(Primitive::Empty {
                title: "Nothing is paused".to_string(),
                detail: "No subscription is paused.".to_string(),
            }));
        };
        let event = plane::poison_event(&self.pool, &sub.id).await.map_err(internal)?;
        let mut items = Vec::new();
        match &event {
            Some(ev) => {
                let name = event_name(&ev.topic, short_id(&ev.event_id).unwrap_or(&ev.event_id));
                items.push(prop("Event", text(CellKind::Mono, name, None)));
            }
            None => {
                let none = "No event past the checkpoint is eligible yet";
                items.push(prop("Event", text(CellKind::Text, none, None)));
            }
        }
        let consumer = format!("{} · handler {}", fakes::consumer(&sub.id), fakes::handler(&sub.id));
        items.push(prop("Consumer", text(CellKind::Text, consumer, None)));
        if let Some(error) = &sub.last_error {
            items.push(prop("Error", text(CellKind::Mono, error.clone(), Some(Tone::Err))));
        }
        let waiting = count(sub.lag_events);
        let banner = match sub.consecutive_failures {
            0 => Banner {
                tone: Tone::Warn,
                text: format!("Delivery is paused. {waiting} events are waiting behind it."),
            },
            failures => Banner {
                tone: Tone::Err,
                text: format!(
                    "Consumer failed {failures}× on the same event and stopped to preserve ordering. {waiting} \
                     events are waiting behind it."
                ),
            },
        };
        let event_id = event.as_ref().map_or("", |ev| ev.event_id.as_str());
        let code = event.as_ref().map(|ev| preview(serde_json::to_string_pretty(&ev.payload).unwrap_or_default()));
        Ok(PanelBody {
            primitive: Primitive::PropertyList(PropertyList {
                items,
                columns: 1,
                item: Some(poison_item(&sub.id, event_id)),
                code,
            }),
            meta: sub.paused_since.as_deref().map(since),
            heading: Some(sub.id.clone()),
            note: Some(POISON_HINT.to_string()),
            banner: Some(banner),
        })
    }

    /// The skip confirmation for the previewed event in `req.item`. The topic is fixed per
    /// subscription, so reading it does not substitute the event the operator saw.
    async fn prepare_skip(&self, req: &PrepareCall) -> Result<Then, opsapi::Error> {
        let (id, short) = req
            .item
            .as_deref()
            .and_then(parse_poison_item)
            .and_then(|(id, event_id)| Some((id, short_id(event_id)?)))
            .ok_or_else(|| opsapi::Error::invalid(SKIP_NEEDS_ITEM))?;
        let topic = self
            .infos()
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.topic)
            .ok_or_else(|| opsapi::Error::not_found(format!("no subscription {id:?}")))?;
        Ok(Then::Confirm(ConfirmSpec {
            title: format!("Skip event {short}?"),
            body: SKIP_BODY.to_string(),
            tone: ConfirmTone::Danger,
            impact: vec![
                prop("Subscription", text(CellKind::Mono, id, None)),
                prop("Event", text(CellKind::Mono, event_name(&topic, short), None)),
            ],
            reason: ReasonReq::Required,
            reason_hint: Some(SKIP_REASON_HINT.to_string()),
            type_to_confirm: Some(short.to_string()),
            confirm_label: "Skip and resume".to_string(),
        }))
    }

    async fn header(&self, entity: &EntityRef) -> Result<Primitive, opsapi::Error> {
        let sub = self.state_of(&entity.id).await?;
        let snap = plane::snapshot(&self.pool, &sub.id).await.map_err(internal)?;
        Ok(Primitive::EntityHeader(EntityHeader {
            title: sub.id.clone(),
            subtitle: format!("checkpoint {}", snap.cursor),
            status: Some(state_cell(&snap.state, snap.consecutive_failures)),
            avatar_seed: sub.id,
        }))
    }

    async fn details(&self, entity: &EntityRef) -> Result<Primitive, opsapi::Error> {
        let sub = self.state_of(&entity.id).await?;
        let snap = plane::snapshot(&self.pool, &sub.id).await.map_err(internal)?;
        let none = || "—".to_string();
        let items = vec![
            prop("State", state_cell(&snap.state, snap.consecutive_failures)),
            prop("Checkpoint", text(CellKind::Mono, snap.cursor.clone(), None)),
            prop("Failures", text(CellKind::Number, snap.consecutive_failures.to_string(), None)),
            prop("Next attempt", text(CellKind::Text, snap.next_attempt_at.as_deref().map_or_else(none, day_time), None)),
            prop("Last error", text(CellKind::Mono, snap.last_error.clone().unwrap_or_else(none), None)),
            prop("Consumer", text(CellKind::Text, fakes::consumer(&sub.id), None)),
        ];
        Ok(Primitive::PropertyList(PropertyList { items, columns: 2, item: None, code: None }))
    }

    /// Runs one state-setting verb; an unchanged snapshot is a `NoOp`.
    async fn transition<F, Fut>(
        &self,
        req: &InvokeCall,
        verb: F,
        verb_done: &str,
    ) -> Result<InvokeOutcome, opsapi::Error>
    where
        F: FnOnce(PgPool, String) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<(StateSnapshot, StateSnapshot)>>,
    {
        let id = match &req.entity {
            Some(e) if e.kind == KIND => e.id.clone(),
            _ => return Err(opsapi::Error::invalid(format!("{} needs a {KIND} entity", req.action))),
        };
        self.state_of(&id).await?;
        let (before, after) = verb(self.pool.clone(), id.clone()).await.map_err(refused)?;
        if before == after {
            return Ok(unchanged(&id, &after));
        }
        Ok(done(format!("{verb_done} {id}")))
    }

    async fn resume_previewed(&self, req: &InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        let (id, _) = req
            .item
            .as_deref()
            .and_then(parse_poison_item)
            .ok_or_else(|| opsapi::Error::invalid("Resume needs the previewed subscription"))?;
        self.state_of(id).await?;
        let (before, after) = plane::resume_retry(&self.pool, id).await.map_err(refused)?;
        if before == after {
            return Ok(unchanged(id, &after));
        }
        Ok(done(format!("Resumed {id}; its failing event is retried")))
    }

    async fn skip_previewed(&self, req: &InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        let (id, event_id) = req
            .item
            .as_deref()
            .and_then(parse_poison_item)
            .filter(|(_, event_id)| !event_id.is_empty())
            .ok_or_else(|| opsapi::Error::invalid(SKIP_NEEDS_ITEM))?;
        let reason = req.reason.as_deref().unwrap_or("").trim();
        let reason_error = match reason.len() {
            0 => Some("a reason is required".to_string()),
            n if n > MAX_REASON_BYTES => Some(format!("at most {MAX_REASON_BYTES} bytes")),
            _ => None,
        };
        if let Some(error) = reason_error {
            return Ok(InvokeOutcome::FieldErrors(BTreeMap::from([(
                crate::engine::actions::FIELD_REASON.to_string(),
                error,
            )])));
        }
        self.state_of(id).await?;
        let note = format!("operator {}: {reason}", req.operator);
        match plane::skip(&self.pool, id, &note, Some(event_id)).await.map_err(refused)? {
            SkipResult::Skipped(outcome) => {
                Ok(done(format!("Skipped event {} on {id}; the subscription resumed", outcome.skipped_event_id)))
            }
            SkipResult::AlreadyResolved { healthy: true } => Ok(noop(NOTHING_TO_SKIP)),
            SkipResult::AlreadyResolved { healthy: false } => Ok(noop(ALREADY_RESOLVED)),
        }
    }

    async fn primitive(&self, req: &PanelQuery) -> Result<Primitive, opsapi::Error> {
        let entity = || {
            req.entity.as_ref().filter(|e| e.kind == KIND).ok_or_else(|| opsapi::Error::invalid("needs a subscription"))
        };
        match req.contribution.as_str() {
            NAV_PAUSED => self.nav_counter().await,
            TILE_PAUSED => self.paused_tile().await,
            ATTENTION_PAUSED => self.attention().await,
            KPI_HEAD => Ok(Primitive::KpiTile(KpiTile {
                label: "Head sequence".to_string(),
                value: fakes::HEAD_SEQUENCE.to_string(),
                unit: None,
                delta: Some(Delta { value: String::new(), period: fakes::HEAD_RATE.to_string(), good: Trend::None }),
                series: Vec::new(),
                spark: Palette::Chart1,
                capacity: None,
                link: None,
            })),
            KPI_SUBSCRIPTIONS => self.subscriptions_kpi().await,
            KPI_MAX_LAG => self.max_lag_kpi().await,
            KPI_DEAD_LETTERED => Ok(Primitive::KpiTile(KpiTile {
                label: "Dead-lettered (7 d)".to_string(),
                value: fakes::DEAD_LETTERED.to_string(),
                unit: None,
                delta: Some(Delta {
                    value: String::new(),
                    // FAKE: skipped events are not kept, so who skipped them is not either.
                    period: "all skipped by operator".to_string(),
                    good: Trend::None,
                }),
                series: Vec::new(),
                spark: Palette::Chart1,
                capacity: None,
                link: None,
            })),
            SUBSCRIPTIONS => self.table(req).await,
            HEADER => self.header(entity()?).await,
            DETAILS => self.details(entity()?).await,
            other => Err(opsapi::Error::not_found(format!("{MODULE} has no contribution {other:?}"))),
        }
    }
}

/// `payload` cut to [`PAYLOAD_PREVIEW_BYTES`] on a char boundary.
fn preview(mut payload: String) -> String {
    if payload.len() > PAYLOAD_PREVIEW_BYTES {
        let mut end = PAYLOAD_PREVIEW_BYTES;
        while !payload.is_char_boundary(end) {
            end -= 1;
        }
        payload.truncate(end);
        payload.push('…');
    }
    payload
}

#[async_trait]
impl Provider for EventsProvider {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    async fn panel(&self, req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        match req.contribution.as_str() {
            POISON => self.poison().await,
            _ => self.primitive(&req).await.map(PanelBody::new),
        }
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        if !input_within_cap(&req.input) {
            return Err(opsapi::Error::invalid(format!("an input value is longer than {MAX_INPUT_VALUE_BYTES} bytes")));
        }
        match req.action.as_str() {
            POISON_RESUME => self.resume_previewed(&req).await,
            POISON_SKIP => self.skip_previewed(&req).await,
            SUB_PAUSE => {
                let verb = |pool: PgPool, id: String| async move { plane::pause(&pool, &id).await };
                self.transition(&req, verb, "Paused").await
            }
            SUB_RESUME => {
                let verb = |pool: PgPool, id: String| async move { plane::resume_retry(&pool, &id).await };
                self.transition(&req, verb, "Resumed").await
            }
            SUB_RETRY => {
                let verb = |pool: PgPool, id: String| async move { plane::retry(&pool, &id).await };
                self.transition(&req, verb, "Retry scheduled for").await
            }
            POISON_COPY | SUB_REWIND => Err(opsapi::Error::forbidden("not implemented yet")),
            other => Err(opsapi::Error::not_found(format!("{MODULE} has no action {other:?}"))),
        }
    }

    async fn prepare(&self, req: PrepareCall) -> Result<Then, opsapi::Error> {
        match req.action.as_str() {
            POISON_SKIP => self.prepare_skip(&req).await,
            other => Err(opsapi::Error::not_found(format!("{MODULE} prepares no action {other:?}"))),
        }
    }
}

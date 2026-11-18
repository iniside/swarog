use controlplane::{ServiceState, Tone};
use serde::Serialize;
use super::*;

// ---- PropertyList ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PropsView {
    pub(crate) columns: u8,
    pub(crate) items: Vec<(String, CellView)>,
    pub(crate) code: Option<String>,
}

// ---- KpiTile ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct KpiView {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) unit: Option<String>,
    pub(crate) delta_value: Option<String>,
    /// `up`, `down`, `flat` or `bad-up` (`bundle.css:251-252`).
    pub(crate) delta_class: &'static str,
    pub(crate) delta_period: String,
    pub(crate) bar: Option<f64>,
    pub(crate) spark: Option<SparkView>,
    pub(crate) href: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SparkView {
    pub(crate) line: String,
    pub(crate) area: String,
    pub(crate) color: &'static str,
}

/// The delta's colour class (`bundle.css:251-252`) from `tone` alone, never from the
/// value's text.
fn delta_class(d: &controlplane::Delta) -> &'static str {
    if let Some(t) = &d.tone {
        seen::record(t);
    }
    match d.tone {
        Some(Tone::Ok) => "up",
        Some(Tone::Err) => "down",
        Some(Tone::Warn) => "bad-up",
        Some(Tone::Info | Tone::Accent) | None => "flat",
    }
}

pub(super) fn palette(p: controlplane::Palette) -> &'static str {
    seen::record(&p);
    match p {
        controlplane::Palette::Chart1 => "var(--chart-1)",
        controlplane::Palette::Chart2 => "var(--chart-2)",
        controlplane::Palette::Warn => "var(--warn)",
    }
}

/// The mockup's `spark` path (`bundle.js:170-175`).
fn spark(values: &[f64], color: controlplane::Palette) -> Option<SparkView> {
    if values.len() < 2 {
        return None;
    }
    let (w, h) = (160.0_f64, 26.0_f64);
    let max = values.iter().copied().fold(f64::MIN, f64::max);
    let min = values.iter().copied().fold(f64::MAX, f64::min);
    let span = if max - min == 0.0 { 1.0 } else { max - min };
    let step = w / (values.len() - 1) as f64;
    let mut line = String::new();
    for (i, v) in values.iter().enumerate() {
        let x = i as f64 * step;
        let y = h - 2.0 - ((v - min) / span) * (h - 4.0);
        line.push_str(&format!("{}{x:.1},{y:.1}", if i == 0 { 'M' } else { 'L' }));
    }
    let area = format!("{line}L{w},{h}L0,{h}Z");
    Some(SparkView { line, area, color: palette(color) })
}

pub(super) fn kpi(k: &controlplane::KpiTile) -> KpiView {
    KpiView {
        label: k.label.clone(),
        value: k.value.clone(),
        unit: k.unit.clone(),
        delta_value: k.delta.as_ref().map(|d| d.value.clone()),
        delta_class: k.delta.as_ref().map(delta_class).unwrap_or("flat"),
        delta_period: k.delta.as_ref().map(|d| d.period.clone()).unwrap_or_default(),
        bar: k.capacity.map(|(used, total)| if total > 0.0 { (used / total * 100.0).clamp(0.0, 100.0) } else { 0.0 }),
        spark: spark(&k.series, k.spark),
        href: k.link.as_ref().map(link_url),
    }
}

// ---- Timeline ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineItem {
    pub(crate) time: String,
    pub(crate) icon: String,
    pub(crate) tone: &'static str,
    pub(crate) actor: Option<CellView>,
    pub(crate) verb: String,
    pub(crate) summary: String,
    pub(crate) target: Option<CellView>,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineDay {
    pub(crate) label: String,
    pub(crate) items: Vec<TimelineItem>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TimelineView {
    pub(crate) days: Vec<TimelineDay>,
    pub(crate) older_url: Option<String>,
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `("2026-10-20", "Oct 20", "17:44")` from an RFC 3339 timestamp or Postgres timestamptz
/// text (`2026-10-20 17:44:05+01`); unparseable `at` gives `("", "", at)`.
pub(crate) fn day_and_time(at: &str) -> (String, String, String) {
    let date = at.get(0..10).unwrap_or("");
    let time = at.get(11..16).unwrap_or(at);
    let month = date.get(5..7).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m));
    let day = date.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, day) {
        (Some(m), Some(d)) => (date.to_string(), format!("{} {d}", MONTHS[m - 1]), time.to_string()),
        _ => (String::new(), String::new(), at.to_string()),
    }
}

pub(super) fn timeline(t: &controlplane::Timeline, ctx: &Ctx<'_>) -> TimelineView {
    let mut days: Vec<(String, TimelineDay)> = Vec::new();
    for e in &t.entries {
        let (key, label, time) = day_and_time(&e.at);
        let item = TimelineItem {
            time,
            icon: if e.icon.is_empty() { "activity".to_string() } else { e.icon.clone() },
            tone: e.tone.map(tone).unwrap_or(""),
            actor: e.actor.as_ref().map(ref_cell),
            verb: e.verb.clone(),
            summary: e.summary.clone(),
            target: e.target.as_ref().map(|t| CellView {
                kind: "text",
                value: entity_param(t),
                tone: "",
                href: Some(entity_url(t)),
                color: "",
                initials: String::new(),
                bar: None,
                bar_tone: "",
                more: Vec::new(),
                inner_link: false,
                row: false,
            }),
            detail: e.detail.clone(),
        };
        // An entry whose `at` does not parse (empty key) joins the current run, and a run of
        // only such entries takes the day of the first parsed entry after them.
        match days.last_mut() {
            Some((k, day)) if *k == key || key.is_empty() => day.items.push(item),
            Some((k, day)) if k.is_empty() => {
                *k = key;
                day.label = label;
                day.items.push(item);
            }
            _ => days.push((key, TimelineDay { label, items: vec![item] })),
        }
    }
    TimelineView {
        days: days.into_iter().map(|(_, d)| d).collect(),
        older_url: t.cursor_before.as_deref().map(|c| with_param(ctx.path, ctx.query, QUERY_BEFORE, Some(c))),
    }
}

// ---- ServiceStatus ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServiceView {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) running: bool,
    pub(crate) tone: &'static str,
    pub(crate) bars: Vec<&'static str>,
    pub(crate) value: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ServicesView {
    pub(crate) items: Vec<ServiceView>,
    pub(crate) columns: u8,
    pub(crate) value_label: Option<String>,
}

fn service_state(s: ServiceState) -> (&'static str, &'static str, &'static str) {
    seen::record(&s);
    match s {
        ServiceState::Running => ("running", "ok", ""),
        ServiceState::Degraded => ("degraded", "warn", "w"),
        ServiceState::Restarting => ("restarting", "warn", "w"),
        ServiceState::Crashed => ("crashed", "err", "e"),
        ServiceState::Down => ("down", "err", "e"),
    }
}

pub(crate) fn service_items(items: &[controlplane::ServiceItem]) -> Vec<ServiceView> {
    items
        .iter()
        .map(|s| {
            let (state, tone, _) = service_state(s.state);
            ServiceView {
                name: s.name.clone(),
                state: state.to_string(),
                running: s.state == ServiceState::Running,
                tone,
                bars: s.history.iter().map(|(_, st)| service_state(*st).2).collect(),
                value: s.value.clone(),
                note: s.note.clone(),
            }
        })
        .collect()
}

pub(super) fn services(s: &controlplane::ServiceStatus) -> ServicesView {
    ServicesView { items: service_items(&s.items), columns: s.columns, value_label: s.value_label.clone() }
}

// ---- TimeSeriesChart ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartDot {
    pub(crate) x: String,
    pub(crate) y: String,
    pub(crate) title: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartSeries {
    pub(crate) label: String,
    pub(crate) color: &'static str,
    pub(crate) path: String,
    pub(crate) dots: Vec<ChartDot>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartGrid {
    pub(crate) y: String,
    pub(crate) ty: String,
    pub(crate) left: String,
    pub(crate) right: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartLabel {
    pub(crate) x: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChartView {
    pub(crate) left: f64,
    pub(crate) right: f64,
    pub(crate) left_text_x: f64,
    pub(crate) right_text_x: f64,
    pub(crate) grid: Vec<ChartGrid>,
    pub(crate) labels: Vec<ChartLabel>,
    pub(crate) series: Vec<ChartSeries>,
}

const CHART_COLORS: [&str; 2] = ["var(--chart-1)", "var(--chart-2)"];


/// The mockup's `chart` geometry (`bundle.js:244-260`), with each axis's max computed.
pub(super) fn chart(c: &controlplane::TimeSeriesChart) -> ChartView {
    let (w, h, l, t, b) = (640.0_f64, 220.0_f64, 38.0_f64, 12.0_f64, 24.0_f64);
    let has_right = c.series.iter().any(|s| s.axis == controlplane::Axis::Right);
    let r = if has_right { 40.0 } else { 12.0 };
    let n = c.buckets.len().max(1);
    let step = if n > 1 { (w - l - r) / (n - 1) as f64 } else { 0.0 };
    let xs: Vec<f64> = (0..n).map(|i| l + i as f64 * step).collect();
    let axis_max = |axis: controlplane::Axis| {
        let m = c
            .series
            .iter()
            .filter(|s| s.axis == axis)
            .flat_map(|s| s.values.iter().copied())
            .fold(0.0_f64, f64::max);
        if m > 0.0 {
            m
        } else {
            1.0
        }
    };
    let (left_max, right_max) = (axis_max(controlplane::Axis::Left), axis_max(controlplane::Axis::Right));
    let y = |v: f64, max: f64| t + (1.0 - v / max) * (h - t - b);
    let series = c
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            seen::record(&s.axis);
            let max = if s.axis == controlplane::Axis::Right { right_max } else { left_max };
            let mut path = String::new();
            let mut dots = Vec::new();
            for (j, v) in s.values.iter().enumerate().take(xs.len()) {
                let (px, py) = (xs[j], y(*v, max));
                path.push_str(&format!("{}{px:.1},{py:.1}", if j == 0 { 'M' } else { 'L' }));
                dots.push(ChartDot {
                    x: format!("{px:.1}"),
                    y: format!("{py:.1}"),
                    title: format!("{}: {} {}", c.buckets.get(j).map(String::as_str).unwrap_or(""), fixed1(*v), s.unit),
                });
            }
            ChartSeries { label: s.label.clone(), color: CHART_COLORS[i % CHART_COLORS.len()], path, dots }
        })
        .collect();
    let grid = [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|f| ChartGrid {
            y: format!("{:.1}", t + (1.0 - f) * (h - t - b)),
            ty: format!("{:.1}", t + (1.0 - f) * (h - t - b) + 3.5),
            left: fixed1(left_max * f),
            right: has_right.then(|| fixed1(right_max * f)),
        })
        .collect();
    let labels = c
        .buckets
        .iter()
        .zip(&xs)
        .map(|(text, x)| ChartLabel { x: format!("{x:.1}"), text: text.clone() })
        .collect();
    ChartView { left: l, right: w - r, left_text_x: l - 6.0, right_text_x: w - r + 6.0, grid, labels, series }
}

// ---- AttentionItems / EntityHeader ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttentionRow {
    /// The `modal-icon` class: `danger` or `warn`.
    pub(crate) tone: &'static str,
    pub(crate) icon: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) href: String,
    pub(crate) action_label: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttentionView {
    pub(crate) items: Vec<AttentionRow>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HeaderView {
    pub(crate) title: String,
    pub(crate) crumb: String,
    pub(crate) subtitle: String,
    pub(crate) status: Option<CellView>,
    pub(crate) badges: Vec<CellView>,
    pub(crate) color: &'static str,
    pub(crate) initials: String,
}

//! The scheduler's page reads: the schedules with each one's next fire and last fire as the
//! page draws them, and the fires of the last 24 hours. Every time is computed in SQL from
//! `now()`; nothing here reads a Rust clock.

use std::sync::LazyLock;

use sqlx::PgPool;

use crate::{INTERVAL_SECONDS_CEILING, TIMESTAMP_MAX_EPOCH_SECONDS};

/// One schedule as the Schedules table draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScheduleRow {
    pub(crate) name: String,
    pub(crate) interval_seconds: i64,
    /// `due` once `last_fired + interval` has passed, else that time as UTC
    /// `YYYY-MM-DD HH24:MI`; `None` for a legacy row whose interval is out of range, or
    /// when that time lies past PostgreSQL's largest timestamp.
    pub(crate) next_fire: Option<String>,
    /// `last_fired` as UTC `HH24:MI`; `None` when the schedule never fired.
    pub(crate) last_at: Option<String>,
}

/// One fire of the last 24 hours: a schedule whose `last_fired` falls in that window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FireRow {
    pub(crate) name: String,
    /// RFC 3339, UTC.
    pub(crate) at: String,
}

/// The schedules page: the first `$1` by name and the count of all rows. One row's
/// arithmetic error fails the whole SELECT, so each step is guarded: the outer `CASE`
/// holds `DUE_SQL`'s bounds, so `make_interval` never sees a legacy out-of-range
/// interval; "due" is `DUE_SQL`'s interval comparison, which adds nothing to a
/// timestamp; and `last_fired + interval` is computed only when its epoch seconds fit
/// under [`TIMESTAMP_MAX_EPOCH_SECONDS`], else the next fire is `NULL`.
pub(crate) static SCHEDULES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        r#"SELECT name, interval_seconds,
       CASE WHEN interval_seconds BETWEEN 1 AND {INTERVAL_SECONDS_CEILING} THEN
           CASE WHEN now() - last_fired >= make_interval(secs => interval_seconds) THEN 'due'
                WHEN extract(epoch FROM last_fired) + interval_seconds <= {TIMESTAMP_MAX_EPOCH_SECONDS} THEN
                     to_char((last_fired + make_interval(secs => interval_seconds)) AT TIME ZONE 'UTC',
                             'YYYY-MM-DD HH24:MI')
           END
       END AS next_fire,
       CASE WHEN last_fired = to_timestamp(0) THEN NULL
            ELSE to_char(last_fired AT TIME ZONE 'UTC', 'HH24:MI')
       END AS last_at,
       count(*) OVER () AS total
  FROM scheduler.schedules
 ORDER BY name COLLATE "C"
 LIMIT $1"#
    )
});

const FIRES_SQL: &str = r#"SELECT name, to_char(last_fired AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS at
  FROM scheduler.schedules
 WHERE last_fired > now() - interval '24 hours' AND last_fired <= now()
 ORDER BY last_fired DESC, name COLLATE "C"
 LIMIT $1"#;

/// One `SCHEDULES_SQL` row: name, interval, next fire, last fire, count of all rows.
type ScheduleTuple = (String, i64, Option<String>, Option<String>, i64);

/// The first `limit` schedules by name, and how many schedules exist.
pub(crate) async fn schedules(pool: &PgPool, limit: i64) -> Result<(Vec<ScheduleRow>, i64), sqlx::Error> {
    let rows: Vec<ScheduleTuple> = sqlx::query_as(&SCHEDULES_SQL).bind(limit).fetch_all(pool).await?;
    let total = rows.first().map_or(0, |r| r.4);
    let rows = rows
        .into_iter()
        .map(|(name, interval_seconds, next_fire, last_at, _)| ScheduleRow {
            name,
            interval_seconds,
            next_fire,
            last_at,
        })
        .collect();
    Ok((rows, total))
}

/// The schedules that fired in the last 24 hours, newest first, at most `limit`.
pub(crate) async fn recent_fires(pool: &PgPool, limit: i64) -> Result<Vec<FireRow>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(FIRES_SQL).bind(limit).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(name, at)| FireRow { name, at }).collect())
}

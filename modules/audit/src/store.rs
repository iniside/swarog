//! The audit log readers behind the admin page: one page of `audit.log` rows with the
//! per-topic columns derived in SQL, its exact total, and the actor filter's options; and
//! behind the dashboard, the newest operator rows.
//! They read schema `audit` only (never `admin.users`): operator logins come from the
//! `admin.action` rows themselves. Payload keys are parsed in SQL (`payload->>'k'`), so
//! audit still imports no payload type; the key names are the contract crates' field
//! names (`api/*/events/src/lib.rs`, `controlplane::events::AdminAction`).

use sqlx::PgPool;

use crate::DURABLE_TOPICS;

/// Rows per page of the audit log table.
pub(crate) const PAGE_SIZE: i64 = 25;

/// The actor every row other than `admin.action` shows: the emitting module, not a person.
pub(crate) const SYSTEM_ACTOR: &str = "system";

/// What a derived column shows when its payload key is missing or empty, or the payload
/// is not a JSON object.
pub(crate) const MISSING: &str = "—";

/// How many distinct operator logins the actor filter offers.
const ACTOR_OPTIONS_LIMIT: i64 = 50;

/// How many entries the dashboard's "Recent operator activity" timeline shows.
pub(crate) const ACTIVITY_LIMIT: i64 = 7;

/// The topic filter's options: the logged topics, in [`DURABLE_TOPICS`] order. Static,
/// so the select costs no scan.
pub(crate) fn topic_options() -> &'static [&'static str] {
    DURABLE_TOPICS
}

/// The narrowing a page and its total share. `actor` matches the derived actor column
/// (`system` selects every non-operator row); `search` is a case-insensitive substring
/// of the derived target or detail.
#[derive(Clone, Debug, Default)]
pub(crate) struct LogFilter<'a> {
    pub topic: Option<&'a str>,
    pub actor: Option<&'a str>,
    pub search: Option<&'a str>,
}

/// The keyset position of a row: `at` in microseconds since the epoch, then `id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogCursor {
    pub at_us: i64,
    pub id: i64,
}

/// One audit log row as the table draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogRow {
    pub id: i64,
    pub at_us: i64,
    /// `HH:MM:SS` when the row's UTC day is today, else `Mon DD HH:MM` (UTC).
    pub time: String,
    /// The operator login of an `admin.action` row, else [`SYSTEM_ACTOR`].
    pub actor: String,
    pub topic: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

impl LogRow {
    pub(crate) fn cursor(&self) -> LogCursor {
        LogCursor { at_us: self.at_us, id: self.id }
    }

    /// True for an operator row (`admin.action`), whose actor is a login.
    pub(crate) fn is_operator(&self) -> bool {
        self.topic == "admin.action"
    }
}

/// One page, newest first; `next` is the cursor of the page after it, `None` on the last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogPage {
    pub rows: Vec<LogRow>,
    pub next: Option<LogCursor>,
}

/// One `admin.action` row as the dashboard timeline draws it; `actor`, `action`, `target`
/// and `detail` are the derived columns of [`LogRow`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityRow {
    /// RFC 3339, UTC, to the second.
    pub at: String,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

/// A payload key as text: [`MISSING`] when absent, empty, or the payload is not an
/// object (`->>` on an array or scalar is NULL, not an error).
fn field(key: &str) -> String {
    format!("coalesce(nullif(payload->>'{key}', ''), '{MISSING}')")
}

/// `<kind>:<payload key>`, or [`MISSING`] when the key is absent or empty.
fn subject(kind: &str, key: &str) -> String {
    format!("coalesce('{kind}:' || nullif(payload->>'{key}', ''), '{MISSING}')")
}

/// The derived columns of one `audit.log` row, in [`LogTuple`] order after `id`, `at_us`:
/// time, actor, topic, action, target, detail. One CASE on `topic` per column. An
/// `admin.action` row shows its own actor/action/target/detail; every other topic shows
/// [`SYSTEM_ACTOR`], the topic after its first dot, the payload subject as `kind:id`
/// (config: `namespace/key`) and a short summary. The config summary never includes
/// `value`. A topic not listed here (a future logged topic) shows [`MISSING`] target
/// and detail until it gets its own arm.
fn derived_columns() -> String {
    let wallet_delta = format!(
        "coalesce(CASE WHEN payload->>'delta' LIKE '-%' THEN payload->>'delta' \
         ELSE '+' || nullif(payload->>'delta', '') END, '{MISSING}')"
    );
    let actor = format!(
        "CASE WHEN topic = 'admin.action' THEN {} ELSE '{SYSTEM_ACTOR}' END",
        field("actor")
    );
    let action = format!(
        "CASE WHEN topic = 'admin.action' THEN {} \
         ELSE substr(topic, strpos(topic, '.') + 1) END",
        field("action")
    );
    let target = format!(
        "CASE topic \
         WHEN 'admin.action' THEN {admin} \
         WHEN 'character.created' THEN {character} \
         WHEN 'character.deleted' THEN {character} \
         WHEN 'player.registered' THEN {player} \
         WHEN 'player.promoted' THEN {player} \
         WHEN 'player.deleted' THEN {player} \
         WHEN 'config.changed' THEN coalesce(nullif(payload->>'namespace', '') || '/' || \
              nullif(payload->>'key', ''), '{MISSING}') \
         WHEN 'match.finished' THEN {match_} \
         WHEN 'wallet.changed' THEN {player} \
         WHEN 'friend.requested' THEN {edge} \
         WHEN 'friend.accepted' THEN {edge} \
         WHEN 'friend.removed' THEN {edge} \
         WHEN 'group.created' THEN {group} \
         WHEN 'group.member_joined' THEN {group} \
         WHEN 'group.member_left' THEN {group} \
         WHEN 'group.role_changed' THEN {group} \
         ELSE '{MISSING}' END",
        admin = field("target"),
        character = subject("character", "character_id"),
        player = subject("player", "player_id"),
        match_ = subject("match", "match_id"),
        edge = subject("friendship", "edge_id"),
        group = subject("group", "group_id"),
    );
    let detail = format!(
        "CASE topic \
         WHEN 'admin.action' THEN {admin} \
         WHEN 'character.created' THEN {name} || ' · ' || {class} \
         WHEN 'character.deleted' THEN {owner} \
         WHEN 'player.registered' THEN {display_name} || ' · ' || {provider} \
         WHEN 'player.promoted' THEN {from_provider} || ' → ' || {to_provider} \
         WHEN 'player.deleted' THEN {handle} \
         WHEN 'config.changed' THEN {operation} || ' · rev ' || {revision} \
         WHEN 'match.finished' THEN 'winner ' || {winner} || ' · loser ' || {loser} \
         WHEN 'wallet.changed' THEN {wallet_delta} || ' ' || {currency} || ' · ' || {reason} \
         WHEN 'friend.requested' THEN {requester_handle} || ' → ' || {addressee_handle} \
         WHEN 'friend.accepted' THEN {requester_handle} || ' → ' || {addressee_handle} \
         WHEN 'friend.removed' THEN {reason} || ' · ' || {actor_handle} || ' → ' || {other_handle} \
         WHEN 'group.created' THEN {name} || ' · ' || {join_policy} \
         WHEN 'group.member_joined' THEN {member} || ' · ' || {role} \
         WHEN 'group.member_left' THEN {member} || ' · ' || {reason} \
         WHEN 'group.role_changed' THEN {member} || ' → ' || {role} \
         ELSE '{MISSING}' END",
        admin = field("detail"),
        name = field("name"),
        class = field("class"),
        owner = subject("player", "player_id"),
        display_name = field("display_name"),
        provider = field("provider"),
        from_provider = field("from_provider"),
        to_provider = field("to_provider"),
        handle = field("handle"),
        operation = field("operation"),
        revision = field("revision"),
        winner = field("winner"),
        loser = field("loser"),
        currency = field("currency"),
        reason = field("reason"),
        requester_handle = field("requester_handle"),
        addressee_handle = field("addressee_handle"),
        actor_handle = field("actor_handle"),
        other_handle = field("other_handle"),
        join_policy = field("join_policy"),
        member = subject("player", "player_id"),
        role = field("role"),
    );
    format!(
        "CASE WHEN (at AT TIME ZONE 'UTC')::date = (now() AT TIME ZONE 'UTC')::date \
              THEN to_char(at AT TIME ZONE 'UTC', 'HH24:MI:SS') \
              ELSE to_char(at AT TIME ZONE 'UTC', 'Mon DD HH24:MI') END AS at_text, \
         {actor} AS actor, topic, {action} AS action, {target} AS target, {detail} AS detail"
    )
}

/// Every `audit.log` row with its derived columns, for a reader to filter and order.
fn derived_rows() -> String {
    format!(
        "SELECT id, (EXTRACT(EPOCH FROM at) * 1000000)::int8 AS at_us, at, {} \
           FROM audit.log",
        derived_columns()
    )
}

/// The filter clause over [`derived_rows`], binding `$1` topic, `$2` actor, `$3` an
/// `ILIKE` pattern. No index serves topic, actor or search (only `at` is indexed,
/// `audit.log_at_idx`): each is a scan of the retained rows (≤ `AUDIT_RETENTION_DAYS`).
const FILTER: &str = "($1::text IS NULL OR topic = $1) \
    AND ($2::text IS NULL OR actor = $2) \
    AND ($3::text IS NULL OR target ILIKE $3 OR detail ILIKE $3)";

type LogTuple = (i64, i64, String, String, String, String, String, String);

fn log_row((id, at_us, time, actor, topic, action, target, detail): LogTuple) -> LogRow {
    LogRow { id, at_us, time, actor, topic, action, target, detail }
}

/// The `ILIKE` pattern matching `needle` as a literal substring: `\`, `%` and `_` are
/// escaped (backslash is `ILIKE`'s default escape).
fn contains_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in needle.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// The audit log readers over the module's own pool.
pub(crate) struct Store {
    pub pool: PgPool,
}

impl Store {
    pub(crate) fn new(pool: PgPool) -> Store {
        Store { pool }
    }

    /// One page of rows matching `filter`, newest first, keyset on `(at, id)` DESC:
    /// `after` is the previous page's `next`. Reads `limit + 1` rows to know whether a
    /// next page exists.
    pub(crate) async fn log_page(
        &self,
        filter: &LogFilter<'_>,
        after: Option<LogCursor>,
        limit: i64,
    ) -> Result<LogPage, sqlx::Error> {
        let limit = limit.max(1);
        let sql = format!(
            "SELECT id, at_us, at_text, actor, topic, action, target, detail \
               FROM ({rows}) r \
              WHERE {FILTER} \
                AND ($4::int8 IS NULL \
                     OR (at, id) < (to_timestamp(0) + $4 * interval '1 microsecond', $5::int8)) \
              ORDER BY at DESC, id DESC \
              LIMIT $6",
            rows = derived_rows()
        );
        let mut rows: Vec<LogRow> = sqlx::query_as::<_, LogTuple>(&sql)
            .bind(filter.topic)
            .bind(filter.actor)
            .bind(filter.search.map(contains_pattern))
            .bind(after.map(|c| c.at_us))
            .bind(after.map(|c| c.id))
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(log_row)
            .collect();
        let next = if rows.len() as i64 > limit {
            rows.truncate(limit as usize);
            rows.last().map(LogRow::cursor)
        } else {
            None
        };
        Ok(LogPage { rows, next })
    }

    /// The number of rows matching `filter`. Exact `count(*)`, unindexed: nothing
    /// indexes `topic` or any payload key (only `at`, `audit.log_at_idx`), so this
    /// scans the retained rows; an index on `topic` and on `payload->>'actor'` would
    /// serve the topic and actor filters.
    pub(crate) async fn log_total(&self, filter: &LogFilter<'_>) -> Result<i64, sqlx::Error> {
        let sql = format!("SELECT count(*) FROM ({rows}) r WHERE {FILTER}", rows = derived_rows());
        sqlx::query_scalar(&sql)
            .bind(filter.topic)
            .bind(filter.actor)
            .bind(filter.search.map(contains_pattern))
            .fetch_one(&self.pool)
            .await
    }

    /// The actor filter's options: up to [`ACTOR_OPTIONS_LIMIT`] distinct operator logins
    /// from `admin.action` rows, sorted, then [`SYSTEM_ACTOR`]. Unindexed: no index on
    /// `topic` or `payload->>'actor'`, so this scans the retained rows.
    pub(crate) async fn actor_options(&self) -> Result<Vec<String>, sqlx::Error> {
        let mut actors: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT payload->>'actor' AS actor \
               FROM audit.log \
              WHERE topic = 'admin.action' AND nullif(payload->>'actor', '') IS NOT NULL \
              ORDER BY actor \
              LIMIT $1",
        )
        .bind(ACTOR_OPTIONS_LIMIT)
        .fetch_all(&self.pool)
        .await?;
        if !actors.iter().any(|a| a == SYSTEM_ACTOR) {
            actors.push(SYSTEM_ACTOR.to_string());
        }
        Ok(actors)
    }

    /// The newest `limit` operator rows (`admin.action`), newest first, over the whole
    /// retained ledger. Nothing indexes `topic` (only `at`, `audit.log_at_idx`), so the
    /// operator rows are found among the retained rows by a filter, not an index.
    pub(crate) async fn recent_operator_actions(&self, limit: i64) -> Result<Vec<ActivityRow>, sqlx::Error> {
        let sql = format!(
            "SELECT to_char(at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), \
                    actor, action, target, detail \
               FROM ({rows}) r \
              WHERE topic = 'admin.action' \
              ORDER BY at DESC, id DESC \
              LIMIT $1",
            rows = derived_rows()
        );
        let rows: Vec<(String, String, String, String, String)> =
            sqlx::query_as(&sql).bind(limit.max(1)).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(at, actor, action, target, detail)| ActivityRow { at, actor, action, target, detail })
            .collect())
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

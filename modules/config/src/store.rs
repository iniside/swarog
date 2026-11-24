//! The Config & flags page reads: the keys a key prefix matches, each with its revision, update
//! time and writer, and the one key the page selects with its newest revisions; and its two
//! single-row writes. Every time is
//! computed in SQL from `now()`; nothing here reads a Rust clock. Each write goes through the
//! `config.settings` triggers, which stamp the row and emit `config.changed`.

use sqlx::PgPool;

use crate::SET_WRITER;

/// The `config.setting_history` operation of a delete.
pub(crate) const OP_DELETE: &str = "delete";

/// The revisions [`Selected::revisions`] holds at most.
pub(crate) const MAX_REVISIONS: i64 = 10;

/// A row's `updated_at` as the Updated column draws it: UTC `HH24:MI` on today's UTC date,
/// else `Mon DD`.
pub(crate) const UPDATED_EXPR: &str = r#"CASE WHEN (updated_at AT TIME ZONE 'UTC')::date = (now() AT TIME ZONE 'UTC')::date
            THEN to_char(updated_at AT TIME ZONE 'UTC', 'HH24:MI')
            ELSE to_char(updated_at AT TIME ZONE 'UTC', 'Mon DD')
       END"#;

/// The writer of the current revision of the `config.settings` row `c`.
const WRITER_EXPR: &str = "(SELECT w.by FROM config.setting_history w \
     WHERE w.namespace = c.namespace AND w.key = c.key AND w.revision = c.revision)";

/// One `config.settings` row as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyRow {
    pub(crate) namespace: String,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) revision: i64,
    /// [`UPDATED_EXPR`].
    pub(crate) updated: String,
    /// The writer of `revision`; `None` when the history holds no row for it.
    pub(crate) by: Option<String>,
}

impl KeyRow {
    /// `namespace.key`: ids never hold a '.', so the display key splits back at its only one.
    pub(crate) fn display(&self) -> String {
        format!("{}.{}", self.namespace, self.key)
    }
}

/// The keys a prefix matches, with the count of all keys and the first key of all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyPage {
    pub(crate) rows: Vec<KeyRow>,
    pub(crate) total: i64,
    /// The display key of the first key by `namespace, key` in byte order; `None` with no keys.
    pub(crate) first: Option<String>,
}

/// One `config.setting_history` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    pub(crate) revision: i64,
    /// `None` for a delete.
    pub(crate) value: Option<String>,
    /// `insert`, `update` or `delete`.
    pub(crate) operation: String,
    pub(crate) by: String,
    /// RFC 3339 UTC.
    pub(crate) at: String,
}

/// The selected key, with its newest [`MAX_REVISIONS`] revisions, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) row: KeyRow,
    pub(crate) revisions: Vec<Revision>,
}

// count(*) is exact and scans config.settings: no index counts its rows.
static KEYS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"SELECT t.total, t.first, s.namespace, s.key, s.value, s.revision, s.updated, s.by
  FROM (SELECT count(*) AS total,
               (SELECT namespace || '.' || key FROM config.settings
                 ORDER BY namespace COLLATE "C", key COLLATE "C" LIMIT 1) AS first
          FROM config.settings) t
  LEFT JOIN LATERAL (
       SELECT namespace, key, value, revision, {UPDATED_EXPR} AS updated, {WRITER_EXPR} AS by
         FROM config.settings c
        WHERE $1::text IS NULL OR starts_with(namespace || '.' || key, $1::text)
        ORDER BY namespace COLLATE "C", key COLLATE "C"
        LIMIT $2) s ON true
 ORDER BY s.namespace COLLATE "C", s.key COLLATE "C""#
    )
});

static SELECTED_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"SELECT s.namespace, s.key, s.value, s.revision, s.updated, s.by,
       h.revision, h.value, h.operation, h.by, h.at
  FROM (SELECT namespace, key, value, revision, {UPDATED_EXPR} AS updated, {WRITER_EXPR} AS by
          FROM config.settings c
         WHERE $1::text IS NULL OR (namespace = $1::text AND key = $2::text)
         ORDER BY namespace COLLATE "C", key COLLATE "C"
         LIMIT 1) s
  LEFT JOIN LATERAL (
       SELECT revision, value, operation, by, to_char(at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS at
         FROM config.setting_history
        WHERE namespace = s.namespace AND key = s.key
        ORDER BY revision DESC
        LIMIT $3) h ON true
 ORDER BY h.revision DESC"#
    )
});

/// One `KEYS_SQL` row: count of all keys, first key, then the matched key's columns, all
/// `NULL` when nothing matches.
type KeysTuple =
    (i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>, Option<String>);

/// The first `limit` keys whose display key starts with `prefix` (every key when `None`) in
/// `namespace, key` byte order, the count of all keys and the first key of all, in one
/// statement.
pub(crate) async fn keys(pool: &PgPool, prefix: Option<&str>, limit: i64) -> Result<KeyPage, sqlx::Error> {
    let tuples: Vec<KeysTuple> = sqlx::query_as(&KEYS_SQL).bind(prefix).bind(limit).fetch_all(pool).await?;
    let total = tuples.first().map_or(0, |t| t.0);
    let first = tuples.first().and_then(|t| t.1.clone());
    let rows = tuples
        .into_iter()
        .filter_map(|(_, _, namespace, key, value, revision, updated, by)| {
            Some(KeyRow { namespace: namespace?, key: key?, value: value?, revision: revision?, updated: updated?, by })
        })
        .collect();
    Ok(KeyPage { rows, total, first })
}

/// Writes `value` to the key `(namespace, key)` as `by` if its revision is still `expected`;
/// the revision the write stamped, or `None` when no row matched.
pub(crate) async fn update(
    pool: &PgPool,
    namespace: &str,
    key: &str,
    value: &str,
    expected: i64,
    by: &str,
) -> anyhow::Result<Option<i64>> {
    crate::validate_ident(namespace, key)?;
    let stamped: Option<(i64,)> = sqlx::query_as(&format!(
        "UPDATE config.settings SET value = $3 FROM {SET_WRITER} \
         WHERE namespace = $1 AND key = $2 AND revision = $5 RETURNING revision"
    ))
    .bind(namespace)
    .bind(key)
    .bind(value)
    .bind(by)
    .bind(expected)
    .fetch_optional(pool)
    .await?;
    Ok(stamped.map(|(revision,)| revision))
}

/// Inserts the key `(namespace, key)` as `by`; `false` when it exists, which it leaves as it is.
pub(crate) async fn create(pool: &PgPool, namespace: &str, key: &str, value: &str, by: &str) -> anyhow::Result<bool> {
    crate::validate_ident(namespace, key)?;
    let done = sqlx::query(&format!(
        "INSERT INTO config.settings (namespace, key, value) SELECT $1::text, $2::text, $3::text FROM {SET_WRITER} \
         ON CONFLICT (namespace, key) DO NOTHING"
    ))
    .bind(namespace)
    .bind(key)
    .bind(value)
    .bind(by)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// One `SELECTED_SQL` row: the key's columns, then one revision's columns, all `NULL` when the
/// history holds none.
type SelectedTuple = (
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The key `(namespace, key)` names, or the first key of all when `None`; `None` when there is
/// no such key.
pub(crate) async fn selected(pool: &PgPool, id: Option<(&str, &str)>) -> Result<Option<Selected>, sqlx::Error> {
    let tuples: Vec<SelectedTuple> = sqlx::query_as(&SELECTED_SQL)
        .bind(id.map(|i| i.0))
        .bind(id.map(|i| i.1))
        .bind(MAX_REVISIONS)
        .fetch_all(pool)
        .await?;
    let Some(first) = tuples.first() else { return Ok(None) };
    let (namespace, key, value, revision, updated, by) =
        (first.0.clone(), first.1.clone(), first.2.clone(), first.3, first.4.clone(), first.5.clone());
    let revisions = tuples
        .into_iter()
        .filter_map(|(_, _, _, _, _, _, revision, value, operation, by, at)| {
            Some(Revision { revision: revision?, value, operation: operation?, by: by?, at: at? })
        })
        .collect();
    Ok(Some(Selected { row: KeyRow { namespace, key, value, revision, updated, by }, revisions }))
}

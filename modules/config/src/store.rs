//! The Config & flags page reads: the keys a key prefix matches, each with its revision and
//! update time, and the one key the page selects. Every time is computed in SQL from `now()`;
//! nothing here reads a Rust clock.

use sqlx::PgPool;

/// The furthest back, in days, [`Selected::at_days_back`] reaches.
pub(crate) const MAX_DAYS_BACK: i32 = 14;

/// A row's `updated_at` as the Updated column draws it: UTC `HH24:MI` on today's UTC date,
/// else `Mon DD`.
pub(crate) const UPDATED_EXPR: &str = r#"CASE WHEN (updated_at AT TIME ZONE 'UTC')::date = (now() AT TIME ZONE 'UTC')::date
            THEN to_char(updated_at AT TIME ZONE 'UTC', 'HH24:MI')
            ELSE to_char(updated_at AT TIME ZONE 'UTC', 'Mon DD')
       END"#;

/// One `config.settings` row as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyRow {
    pub(crate) namespace: String,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) revision: i64,
    /// [`UPDATED_EXPR`].
    pub(crate) updated: String,
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

/// The selected key, with the times its FAKE prior revisions are drawn at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) row: KeyRow,
    /// `updated_at` minus `d` days for each `d` in `0..=MAX_DAYS_BACK`, RFC 3339 UTC.
    pub(crate) at_days_back: Vec<String>,
}

// count(*) is exact and scans config.settings: no index counts its rows.
static KEYS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"SELECT t.total, t.first, s.namespace, s.key, s.value, s.revision, s.updated
  FROM (SELECT count(*) AS total,
               (SELECT namespace || '.' || key FROM config.settings
                 ORDER BY namespace COLLATE "C", key COLLATE "C" LIMIT 1) AS first
          FROM config.settings) t
  LEFT JOIN LATERAL (
       SELECT namespace, key, value, revision, {UPDATED_EXPR} AS updated
         FROM config.settings
        WHERE $1::text IS NULL OR starts_with(namespace || '.' || key, $1::text)
        ORDER BY namespace COLLATE "C", key COLLATE "C"
        LIMIT $2) s ON true
 ORDER BY s.namespace COLLATE "C", s.key COLLATE "C""#
    )
});

static SELECTED_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r#"SELECT namespace, key, value, revision, {UPDATED_EXPR} AS updated,
       ARRAY(SELECT to_char((updated_at - make_interval(days => d)) AT TIME ZONE 'UTC',
                            'YYYY-MM-DD"T"HH24:MI:SS"Z"')
               FROM generate_series(0, $3::int) AS d
              ORDER BY d) AS at_days_back
  FROM config.settings
 WHERE $1::text IS NULL OR (namespace = $1::text AND key = $2::text)
 ORDER BY namespace COLLATE "C", key COLLATE "C"
 LIMIT 1"#
    )
});

/// One `KEYS_SQL` row: count of all keys, first key, then the matched key's columns, all
/// `NULL` when nothing matches.
type KeysTuple = (i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>);

/// The first `limit` keys whose display key starts with `prefix` (every key when `None`) in
/// `namespace, key` byte order, the count of all keys and the first key of all, in one
/// statement.
pub(crate) async fn keys(pool: &PgPool, prefix: Option<&str>, limit: i64) -> Result<KeyPage, sqlx::Error> {
    let tuples: Vec<KeysTuple> = sqlx::query_as(&KEYS_SQL).bind(prefix).bind(limit).fetch_all(pool).await?;
    let total = tuples.first().map_or(0, |t| t.0);
    let first = tuples.first().and_then(|t| t.1.clone());
    let rows = tuples
        .into_iter()
        .filter_map(|(_, _, namespace, key, value, revision, updated)| {
            Some(KeyRow { namespace: namespace?, key: key?, value: value?, revision: revision?, updated: updated? })
        })
        .collect();
    Ok(KeyPage { rows, total, first })
}

type SelectedTuple = (String, String, String, i64, String, Vec<String>);

/// The key `(namespace, key)` names, or the first key of all when `None`; `None` when there is
/// no such key.
pub(crate) async fn selected(pool: &PgPool, id: Option<(&str, &str)>) -> Result<Option<Selected>, sqlx::Error> {
    let tuple: Option<SelectedTuple> = sqlx::query_as(&SELECTED_SQL)
        .bind(id.map(|i| i.0))
        .bind(id.map(|i| i.1))
        .bind(MAX_DAYS_BACK)
        .fetch_optional(pool)
        .await?;
    Ok(tuple.map(|(namespace, key, value, revision, updated, at_days_back)| Selected {
        row: KeyRow { namespace, key, value, revision, updated },
        at_days_back,
    }))
}

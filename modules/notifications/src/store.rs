use notificationsapi::Notification;
use sqlx::{PgConnection, PgPool};

/// "Invalid text representation": both id columns are `uuid` while the contract and the
/// event payloads carry `String`, so a malformed id arrives as this SQLSTATE from the
/// `$n::uuid` cast rather than as a match failure. Reads and the two owned mutations
/// answer "no such row"; `insert_tx` instead propagates it, and its caller answers
/// `Status::Invalid`.
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// `created_at`/`read_at` are rendered in SQL because the workspace's `sqlx` carries no
/// date/time feature: the columns arrive as text already in the contract's RFC3339 shape,
/// and the cursor codec's shape check (`service::is_cursor_time`) is pinned to it.
/// An unread row's `read_at` is the EMPTY STRING, which is the contract's "unread".
const COLS: &str = r#"id::text, kind, title, body,
	     to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
	     COALESCE(to_char(read_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'), '')"#;

type Row = (String, String, String, String, String, String);

fn row_to_notification(row: Row) -> Notification {
    let (id, kind, title, body, created_at, read_at) = row;
    Notification {
        id,
        kind,
        title,
        body,
        created_at,
        read_at,
    }
}

/// Every write takes `&mut PgConnection`, never the pool, so one implementation serves a
/// pool-owned transaction and the event plane's HANDED delivery transaction alike; reads
/// use the pool.
pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    /// One page of a player's inbox, newest first. `after` is the previous page's last
    /// `(created_at, id)`; the KEYSET tuple comparison — not `OFFSET` — is what keeps paging
    /// correct while rows are deleted underneath the client, and `(created_at, id)` breaks
    /// the tie on rows sharing a timestamp, which a bare `created_at <` would skip or repeat.
    ///
    /// The caller asks for `limit + 1`: the surplus row is how the page decides whether a
    /// `next_cursor` exists, with no second COUNT over a table that grows per player.
    ///
    /// A `player_id` that is not a uuid is an EMPTY inbox, not a 500 (wallet's
    /// `list_balances` convention): a dev/non-uuid identity owns no rows, which is the same
    /// answer the statement would give if it could run. `$2`/`$3` cannot raise a cast error —
    /// [`crate::service::decode_cursor`] admits only a real calendar timestamp and the
    /// canonical uuid spelling — so this arm can only be `$1`.
    pub(crate) async fn page_by_player(
        &self,
        player_id: &str,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<Notification>, sqlx::Error> {
        let res = match after {
            Some((created_at, id)) => {
                sqlx::query_as::<_, Row>(&format!(
                    "SELECT {COLS} FROM notifications.messages \
                      WHERE player_id = $1::uuid \
                        AND (created_at, id) < ($2::timestamptz, $3::uuid) \
                      ORDER BY created_at DESC, id DESC LIMIT $4"
                ))
                .bind(player_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, Row>(&format!(
                    "SELECT {COLS} FROM notifications.messages WHERE player_id = $1::uuid \
                      ORDER BY created_at DESC, id DESC LIMIT $2"
                ))
                .bind(player_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        };
        match res {
            Ok(rows) => Ok(rows.into_iter().map(row_to_notification).collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// `COALESCE(read_at, now())` keeps the FIRST read timestamp, so a replay answers with
    /// the same state — that is what licenses `#[retry_safe]` on the contract method.
    ///
    /// `player_id` is part of the predicate, never checked afterwards: a row belonging to
    /// another player must be indistinguishable from an absent one (see
    /// [`crate::service::NOT_FOUND`]).
    pub(crate) async fn mark_read_owned_tx(
        &self,
        conn: &mut PgConnection,
        id: &str,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query_scalar::<_, i32>(
            "UPDATE notifications.messages SET read_at = COALESCE(read_at, now()) \
              WHERE id = $1::uuid AND player_id = $2::uuid RETURNING 1",
        )
        .bind(id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row.is_some()),
            Err(e) if is_invalid_uuid(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Ownership rides in the predicate, for the reason in [`Store::mark_read_owned_tx`].
    pub(crate) async fn delete_owned_tx(
        &self,
        conn: &mut PgConnection,
        id: &str,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query_scalar::<_, i32>(
            "DELETE FROM notifications.messages \
              WHERE id = $1::uuid AND player_id = $2::uuid RETURNING 1",
        )
        .bind(id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row.is_some()),
            Err(e) if is_invalid_uuid(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Appends one row and returns its DB-canonical id, or `None` when the partial unique
    /// index already holds `source_event_id`.
    ///
    /// `ON CONFLICT` is not defensive: without it an operator re-drive raises 23505, the
    /// durable handler returns `Err`, and the plane backs off and PAUSES the subscription —
    /// every player's inbox goes offline rather than one duplicate row being skipped. The
    /// `WHERE` clause is repeated verbatim from the index because Postgres cannot infer a
    /// PARTIAL unique index without it.
    ///
    /// An empty `source_event_id` is stored as NULL (operator mail), which the partial index
    /// ignores — two hand-sent messages are two rows, as they should be. A `player_id` that
    /// is not a uuid is 22P02 here, NOT a silently-written row; the caller
    /// ([`crate::service::Service::deliver_on`]) turns it into a data-quality rejection.
    pub(crate) async fn insert_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        kind: &str,
        title: &str,
        body: &str,
        source_event_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO notifications.messages \
                 (id, player_id, kind, title, body, source_event_id) \
             VALUES (gen_random_uuid(), $1::uuid, $2, $3, $4, NULLIF($5, '')) \
             ON CONFLICT (source_event_id) WHERE source_event_id IS NOT NULL DO NOTHING \
             RETURNING id::text",
        )
        .bind(player_id)
        .bind(kind)
        .bind(title)
        .bind(body)
        .bind(source_event_id)
        .fetch_optional(&mut *conn)
        .await
    }
}

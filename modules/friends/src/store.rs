use sqlx::{PgConnection, PgPool};

/// "Invalid text representation": the pair columns are `uuid` while the contract carries
/// `String`, so a malformed id arrives as this SQLSTATE from the `$n::uuid` cast rather
/// than as a match failure. The edge-addressed statements fold it into "no such row" (a
/// client-supplied id that cannot name one); the pair-addressed write path propagates it
/// and its caller answers `Status::Invalid`.
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// `created_at` is rendered in SQL because the workspace's `sqlx` carries no date/time
/// feature: the column arrives as text already in the cursor codec's shape
/// (`service::is_cursor_time` is pinned to it).
const CREATED_TEXT: &str =
    r#"to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#;

/// One relation as the CALLER sees it: `other_id` is the party that is not the caller and
/// `requester_is_caller` is what the contract's `direction` is computed from. Both are
/// decided in SQL against `$1::uuid`, so neither depends on how the caller spelled its own
/// id, and `player_id` never has to be compared in Rust.
pub(crate) struct EdgeRow {
    pub(crate) edge_id: String,
    pub(crate) other_id: String,
    pub(crate) state: String,
    pub(crate) created_at: String,
    pub(crate) requester_is_caller: bool,
}

/// The pair-addressed shape of [`EdgeRow`]: the caller already knows the other player, so
/// the row only has to say which relation exists and who authored it.
pub(crate) struct PairRow {
    pub(crate) edge_id: String,
    pub(crate) state: String,
    pub(crate) requester_is_caller: bool,
}

/// Per-REQUESTER transaction-scoped advisory-lock key for `request`'s outstanding cap: two
/// concurrent requests by one player must serialize their insert-then-count, or both count
/// below the cap (neither committed yet, READ COMMITTED) and both land past it. FNV-1a over
/// a DISTINCT namespace prefix so the key can never collide with characters' or scheduler's
/// keys — a collision would only over-serialize, never break correctness.
///
/// The id is normalized to Postgres's uuid-EQUALITY form before hashing (the same
/// discipline as `characters::player_lock_key`, deliberately duplicated across the two
/// fortresses — friends cannot import that impl crate): the row SQL is `$1::uuid`, so a
/// differently-spelled but DB-equal id must yield the SAME lock.
fn requester_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = player_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { player_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"friends.requester/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

/// Every write takes `&mut PgConnection` — the service owns the transaction, because the
/// durable event append must ride it.
pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    /// Serializes one requester's concurrent `request` calls for the life of the
    /// transaction. Taken ON THE TX CONNECTION — a separate pool connection would lose the
    /// serialization — and released at commit/rollback.
    pub(crate) async fn lock_requester_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(requester_lock_key(player_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Branch 1 of `request`: the pair has no row yet. `least`/`greatest` is the ordered
    /// pair's ONE authority, so the caller's side of it never depends on which argument it
    /// passed, and `ON CONFLICT DO NOTHING` answers "a row already exists" as zero rows
    /// instead of an exception — a duplicate key here has three distinct causes and only a
    /// second statement can tell them apart.
    ///
    /// Returns the row's id and the requester's id as the DATABASE spells them — an event
    /// must never carry the caller's spelling of an id.
    pub(crate) async fn insert_pending_tx(
        &self,
        conn: &mut PgConnection,
        me: &str,
        other: &str,
        pending: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as(
            "INSERT INTO friends.edges (id, low_id, high_id, requester_id, state) \
             SELECT gen_random_uuid(), least($1::uuid, $2::uuid), greatest($1::uuid, $2::uuid), \
                    $1::uuid, $3::text \
             ON CONFLICT (low_id, high_id) DO NOTHING \
             RETURNING id::text, requester_id::text",
        )
        .bind(me)
        .bind(other)
        .bind(pending)
        .fetch_optional(&mut *conn)
        .await
    }

    /// The caller's OUTSTANDING requests: rows they authored that are still unanswered.
    pub(crate) async fn count_authored_pending_tx(
        &self,
        conn: &mut PgConnection,
        me: &str,
        pending: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT count(*) FROM friends.edges \
              WHERE requester_id = $1::uuid AND state = $2",
        )
        .bind(me)
        .bind(pending)
        .fetch_one(&mut *conn)
        .await
    }

    /// Branch 2 of `request`: the OTHER player already asked, so this request answers it.
    /// `requester_id <> $1::uuid` is what keeps it from turning a caller's own duplicate
    /// request into an acceptance of itself — the consent bypass a blind
    /// conflict-means-accept would open with a double POST.
    ///
    /// Returns the row's id and the CALLER's id as the database spells it.
    pub(crate) async fn accept_crossing_tx(
        &self,
        conn: &mut PgConnection,
        me: &str,
        other: &str,
        pending: &str,
        accepted: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as(
            "UPDATE friends.edges SET state = $4, accepted_at = now() \
              WHERE low_id = least($1::uuid, $2::uuid) \
                AND high_id = greatest($1::uuid, $2::uuid) \
                AND state = $3 AND requester_id <> $1::uuid \
             RETURNING id::text, \
                       CASE WHEN low_id = $1::uuid THEN low_id ELSE high_id END::text",
        )
        .bind(me)
        .bind(other)
        .bind(pending)
        .bind(accepted)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Branch 3 of `request`: the relation the two preceding statements declined to touch —
    /// the caller's own pending request, or one already accepted.
    pub(crate) async fn find_pair_tx(
        &self,
        conn: &mut PgConnection,
        me: &str,
        other: &str,
    ) -> Result<Option<PairRow>, sqlx::Error> {
        let row = sqlx::query_as::<_, (String, String, bool)>(
            "SELECT id::text, state, (requester_id = $1::uuid) FROM friends.edges \
              WHERE low_id = least($1::uuid, $2::uuid) \
                AND high_id = greatest($1::uuid, $2::uuid)",
        )
        .bind(me)
        .bind(other)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(edge_id, state, requester_is_caller)| PairRow {
            edge_id,
            state,
            requester_is_caller,
        }))
    }

    /// The relation `edge_id` names, as the caller sees it — for HYDRATION only: it names
    /// the other party so their handle can be resolved BEFORE a transaction opens, since a
    /// directory RPC issued while holding one would pin a connection and the row's locks
    /// across the network. Every consent decision is re-made by the mutating statement
    /// itself, which is why this read may be stale without being wrong.
    ///
    /// A row the caller is not party to, and an id that is not a uuid at all, are both
    /// `None` — the contract's one answer for "no such edge" and "not yours" (a 403 would
    /// confirm the id names a real relation).
    pub(crate) async fn view_edge(
        &self,
        edge_id: &str,
        me: &str,
    ) -> Result<Option<EdgeRow>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String, String, String, bool)>(&format!(
            "SELECT id::text, \
                    CASE WHEN low_id = $2::uuid THEN high_id ELSE low_id END::text, \
                    state, {CREATED_TEXT}, (requester_id = $2::uuid) \
               FROM friends.edges \
              WHERE id = $1::uuid AND $2::uuid IN (low_id, high_id)"
        ))
        .bind(edge_id)
        .bind(me)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(row.map(row_to_edge)),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Accepts the pending request `edge_id` names. The predicate is the whole consent
    /// rule: only while pending, only by a party of the pair, and only by the party that
    /// did NOT author it — `requester_id`, never `high_id`, because which column holds the
    /// addressee depends on uuid ordering. Returns the row's OWN id and the other party's,
    /// both DB-canonical — an event must never carry the caller's spelling of an id.
    pub(crate) async fn accept_tx(
        &self,
        conn: &mut PgConnection,
        edge_id: &str,
        me: &str,
        pending: &str,
        accepted: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(
            "UPDATE friends.edges SET state = $4, accepted_at = now() \
              WHERE id = $1::uuid AND state = $3 AND requester_id <> $2::uuid \
                AND $2::uuid IN (low_id, high_id) \
             RETURNING id::text, \
                       CASE WHEN low_id = $2::uuid THEN high_id ELSE low_id END::text",
        )
        .bind(edge_id)
        .bind(me)
        .bind(pending)
        .bind(accepted)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Declines the pending request `edge_id` names — the same consent predicate as
    /// [`Store::accept_tx`], so a replay after either answer is the same "no such edge".
    pub(crate) async fn decline_tx(
        &self,
        conn: &mut PgConnection,
        edge_id: &str,
        me: &str,
        pending: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(
            "DELETE FROM friends.edges \
              WHERE id = $1::uuid AND state = $3 AND requester_id <> $2::uuid \
                AND $2::uuid IN (low_id, high_id) \
             RETURNING id::text, \
                       CASE WHEN low_id = $2::uuid THEN high_id ELSE low_id END::text",
        )
        .bind(edge_id)
        .bind(me)
        .bind(pending)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Drops the relation `edge_id` names: EITHER party, in EITHER state, so no
    /// `requester_id` clause. The returned `state`/`requester_is_caller` are the DELETED
    /// row's, not a prior read's — which ending the removal is (`withdrawn` / `declined` /
    /// `unfriended`) is decided from them, so a state that changed underneath the caller
    /// cannot mislabel the emitted event.
    pub(crate) async fn delete_tx(
        &self,
        conn: &mut PgConnection,
        edge_id: &str,
        me: &str,
    ) -> Result<Option<(String, String, String, bool)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String, String, bool)>(
            "DELETE FROM friends.edges \
              WHERE id = $1::uuid AND $2::uuid IN (low_id, high_id) \
             RETURNING id::text, \
                       CASE WHEN low_id = $2::uuid THEN high_id ELSE low_id END::text, \
                       state, (requester_id = $2::uuid)",
        )
        .bind(edge_id)
        .bind(me)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// One page of the caller's relations in `state`, newest first.
    ///
    /// A UNION ALL of the two SIDE branches, not one `low_id = $1 OR high_id = $1`
    /// predicate: the `OR` is a BitmapOr plus a sort of every matching row, which the outer
    /// `LIMIT` does not bound, so the keyset would stop being a keyset as a player's graph
    /// grows. Each branch is served by its own index and is itself bounded, and the outer
    /// merge sorts at most `2 * limit` rows.
    ///
    /// The outer ORDER BY reads the RAW `created_at`/`id` (carried out of the subquery as
    /// `sort_at`/`sort_id`), never the rendered text: sorting a UNION's output columns would
    /// order uuids by their text collation while the branches' keyset predicate compares
    /// them as uuids.
    ///
    /// `after` is the previous page's last `(created_at, id)`; the tuple comparison is what
    /// keeps paging correct while rows are removed underneath the client, and it breaks the
    /// tie on rows sharing a timestamp that a bare `created_at <` would skip or repeat. The
    /// caller asks for `limit + 1` — the surplus row IS the "has more" answer, with no
    /// second COUNT.
    ///
    /// A `player_id` that is not a uuid is an EMPTY page, not a 500: a non-uuid identity is
    /// party to no relation, which is the answer the statement would give if it could run.
    pub(crate) async fn page(
        &self,
        me: &str,
        state: &str,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<EdgeRow>, sqlx::Error> {
        let res = match after {
            Some((created_at, id)) => {
                let cut = "AND (created_at, id) < ($2::timestamptz, $3::uuid)";
                sqlx::query_as::<_, (String, String, String, String, bool)>(&page_sql(
                    cut, "$4", "$5",
                ))
                .bind(me)
                .bind(created_at)
                .bind(id)
                .bind(state)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, (String, String, String, String, bool)>(&page_sql(
                    "", "$2", "$3",
                ))
                .bind(me)
                .bind(state)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        };
        match res {
            Ok(rows) => Ok(rows.into_iter().map(row_to_edge).collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

fn row_to_edge(row: (String, String, String, String, bool)) -> EdgeRow {
    let (edge_id, other_id, state, created_at, requester_is_caller) = row;
    EdgeRow {
        edge_id,
        other_id,
        state,
        created_at,
        requester_is_caller,
    }
}

/// Placeholder numbering is per-VARIANT: Postgres rejects a bind for a parameter the
/// statement does not reference, so the keyed page's `$4`/`$5` become `$2`/`$3` when the
/// cursor clause is absent.
fn page_sql(cut: &str, state: &str, limit: &str) -> String {
    let low = side_sql("low_id", "high_id", cut, state, limit);
    let high = side_sql("high_id", "low_id", cut, state, limit);
    format!(
        "SELECT edge_id, other_id, state, created_text, mine FROM ( \
           {low} UNION ALL {high} \
         ) t ORDER BY sort_at DESC, sort_id DESC LIMIT {limit}"
    )
}

fn side_sql(me_col: &str, other_col: &str, cut: &str, state: &str, limit: &str) -> String {
    format!(
        "(SELECT id::text AS edge_id, {other_col}::text AS other_id, state, \
                 {CREATED_TEXT} AS created_text, (requester_id = $1::uuid) AS mine, \
                 created_at AS sort_at, id AS sort_id \
            FROM friends.edges \
           WHERE {me_col} = $1::uuid AND state = {state} {cut} \
           ORDER BY created_at DESC, id DESC LIMIT {limit})"
    )
}

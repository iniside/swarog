use sqlx::{PgConnection, PgPool};

/// "Invalid text representation": a malformed id arrives as this SQLSTATE from the
/// `$n::uuid` cast, not as a match failure. Edge-addressed statements fold it into "no such
/// row"; the pair-addressed write path propagates it and its caller answers 400.
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// `created_at` is rendered in SQL because the workspace's `sqlx` carries no date/time
/// feature: the column arrives as text already in the cursor codec's shape
/// (`service::is_cursor_time` is pinned to it).
const CREATED_TEXT: &str =
    r#"to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#;

/// One relation as the CALLER sees it. `other_id` and `requester_is_caller` are decided in
/// SQL against `$1::uuid`, so neither depends on how the caller spelled its own id.
pub(crate) struct EdgeRow {
    pub(crate) edge_id: String,
    pub(crate) other_id: String,
    pub(crate) state: String,
    pub(crate) created_at: String,
    pub(crate) requester_is_caller: bool,
}

/// One relation as the ADMIN sees it: the pair spelled requester-then-addressee, with no
/// caller to be relative to.
pub(crate) struct AdminEdgeRow {
    pub(crate) edge_id: String,
    pub(crate) requester_id: String,
    pub(crate) addressee_id: String,
    pub(crate) state: String,
    pub(crate) created_at: String,
}

pub(crate) struct PairRow {
    pub(crate) edge_id: String,
    pub(crate) state: String,
    pub(crate) requester_is_caller: bool,
}

/// Per-REQUESTER lock key for `request`'s outstanding cap: without it two concurrent
/// requests by one player both count below the cap (neither committed yet, READ COMMITTED)
/// and both land past it. The namespace prefix keeps it from colliding with characters' or
/// scheduler's keys — a collision would only over-serialize.
///
/// The id is normalized to Postgres's uuid-EQUALITY form before hashing, because the row SQL
/// is `$1::uuid`: a differently-spelled but DB-equal id must yield the SAME lock.
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
    /// Taken ON THE TX CONNECTION — a separate pool connection would lose the
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

    /// `least`/`greatest` is the ordered pair's ONE authority: the caller's side of the pair
    /// never depends on which argument it passed. `DO NOTHING` answers a duplicate key as
    /// zero rows rather than an exception — the key has three distinct causes and only a
    /// second statement can tell them apart.
    ///
    /// Returns both ids as the DATABASE spells them; an event must never carry the caller's.
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

    /// The OTHER player already asked, so this request answers it. `requester_id <>
    /// $1::uuid` is what stops a caller's own duplicate request from accepting itself — the
    /// consent bypass a blind conflict-means-accept opens with a double POST.
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

    /// For HYDRATION only: it names the other party so their handle can be resolved BEFORE a
    /// transaction opens — a directory RPC issued while holding one would pin a connection
    /// and the row's locks across the network. Every consent decision is re-made by the
    /// mutating statement, which is why this read may be stale without being wrong.
    ///
    /// "Not yours", "no such edge" and "not a uuid" are all `None`: a 403 would confirm the
    /// id names a real relation.
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

    /// The predicate is the whole consent rule: only while pending, only by a party of the
    /// pair, and only by the one that did NOT author it — `requester_id`, never `high_id`,
    /// because which column holds the addressee depends on uuid ordering.
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

    /// The same consent predicate as [`Store::accept_tx`], so a replay after either answer
    /// is the same "no such edge".
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

    /// EITHER party, in EITHER state, so no `requester_id` clause. The returned
    /// `state`/`requester_is_caller` are the DELETED row's, not a prior read's: a state that
    /// changed underneath the caller must not mislabel the emitted event's reason.
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

    /// A UNION ALL of the two SIDE branches, not one `low_id = $1 OR high_id = $1`: the
    /// `OR` is a BitmapOr plus a sort of every matching row, which the outer `LIMIT` does
    /// not bound — the keyset would stop being a keyset as a player's graph grows. Each
    /// branch has its own index and its own `LIMIT`, so the merge sorts at most `2 * limit`.
    ///
    /// The outer ORDER BY reads the RAW `created_at`/`id` (carried out as
    /// `sort_at`/`sort_id`), never the rendered text: sorting a UNION's OUTPUT columns would
    /// order uuids by text collation while the branches' keyset compares them as uuids.
    ///
    /// The keyset tuple breaks the tie on rows sharing a timestamp that a bare `created_at
    /// <` would skip or repeat.
    ///
    /// A `player_id` that is not a uuid is an EMPTY page, not a 500: a non-uuid identity is
    /// party to no relation.
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

    /// The admin page's three counts in ONE statement: three separate `count(*)`s would
    /// report a total no single instant of the table ever had. `player_id` scopes every
    /// count to the relations that player is party to.
    pub(crate) async fn admin_counts(
        &self,
        player_id: Option<&str>,
        pending: &str,
        accepted: &str,
    ) -> Result<(i64, i64, i64), sqlx::Error> {
        let sql = format!(
            "SELECT count(*), \
                    count(*) FILTER (WHERE state = $1), \
                    count(*) FILTER (WHERE state = $2) \
               FROM friends.edges{}",
            match player_id {
                Some(_) => " WHERE $3::uuid IN (low_id, high_id)",
                None => "",
            }
        );
        let q = sqlx::query_as::<_, (i64, i64, i64)>(&sql).bind(pending).bind(accepted);
        let res = match player_id {
            Some(id) => q.bind(id).fetch_one(&self.pool).await,
            None => q.fetch_one(&self.pool).await,
        };
        match res {
            Ok(counts) => Ok(counts),
            Err(e) if is_invalid_uuid(&e) => Ok((0, 0, 0)),
            Err(e) => Err(e),
        }
    }

    /// The newest relations, either across the table or scoped to one player. Unlike
    /// [`Store::page`] this is NOT caller-relative: the admin sees the pair as the database
    /// holds it, requester first, so a row means the same thing whichever player it lists.
    pub(crate) async fn admin_recent(
        &self,
        player_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AdminEdgeRow>, sqlx::Error> {
        let sql = format!(
            "SELECT id::text, requester_id::text, \
                    CASE WHEN low_id = requester_id THEN high_id ELSE low_id END::text, \
                    state, {CREATED_TEXT} \
               FROM friends.edges{} \
              ORDER BY created_at DESC, id DESC LIMIT $1",
            match player_id {
                Some(_) => " WHERE $2::uuid IN (low_id, high_id)",
                None => "",
            }
        );
        let q =
            sqlx::query_as::<_, (String, String, String, String, String)>(&sql).bind(limit);
        let res = match player_id {
            Some(id) => q.bind(id).fetch_all(&self.pool).await,
            None => q.fetch_all(&self.pool).await,
        };
        match res {
            Ok(rows) => Ok(rows.into_iter().map(row_to_admin_edge).collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

fn row_to_admin_edge(row: (String, String, String, String, String)) -> AdminEdgeRow {
    let (edge_id, requester_id, addressee_id, state, created_at) = row;
    AdminEdgeRow {
        edge_id,
        requester_id,
        addressee_id,
        state,
        created_at,
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

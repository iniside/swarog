use sqlx::{PgConnection, PgPool};

/// "Invalid text representation": a malformed id arrives as this SQLSTATE from the
/// `$n::uuid` cast, not as a match failure. Statements addressed by a caller-supplied
/// group id fold it into "no such row"; the ones that can only have miscast the caller's
/// own identity propagate it and their caller answers 400.
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// `created_at` is rendered in SQL because the workspace's `sqlx` carries no date/time
/// feature: the column arrives as text already in the cursor codec's shape
/// (`service::is_cursor_time` is pinned to it).
fn created_text(col: &str) -> String {
    format!(r#"to_char({col} AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#)
}

/// One group the CALLER holds a row in: the group's own metadata, the caller's row, and
/// the MEMBERSHIP timestamp the keyset orders on — which is not `created_at`, because the
/// list is ordered by when the caller's relation to each group began.
pub(crate) struct MyGroupRow {
    pub(crate) group_id: String,
    pub(crate) name: String,
    pub(crate) join_policy: String,
    pub(crate) created_at: String,
    pub(crate) my_state: String,
    pub(crate) my_role: String,
    pub(crate) sort_at: String,
}

/// One membership row of a group, as `members`/`pending` list it.
pub(crate) struct MemberRow {
    pub(crate) player_id: String,
    pub(crate) state: String,
    pub(crate) role: String,
    pub(crate) created_at: String,
}

/// Live rows of one group in ONE statement: three separate counts would report a total no
/// single instant of the table ever had, and `leave` decides two rules from them at once.
pub(crate) struct GroupCounts {
    pub(crate) rows: i64,
    pub(crate) members: i64,
    pub(crate) admins: i64,
}

/// One group as the ADMIN page lists it: the group's own row plus its live counts. The
/// counts ride the SAME statement as the row (a lateral aggregate), so a group can never
/// be listed with a member count taken at a different instant than its name.
pub(crate) struct AdminGroupRow {
    pub(crate) group_id: String,
    pub(crate) name: String,
    pub(crate) join_policy: String,
    pub(crate) created_at: String,
    pub(crate) members: i64,
    pub(crate) pending: i64,
}

/// Per-GROUP lock key: `MAX_MEMBERS` cannot be enforced by one statement, because
/// `INSERT … WHERE (SELECT count(*)) < 500` is a count-then-insert however it is spelled —
/// under READ COMMITTED two concurrent joins both read 499 (neither committed yet) and
/// both land past the cap. Every op that decides from the group's roster takes this lock
/// before reading it, so the cap, the last-admin rule and the last-member teardown all
/// decide on a roster no concurrent writer can change under them.
///
/// The namespace prefix keeps it from colliding with friends' requester key or
/// scheduler's; a collision would only over-serialize. The id is normalized to Postgres's
/// uuid-EQUALITY form before hashing, because the row SQL is `$1::uuid`: a
/// differently-spelled but DB-equal id must yield the SAME lock.
fn group_lock_key(group_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = group_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { group_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"groups.group/".iter().copied().chain(normalized.iter().copied()) {
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
    pub(crate) async fn lock_group_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(group_lock_key(group_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// The caller's role in a group, gated by the WHOLE visibility rule in the predicate:
    /// `required_role` empty means "any member". A group that does not exist, one the
    /// caller holds no `member` row in, one whose row is pending, and a group id that is
    /// not a uuid are all the same `None` — a distinction here would be an enumeration
    /// oracle over group ids.
    pub(crate) async fn visible_role(
        &self,
        group_id: &str,
        player_id: &str,
        member: &str,
        required_role: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(VISIBLE_ROLE_SQL)
            .bind(group_id)
            .bind(player_id)
            .bind(member)
            .bind(required_role)
            .fetch_optional(&self.pool)
            .await;
        match res {
            Ok(row) => Ok(row.map(|(role, _)| role)),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// [`Store::visible_role`] re-asked inside the transaction, under the group lock: the
    /// pool read is only ever a pre-check, and a role that changed in between must not
    /// authorize the write. Answers `(role, canonical player id)` — the write that
    /// follows may put the caller's id in an event payload.
    pub(crate) async fn visible_role_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
        member: &str,
        required_role: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(VISIBLE_ROLE_SQL)
            .bind(group_id)
            .bind(player_id)
            .bind(member)
            .bind(required_role)
            .fetch_optional(&mut *conn)
            .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Binds ONLY the group id, so a `22P02` here can only be a malformed group id and
    /// folds to "no such group" — which is what lets every later statement in the same
    /// transaction read a `22P02` as the caller's own identity instead.
    ///
    /// Returns the id as the DATABASE spells it, for the reason in
    /// [`Store::insert_group_tx`].
    pub(crate) async fn join_policy_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String)>(
            "SELECT join_policy, id::text FROM groups.groups WHERE id = $1::uuid",
        )
        .bind(group_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn counts_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        member: &str,
        admin: &str,
    ) -> Result<GroupCounts, sqlx::Error> {
        let (rows, members, admins) = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT count(*), \
                    count(*) FILTER (WHERE state = $2), \
                    count(*) FILTER (WHERE state = $2 AND role = $3) \
               FROM groups.memberships WHERE group_id = $1::uuid",
        )
        .bind(group_id)
        .bind(member)
        .bind(admin)
        .fetch_one(&mut *conn)
        .await?;
        Ok(GroupCounts {
            rows,
            members,
            admins,
        })
    }

    /// Returns the ids as the DATABASE spells them: an event must never carry the
    /// caller's spelling of an id it did not mint.
    pub(crate) async fn insert_group_tx(
        &self,
        conn: &mut PgConnection,
        name: &str,
        join_policy: &str,
        creator_id: &str,
    ) -> Result<(String, String, String), sqlx::Error> {
        sqlx::query_as::<_, (String, String, String)>(&format!(
            "INSERT INTO groups.groups (name, join_policy, creator_id) \
             VALUES ($1, $2, $3::uuid) RETURNING id::text, creator_id::text, {}",
            created_text("created_at")
        ))
        .bind(name)
        .bind(join_policy)
        .bind(creator_id)
        .fetch_one(&mut *conn)
        .await
    }

    /// `DO NOTHING` answers an existing row as zero rows rather than an exception: the
    /// caller already holds a relation to this group, which is a `Conflict` and never a
    /// silent second row.
    pub(crate) async fn insert_membership_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
        state: &str,
        role: &str,
    ) -> Result<Option<(String, String, String)>, sqlx::Error> {
        sqlx::query_as::<_, (String, String, String)>(&format!(
            "INSERT INTO groups.memberships (group_id, player_id, state, role) \
             VALUES ($1::uuid, $2::uuid, $3, $4) \
             ON CONFLICT (group_id, player_id) DO NOTHING \
             RETURNING group_id::text, player_id::text, {}",
            created_text("created_at")
        ))
        .bind(group_id)
        .bind(player_id)
        .bind(state)
        .bind(role)
        .fetch_optional(&mut *conn)
        .await
    }

    /// The subject's row, whatever state it holds. Read under the group lock, so the
    /// state it reports is the one the following write acts on.
    ///
    /// Answers `(state, role, canonical player id)`. The id is the DATABASE's spelling:
    /// the predicate is `player_id = $2::uuid`, so two textually different but uuid-EQUAL
    /// spellings both find this row, and a caller comparing subjects must compare what
    /// the column holds rather than what it was handed.
    pub(crate) async fn membership_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
    ) -> Result<Option<(String, String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String, String)>(
            "SELECT state, role, player_id::text FROM groups.memberships \
              WHERE group_id = $1::uuid AND player_id = $2::uuid",
        )
        .bind(group_id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `from_state` rides the predicate so an accept can only ever promote the state it
    /// was authorized against — and `role` is set in the SAME statement, because
    /// `memberships_role_check` is an equivalence: a `member` row with an empty role would
    /// make `role_of` answer `""` for a real member.
    pub(crate) async fn promote_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
        from_state: &str,
        member: &str,
        role: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query_as::<_, (String, String)>(
            "UPDATE groups.memberships SET state = $4, role = $5 \
              WHERE group_id = $1::uuid AND player_id = $2::uuid AND state = $3 \
             RETURNING group_id::text, player_id::text",
        )
        .bind(group_id)
        .bind(player_id)
        .bind(from_state)
        .bind(member)
        .bind(role)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Returns the DELETED row's state and role, not a prior read's: the emitted event's
    /// `reason` must describe the row that actually went away.
    pub(crate) async fn delete_membership_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
        player_id: &str,
    ) -> Result<Option<(String, String, String, String)>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String, String, String)>(
            "DELETE FROM groups.memberships \
              WHERE group_id = $1::uuid AND player_id = $2::uuid \
             RETURNING state, role, group_id::text, player_id::text",
        )
        .bind(group_id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await;
        match res {
            Ok(row) => Ok(row),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The last-member teardown: the group row AND every remaining pending row it owns.
    /// `groups.groups` has no other collector, and a `groups.memberships` row pointing at
    /// a deleted group is unreachable by every op in the contract.
    ///
    /// Returns the removed rows' player ids as the DATABASE spells them: each one is a
    /// pending relation that ends here, and its caller owes the log a terminal event.
    pub(crate) async fn delete_group_tx(
        &self,
        conn: &mut PgConnection,
        group_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        let removed = sqlx::query_as::<_, (String,)>(
            "DELETE FROM groups.memberships WHERE group_id = $1::uuid \
             RETURNING player_id::text",
        )
        .bind(group_id)
        .fetch_all(&mut *conn)
        .await?;
        sqlx::query("DELETE FROM groups.groups WHERE id = $1::uuid")
            .bind(group_id)
            .execute(&mut *conn)
            .await?;
        Ok(removed.into_iter().map(|(id,)| id).collect())
    }

    /// Every group the caller holds ANY row in, newest RELATION first. The keyset tuple
    /// breaks the tie on rows sharing a timestamp that a bare `created_at <` would skip or
    /// repeat.
    ///
    /// A `player_id` that is not a uuid is an EMPTY page, not a 500: a non-uuid identity
    /// holds no membership.
    pub(crate) async fn page_mine(
        &self,
        player_id: &str,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<MyGroupRow>, sqlx::Error> {
        let res = match after {
            Some((created_at, id)) => {
                sqlx::query_as::<_, (String, String, String, String, String, String, String)>(
                    &mine_sql(
                        "AND (m.created_at, m.group_id) < ($2::timestamptz, $3::uuid)",
                        "$4",
                    ),
                )
                .bind(player_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, (String, String, String, String, String, String, String)>(
                    &mine_sql("", "$2"),
                )
                .bind(player_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        };
        match res {
            Ok(rows) => Ok(rows.into_iter().map(row_to_my_group).collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// One page of a group's rows. `state_clause` is a compile-time literal chosen by the
    /// caller ([`MEMBER_ROWS`] or [`PENDING_ROWS`]) — never caller input, and PARENTHESIZED
    /// at the interpolation site so respelling one of them with an `OR` cannot bind looser
    /// than the `group_id` term and page another group's rows. Authorization is
    /// NOT in this statement: it is decided by [`Store::visible_role`] first, because an
    /// empty page and an invisible group must answer differently.
    pub(crate) async fn page_group(
        &self,
        group_id: &str,
        state_clause: &str,
        after: Option<(&str, &str)>,
        limit: i64,
    ) -> Result<Vec<MemberRow>, sqlx::Error> {
        let res = match after {
            Some((created_at, id)) => {
                sqlx::query_as::<_, (String, String, String, String)>(&group_page_sql(
                    state_clause,
                    "AND (created_at, player_id) < ($2::timestamptz, $3::uuid)",
                    "$4",
                ))
                .bind(group_id)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query_as::<_, (String, String, String, String)>(&group_page_sql(
                    state_clause,
                    "",
                    "$2",
                ))
                .bind(group_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        };
        match res {
            Ok(rows) => Ok(rows.into_iter().map(row_to_member).collect()),
            Err(e) if is_invalid_uuid(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The three totals the admin overview reports, in ONE statement: three separate
    /// round-trips would report a total no single instant of the table ever had.
    pub(crate) async fn admin_totals(&self, member: &str) -> Result<(i64, i64, i64), sqlx::Error> {
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT (SELECT count(*) FROM groups.groups), \
                    (SELECT count(*) FROM groups.memberships WHERE state = $1), \
                    (SELECT count(*) FROM groups.memberships WHERE state <> $1)",
        )
        .bind(member)
        .fetch_one(&self.pool)
        .await
    }

    /// The newest groups with their live counts. Unpaged by design — the operator narrows
    /// by drilling into one group, not by walking the table.
    pub(crate) async fn admin_recent_groups(
        &self,
        member: &str,
        limit: i64,
    ) -> Result<Vec<AdminGroupRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (String, String, String, String, i64, i64)>(&format!(
            "SELECT g.id::text, g.name, g.join_policy, {}, c.members, c.pending \
               FROM groups.groups g JOIN LATERAL ({}) c ON true \
              ORDER BY g.created_at DESC, g.id DESC LIMIT $2",
            created_text("g.created_at"),
            COUNTS_LATERAL
        ))
        .bind(member)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_admin_group).collect())
    }

    /// One group by id, counts included. A group id that is not a uuid is `None`, not a
    /// 500 — the page renders it as "no such group" (see [`is_invalid_uuid`]).
    pub(crate) async fn admin_group(
        &self,
        member: &str,
        group_id: &str,
    ) -> Result<Option<AdminGroupRow>, sqlx::Error> {
        let res = sqlx::query_as::<_, (String, String, String, String, i64, i64)>(&format!(
            "SELECT g.id::text, g.name, g.join_policy, {}, c.members, c.pending \
               FROM groups.groups g JOIN LATERAL ({}) c ON true \
              WHERE g.id = $2::uuid",
            created_text("g.created_at"),
            COUNTS_LATERAL
        ))
        .bind(member)
        .bind(group_id)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(row.map(row_to_admin_group)),
            Err(e) if is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// `$3` empty means "any member"; a required role is compared IN the predicate, so a
/// non-admin and a nonexistent group leave by the same door.
///
/// Selects the id as the DATABASE spells it alongside the role: the predicate is
/// `player_id = $2::uuid`, so a uuid-EQUAL but differently-spelled caller id authorizes
/// fine — and an id that goes on to reach an event payload must be the canonical one.
const VISIBLE_ROLE_SQL: &str = "SELECT role, player_id::text FROM groups.memberships \
      WHERE group_id = $1::uuid AND player_id = $2::uuid AND state = $3 \
        AND ($4::text = '' OR role = $4::text)";

pub(crate) const MEMBER_ROWS: &str = "state = 'member'";
pub(crate) const PENDING_ROWS: &str = "state <> 'member'";
/// Every row of a group, whatever state it holds — the admin page's list, which shows
/// members and pending rows in ONE table with the state as a column.
pub(crate) const ALL_ROWS: &str = "true";

/// Placeholder numbering is per-VARIANT: Postgres rejects a bind for a parameter the
/// statement does not reference, so the keyed page's `$4` becomes `$2` when the cursor
/// clause is absent.
fn mine_sql(cut: &str, limit: &str) -> String {
    let group_created = created_text("g.created_at");
    let joined = created_text("m.created_at");
    format!(
        "SELECT g.id::text, g.name, g.join_policy, {group_created}, m.state, m.role, \
                {joined} \
           FROM groups.memberships m JOIN groups.groups g ON g.id = m.group_id \
          WHERE m.player_id = $1::uuid {cut} \
          ORDER BY m.created_at DESC, m.group_id DESC LIMIT {limit}"
    )
}

fn group_page_sql(state_clause: &str, cut: &str, limit: &str) -> String {
    let joined = created_text("created_at");
    format!(
        "SELECT player_id::text, state, role, {joined} FROM groups.memberships \
          WHERE group_id = $1::uuid AND ({state_clause}) {cut} \
          ORDER BY created_at DESC, player_id DESC LIMIT {limit}"
    )
}

fn row_to_my_group(
    row: (String, String, String, String, String, String, String),
) -> MyGroupRow {
    let (group_id, name, join_policy, created_at, my_state, my_role, sort_at) = row;
    MyGroupRow {
        group_id,
        name,
        join_policy,
        created_at,
        my_state,
        my_role,
        sort_at,
    }
}

fn row_to_member(row: (String, String, String, String)) -> MemberRow {
    let (player_id, state, role, created_at) = row;
    MemberRow {
        player_id,
        state,
        role,
        created_at,
    }
}

/// The per-group live counts both admin reads share, correlated on `g.id`. `$1` is the
/// `member` state; a group with no rows at all still yields one row of zeros, which is why
/// the join is a plain `LATERAL ... ON true`.
const COUNTS_LATERAL: &str = "SELECT count(*) FILTER (WHERE state = $1) AS members, \
                                     count(*) FILTER (WHERE state <> $1) AS pending \
                                FROM groups.memberships WHERE group_id = g.id";

fn row_to_admin_group(row: (String, String, String, String, i64, i64)) -> AdminGroupRow {
    let (group_id, name, join_policy, created_at, members, pending) = row;
    AdminGroupRow {
        group_id,
        name,
        join_policy,
        created_at,
        members,
        pending,
    }
}

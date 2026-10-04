use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::sweep::{SWEEP_ACQUIRE_DEADLINE, SWEEP_STATEMENT_TIMEOUT};

pub(crate) fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// Per-PARTY lock key. Every write that removes a member from a party, stamps it or counts
/// it takes this lock first, so under it a party's roster and `parties` row can change only
/// by this transaction. The id is normalized to Postgres's uuid-equality form before hashing
/// because the row SQL binds `$1::uuid`.
fn party_lock_key(party_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hex: Vec<u8> = party_id
        .bytes()
        .filter(u8::is_ascii_hexdigit)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    let normalized: &[u8] = if hex.len() == 32 { &hex } else { party_id.as_bytes() };
    let mut h = OFFSET_BASIS;
    for b in b"party.party/".iter().copied().chain(normalized.iter().copied()) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h as i64
}

/// The workspace's `sqlx` carries no date/time feature, so instants leave SQL as RFC3339 text.
macro_rules! rfc3339 {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            r#" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')"#
        )
    };
}

/// A view as ONE statement read it. `party_id` empty is "no party", and then `revision` is
/// the player's own `player_revisions` value (0 if none) and `lease_until` is empty.
#[derive(Clone)]
pub(crate) struct ViewRow {
    pub(crate) party_id: String,
    pub(crate) revision: i64,
    pub(crate) leader_id: String,
    pub(crate) member_ids: Vec<String>,
    pub(crate) readies: Vec<String>,
    pub(crate) lease_until: String,
}

pub(crate) struct InviteRow {
    pub(crate) invite_id: String,
    pub(crate) party_id: String,
    pub(crate) inviter_id: String,
    pub(crate) expires_at: String,
}

/// The caller's view in ONE statement, so revision, leader, roster, ready states, lease and
/// the no-party revision all come from one snapshot: two views with equal revisions never
/// differ in membership or ready state.
const PLAYER_VIEW_SQL: &str = concat!(
    "SELECT coalesce(p.id::text, ''), \
            coalesce(p.revision, \
                     (SELECT r.revision FROM party.player_revisions r \
                       WHERE r.player_id = $1::uuid), \
                     0), \
            coalesce(p.leader_id::text, ''), \
            coalesce(array_agg(m.player_id::text ORDER BY m.joined_at, m.player_id) \
                     FILTER (WHERE m.player_id IS NOT NULL), '{}'::text[]), \
            coalesce(array_agg(m.ready ORDER BY m.joined_at, m.player_id) \
                     FILTER (WHERE m.player_id IS NOT NULL), '{}'::text[]), \
            CASE WHEN p.id IS NULL THEN '' ELSE ",
    rfc3339!("me.lease_until"),
    " END \
       FROM (SELECT 1) AS one \
       LEFT JOIN party.members me ON me.player_id = $1::uuid \
       LEFT JOIN party.parties p ON p.id = me.party_id \
       LEFT JOIN party.members m ON m.party_id = p.id \
      GROUP BY p.id, p.revision, p.leader_id, me.lease_until"
);

const PARTY_VIEW_SQL: &str = "SELECT p.id::text, p.revision, p.leader_id::text, \
        array_agg(m.player_id::text ORDER BY m.joined_at, m.player_id), \
        array_agg(m.ready ORDER BY m.joined_at, m.player_id) \
   FROM party.parties p JOIN party.members m ON m.party_id = p.id \
  WHERE p.id = $1::uuid \
  GROUP BY p.id, p.revision, p.leader_id";

type ViewTuple = (String, i64, String, Vec<String>, Vec<String>, String);
type PartyTuple = (String, i64, String, Vec<String>, Vec<String>);

pub(crate) struct Store {
    pub(crate) pool: PgPool,
}

impl Store {
    pub(crate) async fn player_view(&self, player_id: &str) -> Result<ViewRow, sqlx::Error> {
        let (party_id, revision, leader_id, member_ids, readies, lease_until): ViewTuple =
            sqlx::query_as(PLAYER_VIEW_SQL)
                .bind(player_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(ViewRow {
            party_id,
            revision,
            leader_id,
            member_ids,
            readies,
            lease_until,
        })
    }

    /// `None` once the party is dissolved.
    pub(crate) async fn party_view(&self, party_id: &str) -> Result<Option<ViewRow>, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.party_view_tx(&mut conn, party_id).await
    }

    pub(crate) async fn party_view_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
    ) -> Result<Option<ViewRow>, sqlx::Error> {
        let row: Option<PartyTuple> = sqlx::query_as(PARTY_VIEW_SQL)
            .bind(party_id)
            .fetch_optional(&mut *conn)
            .await?;
        Ok(row.map(|(party_id, revision, leader_id, member_ids, readies)| ViewRow {
            party_id,
            revision,
            leader_id,
            member_ids,
            readies,
            lease_until: String::new(),
        }))
    }

    /// Lease renewal: never stamps a revision. A caller in no party updates nothing.
    pub(crate) async fn renew_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        lease_secs: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE party.members \
                SET lease_until = clock_timestamp() + make_interval(secs => $2::double precision) \
              WHERE player_id = $1::uuid",
        )
        .bind(player_id)
        .bind(lease_secs)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    pub(crate) async fn renew(&self, player_id: &str, lease_secs: i64) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.renew_tx(&mut conn, player_id, lease_secs).await
    }

    pub(crate) async fn party_of_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT party_id::text FROM party.members WHERE player_id = $1::uuid")
            .bind(player_id)
            .fetch_optional(&mut *conn)
            .await
    }

    /// Taken ON THE TX CONNECTION and released at commit/rollback.
    pub(crate) async fn lock_party_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(party_lock_key(party_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// The party the player LEADS, if any.
    pub(crate) async fn led_party(&self, player_id: &str) -> Result<Option<String>, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.led_party_tx(&mut conn, player_id).await
    }

    pub(crate) async fn led_party_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT m.party_id::text FROM party.members m \
               JOIN party.parties p ON p.id = m.party_id \
              WHERE m.player_id = $1::uuid AND p.leader_id = $1::uuid",
        )
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await
    }

    pub(crate) async fn is_member_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM party.members WHERE player_id = $1::uuid AND party_id = $2::uuid",
        )
        .bind(player_id)
        .bind(party_id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(found.is_some())
    }

    pub(crate) async fn count_members_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT count(*)::bigint FROM party.members WHERE party_id = $1::uuid")
            .bind(party_id)
            .fetch_one(&mut *conn)
            .await
    }

    /// The ONE source of a revision. Callers reach it only through [`Store::create_tx`],
    /// [`Store::join_tx`], [`Store::remove_member_tx`], [`Store::remove_expired_tx`] and
    /// [`Store::stamp_tx`], each of which runs its `members` INSERT/DELETE first: an INSERT of
    /// a player's row waits out a concurrent uncommitted DELETE of it, so drawing after the
    /// write is what puts a player's join strictly above their concurrent leave.
    async fn next_revision_tx(&self, conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT nextval('party.revision_seq')")
            .fetch_one(&mut *conn)
            .await
    }

    async fn record_player_revision_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        revision: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO party.player_revisions (player_id, revision) VALUES ($1::uuid, $2) \
             ON CONFLICT (player_id) DO UPDATE SET revision = EXCLUDED.revision",
        )
        .bind(player_id)
        .bind(revision)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    async fn insert_member_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        party_id: Option<&str>,
        lease_secs: i64,
    ) -> Result<String, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO party.members (player_id, party_id, ready, joined_at, lease_until) \
             VALUES ($1::uuid, coalesce($2::uuid, gen_random_uuid()), 'not_ready', \
                     clock_timestamp(), \
                     clock_timestamp() + make_interval(secs => $3::double precision)) \
             RETURNING party_id::text",
        )
        .bind(player_id)
        .bind(party_id)
        .bind(lease_secs)
        .fetch_one(&mut *conn)
        .await
    }

    /// A new party led by `player_id`. A `23505` means the player is already in a party.
    /// Answers `(party_id, revision)`.
    pub(crate) async fn create_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        lease_secs: i64,
    ) -> Result<(String, i64), sqlx::Error> {
        let party_id = self
            .insert_member_tx(conn, player_id, None, lease_secs)
            .await?;
        let revision = self.next_revision_tx(conn).await?;
        sqlx::query(
            "INSERT INTO party.parties (id, leader_id, revision) VALUES ($1::uuid, $2::uuid, $3)",
        )
        .bind(&party_id)
        .bind(player_id)
        .bind(revision)
        .execute(&mut *conn)
        .await?;
        self.record_player_revision_tx(conn, player_id, revision).await?;
        Ok((party_id, revision))
    }

    /// Adds `player_id` to a LOCKED party. A `23505` means the player is already in a party.
    pub(crate) async fn join_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        player_id: &str,
        lease_secs: i64,
    ) -> Result<i64, sqlx::Error> {
        self.insert_member_tx(conn, player_id, Some(party_id), lease_secs)
            .await?;
        let revision = self.next_revision_tx(conn).await?;
        sqlx::query("UPDATE party.parties SET revision = $2 WHERE id = $1::uuid")
            .bind(party_id)
            .bind(revision)
            .execute(&mut *conn)
            .await?;
        self.record_player_revision_tx(conn, player_id, revision).await?;
        Ok(revision)
    }

    /// Stamps a LOCKED party whose roster this transaction did not change (promote,
    /// set_ready), optionally handing leadership to `leader_id`.
    pub(crate) async fn stamp_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        leader_id: Option<&str>,
    ) -> Result<i64, sqlx::Error> {
        let revision = self.next_revision_tx(conn).await?;
        sqlx::query(
            "UPDATE party.parties SET revision = $2, leader_id = coalesce($3::uuid, leader_id) \
              WHERE id = $1::uuid",
        )
        .bind(party_id)
        .bind(revision)
        .bind(leader_id)
        .execute(&mut *conn)
        .await?;
        Ok(revision)
    }

    /// Removes `player_id` from a LOCKED party under the leave rule. `None` when the player
    /// is not a member of that party.
    pub(crate) async fn remove_member_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        player_id: &str,
    ) -> Result<Option<Removal>, sqlx::Error> {
        let removed: Vec<String> = sqlx::query_scalar(
            "DELETE FROM party.members WHERE player_id = $1::uuid AND party_id = $2::uuid \
             RETURNING player_id::text",
        )
        .bind(player_id)
        .bind(party_id)
        .fetch_all(&mut *conn)
        .await?;
        if removed.is_empty() {
            return Ok(None);
        }
        self.settle_removal_tx(conn, party_id, removed).await.map(Some)
    }

    /// Removes every member of a LOCKED party whose lease has expired, re-checked against
    /// `now()` here, under the lock. `None` when none has.
    pub(crate) async fn remove_expired_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
    ) -> Result<Option<Removal>, sqlx::Error> {
        let mut removed: Vec<String> = sqlx::query_scalar(
            "DELETE FROM party.members WHERE party_id = $1::uuid AND lease_until < now() \
             RETURNING player_id::text",
        )
        .bind(party_id)
        .fetch_all(&mut *conn)
        .await?;
        if removed.is_empty() {
            return Ok(None);
        }
        removed.sort();
        self.settle_removal_tx(conn, party_id, removed).await.map(Some)
    }

    /// The leave rule after the DELETEs: the leader stays if still a member, else the
    /// earliest `(joined_at, player_id)` remaining member leads; no member left dissolves the
    /// party and its invites. Every removed player's `player_revisions` takes the stamp.
    async fn settle_removal_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        removed: Vec<String>,
    ) -> Result<Removal, sqlx::Error> {
        let leader: Option<String> = sqlx::query_scalar(
            "SELECT m.player_id::text FROM party.members m \
               JOIN party.parties p ON p.id = m.party_id \
              WHERE m.party_id = $1::uuid \
              ORDER BY (m.player_id = p.leader_id) DESC, m.joined_at, m.player_id \
              LIMIT 1",
        )
        .bind(party_id)
        .fetch_optional(&mut *conn)
        .await?;
        let revision = self.next_revision_tx(conn).await?;
        match &leader {
            Some(leader) => {
                sqlx::query(
                    "UPDATE party.parties SET revision = $2, leader_id = $3::uuid \
                      WHERE id = $1::uuid",
                )
                .bind(party_id)
                .bind(revision)
                .bind(leader)
                .execute(&mut *conn)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM party.invites WHERE party_id = $1::uuid")
                    .bind(party_id)
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("DELETE FROM party.parties WHERE id = $1::uuid")
                    .bind(party_id)
                    .execute(&mut *conn)
                    .await?;
            }
        }
        for player_id in &removed {
            self.record_player_revision_tx(conn, player_id, revision).await?;
        }
        Ok(Removal {
            removed,
            revision,
            dissolved: leader.is_none(),
        })
    }

    /// Sets the caller's ready state and renews its lease in a LOCKED party. `false` when the
    /// caller is not a member of it.
    pub(crate) async fn set_ready_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        player_id: &str,
        state: &str,
        lease_secs: i64,
    ) -> Result<bool, sqlx::Error> {
        let done = sqlx::query(
            "UPDATE party.members \
                SET ready = $3, \
                    lease_until = clock_timestamp() + make_interval(secs => $4::double precision) \
              WHERE player_id = $1::uuid AND party_id = $2::uuid",
        )
        .bind(player_id)
        .bind(party_id)
        .bind(state)
        .bind(lease_secs)
        .execute(&mut *conn)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// The party an unexpired invite addressed to `invitee_id` names.
    pub(crate) async fn pending_invite_party(
        &self,
        invite_id: &str,
        invitee_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT party_id::text FROM party.invites \
              WHERE id = $1::uuid AND invitee_id = $2::uuid AND expires_at > now()",
        )
        .bind(invite_id)
        .bind(invitee_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Consumes the invite under its party's lock; `false` when it is gone, expired or no
    /// longer the caller's.
    pub(crate) async fn consume_invite_tx(
        &self,
        conn: &mut PgConnection,
        invite_id: &str,
        invitee_id: &str,
        party_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let done = sqlx::query(
            "DELETE FROM party.invites \
              WHERE id = $1::uuid AND invitee_id = $2::uuid AND party_id = $3::uuid \
                AND expires_at > now()",
        )
        .bind(invite_id)
        .bind(invitee_id)
        .bind(party_id)
        .execute(&mut *conn)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Issues or reissues (fresh TTL, same id) the invite of `invitee_id` to a LOCKED party.
    pub(crate) async fn upsert_invite_tx(
        &self,
        conn: &mut PgConnection,
        party_id: &str,
        inviter_id: &str,
        invitee_id: &str,
        ttl_secs: i64,
    ) -> Result<InviteRow, sqlx::Error> {
        let (invite_id, party_id, inviter_id, expires_at): (String, String, String, String) =
            sqlx::query_as(concat!(
                "INSERT INTO party.invites (id, party_id, inviter_id, invitee_id, expires_at) \
                 VALUES (gen_random_uuid(), $1::uuid, $2::uuid, $3::uuid, \
                         now() + make_interval(secs => $4::double precision)) \
                 ON CONFLICT (party_id, invitee_id) DO UPDATE \
                    SET inviter_id = EXCLUDED.inviter_id, expires_at = EXCLUDED.expires_at \
                 RETURNING id::text, party_id::text, inviter_id::text, ",
                rfc3339!("expires_at")
            ))
            .bind(party_id)
            .bind(inviter_id)
            .bind(invitee_id)
            .bind(ttl_secs)
            .fetch_one(&mut *conn)
            .await?;
        Ok(InviteRow {
            invite_id,
            party_id,
            inviter_id,
            expires_at,
        })
    }

    pub(crate) async fn invites_of(&self, invitee_id: &str) -> Result<Vec<InviteRow>, sqlx::Error> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(concat!(
            "SELECT id::text, party_id::text, inviter_id::text, ",
            rfc3339!("expires_at"),
            " FROM party.invites WHERE invitee_id = $1::uuid AND expires_at > now() \
              ORDER BY expires_at, id"
        ))
        .bind(invitee_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(invite_id, party_id, inviter_id, expires_at)| InviteRow {
                invite_id,
                party_id,
                inviter_id,
                expires_at,
            })
            .collect())
    }

    pub(crate) async fn decline(&self, invite_id: &str, invitee_id: &str) -> Result<bool, sqlx::Error> {
        let done = sqlx::query(
            "DELETE FROM party.invites WHERE id = $1::uuid AND invitee_id = $2::uuid",
        )
        .bind(invite_id)
        .bind(invitee_id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    pub(crate) async fn delete_invites_involving_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "DELETE FROM party.invites WHERE invitee_id = $1::uuid OR inviter_id = $1::uuid",
        )
        .bind(player_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    pub(crate) async fn delete_player_revision_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM party.player_revisions WHERE player_id = $1::uuid")
            .bind(player_id)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// A sweep transaction: the checkout bounded by `acquire`, every statement in it by
    /// `statement_timeout` (which also bounds an advisory-lock wait). `SET LOCAL` reverts at
    /// commit, so the bound never leaks into the next borrower of the pooled connection.
    pub(crate) async fn begin_bounded(
        &self,
        acquire: std::time::Duration,
        statement_timeout: std::time::Duration,
    ) -> anyhow::Result<Transaction<'static, Postgres>> {
        let mut tx = tokio::time::timeout(acquire, self.pool.begin())
            .await
            .map_err(|_| {
                anyhow::anyhow!("party: pool checkout timed out after {}s", acquire.as_secs())
            })??;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = {}",
            statement_timeout.as_millis().clamp(1, i32::MAX as u128)
        ))
        .execute(&mut *tx)
        .await?;
        Ok(tx)
    }

    pub(crate) async fn expired_parties(&self, limit: i64) -> anyhow::Result<Vec<String>> {
        let mut tx = self
            .begin_bounded(SWEEP_ACQUIRE_DEADLINE, SWEEP_STATEMENT_TIMEOUT)
            .await?;
        let parties: Vec<String> = sqlx::query_scalar(
            "SELECT party_id::text FROM party.members WHERE lease_until < now() \
              GROUP BY party_id ORDER BY min(lease_until) LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(parties)
    }

    pub(crate) async fn delete_expired_invites(&self) -> anyhow::Result<u64> {
        let mut tx = self
            .begin_bounded(SWEEP_ACQUIRE_DEADLINE, SWEEP_STATEMENT_TIMEOUT)
            .await?;
        let done = sqlx::query("DELETE FROM party.invites WHERE expires_at < now()")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(done.rows_affected())
    }
}

/// What a leave-rule removal committed: who left, the stamp, and whether the party is gone.
pub(crate) struct Removal {
    pub(crate) removed: Vec<String>,
    pub(crate) revision: i64,
    pub(crate) dissolved: bool,
}

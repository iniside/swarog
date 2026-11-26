//! The SQL layer for the `accounts` schema — pure persistence, no event/bus
//! knowledge (port of Go's `modules/accounts/store.go`). Write paths that must be
//! atomic with the `player.registered` durable event append take `&mut PgConnection` so the
//! caller (the service) owns the transaction; reads use the pool.

use base64::Engine as _;
use rand::RngCore as _;
use sqlx::{PgConnection, PgPool};

/// Access-session lifetime, applied in SQL. Short on purpose: a leaked bearer dies
/// within the hour and the client renews through its rotating refresh token.
pub(crate) const ACCESS_TTL_MINUTES: i32 = 60;

/// The HARD life of a refresh family, applied in SQL when a family is born. A rotation
/// never extends it — see [`Store::insert_refresh_successor_tx`].
pub(crate) const REFRESH_TTL_DAYS: i32 = 30;

/// How long after its rotation a consumed refresh token still answers with the
/// successor it recorded instead of being treated as theft. The failure this covers is
/// a client that rotated and lost the RESPONSE (mobile handoff, a 429 from the
/// gateway's rate limiter), not two racing dials. `f64` because Postgres'
/// `make_interval(secs => …)` takes double precision.
pub(crate) const REFRESH_GRACE_SECONDS: f64 = 30.0;

/// How long a `begin_delete` confirmation ticket stays live. Short for the same reason
/// the OAuth state store's window is: it exists to carry one user confirmation across a
/// couple of screens, not to be stored.
pub(crate) const DELETE_TICKET_TTL_MINUTES: i32 = 10;

/// The lifetime the API reports for a freshly minted access token, derived from the
/// SAME constant the INSERT applies so the number a client schedules against cannot
/// drift from the number the row expires by.
pub(crate) fn access_expires_in_secs() -> i64 {
    i64::from(ACCESS_TTL_MINUTES) * 60
}

/// The product-scoped identity row (`accounts.players`). Module-private: the wire
/// types (`Session`/`MeView`) live in `accountsapi`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Player {
    pub id: String,
    pub display_name: String,
}

/// Typed store outcomes the service maps onto `opsapi::Status` (Go's ErrEmailTaken /
/// ErrInvalidCredentials / ErrIdentityLinked, as enums instead of sentinel errors).
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// A `(provider, subject)` unique violation on registration/linking.
    #[error("identity already registered")]
    Taken,
    /// Every minting attempt for this display name collided on the handle index.
    #[error("no free handle for this display name")]
    HandleExhausted,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// `true` for a Postgres unique violation (23505) — a duplicate email / an already
/// linked external identity (Go's `isUniqueViolation`).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// `true` for a Postgres foreign-key violation (23503) — on the delete-ticket upsert or
/// a browser session insert, a player that no longer exists.
pub(crate) fn is_foreign_key_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23503"))
}

/// `true` for a Postgres invalid-text-representation (22P02) — an id the `::uuid` cast
/// could not parse. The delete path folds it into the same `NotFound` an unknown player
/// gets, rather than a 500 (friends' precedent).
pub(crate) fn is_invalid_uuid(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("22P02"))
}

/// How many discriminators one display name may try before minting fails. The primary
/// key of `accounts.handles` is the mint-time freeness check: each attempt INSERTs a
/// claim and reads back whether the key accepted it, so there is no window between a
/// check and the write for a concurrent registration — or for a deletion's permanent
/// reservation — to slip through. `accounts.players`' `accounts_handle_idx` is a second,
/// DB-level constraint on the same pair (`Module::migrate` asserts every player is
/// claimed so it can never trip). A name whose free discriminators are thinned by
/// reservations fails minting sooner; that is the cost of never recycling a handle.
const HANDLE_MINT_ATTEMPTS: usize = 8;

/// One four-digit discriminator candidate, `"0000"`..=`"9999"`.
fn new_discriminator() -> String {
    format!("{:04}", rand::rngs::OsRng.next_u32() % 10_000)
}

/// True for the canonical 8-4-4-4-12 hex spelling — a STRICT subset of what the uuid
/// cast parses, so an id rejected here is one Postgres could still have parsed (a
/// braced spelling), never the reverse. The batch lookup filters with this before
/// binding: ONE malformed element fails the whole statement (22P02), which would turn
/// a caller's stale id into an error instead of an omission.
pub(crate) fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Splits `"Name#1234"` into its name and discriminator halves. The LAST `'#'` divides
/// them — a display name may contain one. `None` for anything that is not a name plus
/// exactly four digits.
fn split_handle(handle: &str) -> Option<(&str, &str)> {
    let (name, disc) = handle.rsplit_once('#')?;
    if name.is_empty() || disc.len() != 4 || !disc.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((name, disc))
}

/// The one projection every [`accountsapi::PlayerSummary`] read shares: the summary
/// columns plus the newest live access token's expiry over the player's token families,
/// rendered RFC3339 (empty when none). `max(...)` over a join FILTERED to live families,
/// so one statement answers for a whole batch — never a per-player follow-up query.
const SUMMARY_SELECT: &str = "SELECT p.id::text, p.display_name, p.discriminator, \
     coalesce(to_char(max(f.access_expires_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), '') \
       FROM accounts.players p \
       LEFT JOIN accounts.token_families f ON f.player_id = p.id AND f.access_expires_at > now() ";

/// The ONE rendering of a `"Name#1234"` handle.
pub(crate) fn render_handle(display_name: &str, discriminator: &str) -> String {
    format!("{display_name}#{discriminator}")
}

/// A `timestamptz` column rendered as the RFC3339 instant the wire types here carry.
fn rfc3339(column: &str) -> String {
    format!("to_char({column} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')")
}

fn summary_of(row: (String, String, String, String)) -> accountsapi::PlayerSummary {
    let (player_id, display_name, discriminator, online_until) = row;
    accountsapi::PlayerSummary {
        handle: render_handle(&display_name, &discriminator),
        player_id,
        display_name,
        online_until,
    }
}

/// Domain-separated stable FNV-1a key for serializing every writer of one external
/// `(provider, subject)` identity. Hash collisions only add serialization.
pub(crate) fn identity_lock_key(provider: &str, subject: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in b"accounts.identity.v1\0"
        .iter()
        .chain(provider.as_bytes())
        .chain([0].iter())
        .chain(subject.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as i64
}

/// Domain-separated stable FNV-1a key for serializing every writer of one PLAYER's
/// identity set. Distinct from [`identity_lock_key`] on purpose: that one serializes
/// writers of a single `(provider, subject)`, which two links of DIFFERENT providers
/// to the same player never share — only this key makes them mutually exclusive.
pub(crate) fn player_lock_key(player_id: &str) -> i64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in b"accounts.player.v1\0".iter().chain(player_id.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash as i64
}

/// What a link attempt did to `accounts.identities`, so the caller decides whether
/// anything happened without asking the database a second time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkOutcome {
    /// The identity row was inserted by this call.
    Linked,
    /// The identity already belonged to this player; nothing was written.
    AlreadyLinked,
}

/// A fresh opaque bearer token: 32 random bytes, base64url without padding — Go's
/// `newToken` byte-for-byte (43 chars).
pub(crate) fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The stored facts about one presented refresh token, read after a rotation attempt
/// declined it. Booleans are evaluated by Postgres against the SAME `now()` the failed
/// UPDATE used, so the classification cannot disagree with the predicate that produced
/// it.
pub(crate) struct RefreshRow {
    pub player_id: String,
    pub family_id: String,
    /// The successor recorded when this token was consumed.
    pub replaced_by: Option<String>,
    pub used: bool,
    pub used_within_grace: bool,
    pub expired: bool,
}

/// What a presented refresh token earns. The ONE decision every refresh outcome comes
/// from — deliberately pure, so the theft branch is executable without a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefreshVerdict {
    /// The token was live and unused: it is now consumed in favour of `successor`.
    Rotate { player_id: String, family_id: String },
    /// A consumed token presented within the grace window — the lost-response case.
    /// The recorded successor is handed back; nothing is revoked.
    GraceReplay {
        player_id: String,
        family_id: String,
        successor: String,
    },
    /// A consumed token presented too late to be a lost response: a stolen credential.
    /// The family dies and the caller still gets the plain 401.
    Revoke { family_id: String },
    /// Unknown or expired — 401, and nothing to revoke.
    Deny,
}

/// Classifies one presentation from the rotation attempt's outcome and the row behind
/// it. `rotated` is [`Store::rotate_refresh_tx`]'s return, `row` is
/// [`Store::refresh_row_tx`]'s, read on the same transaction.
///
/// Every ambiguous state falls through to [`RefreshVerdict::Deny`]: a row that is
/// neither used nor expired yet refused to rotate, and a consumed row with no recorded
/// successor, are both states a single-transaction rotation cannot produce, so the
/// answer is the fail-closed one rather than a guess that hands out a session.
pub(crate) fn classify_presentation(
    rotated: Option<(String, String)>,
    row: Option<RefreshRow>,
) -> RefreshVerdict {
    if let Some((player_id, family_id)) = rotated {
        return RefreshVerdict::Rotate {
            player_id,
            family_id,
        };
    }
    let Some(row) = row else {
        return RefreshVerdict::Deny;
    };
    if row.expired || !row.used {
        return RefreshVerdict::Deny;
    }
    match row.replaced_by {
        Some(successor) if row.used_within_grace => RefreshVerdict::GraceReplay {
            player_id: row.player_id,
            family_id: row.family_id,
            successor,
        },
        Some(_) => RefreshVerdict::Revoke {
            family_id: row.family_id,
        },
        None => RefreshVerdict::Deny,
    }
}

/// One player as the admin portal lists it.
pub(crate) struct PlayerListRow {
    pub id: String,
    pub display_name: String,
    pub discriminator: String,
    /// `YYYY-MM-DD`, UTC.
    pub created: String,
    pub created_at_micros: i64,
    /// The linked providers, sorted and distinct.
    pub providers: Vec<String>,
    /// Seconds since the player's last token mint, login or refresh, on the database clock;
    /// `None` once every family of the player is revoked or pruned.
    pub last_seen_secs: Option<i64>,
}

/// [`PlayerListRow`]'s columns, in field order; the providers comma-joined, then the
/// seconds since the newest access token of the player's families expires (negative while
/// it is live).
type PlayerListColumns = (String, String, String, String, i64, String, Option<i64>);

/// [`PlayerListRow`]'s projection. The two correlated reads use `identities_player_idx` and
/// `token_families_player_idx`. Every mint, login or refresh, moves its family's
/// `access_expires_at` to [`ACCESS_TTL_MINUTES`] past the mint, so the newest expiry less
/// that TTL is the player's last mint; [`player_list_row`] adds the TTL back.
const PLAYER_LIST_SELECT: &str = "SELECT p.id::text, p.display_name, p.discriminator, \
        to_char(p.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD'), \
        (EXTRACT(EPOCH FROM p.created_at) * 1000000)::int8, \
        coalesce((SELECT string_agg(DISTINCT i.provider, ',' ORDER BY i.provider) \
                    FROM accounts.identities i WHERE i.player_id = p.id), ''), \
        (SELECT floor(EXTRACT(EPOCH FROM (now() - max(f.access_expires_at))))::int8 \
           FROM accounts.token_families f WHERE f.player_id = p.id) \
   FROM accounts.players p ";

fn player_list_row(row: PlayerListColumns) -> PlayerListRow {
    let (id, display_name, discriminator, created, created_at_micros, providers, since_expiry) = row;
    let last_seen_secs = since_expiry.map(|secs| secs + access_expires_in_secs());
    PlayerListRow {
        id,
        display_name,
        discriminator,
        created,
        created_at_micros,
        providers: providers
            .split(',')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        last_seen_secs,
    }
}

/// One page of the admin players list, newest first.
pub(crate) struct PlayerListing<'a> {
    /// Matches an exact player id, an exact `Name#1234` handle, an exact `dev` email, or a
    /// substring of the display name.
    pub needle: Option<&'a str>,
    /// Only players with an identity of this provider.
    pub provider: Option<&'a str>,
    /// The `(created_at µs, id)` keyset of the last row already shown.
    pub after: Option<(i64, &'a str)>,
    pub limit: i64,
}

/// One identity as the admin portal shows it.
pub(crate) struct IdentityRow {
    pub provider: String,
    pub subject: String,
    /// `YYYY-MM-DD`, UTC.
    pub linked: String,
    /// Whole seconds since the newest session minted through it; `None` when none was.
    pub last_used_secs: Option<i64>,
}

/// One refresh family that still grants access: a login on one device, whatever its
/// token rotations.
pub(crate) struct FamilyRow {
    pub family_id: String,
    /// The family's first issue, `YYYY-MM-DD HH24:MI` UTC.
    pub started: String,
    /// The same instant in microseconds since the epoch: the Sessions panel's sort key.
    pub started_us: i64,
    /// Seconds until the last of the family's tokens expires, on the database clock.
    pub expires_in_secs: i64,
}

/// The tables whose rows each belong to one token family and die at `expires_at`, the
/// children [`Store::prune_expired_token_rows`] drains (alias `c` in [`EXPIRED_CHILD`]).
const PRUNED_FAMILY_CHILDREN: [&str; 2] = ["accounts.sessions", "accounts.refresh_tokens"];

/// A child row (alias `c`) the prune deletes: an expired one, consumed or not.
const EXPIRED_CHILD: &str = "c.expires_at <= now()";

/// A family (alias `f`) that no longer grants access ([`granting_families`]): its newest
/// access token has expired and it holds no unexpired refresh token.
const STALE_FAMILY: &str = "f.access_expires_at <= now() \
     AND NOT EXISTS (SELECT 1 FROM accounts.refresh_tokens t \
                      WHERE t.family_id = f.id AND t.expires_at > now())";

/// The token families of player `$1` that still grant access, as one statement
/// selecting `$select` per family and ending in `$tail`. A family grants access while its
/// newest access token is unexpired or it holds an unexpired refresh token; per family,
/// `since` is when its rows were minted and `expires_at` when they expire. The Sessions panel
/// and Revoke all both read this one predicate, so the panel lists what the modal counts.
macro_rules! granting_families {
    ($select:literal, $tail:literal) => {
        concat!(
            "SELECT ", $select, " FROM (\
               SELECT family_id, issued_at AS since, expires_at FROM accounts.refresh_tokens \
                WHERE player_id = $1::uuid \
               UNION ALL \
               SELECT id, created_at, access_expires_at FROM accounts.token_families \
                WHERE player_id = $1::uuid\
             ) AS family_rows \
             GROUP BY family_id HAVING bool_or(expires_at > now()) ",
            $tail,
        )
    };
}

pub(crate) struct Store {
    pub pool: PgPool,
}

impl Store {
    /// Creates a player and its first identity ON THE GIVEN CONNECTION (the caller's
    /// tx), so a failed identity insert rolls back the orphaned player AND the
    /// caller can ride its `player.registered` durable event append on the same tx. A
    /// `(provider, subject)` collision is [`StoreError::Taken`].
    pub async fn insert_player_with_identity_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
        display_name: &str,
        secret_hash: Option<&str>,
    ) -> Result<Player, StoreError> {
        let mut minted: Option<String> = None;
        for _ in 0..HANDLE_MINT_ATTEMPTS {
            let discriminator = new_discriminator();
            let Some(id) = self
                .claim_handle_tx(&mut *conn, display_name, &discriminator)
                .await?
            else {
                continue;
            };
            sqlx::query(
                "INSERT INTO accounts.players (id, display_name, discriminator) \
                 VALUES ($1::uuid, $2, $3)",
            )
            .bind(&id)
            .bind(display_name)
            .bind(&discriminator)
            .execute(&mut *conn)
            .await?;
            minted = Some(id);
            break;
        }
        let Some(id) = minted else {
            return Err(StoreError::HandleExhausted);
        };
        let display_name = display_name.to_string();
        let res = sqlx::query(
            "INSERT INTO accounts.identities (provider, subject, player_id, secret_hash) \
             VALUES ($1, $2, $3::uuid, $4)",
        )
        .bind(provider)
        .bind(subject)
        .bind(&id)
        .bind(secret_hash)
        .execute(&mut *conn)
        .await;
        match res {
            Ok(_) => Ok(Player { id, display_name }),
            Err(e) if is_unique_violation(&e) => Err(StoreError::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// Claims one `(lower(display_name), discriminator)` pair, answering the player id
    /// minted with it, or `None` when the pair is already claimed — by a live player OR
    /// by a deletion's permanent reservation, which are the same row shape and the same
    /// refusal. The id is minted HERE so the claim can precede the player row it belongs
    /// to; the caller inserts that row on the same transaction, so a rollback frees the
    /// claim with it.
    ///
    /// ON CONFLICT DO NOTHING, not a caught 23505: a raised unique violation would abort
    /// the caller's whole transaction, leaving nothing to retry into.
    async fn claim_handle_tx(
        &self,
        conn: &mut PgConnection,
        display_name: &str,
        discriminator: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "INSERT INTO accounts.handles (display_name_lower, discriminator, player_id) \
             VALUES (lower($1), $2, gen_random_uuid()) \
             ON CONFLICT (display_name_lower, discriminator) DO NOTHING \
             RETURNING player_id::text",
        )
        .bind(display_name)
        .bind(discriminator)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// Mints or re-reads the ONE live delete ticket of a player, answering
    /// `(ticket, expires_at)`. A player already holding a live ticket gets that same
    /// ticket and its original expiry back: the conflict arm keeps the stored values
    /// while they are live and replaces them only once they are not, so a repeated
    /// `begin_delete` can neither invalidate an outstanding confirmation nor extend one
    /// indefinitely. One statement, so two concurrent calls cannot each mint.
    ///
    /// `None` is a `player_id` that names no player — the foreign key's rejection, or an
    /// id the uuid cast could not parse at all; the caller answers `NotFound` for both.
    pub async fn upsert_delete_ticket(
        &self,
        player_id: &str,
        ticket: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let res: Result<(String, String), sqlx::Error> = sqlx::query_as(&format!(
            "INSERT INTO accounts.delete_tickets (player_id, ticket, expires_at) \
             VALUES ($1::uuid, $2, now() + make_interval(mins => $3)) \
             ON CONFLICT (player_id) DO UPDATE SET \
               ticket = CASE WHEN accounts.delete_tickets.expires_at > now() \
                             THEN accounts.delete_tickets.ticket ELSE excluded.ticket END, \
               expires_at = CASE WHEN accounts.delete_tickets.expires_at > now() \
                             THEN accounts.delete_tickets.expires_at ELSE excluded.expires_at END \
             RETURNING ticket, {}",
            rfc3339("expires_at")
        ))
        .bind(player_id)
        .bind(ticket)
        .bind(DELETE_TICKET_TTL_MINUTES)
        .fetch_one(&self.pool)
        .await;
        match res {
            Ok(row) => Ok(Some(row)),
            Err(e) if is_foreign_key_violation(&e) || is_invalid_uuid(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Consumes the caller's live delete ticket on the caller's transaction, answering
    /// the DB-canonical spelling of the player id (wallet's precedent: what the receipt
    /// and the event carry is the database's spelling, never the caller's). A ticket
    /// belonging to another player is simply not this player's row, so it reads the same
    /// as an unknown one — the caller owes both the same `NotFound`.
    pub async fn consume_delete_ticket_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        ticket: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "DELETE FROM accounts.delete_tickets \
              WHERE player_id = $1::uuid AND ticket = $2 AND expires_at > now() \
             RETURNING player_id::text",
        )
        .bind(player_id)
        .bind(ticket)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id,)| id))
    }

    /// The `"Name#1234"` of a player on the caller's transaction, read from the two
    /// columns the claim was minted against so the reserved pair and the reported handle
    /// cannot disagree.
    pub async fn handle_of_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT display_name, discriminator FROM accounts.players WHERE id = $1::uuid",
        )
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(display_name, discriminator)| render_handle(&display_name, &discriminator)))
    }

    /// Reserves a player's handle FOREVER on the caller's transaction, from the player's
    /// own row — so the reserved pair is by construction the pair the claim holds. The
    /// claim row survives the player: `player_id` goes NULL (the deleted player is not
    /// named by the registry that outlives it) and the pair stays unmintable, since
    /// [`Store::claim_handle_tx`] asks the same primary key.
    ///
    /// The INSERT arm covers a player whose claim predates this table; the UPDATE arm is
    /// the ordinary release. Run BEFORE the player row is deleted — it reads it.
    pub async fn reserve_handle_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO accounts.handles (display_name_lower, discriminator, player_id) \
             SELECT lower(display_name), discriminator, NULL \
               FROM accounts.players WHERE id = $1::uuid \
             ON CONFLICT (display_name_lower, discriminator) DO UPDATE SET player_id = NULL",
        )
        .bind(player_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Writes the permanent deletion receipt on the caller's transaction and answers it.
    pub async fn insert_deletion_receipt_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        handle: &str,
        ticket: &str,
    ) -> Result<accountsapi::DeleteReceipt, sqlx::Error> {
        let (deleted_at,): (String,) = sqlx::query_as(&format!(
            "INSERT INTO accounts.deletion_receipts (player_id, handle, ticket) \
             VALUES ($1::uuid, $2, $3) RETURNING {}",
            rfc3339("deleted_at")
        ))
        .bind(player_id)
        .bind(handle)
        .bind(ticket)
        .fetch_one(&mut *conn)
        .await?;
        Ok(accountsapi::DeleteReceipt {
            player_id: player_id.to_string(),
            handle: handle.to_string(),
            deleted_at,
        })
    }

    /// The receipt of an ALREADY completed deletion, matched on the ticket that produced
    /// it — what an internal-edge replay is answered from once the player, and with it
    /// the ticket row, are gone. A different ticket answers nothing, so the receipt can
    /// only be re-read by the call that wrote it.
    pub async fn deletion_receipt_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        ticket: &str,
    ) -> Result<Option<accountsapi::DeleteReceipt>, sqlx::Error> {
        let row: Option<(String, String, String)> = sqlx::query_as(&format!(
            "SELECT player_id::text, handle, {} FROM accounts.deletion_receipts \
              WHERE player_id = $1::uuid AND ticket = $2",
            rfc3339("deleted_at")
        ))
        .bind(player_id)
        .bind(ticket)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(player_id, handle, deleted_at)| accountsapi::DeleteReceipt {
            player_id,
            handle,
            deleted_at,
        }))
    }

    /// Deletes the player row on the caller's transaction, answering whether it was
    /// there. Its sessions end first through [`Store::kill_player_families_tx`] — THE
    /// session revocation, committing with whatever else this transaction carries — and
    /// the schema's `ON DELETE CASCADE`s take its identities and delete ticket.
    pub async fn delete_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        if self.kill_player_families_tx(conn, player_id).await?.is_none() {
            return Ok(false);
        }
        let res = sqlx::query("DELETE FROM accounts.players WHERE id = $1::uuid")
            .bind(player_id)
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The player and stored hash for a dev identity, or `Ok(None)` when there is no
    /// such identity OR no stored hash — the same "invalid credentials" answer as a
    /// bad password, so the endpoint doesn't leak which emails exist.
    pub async fn password_identity(
        &self,
        email: &str,
    ) -> Result<Option<(Player, String)>, sqlx::Error> {
        let row: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name, i.secret_hash \
               FROM accounts.identities i \
               JOIN accounts.players p ON p.id = i.player_id \
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(crate::providers::PASSWORD)
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some((id, display_name, Some(hash))) => Some((Player { id, display_name }, hash)),
            _ => None,
        })
    }

    /// Replaces the stored hash of every `password` identity of a player on the caller's
    /// tx, answering their emails (sorted); empty when the player signs in with no password.
    pub async fn set_password_hash_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        secret_hash: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        let mut emails: Vec<String> = sqlx::query_scalar(
            "UPDATE accounts.identities SET secret_hash = $3 \
              WHERE provider = $1 AND player_id = $2::uuid \
             RETURNING subject",
        )
        .bind(crate::providers::PASSWORD)
        .bind(player_id)
        .bind(secret_hash)
        .fetch_all(&mut *conn)
        .await?;
        emails.sort_unstable();
        Ok(emails)
    }

    /// The emails of a player's `password` identities, sorted; empty when it has none.
    pub async fn password_emails(&self, player_id: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT subject FROM accounts.identities \
              WHERE provider = $1 AND player_id = $2::uuid ORDER BY subject",
        )
        .bind(crate::providers::PASSWORD)
        .bind(player_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Whether `subject` names a guest identity whose stored digest is exactly
    /// `secret_hash`. Subject and digest are matched in ONE predicate on purpose: an
    /// unknown subject and a wrong secret must be the same answer, and a lookup that
    /// returned the row first would hand its caller the oracle. A SIBLING of
    /// [`Store::password_identity`] rather than a widening of it — that one resolves a
    /// password identity by EMAIL, and a guest subject must never match it.
    pub async fn guest_identity_matches(
        &self,
        subject: &str,
        secret_hash: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.identities \
              WHERE provider = $1 AND subject = $2 AND secret_hash = $3",
        )
        .bind(crate::providers::GUEST)
        .bind(subject)
        .bind(secret_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Takes the transaction-scoped writer lock for one external identity. Callers
    /// make this the first statement after BEGIN, then re-read under the lock.
    pub async fn lock_identity_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(identity_lock_key(provider, subject))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// The player an external identity maps to on the caller's locked transaction.
    pub async fn player_by_identity_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
    ) -> Result<Option<Player>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name \
               FROM accounts.identities i \
               JOIN accounts.players p ON p.id = i.player_id \
              WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(|(id, display_name)| Player { id, display_name }))
    }

    /// Takes the transaction-scoped writer lock for one PLAYER's identity set. A
    /// caller that takes this AND [`Store::lock_identity_tx`] takes this one FIRST.
    pub async fn lock_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(player_lock_key(player_id))
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Whether `player_id` already holds an identity from a provider other than
    /// `guest`, on the caller's locked transaction. Read BEFORE the link insert:
    /// after it, the answer would include the row being written.
    pub async fn has_real_identity_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.identities \
              WHERE player_id = $1::uuid AND provider <> $2 LIMIT 1",
        )
        .bind(player_id)
        .bind(crate::providers::GUEST)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.is_some())
    }

    /// Attaches an already-verified external identity ON THE CALLER'S transaction,
    /// under the locks the caller took. Same-owner re-link writes nothing
    /// ([`LinkOutcome::AlreadyLinked`]); another owner is [`StoreError::Taken`].
    pub async fn link_identity_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        provider: &str,
        subject: &str,
    ) -> Result<LinkOutcome, StoreError> {
        if let Some(owner) = self
            .player_by_identity_tx(&mut *conn, provider, subject)
            .await?
        {
            if owner.id == player_id {
                return Ok(LinkOutcome::AlreadyLinked);
            }
            return Err(StoreError::Taken);
        }
        let res = sqlx::query(
            "INSERT INTO accounts.identities (provider, subject, player_id) VALUES ($1, $2, $3::uuid)",
        )
        .bind(provider)
        .bind(subject)
        .bind(player_id)
        .execute(&mut *conn)
        .await;
        match res {
            Ok(_) => Ok(LinkOutcome::Linked),
            Err(e) if is_unique_violation(&e) => Err(StoreError::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// Inserts a caller-provided access token ON THE GIVEN CONNECTION, so session
    /// issuance can commit atomically with registration or stand alone in a thin tx.
    /// `family_id` ties the session to the refresh family it was minted under: it is
    /// what makes revocation scopeable to one compromised login instead of every
    /// device the player owns.
    pub async fn insert_session_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        token: &str,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO accounts.sessions (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, $3::uuid, now() + make_interval(mins => $4))",
        )
        .bind(token)
        .bind(player_id)
        .bind(family_id)
        .bind(ACCESS_TTL_MINUTES)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Stamps identity `(provider, subject)` of `player_id` as used now, on the caller's
    /// tx, answering whether it did. With `secret_hash` the identity must still store that
    /// hash: a password verified against a hash an operator reset has since replaced
    /// stamps nothing, and the caller mints nothing. The UPDATE re-checks the row it
    /// waited on, so a reset committing first is always seen.
    pub async fn stamp_identity_used_tx(
        &self,
        conn: &mut PgConnection,
        provider: &str,
        subject: &str,
        player_id: &str,
        secret_hash: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let done = sqlx::query(
            "UPDATE accounts.identities SET last_used_at = now() \
              WHERE provider = $1 AND subject = $2 AND player_id = $3::uuid \
                AND ($4::text IS NULL OR secret_hash = $4)",
        )
        .bind(provider)
        .bind(subject)
        .bind(player_id)
        .bind(secret_hash)
        .execute(&mut *conn)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Starts a NEW token family of `player_id` for `client_id` on the caller's tx, with the
    /// expiry of the access token minted with it, and answers the id Postgres mints for it.
    pub async fn insert_family_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        client_id: &str,
    ) -> Result<String, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO accounts.token_families (id, player_id, client_id, access_expires_at) \
             VALUES (gen_random_uuid(), $1::uuid, $2, now() + make_interval(mins => $3)) \
             RETURNING id::text",
        )
        .bind(player_id)
        .bind(client_id)
        .bind(ACCESS_TTL_MINUTES)
        .fetch_one(&mut *conn)
        .await
    }

    /// Moves family `family_id`'s `access_expires_at` to the expiry of an access token
    /// minted now, on the caller's tx. A vanished family surfaces as `RowNotFound`.
    pub async fn touch_family_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        let done = sqlx::query(
            "UPDATE accounts.token_families \
                SET access_expires_at = now() + make_interval(mins => $2) \
              WHERE id = $1::uuid",
        )
        .bind(family_id)
        .bind(ACCESS_TTL_MINUTES)
        .execute(&mut *conn)
        .await?;
        if done.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    /// Inserts the first refresh token of family `family_id` on the caller's tx: the only
    /// place a 30-day expiry is ever written.
    pub async fn insert_refresh_root_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        player_id: &str,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO accounts.refresh_tokens (token, player_id, family_id, expires_at) \
             VALUES ($1, $2::uuid, $3::uuid, now() + make_interval(days => $4))",
        )
        .bind(token)
        .bind(player_id)
        .bind(family_id)
        .bind(REFRESH_TTL_DAYS)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Consumes `token` and records `successor` as its replacement, returning the
    /// `(player_id, family_id)` it belonged to. Zero rows — an absent, expired or
    /// ALREADY CONSUMED token — is `Ok(None)`, which the caller resolves by re-reading
    /// the row in this same transaction ([`Store::refresh_row_tx`]).
    ///
    /// The predicate is the whole concurrency story: two dials with the same token
    /// serialize on this row, and only the one that finds `used_at IS NULL` rotates.
    ///
    /// Lock order: the token's player row and family row FOR KEY SHARE first, then the
    /// token row. The successor and access-session inserts that follow need that player
    /// row too, and an operator revocation ([`Store::lock_player_row_tx`]) and a
    /// self-delete take the player row before the token rows, as a client delete takes
    /// the family row before its tokens; a refresh holding its token while it waits for
    /// either would deadlock with them.
    pub async fn rotate_refresh_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        successor: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        sqlx::query(
            "SELECT 1 FROM accounts.players p \
               JOIN accounts.refresh_tokens t ON t.player_id = p.id \
               JOIN accounts.token_families f ON f.id = t.family_id \
              WHERE t.token = $1 \
                FOR KEY SHARE OF p, f",
        )
        .bind(token)
        .execute(&mut *conn)
        .await?;
        sqlx::query_as(
            "UPDATE accounts.refresh_tokens \
                SET used_at = now(), replaced_by = $2 \
              WHERE token = $1 AND used_at IS NULL AND expires_at > now() \
             RETURNING player_id::text, family_id::text",
        )
        .bind(token)
        .bind(successor)
        .fetch_optional(&mut *conn)
        .await
    }

    /// Inserts the successor of an already-consumed `parent` on the caller's tx. The
    /// player, the family AND the expiry are copied from the parent row rather than
    /// passed in, so a rotation cannot slide the family's hard 30-day bound forward —
    /// a family that renewed its own expiry would never expire at all. A vanished
    /// parent inserts nothing and surfaces as `RowNotFound`.
    pub async fn insert_refresh_successor_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
        parent: &str,
    ) -> Result<(), sqlx::Error> {
        let _: (String,) = sqlx::query_as(
            "INSERT INTO accounts.refresh_tokens (token, player_id, family_id, expires_at) \
             SELECT $1, player_id, family_id, expires_at \
               FROM accounts.refresh_tokens WHERE token = $2 \
             RETURNING token",
        )
        .bind(token)
        .bind(parent)
        .fetch_one(&mut *conn)
        .await?;
        Ok(())
    }

    /// The facts a failed rotation is classified from, read on the SAME transaction as
    /// the failed UPDATE so no other writer can move the row in between.
    pub async fn refresh_row_tx(
        &self,
        conn: &mut PgConnection,
        token: &str,
    ) -> Result<Option<RefreshRow>, sqlx::Error> {
        let row: Option<(String, String, Option<String>, bool, bool, bool)> = sqlx::query_as(
            "SELECT player_id::text, family_id::text, replaced_by, \
                    used_at IS NOT NULL, \
                    coalesce(used_at > now() - make_interval(secs => $2), false), \
                    expires_at <= now() \
               FROM accounts.refresh_tokens WHERE token = $1",
        )
        .bind(token)
        .bind(REFRESH_GRACE_SECONDS)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(
            |(player_id, family_id, replaced_by, used, used_within_grace, expired)| RefreshRow {
                player_id,
                family_id,
                replaced_by,
                used,
                used_within_grace,
                expired,
            },
        ))
    }

    /// Revokes one token FAMILY on the caller's tx: its family row, whose deletion takes
    /// every refresh token and access session descended from that login by cascade, and
    /// nothing belonging to the player's other logins. Deletion, not a flag: with the
    /// rows gone every token of the family answers as unknown, so a replay can never
    /// resurrect the family through the grace window.
    pub async fn kill_family_tx(
        &self,
        conn: &mut PgConnection,
        family_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM accounts.token_families WHERE id = $1::uuid")
            .bind(family_id)
            .execute(&mut *conn)
            .await?;
        Ok(())
    }

    /// Takes the player row FOR UPDATE on the caller's tx, answering whether the player
    /// exists. FOR UPDATE conflicts with the FOR KEY SHARE every login and refresh takes on
    /// that row as its FIRST lock (a login through its family insert's FK, a refresh in
    /// [`Store::rotate_refresh_tx`] before it touches its token), so either one commits
    /// before the lock is granted or waits for the locking transaction to end while it
    /// holds nothing the locker needs next.
    pub async fn lock_player_row_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM accounts.players WHERE id = $1::uuid FOR UPDATE")
                .bind(player_id)
                .fetch_optional(&mut *conn)
                .await?;
        Ok(row.is_some())
    }

    /// Whether `family_id` names a token family of `player_id` that has not been revoked.
    pub async fn family_of_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
        family_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM accounts.token_families WHERE id = $1::uuid AND player_id = $2::uuid",
        )
        .bind(family_id)
        .bind(player_id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.is_some())
    }

    /// Revokes every token family of a player on the caller's tx: all its family rows (and
    /// by cascade their refresh tokens and access sessions) and browser sessions. `None`
    /// when there is no such player; otherwise a "did anything end" signal only: deleted
    /// family rows (expired ones included) plus the browser sessions that were still live.
    /// Not a session count.
    ///
    /// Lock order: the player row first ([`Store::lock_player_row_tx`]), then its families
    /// in ascending id (the order every delete of several families takes them, so two such
    /// deletes cannot deadlock), then their token and session rows — the order every login
    /// and refresh takes them (FOR KEY SHARE on the player, then their own rows). Holding
    /// the player row makes the two deletes one cut: a login or refresh racing this either
    /// committed before the lock was granted and both deletes see its rows, or waits on the
    /// lock and commits after, whole.
    pub async fn kill_player_families_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Option<u64>, sqlx::Error> {
        if !self.lock_player_row_tx(conn, player_id).await? {
            return Ok(None);
        }
        sqlx::query(
            "SELECT 1 FROM accounts.token_families WHERE player_id = $1::uuid ORDER BY id FOR UPDATE",
        )
        .bind(player_id)
        .execute(&mut *conn)
        .await?;
        let families = sqlx::query("DELETE FROM accounts.token_families WHERE player_id = $1::uuid")
            .bind(player_id)
            .execute(&mut *conn)
            .await?;
        let browsers = self.delete_player_browser_sessions_tx(conn, player_id).await?;
        Ok(Some(families.rows_affected() + browsers))
    }

    /// Deletes every browser session of a player, expired ones included, on the caller's
    /// tx, answering how many of them were still live.
    pub async fn delete_player_browser_sessions_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<u64, sqlx::Error> {
        let live: i64 = sqlx::query_scalar(
            "WITH gone AS (DELETE FROM accounts.browser_sessions WHERE player_id = $1::uuid \
                           RETURNING expires_at) \
             SELECT count(*) FILTER (WHERE expires_at > now()) FROM gone",
        )
        .bind(player_id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(live as u64)
    }

    /// How many unexpired browser sessions a player holds: what Revoke all ends besides
    /// its families.
    pub async fn live_browser_sessions_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<u64, sqlx::Error> {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM accounts.browser_sessions \
              WHERE player_id = $1::uuid AND expires_at > now()",
        )
        .bind(player_id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(live as u64)
    }

    /// The token families of a player that still grant access ([`granting_families`]), in
    /// ascending id: those whose newest access token is unexpired or that hold an unexpired
    /// refresh token. A family past both expiries grants nothing; the prune sweeps remove
    /// its rows.
    pub async fn revocable_families_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(granting_families!("family_id::text", "ORDER BY family_rows.family_id"))
        .bind(player_id)
        .fetch_all(&mut *conn)
        .await
    }

    /// Revokes every family of a player that still grants access
    /// ([`Store::revocable_families_tx`]) and every browser session on the caller's tx:
    /// `None` when there is no such player, otherwise how many families and live browser
    /// sessions it revoked. The set it counts is the set it deletes, read after the player
    /// row lock ([`Store::lock_player_row_tx`], the lock order of
    /// [`Store::kill_player_families_tx`]), so no login or refresh can add a family between
    /// the two. It deletes the families one at a time in ascending id, the family lock order
    /// of [`Store::kill_player_families_tx`].
    pub async fn revoke_player_families_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<Option<usize>, sqlx::Error> {
        if !self.lock_player_row_tx(conn, player_id).await? {
            return Ok(None);
        }
        let families = self.revocable_families_tx(conn, player_id).await?;
        for family in &families {
            self.kill_family_tx(conn, family).await?;
        }
        let browsers = self.delete_player_browser_sessions_tx(conn, player_id).await?;
        Ok(Some(families.len() + browsers as usize))
    }

    /// Resolves a bearer token to its player, ignoring expired sessions. `Ok(None)`
    /// is a genuine unknown/expired token; an `Err` is a store failure the caller
    /// surfaces as infrastructure trouble (503), never a 401.
    pub async fn player_by_session(&self, token: &str) -> Result<Option<Player>, sqlx::Error> {
        if !crate::session_token_within_cap(token) {
            return Ok(None);
        }
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT p.id::text, p.display_name \
               FROM accounts.sessions s \
               JOIN accounts.players p ON p.id = s.player_id \
              WHERE s.token = $1 AND s.expires_at > now()",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, display_name)| Player { id, display_name }))
    }

    /// Deletes every EXPIRED browser session on the delivery tx; an expired one is
    /// already absent to every lookup.
    pub async fn prune_expired_browser_sessions(
        &self,
        conn: &mut PgConnection,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM accounts.browser_sessions WHERE expires_at <= now()")
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }

    /// Deletes every EXPIRED delete ticket on the delivery tx, the third table this
    /// module's daily prune drains. An expired ticket can no longer be consumed, so
    /// removing it changes no answer.
    pub async fn prune_expired_delete_tickets(
        &self,
        conn: &mut PgConnection,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM accounts.delete_tickets WHERE expires_at <= now()")
            .execute(&mut *conn)
            .await?;
        Ok(res.rows_affected())
    }

    /// Deletes every expired access session and refresh token and every token family that
    /// no longer grants access ([`granting_families`]) on the delivery tx, so the prune
    /// commits with the subscription's checkpoint. A CONSUMED refresh token is the reuse
    /// detector and stays until it expires; an expired one can only answer `Deny`. Every
    /// family these deletes reach is locked first, in one ascending-id pass (the order of
    /// [`Store::kill_player_families_tx`]): a token deleted before its family is locked would
    /// be held while a family delete's cascade waits on it, and that delete holds the family.
    /// The deletes reach only the families that pass locked: a row committed after it is left
    /// to the next prune.
    pub async fn prune_expired_token_rows(&self, conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        // Each delete and the lock clause naming its families come from one predicate, so
        // no delete can exist without the lock that covers it.
        let mut reached: Vec<String> = PRUNED_FAMILY_CHILDREN
            .iter()
            .map(|child| {
                format!(
                    "EXISTS (SELECT 1 FROM {child} c WHERE c.family_id = f.id AND {EXPIRED_CHILD})"
                )
            })
            .collect();
        reached.push(format!("({STALE_FAMILY})"));
        let locked: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT f.id::text FROM accounts.token_families f WHERE {} ORDER BY f.id FOR UPDATE OF f",
            reached.join(" OR ")
        ))
        .fetch_all(&mut *conn)
        .await?;
        if locked.is_empty() {
            return Ok(());
        }
        for child in PRUNED_FAMILY_CHILDREN {
            sqlx::query(&format!(
                "DELETE FROM {child} c WHERE c.family_id = ANY($1::text[]::uuid[]) AND {EXPIRED_CHILD}"
            ))
            .bind(&locked)
            .execute(&mut *conn)
            .await?;
        }
        sqlx::query(&format!(
            "DELETE FROM accounts.token_families f WHERE f.id = ANY($1::text[]::uuid[]) AND {STALE_FAMILY}"
        ))
        .bind(&locked)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// The summaries for `ids` in ONE statement, for the batched
    /// [`accountsapi::Directory::players_by_id`]. Ids that name no player are simply
    /// absent from the result, so the caller gets a short vector, never an error;
    /// non-canonical spellings are filtered out before binding and read as unknown.
    /// The `text[]` half of `$1::text[]::uuid[]` is spelled out so the bound array's
    /// type never depends on parameter inference.
    pub async fn players_by_id(
        &self,
        ids: &[String],
    ) -> Result<Vec<accountsapi::PlayerSummary>, sqlx::Error> {
        let ids: Vec<&str> = ids
            .iter()
            .map(String::as_str)
            .filter(|id| is_canonical_uuid(id))
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(&format!(
            "{SUMMARY_SELECT} WHERE p.id = ANY($1::text[]::uuid[]) \
              GROUP BY p.id, p.display_name, p.discriminator ORDER BY p.id"
        ))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(summary_of).collect())
    }

    /// One player by id, as the SAME summary the batch answers with — `me` reads its
    /// own handle through this, so no second site renders `"Name#1234"`. A malformed id
    /// is `Ok(None)`, like a genuine miss.
    pub async fn player_summary(
        &self,
        id: &str,
    ) -> Result<Option<accountsapi::PlayerSummary>, sqlx::Error> {
        let ids = [id.to_owned()];
        Ok(self.players_by_id(&ids).await?.into_iter().next())
    }

    /// The player one `"Name#1234"` handle names, matched case-insensitively on the name
    /// half — the same `(lower(display_name), discriminator)` pair `claim_handle_tx`
    /// claims, with `accounts_handle_idx` still enforcing uniqueness on `players` itself,
    /// so exactly one row can ever match. A handle that does not parse is `Ok(None)`,
    /// indistinguishable from an unknown one.
    pub async fn player_by_handle(
        &self,
        handle: &str,
    ) -> Result<Option<accountsapi::PlayerSummary>, sqlx::Error> {
        let Some((name, discriminator)) = split_handle(handle) else {
            return Ok(None);
        };
        let row: Option<(String, String, String, String)> = sqlx::query_as(&format!(
            "{SUMMARY_SELECT} WHERE lower(p.display_name) = lower($1) AND p.discriminator = $2 \
              GROUP BY p.id, p.display_name, p.discriminator"
        ))
        .bind(name)
        .bind(discriminator)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(summary_of))
    }

    /// Every credential mapping of a player, ordered for a stable `me` body.
    pub async fn identities_of(
        &self,
        player_id: &str,
    ) -> Result<Vec<accountsapi::IdentityRef>, sqlx::Error> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT provider, subject FROM accounts.identities \
              WHERE player_id = $1::uuid ORDER BY provider, subject",
        )
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(provider, subject)| accountsapi::IdentityRef { provider, subject })
            .collect())
    }

    /// Every player, for the admin total and nav counter. An exact `count(*)` scans the
    /// whole table on each call: no estimate is kept (plan R1 adds no table or index).
    pub async fn player_count(&self) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT count(*) FROM accounts.players")
            .fetch_one(&self.pool)
            .await
    }

    /// Players with a token minted in the last 24 hours, on the pool.
    pub async fn active_players_24h(&self) -> Result<i64, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        self.active_players_24h_tx(&mut conn).await
    }

    /// Players with a token minted in the last 24 hours, login or refresh, on `conn`. A
    /// family's last mint is its `access_expires_at` less [`ACCESS_TTL_MINUTES`] (see
    /// [`PLAYER_LIST_SELECT`]), so the bound is on `access_expires_at` itself and the scan
    /// goes through `token_families_access_expires_idx`. A revoked or pruned family no
    /// longer counts.
    pub async fn active_players_24h_tx(&self, conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT count(DISTINCT player_id) FROM accounts.token_families \
              WHERE access_expires_at > now() - interval '24 hours' + make_interval(mins => $1)",
        )
        .bind(ACCESS_TTL_MINUTES)
        .fetch_one(&mut *conn)
        .await
    }

    /// One page of players, `(created_at, id)` descending, resumed after `listing.after`.
    /// No index covers `players.created_at` (plan R1), so each page sorts every matching
    /// player, and a needle that is not an exact id, handle or email scans the table.
    pub async fn players_page(
        &self,
        listing: &PlayerListing<'_>,
    ) -> Result<Vec<PlayerListRow>, sqlx::Error> {
        let needle = listing.needle;
        let id = needle.filter(|n| is_canonical_uuid(n));
        let handle = needle.and_then(split_handle);
        let rows: Vec<PlayerListColumns> = sqlx::query_as(&format!(
            "{PLAYER_LIST_SELECT} \
              WHERE ($1::text IS NULL \
                     OR p.id = $2::uuid \
                     OR (lower(p.display_name) = lower($3) AND p.discriminator = $4) \
                     OR EXISTS (SELECT 1 FROM accounts.identities d \
                                 WHERE d.player_id = p.id AND d.provider = $5 AND d.subject = $10) \
                     OR strpos(lower(p.display_name), lower($1)) > 0) \
                AND ($6::text IS NULL \
                     OR EXISTS (SELECT 1 FROM accounts.identities f \
                                 WHERE f.player_id = p.id AND f.provider = $6)) \
                AND ($7::int8 IS NULL \
                     OR (p.created_at, p.id) < (to_timestamp(0) + $7 * interval '1 microsecond', $8::uuid)) \
              ORDER BY p.created_at DESC, p.id DESC \
              LIMIT $9"
        ))
        .bind(needle)
        .bind(id)
        .bind(handle.map(|(name, _)| name))
        .bind(handle.map(|(_, discriminator)| discriminator))
        .bind(crate::providers::PASSWORD)
        .bind(listing.provider)
        .bind(listing.after.map(|(micros, _)| micros))
        .bind(listing.after.map(|(_, id)| id))
        .bind(listing.limit)
        .bind(needle.map(crate::normalize_email))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(player_list_row).collect())
    }

    /// One player as the list shows it; a malformed id is `Ok(None)`, like a miss.
    pub async fn player_row(&self, id: &str) -> Result<Option<PlayerListRow>, sqlx::Error> {
        if !is_canonical_uuid(id) {
            return Ok(None);
        }
        let row: Option<PlayerListColumns> =
            sqlx::query_as(&format!("{PLAYER_LIST_SELECT} WHERE p.id = $1::uuid"))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(player_list_row))
    }

    /// A player's identities, oldest link first; a malformed id has none.
    pub async fn identity_rows(&self, player_id: &str) -> Result<Vec<IdentityRow>, sqlx::Error> {
        if !is_canonical_uuid(player_id) {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, String, String, Option<i64>)> = sqlx::query_as(
            "SELECT provider, subject, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD'), \
                    floor(EXTRACT(EPOCH FROM (now() - last_used_at)))::int8 \
               FROM accounts.identities WHERE player_id = $1::uuid \
              ORDER BY created_at, provider, subject",
        )
        .bind(player_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(provider, subject, linked, last_used_secs)| IdentityRow {
                provider,
                subject,
                linked,
                last_used_secs,
            })
            .collect())
    }

    /// The newest `limit` refresh families of a player that still grant access, plus how
    /// many do: the set [`Store::revocable_families_tx`] counts and the families Revoke all
    /// deletes ([`granting_families`]). A revoked family has no rows left.
    pub async fn live_families(
        &self,
        player_id: &str,
        limit: i64,
    ) -> Result<(Vec<FamilyRow>, i64), sqlx::Error> {
        if !is_canonical_uuid(player_id) {
            return Ok((Vec::new(), 0));
        }
        let rows: Vec<(String, String, i64, i64, i64)> = sqlx::query_as(
            granting_families!(
                "family_id::text, \
                 to_char(min(since) AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI'), \
                 (EXTRACT(EPOCH FROM min(since)) * 1000000)::int8, \
                 floor(EXTRACT(EPOCH FROM (max(expires_at) - now())))::int8, \
                 count(*) OVER ()",
                "ORDER BY min(since) DESC, family_rows.family_id LIMIT $2"
            ),
        )
        .bind(player_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let live = rows.first().map_or(0, |row| row.4);
        let families = rows
            .into_iter()
            .map(|(family_id, started, started_us, expires_in_secs, _)| FamilyRow {
                family_id,
                started,
                started_us,
                expires_in_secs,
            })
            .collect();
        Ok((families, live))
    }
}

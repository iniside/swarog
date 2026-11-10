//! The `admin.forms` store: minting form instances, claiming them and click rows, and
//! settling or releasing a claim. Every time comparison runs on Postgres `now()`.

use controlplane::EntityRef;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use super::actions::{Claimant, FormRow, StoredSpec, CLAIM_WINDOW, CLICK_REPEAT_WINDOW, FORM_TTL};

/// The row columns every statement returns, timestamps as epoch microseconds.
const COLUMNS: &str = "id, login, module, action, contribution, entity_kind, entity_id, item, spec::text AS spec, \
     generation, incarnation, \
     (extract(epoch FROM claimed_at) * 1000000)::bigint AS claimed_us, \
     (extract(epoch FROM consumed_at) * 1000000)::bigint AS consumed_us, \
     (extract(epoch FROM expires_at) * 1000000)::bigint AS expires_us";

fn decode(row: &PgRow) -> anyhow::Result<FormRow> {
    let kind: Option<String> = row.try_get("entity_kind")?;
    let id: Option<String> = row.try_get("entity_id")?;
    let spec: String = row.try_get("spec")?;
    Ok(FormRow {
        id: row.try_get("id")?,
        login: row.try_get("login")?,
        module: row.try_get("module")?,
        action: row.try_get("action")?,
        contribution: row.try_get("contribution")?,
        entity: kind.zip(id).map(|(kind, id)| EntityRef { kind, id }),
        item: row.try_get("item")?,
        spec: serde_json::from_str(&spec)?,
        generation: row.try_get("generation")?,
        incarnation: row.try_get("incarnation")?,
        claimed_at: row.try_get("claimed_us")?,
        consumed_at: row.try_get("consumed_us")?,
        expires_at: row.try_get("expires_us")?,
    })
}

/// Deletes expired rows, except one claimed within [`CLAIM_WINDOW`], whose settle must find
/// it. Best-effort: a failure is logged.
async fn prune(pool: &PgPool) {
    let sql = format!(
        "DELETE FROM admin.forms WHERE expires_at < now() \
         AND (claimed_at IS NULL OR claimed_at < now() - interval '{} seconds')",
        CLAIM_WINDOW.as_secs()
    );
    if let Err(err) = sqlx::query(&sql).execute(pool).await {
        tracing::warn!(%err, "admin forms: expired-row prune failed");
    }
}

/// What a new form instance is bound to.
pub(crate) struct Mint<'a> {
    pub(crate) login: &'a str,
    pub(crate) module: &'a str,
    pub(crate) action: &'a str,
    pub(crate) contribution: Option<&'a str>,
    pub(crate) entity: Option<&'a EntityRef>,
    pub(crate) item: Option<&'a str>,
    pub(crate) spec: &'a StoredSpec,
}

/// Inserts a form instance `<prefix>:<token>` and returns its id.
pub(crate) async fn mint(pool: &PgPool, prefix: &str, m: &Mint<'_>) -> anyhow::Result<String> {
    prune(pool).await;
    let token = crate::new_token();
    let id = format!("{prefix}:{token}");
    let sql = format!(
        "INSERT INTO admin.forms (id, login, module, action, contribution, entity_kind, entity_id, item, spec, \
         incarnation, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, now() + interval '{} seconds')",
        FORM_TTL.as_secs()
    );
    sqlx::query(&sql)
        .bind(&id)
        .bind(m.login)
        .bind(m.module)
        .bind(m.action)
        .bind(m.contribution)
        .bind(m.entity.map(|e| e.kind.as_str()))
        .bind(m.entity.map(|e| e.id.as_str()))
        .bind(m.item)
        .bind(serde_json::to_string(m.spec)?)
        .bind(&token)
        .execute(pool)
        .await?;
    Ok(id)
}

/// Claims an unexpired, unconsumed, unheld form instance of `who`.
pub(crate) async fn claim_form(pool: &PgPool, id: &str, who: &Claimant) -> anyhow::Result<Option<FormRow>> {
    let sql = format!(
        "UPDATE admin.forms SET claimed_at = now() \
         WHERE id = $1 AND login = $2 AND module = $3 AND action = $4 \
           AND spec->>'kind' <> 'Click' \
           AND expires_at > now() AND consumed_at IS NULL \
           AND (claimed_at IS NULL OR claimed_at < now() - interval '{} seconds') \
         RETURNING {COLUMNS}",
        CLAIM_WINDOW.as_secs()
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(&who.login)
        .bind(&who.module)
        .bind(&who.action)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(decode).transpose()
}

/// The row `id`, if any, and Postgres `now()` in epoch microseconds.
pub(crate) async fn lookup(pool: &PgPool, id: &str) -> anyhow::Result<(Option<FormRow>, i64)> {
    let sql = format!(
        "SELECT (extract(epoch FROM now()) * 1000000)::bigint AS now_us, f.* \
         FROM (SELECT 1) AS one LEFT JOIN (SELECT {COLUMNS} FROM admin.forms WHERE id = $1) AS f ON true"
    );
    let row = sqlx::query(&sql).bind(id).fetch_one(pool).await?;
    let now: i64 = row.try_get("now_us")?;
    let found: Option<String> = row.try_get("id")?;
    let form = match found {
        Some(_) => Some(decode(&row)?),
        None => None,
    };
    Ok((form, now))
}

/// Inserts the click row `id`, or only extends its expiry when it exists.
pub(crate) async fn upsert_click(
    pool: &PgPool,
    id: &str,
    who: &Claimant,
    entity: Option<&EntityRef>,
    item: Option<&str>,
) -> anyhow::Result<()> {
    prune(pool).await;
    let sql = format!(
        "INSERT INTO admin.forms (id, login, module, action, entity_kind, entity_id, item, spec, incarnation, \
         expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, now() + interval '{ttl} seconds') \
         ON CONFLICT (id) DO UPDATE SET expires_at = now() + interval '{ttl} seconds'",
        ttl = FORM_TTL.as_secs()
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(&who.login)
        .bind(&who.module)
        .bind(&who.action)
        .bind(entity.map(|e| e.kind.as_str()))
        .bind(entity.map(|e| e.id.as_str()))
        .bind(item)
        .bind(serde_json::to_string(&StoredSpec::Click)?)
        .bind(crate::new_token())
        .execute(pool)
        .await?;
    Ok(())
}

/// Claims the click row `id`: free, abandoned, or consumed longer than
/// [`CLICK_REPEAT_WINDOW`] ago, which takes it over as the next generation.
pub(crate) async fn claim_click(pool: &PgPool, id: &str, who: &Claimant) -> anyhow::Result<Option<FormRow>> {
    let sql = format!(
        "UPDATE admin.forms \
         SET claimed_at = now(), consumed_at = NULL, \
             generation = generation + CASE WHEN consumed_at IS NULL THEN 0 ELSE 1 END \
         WHERE id = $1 AND login = $2 AND module = $3 AND action = $4 \
           AND (claimed_at IS NULL OR claimed_at < now() - interval '{claim} seconds' \
                OR consumed_at IS NOT NULL) \
           AND (consumed_at IS NULL OR consumed_at < now() - interval '{repeat} seconds') \
         RETURNING {COLUMNS}",
        claim = CLAIM_WINDOW.as_secs(),
        repeat = CLICK_REPEAT_WINDOW.as_secs()
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(&who.login)
        .bind(&who.module)
        .bind(&who.action)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(decode).transpose()
}

/// Ends the claim `row` holds: consumed (kept as a tombstone until it expires) or released.
/// Matches the claim's own `claimed_at`, so it never touches a later takeover. A failure is
/// logged.
pub(crate) async fn settle(pool: &PgPool, row: &FormRow, consumed: bool) {
    let set = if consumed { "consumed_at = now()" } else { "claimed_at = NULL" };
    let sql = format!(
        "UPDATE admin.forms SET {set} \
         WHERE id = $1 AND (extract(epoch FROM claimed_at) * 1000000)::bigint = $2"
    );
    if let Err(err) = sqlx::query(&sql).bind(&row.id).bind(row.claimed_at).execute(pool).await {
        tracing::warn!(%err, id = %row.id, consumed, "admin forms: settling a claim failed");
    }
}

//! The OIDC signing keys: ES256 key pairs whose PKCS#8 private half is sealed with
//! AES-256-GCM under `ACCOUNTS_SIGNING_KEK` and stored in `accounts.signing_keys`.

use std::ffi::OsString;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lifecycle::Ddl;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use sha2::{Digest, Sha256};

pub(crate) const KEK_ENV: &str = "ACCOUNTS_SIGNING_KEK";
pub(crate) const ISSUER_ENV: &str = "ACCOUNTS_OIDC_ISSUER";

/// `{schema}` here and in [`SIGNING_KEYS_ONE_ACTIVE_DDL`] stands for the schema
/// [`migrate_signing_keys`] runs on.
const SIGNING_KEYS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS {schema}.signing_keys (
	kid        text PRIMARY KEY,
	alg        text NOT NULL CHECK (alg = 'ES256'),
	sealed_key bytea NOT NULL,
	nonce      bytea NOT NULL,
	public_jwk jsonb NOT NULL,
	status     text NOT NULL CHECK (status IN ('active', 'retiring', 'retired')),
	created_at timestamptz NOT NULL DEFAULT now()
);";

const SIGNING_KEYS_ONE_ACTIVE_DDL: &str =
    "CREATE UNIQUE INDEX signing_keys_one_active_idx \
     ON {schema}.signing_keys (status) WHERE status = 'active'";

/// Every column of `signing_keys` and its `information_schema` data type, by name.
const COLUMNS: [(&str, &str); 7] = [
    ("alg", "text"),
    ("created_at", "timestamp with time zone"),
    ("kid", "text"),
    ("nonce", "bytea"),
    ("public_jwk", "jsonb"),
    ("sealed_key", "bytea"),
    ("status", "text"),
];

/// The AES-256-GCM key-encryption key the signing keys are sealed under.
#[derive(Clone)]
pub(crate) struct SigningKek([u8; 32]);

impl std::fmt::Debug for SigningKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKek(..)")
    }
}

impl SigningKek {
    /// Standard base64 of exactly 32 bytes, canonical padding; the error never holds the value.
    pub(crate) fn parse(raw: &str) -> anyhow::Result<SigningKek> {
        if raw.trim().is_empty() {
            anyhow::bail!(
                "invalid {KEK_ENV}: blank; set standard base64 of 32 random bytes \
                 (openssl rand -base64 32)"
            );
        }
        let bytes = STANDARD.decode(raw).ok().and_then(|b| <[u8; 32]>::try_from(b).ok());
        match bytes {
            Some(key) => Ok(SigningKek(key)),
            None => anyhow::bail!(
                "invalid {KEK_ENV}: not standard base64 of exactly 32 bytes \
                 (openssl rand -base64 32)"
            ),
        }
    }

    fn aead(&self) -> LessSafeKey {
        let key = UnboundKey::new(&AES_256_GCM, &self.0).expect("AES-256 takes a 32-byte key");
        LessSafeKey::new(key)
    }
}

/// The KEK an env value names: unset is `None`; blank, malformed or non-UTF-8 is an error.
pub(crate) fn kek_from(raw: Option<OsString>) -> anyhow::Result<Option<SigningKek>> {
    let Some(raw) = raw else { return Ok(None) };
    let raw = raw
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid {KEK_ENV}: not UTF-8"))?;
    SigningKek::parse(&raw).map(Some)
}

/// [`kek_from`] where an unset variable is an error too.
pub(crate) fn required_kek(raw: Option<OsString>) -> anyhow::Result<SigningKek> {
    kek_from(raw)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{KEK_ENV} is required: standard base64 of 32 random bytes (openssl rand -base64 32) \
             that seals accounts' OIDC signing keys"
        )
    })
}

/// The OIDC issuer an env value names: unset is [`accountsapi::DEFAULT_OIDC_ISSUER`]; any
/// other value must be a canonical absolute http(s) URL whose path is exactly `/accounts/oidc`.
pub(crate) fn oidc_issuer(raw: Option<OsString>) -> anyhow::Result<String> {
    let Some(raw) = raw else { return Ok(accountsapi::DEFAULT_OIDC_ISSUER.to_string()) };
    let raw = raw
        .into_string()
        .map_err(|_| anyhow::anyhow!("invalid {ISSUER_ENV}: not UTF-8"))?;
    let valid = url::Url::parse(&raw).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/accounts/oidc"
            && u.query().is_none()
            && u.fragment().is_none()
            && u.as_str() == raw
    });
    if !valid {
        anyhow::bail!(
            "invalid {ISSUER_ENV} {raw:?}: must be a canonical absolute http or https URL whose \
             path is exactly /accounts/oidc, with no query, fragment or trailing slash"
        );
    }
    Ok(raw)
}

/// A freshly generated key, sealed and ready to insert.
pub(crate) struct NewKey {
    pub(crate) kid: String,
    pub(crate) sealed_key: Vec<u8>,
    pub(crate) nonce: Vec<u8>,
    pub(crate) public_jwk: serde_json::Value,
}

impl NewKey {
    pub(crate) fn generate(kek: &SigningKek) -> anyhow::Result<NewKey> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| anyhow::anyhow!("accounts: generate an ES256 signing key"))?;
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .map_err(|e| anyhow::anyhow!("accounts: parse the generated signing key: {e}"))?;
        let (x, y) = point_coordinates(pair.public_key().as_ref())?;
        let kid = thumbprint(&x, &y);
        let public_jwk = serde_json::json!({
            "kty": "EC",
            "crv": "P-256",
            "x": x,
            "y": y,
            "kid": kid,
            "use": "sig",
            "alg": "ES256",
        });
        let (nonce, sealed_key) = seal(kek, &kid, pkcs8.as_ref())?;
        Ok(NewKey { kid, sealed_key, nonce, public_jwk })
    }
}

/// The base64url `x` and `y` of an uncompressed SEC1 P-256 point.
fn point_coordinates(point: &[u8]) -> anyhow::Result<(String, String)> {
    match point {
        [0x04, xy @ ..] if xy.len() == 64 => {
            Ok((URL_SAFE_NO_PAD.encode(&xy[..32]), URL_SAFE_NO_PAD.encode(&xy[32..])))
        }
        _ => anyhow::bail!("accounts: the signing key's public point is not uncompressed P-256"),
    }
}

/// RFC 7638 JWK thumbprint of a P-256 key: base64url SHA-256 of its required members in
/// lexicographic order, no whitespace.
pub(crate) fn thumbprint(x: &str, y: &str) -> String {
    let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
    URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes()))
}

/// Seals `plaintext` under a fresh random nonce with `kid` as the AAD: `(nonce, ciphertext)`.
pub(crate) fn seal(
    kek: &SigningKek,
    kid: &str,
    plaintext: &[u8],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow::anyhow!("accounts: draw a signing key nonce"))?;
    let mut sealed = plaintext.to_vec();
    kek.aead()
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(kid.as_bytes()),
            &mut sealed,
        )
        .map_err(|_| anyhow::anyhow!("accounts: seal a signing key"))?;
    Ok((nonce.to_vec(), sealed))
}

/// Opens a sealed key; `None` when the KEK, the `kid`, the nonce or the ciphertext differ
/// from the ones it was sealed with.
pub(crate) fn open(kek: &SigningKek, kid: &str, nonce: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut buf = sealed.to_vec();
    let plain = kek.aead().open_in_place(nonce, Aad::from(kid.as_bytes()), &mut buf).ok()?;
    Some(plain.to_vec())
}

/// Creates `{schema}.signing_keys` and its one-active index after refusing a table of
/// another shape, then guarantees one active key and opens every live key under `kek`.
pub(crate) async fn migrate_signing_keys(
    pool: &sqlx::PgPool,
    schema: &str,
    kek: &SigningKek,
) -> anyhow::Result<()> {
    refuse_stale_signing_keys(pool, schema).await?;
    Ddl::new(pool)
        .sql(on_schema(SIGNING_KEYS_DDL, schema))
        .index(
            on_schema("{schema}.signing_keys_one_active_idx", schema),
            on_schema(SIGNING_KEYS_ONE_ACTIVE_DDL, schema),
        )
        .run()
        .await?;
    ensure_active_key(pool, schema, kek).await
}

/// `template` with `{schema}` replaced, leaked once per migrate because `Ddl` holds
/// `&'static str`.
fn on_schema(template: &str, schema: &str) -> &'static str {
    Box::leak(template.replace("{schema}", schema).into_boxed_str())
}

/// Refuses a `{schema}.signing_keys` whose columns differ from [`SIGNING_KEYS_DDL`]'s:
/// `CREATE TABLE IF NOT EXISTS` keeps the old table. Runs before the DDL, whose index
/// would otherwise fail on a table without `status`.
async fn refuse_stale_signing_keys(pool: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name::text, data_type::text FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = 'signing_keys' ORDER BY column_name",
    )
    .bind(schema)
    .fetch_all(pool)
    .await?;
    let matches = columns.iter().map(|(n, t)| (n.as_str(), t.as_str())).eq(COLUMNS);
    if !columns.is_empty() && !matches {
        anyhow::bail!(
            "accounts: {schema}.signing_keys has another shape than this build creates. Remedy: \
             DROP TABLE {schema}.signing_keys; and reboot (invalidates issued tokens)."
        );
    }
    Ok(())
}

/// Inserts a new active ES256 key; does nothing when another boot's active row already holds
/// the one-active index.
async fn insert_active_key(
    pool: &sqlx::PgPool,
    schema: &str,
    kek: &SigningKek,
) -> anyhow::Result<()> {
    let key = NewKey::generate(kek)?;
    sqlx::query(&format!(
        "INSERT INTO {schema}.signing_keys (kid, alg, sealed_key, nonce, public_jwk, status) \
         VALUES ($1, 'ES256', $2, $3, $4::jsonb, 'active') ON CONFLICT DO NOTHING"
    ))
    .bind(&key.kid)
    .bind(&key.sealed_key)
    .bind(&key.nonce)
    .bind(key.public_jwk.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// Opens every `active` and `retiring` row of `{schema}.signing_keys` under `kek`, failing
/// with the remedy on the first that does not open; the number of `active` rows.
async fn open_live_keys(
    pool: &sqlx::PgPool,
    schema: &str,
    kek: &SigningKek,
) -> anyhow::Result<usize> {
    let rows: Vec<(String, Vec<u8>, Vec<u8>, String)> = sqlx::query_as(&format!(
        "SELECT kid, sealed_key, nonce, status FROM {schema}.signing_keys \
         WHERE status IN ('active', 'retiring')"
    ))
    .fetch_all(pool)
    .await?;
    let rng = SystemRandom::new();
    for (kid, sealed, nonce, _) in &rows {
        let opened = open(kek, kid, nonce, sealed).filter(|der| {
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der, &rng).is_ok()
        });
        if opened.is_none() {
            anyhow::bail!(
                "accounts: signing key {kid} in {schema}.signing_keys does not open under \
                 {KEK_ENV}. Remedy: restore the previous {KEK_ENV}, or DROP TABLE \
                 {schema}.signing_keys (invalidates issued tokens)."
            );
        }
    }
    Ok(rows.iter().filter(|r| r.3 == "active").count())
}

/// Inserts an ES256 key only when every live row opens under `kek` and none is active, then
/// re-reads and opens again: a boot whose insert lost the race opens the winner's key.
async fn ensure_active_key(
    pool: &sqlx::PgPool,
    schema: &str,
    kek: &SigningKek,
) -> anyhow::Result<()> {
    let mut active = open_live_keys(pool, schema, kek).await?;
    if active == 0 {
        insert_active_key(pool, schema, kek).await?;
        active = open_live_keys(pool, schema, kek).await?;
    }
    if active != 1 {
        anyhow::bail!(
            "accounts: {schema}.signing_keys holds {active} active keys after boot ensured one"
        );
    }
    Ok(())
}

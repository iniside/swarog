//! The credential-provider seam: one naming authority, one registry of configured
//! verifiers, and one validating parse of the provider configuration.
//!
//! Three names, deliberately distinct: [`KNOWN_PROVIDERS`] is what this build can
//! construct a verifier for, [`Providers`] is what this PROCESS actually configured, and
//! [`ProviderConfig`] is the validated parse that produces the second from the
//! environment. Keeping the first two apart is what makes "you typed a provider that
//! does not exist" and "that provider is not configured here" different answers
//! rather than one `None`.
//!
//! [`ProviderConfig::from_vars`] is the validation authority. The verifier
//! constructors are pure (no I/O at `init`, constraint #8) and therefore cannot
//! reject a bad endpoint themselves — so a malformed value would otherwise enable a
//! provider that fails on every token forever. Here it is a startup failure instead.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;

use crate::oidc::{short_id, IssuerMatch, OidcVerifier};

/// The `accounts.identities.provider` value for Epic — written once and referenced by
/// the name list, the registry key and every `resolve` call, so a typo cannot leave a
/// fully configured provider unresolvable.
pub(crate) const EPIC: &str = "epic";

/// The `accounts.identities.provider` value for Google, the second OIDC provider.
pub(crate) const GOOGLE: &str = "google";

/// Every provider name this build can construct a verifier for, configured or not —
/// the naming authority, separate from the configured-verifier map so a typo and an
/// unconfigured provider are distinguishable outcomes. Membership means buildable,
/// not planned: a name joins this list in the same commit that ships its verifier,
/// so `KnownButUnconfigured` (503) always names something an operator can fix by
/// configuring it.
pub(crate) const KNOWN_PROVIDERS: &[&str] = &[EPIC, GOOGLE];

/// Why a credential failed verification — the taxonomy the caller maps to a status
/// (mirrors the `verify_session` 503-not-401 precedent: an IdP outage must not
/// masquerade as bad credentials).
#[derive(Debug, thiserror::Error)]
pub(crate) enum VerifyError {
    /// The credential itself is demonstrably invalid: bad signature/alg/aud/iss/exp,
    /// or a `kid` absent from a fresh (or fresh-enough) key set. Maps to
    /// Unauthorized (401).
    #[error("credential rejected: {0}")]
    Rejected(#[source] anyhow::Error),
    /// No verdict was reachable: the provider's key/verification endpoint failed and
    /// nothing cached answers. Maps to Unavailable (503) — the caller's credentials
    /// may be perfectly fine.
    #[error("identity provider unavailable: {0}")]
    Infra(#[source] anyhow::Error),
}

/// What a verified credential yields: the provider-scoped subject that keys
/// `accounts.identities`, and the display name a first-sight login provisions.
pub(crate) struct VerifiedSubject {
    pub(crate) subject: String,
    pub(crate) display_name: String,
}

/// One credential provider's verification face. Implementations do the provider's own
/// I/O (JWKS fetch, secret comparison) and return the shared [`VerifyError`] taxonomy.
#[async_trait]
pub(crate) trait CredentialVerifier: Send + Sync {
    /// The widest credential this provider will look at, in bytes. Deliberately has
    /// no default body: a credential's size bound is a property of its shape (a JWT,
    /// a short opaque ticket), so each provider states its own rather than inheriting
    /// the one that happened to be written first.
    fn max_credential_bytes(&self) -> usize;

    async fn verify(&self, credential: &str) -> Result<VerifiedSubject, VerifyError>;
}

/// The outcome of looking a provider name up in a process's registry. The
/// three-way answer is the point: `Unknown` is caller error, `KnownButUnconfigured`
/// is a deployment fact, and only the caller decides their statuses.
pub(crate) enum Resolution<'a> {
    Configured(&'a Arc<dyn CredentialVerifier>),
    KnownButUnconfigured,
    Unknown,
}

/// The verifiers this process configured. Built once from [`ProviderConfig`] and read
/// by every credential path; the in-struct map mirrors `edge::Server`'s handler table,
/// panic-on-duplicate included.
#[derive(Default)]
pub(crate) struct Providers {
    verifiers: HashMap<String, Arc<dyn CredentialVerifier>>,
}

impl Providers {
    /// The registry a process that configured nothing resolves through, so "before
    /// `init` filled the cell" answers exactly like "configured with no providers".
    pub(crate) fn empty() -> &'static Providers {
        static EMPTY: LazyLock<Providers> = LazyLock::new(Providers::default);
        &EMPTY
    }

    /// Registers `name`'s verifier. A duplicate name means two configurations claim
    /// one provider — fail loudly at startup rather than letting one silently win
    /// (`edge::Server::assert_unregistered`'s convention).
    pub(crate) fn insert(&mut self, name: &'static str, verifier: Arc<dyn CredentialVerifier>) {
        if self.verifiers.insert(name.to_string(), verifier).is_some() {
            panic!("accounts: provider {name:?} registered twice — two configurations claim the same provider");
        }
    }

    pub(crate) fn resolve(&self, name: &str) -> Resolution<'_> {
        match self.verifiers.get(name) {
            Some(verifier) => Resolution::Configured(verifier),
            None if KNOWN_PROVIDERS.contains(&name) => Resolution::KnownButUnconfigured,
            None => Resolution::Unknown,
        }
    }
}

/// Epic's validated configuration. The verifier is kept as the CONCRETE type as well
/// as behind the trait: the web-OAuth flow (`epic_oauth::EpicOAuth`) needs
/// `OidcVerifier` itself for its code-exchange path.
pub(crate) struct EpicConfig {
    pub(crate) client_id: String,
    pub(crate) jwks_url: String,
    pub(crate) verifier: Arc<OidcVerifier>,
    /// `Some` iff the confidential client secret enables the browser redirect flow.
    /// The endpoint VALUES are validated either way — see [`EpicOAuthConfig::new`].
    pub(crate) oauth: Option<EpicOAuthConfig>,
}

/// The validated browser-flow half of Epic's configuration, handed to
/// `epic_oauth::EpicOAuth` ready to use.
#[derive(Clone)]
pub(crate) struct EpicOAuthConfig {
    pub(crate) client_secret: String,
    pub(crate) redirect_uri: String,
    pub(crate) authorize_url: String,
    pub(crate) token_url: String,
    /// Derived from the redirect URI's scheme at parse time: HTTPS sets `Secure` on
    /// the binding cookie, the loopback-HTTP dev carve-out does not.
    pub(crate) cookie_secure: bool,
}

impl EpicOAuthConfig {
    /// The one rule for the three browser-flow endpoints. Validation deliberately does
    /// NOT consult `client_secret`: a malformed URL is malformed whether or not the
    /// flow is enabled, and a value whose validity depends on an unrelated variable is
    /// a configuration trap.
    pub(crate) fn new(
        client_secret: String,
        redirect_uri: String,
        authorize_url: String,
        token_url: String,
    ) -> anyhow::Result<EpicOAuthConfig> {
        let cookie_secure = check_redirect_uri("EPIC_REDIRECT_URI", &redirect_uri)?;
        check_endpoint("EPIC_AUTHORIZE_URL", &authorize_url)?;
        check_endpoint("EPIC_TOKEN_URL", &token_url)?;
        Ok(EpicOAuthConfig {
            client_secret,
            redirect_uri,
            authorize_url,
            token_url,
            cookie_secure,
        })
    }
}

/// Google's validated configuration. One real Google project issues separate client
/// ids for its web/iOS/Android clients and all of them are valid audiences of the same
/// user's id_token, which is why the audience is a list.
pub(crate) struct GoogleConfig {
    pub(crate) client_ids: Vec<String>,
    pub(crate) jwks_url: String,
    pub(crate) verifier: Arc<OidcVerifier>,
}

/// The validated provider configuration: one entry per PRESENT provider. An absent
/// provider is simply missing; a present-but-malformed one is an `Err` from
/// [`ProviderConfig::from_vars`], never a silently disabled entry.
pub(crate) struct ProviderConfig {
    pub(crate) epic: Option<EpicConfig>,
    pub(crate) google: Option<GoogleConfig>,
}

impl ProviderConfig {
    pub(crate) fn from_env() -> anyhow::Result<ProviderConfig> {
        // Reads exactly the keys the parse knows, never a whole-environ snapshot: the
        // process env is mutated by `set_var` in test/verify harnesses, so the narrower
        // the read the smaller the unsound window. A new provider extends
        // `provider_env_keys`, which is what keeps this narrow as the list grows.
        let mut vars = BTreeMap::new();
        for key in provider_env_keys() {
            let Some(raw) = std::env::var_os(key) else {
                continue;
            };
            let value = raw
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid {key}: value is not valid UTF-8"))?;
            vars.insert(key.to_string(), value);
        }
        ProviderConfig::from_vars(&vars)
    }

    /// Parses and VALIDATES every present provider. Takes the variables as data so the
    /// failing branches are provable without mutating process env.
    pub(crate) fn from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<ProviderConfig> {
        Ok(ProviderConfig {
            epic: epic_from_vars(vars)?,
            google: google_from_vars(vars)?,
        })
    }

    /// The credential registry this configuration implies — exactly the present
    /// providers, each behind its [`CredentialVerifier`] adapter.
    pub(crate) fn providers(&self) -> Providers {
        let mut providers = Providers::default();
        if let Some(epic) = &self.epic {
            providers.insert(EPIC, oidc_credentials(EPIC, epic.verifier.clone()));
        }
        if let Some(google) = &self.google {
            providers.insert(GOOGLE, oidc_credentials(GOOGLE, google.verifier.clone()));
        }
        providers
    }
}

/// The widest id_token any OIDC provider here accepts. JWTs carrying claim-heavy
/// payloads run into the low tens of KiB; the cap bounds base64/JSON work on an
/// attacker-supplied string before a signature is ever checked.
pub const MAX_OIDC_CREDENTIAL_BYTES: usize = 65_536;

/// Any OIDC provider's `CredentialVerifier` face: an id_token in, the provider's
/// account id plus the `<provider>:<shortID>` first-sight display name out. The
/// provider name is data, so a second OIDC provider is a registration, not a type.
struct OidcCredentials {
    provider: &'static str,
    verifier: Arc<OidcVerifier>,
}

#[async_trait]
impl CredentialVerifier for OidcCredentials {
    fn max_credential_bytes(&self) -> usize {
        MAX_OIDC_CREDENTIAL_BYTES
    }

    async fn verify(&self, credential: &str) -> Result<VerifiedSubject, VerifyError> {
        let subject = self.verifier.verify(credential).await?;
        Ok(VerifiedSubject {
            display_name: format!("{}:{}", self.provider, short_id(&subject)),
            subject,
        })
    }
}

/// Wraps a constructed OIDC verifier in `provider`'s credential face — shared by
/// [`ProviderConfig::providers`] and the tests that inject a fixture verifier.
pub(crate) fn oidc_credentials(
    provider: &'static str,
    verifier: Arc<OidcVerifier>,
) -> Arc<dyn CredentialVerifier> {
    Arc::new(OidcCredentials { provider, verifier })
}

/// Every variable the provider parse reads — the authority [`ProviderConfig::from_env`]
/// collects. A new provider appends its own block here rather than widening the read
/// back out to the whole environment.
fn provider_env_keys() -> impl Iterator<Item = &'static str> {
    EPIC_VARS.iter().chain(GOOGLE_VARS).copied()
}

/// Epic's whole environment surface. Presence of ANY of these means the operator is
/// configuring epic — including the OAuth keys, so a malformed browser-flow endpoint
/// is caught even when nothing else about epic is set.
const EPIC_VARS: &[&str] = &[
    "EPIC_CLIENT_ID",
    "EPIC_JWKS_URL",
    "EPIC_ISSUER_PREFIX",
    "EPIC_CLIENT_SECRET",
    "EPIC_REDIRECT_URI",
    "EPIC_AUTHORIZE_URL",
    "EPIC_TOKEN_URL",
];

/// The subset that configures the browser flow but cannot enable it — setting one
/// without `EPIC_CLIENT_SECRET` is an operator asking for a flow that would never mount.
const EPIC_OAUTH_VARS: &[&str] = &["EPIC_REDIRECT_URI", "EPIC_AUTHORIZE_URL", "EPIC_TOKEN_URL"];

const EPIC_DEFAULT_JWKS_URL: &str =
    "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json";
const EPIC_DEFAULT_ISSUER_PREFIX: &str = "https://api.epicgames.dev/epic/oauth/v1";
const EPIC_DEFAULT_REDIRECT_URI: &str = "http://localhost:8080/accounts/epic/callback";
const EPIC_DEFAULT_AUTHORIZE_URL: &str = "https://www.epicgames.com/id/authorize";
const EPIC_DEFAULT_TOKEN_URL: &str = "https://api.epicgames.dev/epic/oauth/v1/token";

fn epic_from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<Option<EpicConfig>> {
    let Some(present) = EPIC_VARS.iter().find(|key| vars.contains_key(**key)) else {
        return Ok(None);
    };
    // A variable the operator SET is a variable the operator meant: an empty value is a
    // misconfiguration, never a silent fall-back to the default.
    for key in EPIC_VARS {
        if vars.get(*key).is_some_and(String::is_empty) {
            anyhow::bail!("invalid {key}: set but empty — unset it to leave it unconfigured");
        }
    }

    // Per-FIELD validation FIRST, cross-field completeness second: a malformed value is
    // rejected on its own merits, never contingent on which sibling happens to be set.
    let jwks_url = var_or(vars, "EPIC_JWKS_URL", EPIC_DEFAULT_JWKS_URL);
    check_endpoint("EPIC_JWKS_URL", &jwks_url)?;
    // Validated HERE rather than at verifier construction below, so a malformed issuer
    // is rejected on its own merits before the client-id completeness bail. The rule
    // itself belongs to the variant: `IssuerMatch::prefix` carries the absolute-URL
    // floor that `IssuerMatch::exact` deliberately does not.
    let issuer = IssuerMatch::prefix(
        "EPIC_ISSUER_PREFIX",
        &var_or(vars, "EPIC_ISSUER_PREFIX", EPIC_DEFAULT_ISSUER_PREFIX),
    )?;
    let oauth = EpicOAuthConfig::new(
        var_or(vars, "EPIC_CLIENT_SECRET", ""),
        var_or(vars, "EPIC_REDIRECT_URI", EPIC_DEFAULT_REDIRECT_URI),
        var_or(vars, "EPIC_AUTHORIZE_URL", EPIC_DEFAULT_AUTHORIZE_URL),
        var_or(vars, "EPIC_TOKEN_URL", EPIC_DEFAULT_TOKEN_URL),
    )?;

    let client_id = var_or(vars, "EPIC_CLIENT_ID", "");
    if client_id.is_empty() {
        anyhow::bail!(
            "{present} is set but EPIC_CLIENT_ID is not — the epic provider needs its client id \
             (the token audience)"
        );
    }
    let oauth = if oauth.client_secret.is_empty() {
        if let Some(key) = EPIC_OAUTH_VARS.iter().find(|key| vars.contains_key(**key)) {
            anyhow::bail!(
                "{key} is set but EPIC_CLIENT_SECRET is not — the epic web OAuth flow needs the \
                 confidential client secret"
            );
        }
        None
    } else {
        Some(oauth)
    };
    let verifier = Arc::new(OidcVerifier::new(&jwks_url, issuer, vec![client_id.clone()])?);
    Ok(Some(EpicConfig {
        client_id,
        jwks_url,
        verifier,
        oauth,
    }))
}

/// Google's whole environment surface. Presence of ANY of these means the operator is
/// configuring google.
const GOOGLE_VARS: &[&str] = &["GOOGLE_CLIENT_IDS", "GOOGLE_JWKS_URL"];

const GOOGLE_DEFAULT_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// Google's issuer spellings, both of which appear in real id_tokens. NOT an operator
/// knob: they are fixed protocol facts, and an env-widened issuer set would loosen the
/// single guard that keeps `https://accounts.google.com.evil.test` out.
const GOOGLE_ISSUERS: &[&str] = &["https://accounts.google.com", "accounts.google.com"];

fn google_from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<Option<GoogleConfig>> {
    let Some(present) = GOOGLE_VARS.iter().find(|key| vars.contains_key(**key)) else {
        return Ok(None);
    };
    for key in GOOGLE_VARS {
        if vars.get(*key).is_some_and(String::is_empty) {
            anyhow::bail!("invalid {key}: set but empty — unset it to leave it unconfigured");
        }
    }

    // Per-FIELD validation FIRST, cross-field completeness second (epic's order).
    let jwks_url = var_or(vars, "GOOGLE_JWKS_URL", GOOGLE_DEFAULT_JWKS_URL);
    check_endpoint("GOOGLE_JWKS_URL", &jwks_url)?;
    let Some(raw_client_ids) = vars.get("GOOGLE_CLIENT_IDS") else {
        anyhow::bail!(
            "{present} is set but GOOGLE_CLIENT_IDS is not — the google provider needs its \
             client id(s) (the token audiences)"
        );
    };
    let client_ids = split_list("GOOGLE_CLIENT_IDS", raw_client_ids)?;

    let verifier = Arc::new(OidcVerifier::new(
        &jwks_url,
        IssuerMatch::exact("accounts::providers::GOOGLE_ISSUERS", GOOGLE_ISSUERS)?,
        client_ids.clone(),
    )?);
    Ok(Some(GoogleConfig {
        client_ids,
        jwks_url,
        verifier,
    }))
}

/// Parses a comma-separated configuration list. An empty entry (`"a,,b"`, `"a, "`) is
/// an `Err`: dropping it silently would shrink an audience list the operator believes
/// they configured.
fn split_list(key: &str, raw: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            anyhow::bail!("invalid {key}: empty entry in the comma-separated list");
        }
        out.push(part.to_string());
    }
    Ok(out)
}

/// Mirrors the module's `env_or`: an absent OR empty value falls back to `def`, so
/// `FOO=` reads as unset rather than as a configured empty string.
fn var_or(vars: &BTreeMap<String, String>, key: &str, def: &str) -> String {
    match vars.get(key) {
        Some(v) if !v.is_empty() => v.clone(),
        _ => def.to_string(),
    }
}

/// The floor every provider URL sits on: parseable as an absolute URL and carrying a
/// host. Used alone for values that are MATCHED rather than dialed.
pub(crate) fn check_absolute_url(key: &str, raw: &str) -> anyhow::Result<url::Url> {
    let url = url::Url::parse(raw).map_err(|err| anyhow::anyhow!("invalid {key}: {err}"))?;
    if url.host().is_none() {
        anyhow::bail!("invalid {key}: host is required");
    }
    Ok(url)
}

/// The transport rule for a URL a browser or this process will actually go to: the
/// absolute-URL floor plus HTTPS, with plain HTTP allowed only against loopback (the
/// local-dev carve-out). Returns whether the scheme is HTTPS, which is what the OAuth
/// binding cookie's `Secure` flag keys off.
pub(crate) fn check_endpoint(key: &str, raw: &str) -> anyhow::Result<bool> {
    Ok(checked_endpoint(key, raw)?.1)
}

fn checked_endpoint(key: &str, raw: &str) -> anyhow::Result<(url::Url, bool)> {
    let url = check_absolute_url(key, raw)?;
    let secure = match url.scheme() {
        "https" => true,
        "http" if is_loopback(&url) => false,
        "http" => {
            anyhow::bail!("invalid {key}: HTTP is allowed only for localhost or a loopback IP")
        }
        other => {
            anyhow::bail!("invalid {key}: scheme must be HTTPS or loopback HTTP, got {other:?}")
        }
    };
    Ok((url, secure))
}

/// The OAuth redirect URI: the endpoint rule plus the two constraints specific to a
/// callback the IdP redirects a browser to — it must be exactly the route this module
/// mounts, and a fragment would be dropped by the redirect anyway. Returns the
/// binding cookie's `Secure` flag.
fn check_redirect_uri(key: &str, raw: &str) -> anyhow::Result<bool> {
    let (url, secure) = checked_endpoint(key, raw)?;
    if url.path() != "/accounts/epic/callback" {
        anyhow::bail!("invalid {key}: path must be /accounts/epic/callback");
    }
    if url.fragment().is_some() {
        anyhow::bail!("invalid {key}: fragments are not allowed");
    }
    Ok(secure)
}

/// Whether `url`'s host is loopback — the one authority for the plain-HTTP carve-out.
fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

//! The credential-provider seam: one naming authority, one registry of configured
//! verifiers, and one validating parse of the provider configuration.
//!
//! Three names, deliberately distinct: [`KNOWN_PROVIDERS`] is what this build knows
//! how to spell, [`Providers`] is what this PROCESS actually configured, and
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

use crate::epic::{short_id, OidcVerifier};

/// The `accounts.identities.provider` value for Epic — written once and referenced by
/// the name list, the registry key and every `resolve` call, so a typo cannot leave a
/// fully configured provider unresolvable.
pub(crate) const EPIC: &str = "epic";

/// Every provider name this build knows, configured or not — the naming authority,
/// separate from the configured-verifier map so a typo and an unconfigured provider
/// are distinguishable outcomes. A name is listed once the build can spell it, which
/// is not the same as shipping it: `"apple"` is declared and has no verifier.
pub(crate) const KNOWN_PROVIDERS: &[&str] = &["dev", EPIC, "google", "guest", "apple"];

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

/// The validated provider configuration: one entry per PRESENT provider. An absent
/// provider is simply missing; a present-but-malformed one is an `Err` from
/// [`ProviderConfig::from_vars`], never a silently disabled entry.
pub(crate) struct ProviderConfig {
    pub(crate) epic: Option<EpicConfig>,
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
        })
    }

    /// The credential registry this configuration implies — exactly the present
    /// providers, each behind its [`CredentialVerifier`] adapter.
    pub(crate) fn providers(&self) -> Providers {
        let mut providers = Providers::default();
        if let Some(epic) = &self.epic {
            providers.insert(EPIC, epic_credentials(epic.verifier.clone()));
        }
        providers
    }
}

/// Epic's `CredentialVerifier` face: an OIDC id_token in, the Epic account id plus the
/// `epic:<shortID>` first-sight display name out.
struct EpicCredentials {
    verifier: Arc<OidcVerifier>,
}

#[async_trait]
impl CredentialVerifier for EpicCredentials {
    async fn verify(&self, credential: &str) -> Result<VerifiedSubject, VerifyError> {
        let subject = self.verifier.verify(credential).await?;
        Ok(VerifiedSubject {
            display_name: format!("epic:{}", short_id(&subject)),
            subject,
        })
    }
}

/// Wraps a constructed OIDC verifier in Epic's credential face — shared by
/// [`ProviderConfig::providers`] and the tests that inject a fixture verifier.
pub(crate) fn epic_credentials(verifier: Arc<OidcVerifier>) -> Arc<dyn CredentialVerifier> {
    Arc::new(EpicCredentials { verifier })
}

/// Every variable the provider parse reads — the authority [`ProviderConfig::from_env`]
/// collects. A new provider appends its own block here rather than widening the read
/// back out to the whole environment.
fn provider_env_keys() -> impl Iterator<Item = &'static str> {
    EPIC_VARS.iter().copied()
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
    // The issuer prefix is a token-acceptance guard, not an endpoint we dial: it is
    // never fetched, so the scheme rule does not apply, but `epic.rs`'s `starts_with`
    // check makes a truncated value (`h`) accept every https issuer — hence the
    // absolute-URL floor. Step 3's `IssuerMatch` replaces this with a per-variant rule.
    let issuer_prefix = var_or(vars, "EPIC_ISSUER_PREFIX", EPIC_DEFAULT_ISSUER_PREFIX);
    check_absolute_url("EPIC_ISSUER_PREFIX", &issuer_prefix)?;
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
    let verifier = Arc::new(OidcVerifier::new(&jwks_url, &issuer_prefix, &client_id)?);
    Ok(Some(EpicConfig {
        client_id,
        jwks_url,
        verifier,
        oauth,
    }))
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
fn check_absolute_url(key: &str, raw: &str) -> anyhow::Result<url::Url> {
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

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

/// Every provider name this build knows, configured or not — the naming authority,
/// separate from the configured-verifier map so a typo and an unconfigured provider
/// are distinguishable outcomes. A name is listed once the build can spell it, which
/// is not the same as shipping it: `"apple"` is declared and has no verifier.
pub(crate) const KNOWN_PROVIDERS: &[&str] = &["dev", "epic", "google", "guest", "apple"];

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
}

/// The validated provider configuration: one entry per PRESENT provider. An absent
/// provider is simply missing; a present-but-malformed one is an `Err` from
/// [`ProviderConfig::from_vars`], never a silently disabled entry.
pub(crate) struct ProviderConfig {
    pub(crate) epic: Option<EpicConfig>,
}

impl ProviderConfig {
    pub(crate) fn from_env() -> anyhow::Result<ProviderConfig> {
        // `vars_os` + filter rather than `vars()`: the latter PANICS on any non-UTF-8
        // entry anywhere in the process environment, which has nothing to do with
        // provider configuration.
        let vars = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
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
            providers.insert("epic", epic_credentials(epic.verifier.clone()));
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

/// The epic variables that decide PRESENCE. The OAuth-flow variables
/// (`EPIC_CLIENT_SECRET`/`EPIC_REDIRECT_URI`/…) are not here: they configure the
/// browser flow layered on top of a configured verifier, not the verifier itself.
const EPIC_VARS: &[&str] = &["EPIC_CLIENT_ID", "EPIC_JWKS_URL", "EPIC_ISSUER_PREFIX"];

const EPIC_DEFAULT_JWKS_URL: &str =
    "https://api.epicgames.dev/epic/oauth/v1/.well-known/jwks.json";
const EPIC_DEFAULT_ISSUER_PREFIX: &str = "https://api.epicgames.dev/epic/oauth/v1";

fn epic_from_vars(vars: &BTreeMap<String, String>) -> anyhow::Result<Option<EpicConfig>> {
    let Some(present) = EPIC_VARS.iter().find(|key| !var_or(vars, key, "").is_empty()) else {
        return Ok(None);
    };
    let client_id = var_or(vars, "EPIC_CLIENT_ID", "");
    if client_id.is_empty() {
        anyhow::bail!(
            "{present} is set but EPIC_CLIENT_ID is empty — the epic provider needs its client id \
             (the token audience)"
        );
    }
    let jwks_url = var_or(vars, "EPIC_JWKS_URL", EPIC_DEFAULT_JWKS_URL);
    check_endpoint("EPIC_JWKS_URL", &jwks_url)?;
    let issuer_prefix = var_or(vars, "EPIC_ISSUER_PREFIX", EPIC_DEFAULT_ISSUER_PREFIX);
    let verifier = Arc::new(OidcVerifier::new(&jwks_url, &issuer_prefix, &client_id)?);
    Ok(Some(EpicConfig {
        client_id,
        jwks_url,
        verifier,
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

/// The transport rule for a provider endpoint we will dial: a parseable absolute URL
/// with a host, HTTPS, or plain HTTP only against loopback (the local-dev carve-out
/// `EpicOAuth::new` already applies to `EPIC_REDIRECT_URI`).
pub(crate) fn check_endpoint(key: &str, raw: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(raw).map_err(|err| anyhow::anyhow!("invalid {key}: {err}"))?;
    if url.host().is_none() {
        anyhow::bail!("invalid {key}: host is required");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&url) => Ok(()),
        "http" => {
            anyhow::bail!("invalid {key}: HTTP is allowed only for localhost or a loopback IP")
        }
        other => anyhow::bail!("invalid {key}: scheme must be HTTPS or loopback HTTP, got {other:?}"),
    }
}

/// Whether `url`'s host is loopback — the one authority for the plain-HTTP carve-out,
/// shared by the JWKS endpoint check and the OAuth redirect-URI check.
pub(crate) fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

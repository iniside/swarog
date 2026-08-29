//! Tool-owned module policy. Shipping crates contribute only factual probes.

use std::sync::Arc;

use crate::input_inventory::{Exposure, InputKey};
use crate::model::{
    ArgonParams, CapCase, Convention, Entry, EnvCase, Fixture, InputPolicy, OutageCase,
    OutageClass, Stance,
};

fn na(why: &'static str) -> Stance {
    Stance::NotApplicable { why }
}

/// The widest cap any registered credential verifier states — read from the registry
/// accounts actually builds, never a literal, so the one policy row a single wire field
/// permits cannot claim a number no provider accepts.
fn widest_credential_cap() -> usize {
    accounts::conformance::credential_caps()
        .into_values()
        .max()
        .expect("accounts always registers at least the guest verifier")
}

fn argon(params: (u32, u32, u32, usize)) -> Fixture {
    Fixture::ArgonParity(ArgonParams {
        m_cost: params.0,
        t_cost: params.1,
        p_cost: params.2,
        output_len: params.3,
    })
}

pub fn entries() -> Vec<Entry> {
    vec![
        accounts(),
        admin(),
        apikeys(),
        audit(),
        characters(),
        config(),
        gateway(),
        inventory(),
        leaderboard(),
        match_module(),
        rating(),
        scheduler(),
        wallet(),
    ]
}

pub fn input_policies() -> Vec<(InputKey, InputPolicy)> {
    use Exposure::{External, Wire};
    use InputPolicy::{Opaque, Validated};

    let key = |method: &str, field: &str, exposure| InputKey {
        wire_method: method.to_owned(),
        wire_field_name: field.to_owned(),
        exposure,
    };
    vec![
        (key("accounts.link", "credential", External), Validated { cap: widest_credential_cap(), basis: "the same single authority loginFederated traverses: accounts::Service::verify_credential applies the RESOLVED provider's own CredentialVerifier::max_credential_bytes through accounts::credential_within_cap, before any verifier, JWKS or database work. The number stated is the maximum of the cap map the registry accounts really builds; the per-provider caps are exercised by the CapCases below, which call that same shared path" }),
        (key("accounts.link", "provider", External), Validated { cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES, basis: "accounts::provider_name_within_cap runs first in accounts::Service::verify_credential — the one helper link and loginFederated share — before the provider name is used as a registry lookup key" }),
        (key("accounts.login", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production login path" }),
        (key("accounts.login", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production login path" }),
        (key("accounts.loginFederated", "credential", External), Validated { cap: widest_credential_cap(), basis: "there is no single cap here: this ONE wire field carries every provider's credential and the bound applied is the RESOLVED provider's own CredentialVerifier::max_credential_bytes, checked by accounts::credential_within_cap inside accounts::Service::verify_credential (shared with link) after the (already length-capped) provider name resolves in the registry and before any verifier, JWKS or database work. The number stated is the maximum of the cap map the registry accounts really builds, computed here rather than written down, so it cannot name a bound no provider states. The per-provider caps are the SUBJECT of checks::CREDENTIAL_CAPS, which is diffed against that same registry before any assertion runs and requires each provider's own cap to be executed by its own CapCase below" }),
        (key("accounts.loginFederated", "provider", External), Validated { cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES, basis: "accounts::provider_name_within_cap runs first in accounts::Service::verify_credential — the one helper login_federated and link share — before the provider name is used as a registry lookup key" }),
        (key("accounts.register", "displayName", External), Validated { cap: 128, basis: "accounts::display_name_within_cap validates the effective persisted display before Argon or SQL" }),
        (key("accounts.register", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production register path" }),
        (key("accounts.register", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production register path" }),
        (key("accounts.verifySession", "token", Wire), Validated { cap: accountsapi::MAX_SESSION_TOKEN_BYTES, basis: "accounts::session_token_within_cap rejects before session SQL and gateway dispatch uses the same contract cap" }),
        (key("admin.adminData", "params.<key>", Wire), Opaque { rationale: "the operator's flattened query string on a READ-only fan-out; each provider indexes it by a known name (adminapi::param) and ignores the rest, so an unrecognized key is never persisted or interpolated" }),
        (key("admin.adminData", "params.<value>", Wire), Opaque { rationale: "read-path only, and nothing on this path writes it. Two consumer shapes, both bounded by construction: a lookup value reaching SQL is a bound parameter behind a uuid parse (characters/inventory `owner`, wallet `player`), where an over-long value fails the parse or matches no row; and a DISPLAY value that never reaches a statement at all (characters::admin/inventory::admin `owner_name`, which only titles the drill-down page and is HTML-escaped by the portal's minijinja autoescape)" }),
        (key("admin.adminSubmit", "id", Wire), Opaque { rationale: "the provider's own admin slug (adminapi::Item::id), selected by the portal from its resolved item set rather than parsed from operator text" }),
        (key("admin.adminSubmit", "params.<key>", Wire), Opaque { rationale: "the key set is NOT closed — admin::collect_submit_params copies the form's own declared Field/HiddenField names, then accepts ANY submitted name starting with the reserved _expected_ prefix, so the suffix is operator-authored and unbounded. It is opaque because no consumer ever iterates these keys: every provider reads the map by a name it constructed itself (adminapi::param / the module's own _expected_<field> literal), so an unrecognized key is inert — never persisted, interpolated or echoed" }),
        (key("admin.adminSubmit", "params.<value>", Wire), Validated { cap: apikeys::conformance::MAX_POLICY_BYTES, basis: "every module exposing adminapi::AdminSubmit caps its declared form values in Rust and maps the mirroring column CHECK's 23514 back to the SAME verdict: wallet via admin::CATALOG_CAPS + the currencies_*_len_check constraints (widest 64) and its validate_movement caps, apikeys via store::COLUMN_CAPS + the roles_/keys_*_len_check constraints — MAX_NAME_BYTES (128) on every role/key name and MAX_POLICY_BYTES (4096, the widest declared form value) on a role policy. Two of the three claims are executable HERE: checks::ADMIN_SUBMIT_MODULES is diffed against modules/*/src before any assertion runs, so the implementor list cannot go stale, and every listed module must carry an input-byte-caps CapCase whose probe calls its real validator (wallet::conformance -> validate_movement, apikeys::conformance -> store::validate_name/validate_policy), so the caps themselves are exercised. The third — that the cap runs on the SUBMIT path, pre-SQL, rather than only as the column CHECK — is NOT decided by this gate: it is pinned by each module's own tests (wallet's direct admin::check_catalog_caps/check_decimals_range tests, apikeys' store_tests over_cap_* writers), because through apply_submit the Rust verdict and the CHECK's mapping are byte-identical and no input can separate them" }),
        (key("apikeys.lookupKey", "key", Wire), Validated { cap: apikeysapi::MAX_KEY_BYTES, basis: "gateway::RealKeyVerifier::lookup rejects a presented key over apikeysapi::MAX_KEY_BYTES before any store round-trip; secrets are server-generated, so there is no caller-supplied creation path to cap" }),
        (key("characters.create", "class", External), Validated { cap: 64, basis: "characters::class_within_cap validates the defaulted persisted class before SQL" }),
        (key("characters.create", "name", External), Validated { cap: 128, basis: "characters::name_within_cap validates the persisted name before SQL" }),
        (key("characters.delete", "character_id", External), Opaque { rationale: "opaque character UUID resolved by the characters store, not player-authored free text" }),
        (key("characters.ownerOf", "character_id", Wire), Opaque { rationale: "opaque character UUID passed between domain capabilities" }),
        (key("inventory.grant", "item_id", External), Opaque { rationale: "opaque catalog identifier accepted only when it exactly resolves to an existing inventory item" }),
        (key("inventory.listCharacter", "character_id", External), Opaque { rationale: "opaque character UUID authorized through characters::Ownership" }),
        (key("match.report", "Loser", External), Validated { cap: 128, basis: "match_module::validate_participant is called for every new loser before rating or SQL" }),
        (key("match.report", "ReportId", External), Validated { cap: 128, basis: "match_module::validate_report_id is called before the replay lookup" }),
        (key("match.report", "Winner", External), Validated { cap: 128, basis: "match_module::validate_participant is called for every new winner before rating or SQL" }),
        (key("rating.mmr", "player_id", Wire), Opaque { rationale: "opaque player UUID passed between domain capabilities" }),
        (key("wallet.balances", "player_id", Wire), Opaque { rationale: "opaque player UUID passed between domain capabilities" }),
        (key("wallet.credit", "movement.idempotency_key", Wire), Validated { cap: walletapi::MAX_IDEMPOTENCY_KEY_BYTES, basis: "wallet::idempotency_key_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
        (key("wallet.credit", "movement.player_id", Wire), Opaque { rationale: "opaque player UUID passed between domain capabilities" }),
        (key("wallet.credit", "movement.currency", Wire), Validated { cap: walletapi::MAX_CURRENCY_CODE_BYTES, basis: "wallet::currency_code_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
        (key("wallet.credit", "movement.reason", Wire), Validated { cap: walletapi::MAX_REASON_BYTES, basis: "wallet::reason_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
        (key("wallet.debit", "movement.idempotency_key", Wire), Validated { cap: walletapi::MAX_IDEMPOTENCY_KEY_BYTES, basis: "wallet::idempotency_key_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
        (key("wallet.debit", "movement.player_id", Wire), Opaque { rationale: "opaque player UUID passed between domain capabilities" }),
        (key("wallet.debit", "movement.currency", Wire), Validated { cap: walletapi::MAX_CURRENCY_CODE_BYTES, basis: "wallet::currency_code_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
        (key("wallet.debit", "movement.reason", Wire), Validated { cap: walletapi::MAX_REASON_BYTES, basis: "wallet::reason_within_cap runs inside validate_movement, called by every credit/debit before ledger SQL" }),
    ]
}

fn accounts() -> Entry {
    Entry {
        module: "accounts",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![
                    // accounts::providers::ProviderConfig::from_vars is a validating
                    // parse: the verifier constructors do no I/O at init, so a
                    // malformed provider value can only be caught here or never. Each
                    // case sets ONE variable and reaches a distinct branch — the URL
                    // rule, the issuer floor, and the browser-flow endpoint rule that
                    // must not be contingent on EPIC_CLIENT_SECRET being present.
                    EnvCase {
                        var: "EPIC_JWKS_URL",
                        bad_value: "hunter2",
                        expect: "invalid EPIC_JWKS_URL",
                    },
                    EnvCase {
                        var: "EPIC_ISSUER_PREFIX",
                        bad_value: "h",
                        expect: "invalid EPIC_ISSUER_PREFIX",
                    },
                    EnvCase {
                        var: "EPIC_AUTHORIZE_URL",
                        bad_value: "hunter2",
                        expect: "invalid EPIC_AUTHORIZE_URL",
                    },
                    // The second OIDC provider's own parse, one case per variable: a
                    // variable dropped from `provider_env_keys` is never collected by
                    // `from_env`, and only its OWN case turns red.
                    EnvCase {
                        var: "GOOGLE_JWKS_URL",
                        bad_value: "hunter2",
                        expect: "invalid GOOGLE_JWKS_URL",
                    },
                    EnvCase {
                        var: "GOOGLE_CLIENT_IDS",
                        bad_value: ",",
                        expect: "invalid GOOGLE_CLIENT_IDS",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "accounts login/register email",
                        cap: 320,
                        probe: Arc::new(accounts::conformance::conformance_email_rejected),
                    },
                    CapCase {
                        name: "accounts login/register password",
                        cap: 1024,
                        probe: Arc::new(accounts::conformance::conformance_password_rejected),
                    },
                    CapCase {
                        name: "accounts register effective display name",
                        cap: 128,
                        probe: Arc::new(accounts::conformance::conformance_display_name_rejected),
                    },
                    CapCase {
                        name: "accounts federated epic credential",
                        cap: accounts::conformance::MAX_OIDC_CREDENTIAL_BYTES,
                        probe: Arc::new(
                            accounts::conformance::conformance_epic_credential_rejected,
                        ),
                    },
                    CapCase {
                        name: "accounts federated google credential",
                        cap: accounts::conformance::MAX_OIDC_CREDENTIAL_BYTES,
                        probe: Arc::new(
                            accounts::conformance::conformance_google_credential_rejected,
                        ),
                    },
                    CapCase {
                        name: "accounts federated guest ticket",
                        cap: accounts::conformance::MAX_GUEST_CREDENTIAL_BYTES,
                        probe: Arc::new(
                            accounts::conformance::conformance_guest_credential_rejected,
                        ),
                    },
                    CapCase {
                        name: "accounts federated provider name",
                        cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES,
                        probe: Arc::new(accounts::conformance::conformance_provider_name_rejected),
                    },
                    CapCase {
                        name: "accounts session token",
                        cap: accountsapi::MAX_SESSION_TOKEN_BYTES,
                        probe: Arc::new(accounts::conformance::conformance_session_token_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                Stance::Applies(Fixture::InfraOutage503(vec![OutageCase {
                    name: "accounts loginFederated with a known but unconfigured provider",
                    probe: Arc::new(|| {
                        Box::pin(async {
                            match accounts::conformance::conformance_login_federated_unconfigured_provider()
                                .await
                            {
                                Err(error) if error.status.http() == 503 => {
                                    OutageClass::Unavailable
                                }
                                Err(error) if error.status.http() == 401 => OutageClass::Rejected,
                                Err(error) => OutageClass::Other(format!(
                                    "unexpected error status {:?}: {}",
                                    error.status, error.msg
                                )),
                                Ok(_) => OutageClass::Other(
                                    "login_federated succeeded with no provider configured".into(),
                                ),
                            }
                        })
                    }),
                }])),
            ),
            (
                Convention::ArgonParity,
                Stance::Applies(argon(accounts::argon2_params_for_parity_test())),
            ),
        ],
    }
}

fn admin() -> Entry {
    Entry {
        module: "admin",
        stances: vec![
            (
                Convention::EnvValidation,
                na("ADMIN_COOKIE_SECURE and ADMIN_OPEN are behavior gates, not parsed values"),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "admin login username",
                        cap: 128,
                        probe: Arc::new(admin::conformance::conformance_username_rejected),
                    },
                    CapCase {
                        name: "admin login password",
                        cap: 1024,
                        probe: Arc::new(admin::conformance::conformance_password_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("admin has no infrastructure-backed credential verifier of its own"),
            ),
            (
                Convention::ArgonParity,
                Stance::Applies(argon(admin::argon2_params_for_parity_test())),
            ),
        ],
    }
}

fn apikeys() -> Entry {
    Entry {
        module: "apikeys",
        stances: vec![
            (
                Convention::EnvValidation,
                na("APIKEYS_DEV_SEED is a boolean opt-in gate, not a parsed value"),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "apikeys gateway presented-key lookup",
                        cap: apikeysapi::MAX_KEY_BYTES,
                        probe: Arc::new(apikeys::conformance::conformance_key_rejected),
                    },
                    CapCase {
                        name: "apikeys role policy",
                        cap: apikeys::conformance::MAX_POLICY_BYTES,
                        probe: Arc::new(apikeys::conformance::conformance_policy_rejected),
                    },
                    CapCase {
                        name: "apikeys role/key name",
                        cap: apikeys::conformance::MAX_NAME_BYTES,
                        probe: Arc::new(apikeys::conformance::conformance_name_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("the API-key verifier and its outage classification live in gateway"),
            ),
            (
                Convention::ArgonParity,
                na("apikeys hashes high-entropy, server-generated secrets with SHA-256 for O(1) indexed lookup, not a password-KDF — argon2 parity does not apply"),
            ),
        ],
    }
}

fn audit() -> Entry {
    Entry {
        module: "audit",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![
                    EnvCase {
                        var: "AUDIT_RETENTION_DAYS",
                        bad_value: "0",
                        expect: "AUDIT_RETENTION_DAYS",
                    },
                    EnvCase {
                        var: "AUDIT_RETENTION_DAYS",
                        bad_value: "-3",
                        expect: "AUDIT_RETENTION_DAYS",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                na("audit is a raw event sink with no player-supplied free-text field"),
            ),
            (
                Convention::InfraOutage503,
                na("audit has no auth verifier or request-path outage classification"),
            ),
            (
                Convention::ArgonParity,
                na("audit has no credential material and performs no password hashing"),
            ),
        ],
    }
}

fn characters() -> Entry {
    Entry {
        module: "characters",
        stances: vec![
            (
                Convention::EnvValidation,
                na("characters parses no process environment"),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "characters create name",
                        cap: 128,
                        probe: Arc::new(characters::conformance::conformance_name_rejected),
                    },
                    CapCase {
                        name: "characters create class",
                        cap: 64,
                        probe: Arc::new(characters::conformance::conformance_class_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("characters has no external credential verifier"),
            ),
            (
                Convention::ArgonParity,
                na("this module performs no password hashing"),
            ),
        ],
    }
}

fn config() -> Entry {
    all_na(
        "config",
        "config parses no process environment",
        "config values are operator input, not player-facing free text",
        "config has no credential verifier",
    )
}

fn gateway() -> Entry {
    Entry {
        module: "gateway",
        stances: vec![
            (
                Convention::EnvValidation,
                na("gateway topology and peer values are injected by cmd roots; its dev flags are boolean gates"),
            ),
            (
                Convention::InputByteCaps,
                na("gateway owns transport guards; field-level caps belong to operation owners"),
            ),
            (
                Convention::InfraOutage503,
                Stance::Applies(Fixture::InfraOutage503(vec![
                    OutageCase {
                        name: "gateway RealKeyVerifier over a failing apikeys capability",
                        probe: Arc::new(|| {
                            Box::pin(async {
                                match gateway::conformance::conformance_key_outage().await {
                                    Err(_) => OutageClass::Unavailable,
                                    Ok(None) => OutageClass::Rejected,
                                    Ok(Some(_)) => OutageClass::Other(
                                        "lookup returned a record from a down dependency".into(),
                                    ),
                                }
                            })
                        }),
                    },
                    OutageCase {
                        name: "gateway authenticate over a failing session verifier",
                        probe: Arc::new(|| {
                            Box::pin(async {
                                match gateway::conformance::conformance_session_outage_status()
                                    .await
                                    .as_u16()
                                {
                                    503 => OutageClass::Unavailable,
                                    401 => OutageClass::Rejected,
                                    status => OutageClass::Other(format!(
                                        "unexpected HTTP status {status}"
                                    )),
                                }
                            })
                        }),
                    },
                ])),
            ),
            (
                Convention::ArgonParity,
                na("gateway delegates credential verification and performs no password hashing"),
            ),
        ],
    }
}

fn inventory() -> Entry {
    all_na(
        "inventory",
        "INVENTORY_DEV_GRANT is a boolean presence-gate, not a parsed value",
        "inventory player operations take ids and item references, not free text",
        "inventory has no bespoke credential verifier",
    )
}

fn leaderboard() -> Entry {
    all_na(
        "leaderboard",
        "leaderboard parses no process environment",
        "leaderboard takes no player-supplied free-text field",
        "leaderboard has no credential verifier",
    )
}

fn match_module() -> Entry {
    Entry {
        module: "match",
        stances: vec![
            (
                Convention::EnvValidation,
                na("match parses no process environment"),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "match report id",
                        cap: 128,
                        probe: Arc::new(match_module::conformance::conformance_report_id_rejected),
                    },
                    CapCase {
                        name: "match winner",
                        cap: 128,
                        probe: Arc::new(match_module::conformance::conformance_winner_rejected),
                    },
                    CapCase {
                        name: "match loser",
                        cap: 128,
                        probe: Arc::new(match_module::conformance::conformance_loser_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("match has no credential verifier"),
            ),
            (
                Convention::ArgonParity,
                na("this module performs no password hashing"),
            ),
        ],
    }
}

fn rating() -> Entry {
    all_na(
        "rating",
        "rating parses no process environment",
        "rating reads player ids and accepts no player-supplied free-text field",
        "rating has no external credential verifier",
    )
}

fn scheduler() -> Entry {
    all_na(
        "scheduler",
        "SCHEDULER_ENABLED is a boolean gate and intervals are database data",
        "scheduler rows are operator data, not player-supplied free text",
        "scheduler has no credential verifier",
    )
}

fn wallet() -> Entry {
    Entry {
        module: "wallet",
        stances: vec![
            (
                Convention::EnvValidation,
                na("WALLET_DEV_SEED is a boolean presence-gate, not a parsed value"),
            ),
            (
                Convention::InputByteCaps,
                // Covers the three movement fields validate_movement enforces before ledger
                // SQL (idempotency_key/currency/reason — player_id is Opaque, an unvalidated
                // UUID). Wallet's admin-submit catalog fields are capped separately:
                // display_name/kind through `admin::CATALOG_CAPS` backed by the
                // `currencies_*_len_check` columns, decimals as a range-checked i32. The
                // shared `admin.adminSubmit params.<value>` input key carries the
                // cross-module stance for that class.
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "wallet movement idempotency key",
                        cap: walletapi::MAX_IDEMPOTENCY_KEY_BYTES,
                        probe: Arc::new(wallet::conformance::conformance_idempotency_key_rejected),
                    },
                    CapCase {
                        name: "wallet movement currency code",
                        cap: walletapi::MAX_CURRENCY_CODE_BYTES,
                        probe: Arc::new(wallet::conformance::conformance_currency_code_rejected),
                    },
                    CapCase {
                        name: "wallet movement reason",
                        cap: walletapi::MAX_REASON_BYTES,
                        probe: Arc::new(wallet::conformance::conformance_reason_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("wallet has no external credential verifier"),
            ),
            (
                Convention::ArgonParity,
                na("this module performs no password hashing"),
            ),
        ],
    }
}

fn all_na(
    module: &'static str,
    env_why: &'static str,
    caps_why: &'static str,
    outage_why: &'static str,
) -> Entry {
    Entry {
        module,
        stances: vec![
            (Convention::EnvValidation, na(env_why)),
            (Convention::InputByteCaps, na(caps_why)),
            (Convention::InfraOutage503, na(outage_why)),
            (
                Convention::ArgonParity,
                na("this module performs no password hashing"),
            ),
        ],
    }
}

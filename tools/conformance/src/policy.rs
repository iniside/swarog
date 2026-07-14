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
        (key("accounts.login", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production login path" }),
        (key("accounts.login", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production login path" }),
        (key("accounts.loginEpic", "id_token", External), Validated { cap: 65_536, basis: "accounts::epic_id_token_within_cap is called before provider/JWKS work" }),
        (key("accounts.register", "displayName", External), Validated { cap: 128, basis: "accounts::display_name_within_cap validates the effective persisted display before Argon or SQL" }),
        (key("accounts.register", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production register path" }),
        (key("accounts.register", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production register path" }),
        (key("accounts.verifySession", "token", Wire), Validated { cap: accountsapi::MAX_SESSION_TOKEN_BYTES, basis: "accounts::session_token_within_cap rejects before session SQL and gateway dispatch uses the same contract cap" }),
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
    ]
}

fn accounts() -> Entry {
    Entry {
        module: "accounts",
        stances: vec![
            (
                Convention::EnvValidation,
                na("accounts env is presence-gates only; no parsed numeric value is silently defaulted at init"),
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
                        name: "accounts Epic ID token",
                        cap: 65_536,
                        probe: Arc::new(accounts::conformance::conformance_epic_id_token_rejected),
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
                    name: "accounts loginEpic with an unconfigured epic provider",
                    probe: Arc::new(|| {
                        Box::pin(async {
                            match accounts::conformance::conformance_login_epic_without_provider()
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
                                    "login_epic succeeded with no provider configured".into(),
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
                Stance::Applies(Fixture::InputByteCaps(vec![CapCase {
                    name: "apikeys gateway presented-key lookup",
                    cap: apikeysapi::MAX_KEY_BYTES,
                    probe: Arc::new(apikeys::conformance::conformance_key_rejected),
                }])),
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
                    },
                    EnvCase {
                        var: "AUDIT_RETENTION_DAYS",
                        bad_value: "-3",
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

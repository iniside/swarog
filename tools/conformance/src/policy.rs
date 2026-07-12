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
    use InputPolicy::{KnownGap, Opaque, Validated};

    let key = |method: &str, field: &str, exposure| InputKey {
        wire_method: method.to_owned(),
        wire_field_name: field.to_owned(),
        exposure,
    };
    vec![
        (key("accounts.login", "email", External), Validated { cap: 320, basis: "accounts::valid_email_bytes is called by the production login path" }),
        (key("accounts.login", "password", External), Validated { cap: 1024, basis: "accounts::valid_password_bytes is called by the production login path" }),
        (key("accounts.loginEpic", "id_token", External), KnownGap { planned_cap: 65_536, remediation: "Step 17 adds the shared Epic token validator" }),
        (key("accounts.register", "displayName", External), KnownGap { planned_cap: 128, remediation: "Step 17 adds the shared display-name validator" }),
        (key("accounts.register", "email", External), Validated { cap: 320, basis: "accounts::valid_email_bytes is called by the production register path" }),
        (key("accounts.register", "password", External), Validated { cap: 1024, basis: "accounts::valid_password_bytes is called by the production register path" }),
        (key("accounts.verifySession", "token", Wire), KnownGap { planned_cap: 128, remediation: "Step 13 rejects oversized bearer tokens before SQL or remote lookup" }),
        (key("apikeys.lookupKey", "key", Wire), Validated { cap: apikeysapi::MAX_KEY_BYTES, basis: "gateway lookup and apikeys creation both enforce apikeysapi::MAX_KEY_BYTES" }),
        (key("characters.create", "class", External), KnownGap { planned_cap: 64, remediation: "Step 17 adds the shared character-class validator" }),
        (key("characters.create", "name", External), KnownGap { planned_cap: 128, remediation: "Step 17 adds the shared character-name validator" }),
        (key("characters.delete", "character_id", External), Opaque { rationale: "opaque character UUID resolved by the characters store, not player-authored free text" }),
        (key("characters.ownerOf", "character_id", Wire), Opaque { rationale: "opaque character UUID passed between domain capabilities" }),
        (key("inventory.grant", "item_id", External), Opaque { rationale: "opaque catalog identifier accepted only when it exactly resolves to an existing inventory item" }),
        (key("inventory.listCharacter", "character_id", External), Opaque { rationale: "opaque character UUID authorized through characters::Ownership" }),
        (key("match.report", "Loser", External), KnownGap { planned_cap: 128, remediation: "Step 17 adds the shared match participant validator" }),
        (key("match.report", "ReportId", External), KnownGap { planned_cap: 128, remediation: "Step 17 adds the shared report-id validator" }),
        (key("match.report", "Winner", External), KnownGap { planned_cap: 128, remediation: "Step 17 adds the shared match participant validator" }),
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
                Stance::KnownGap {
                    why: "accounts caps email and password, but the wire-reachable register display_name field remains uncapped",
                    remediation: "Step 17 adds a shared production validator capping display_name at 128 bytes while retaining the existing email/password caps",
                },
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
                    name: "apikeys key secret",
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
                na("apikeys stores plaintext key secrets and performs no password hashing"),
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
                Stance::KnownGap {
                    why: "characters.create accepts wire-reachable name and class strings without enforced byte caps",
                    remediation: "Step 17 adds shared production validators capping character name at 128 bytes and class at 64 bytes",
                },
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
                Stance::KnownGap {
                    why: "/match/report accepts wire-reachable ReportId, Winner, and Loser strings without enforced byte caps",
                    remediation: "Step 17 adds shared production validators capping ReportId, winner, and loser at 128 bytes",
                },
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

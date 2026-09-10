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
        friends(),
        gateway(),
        groups(),
        inventory(),
        leaderboard(),
        mail(),
        match_module(),
        notifications(),
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
        (key("accounts.deleteAccount", "ticket", External), Validated { cap: accounts::conformance::MAX_DELETE_TICKET_BYTES, basis: "accounts::delete_ticket_within_cap is the ONLY shape check accountsapi::Auth::delete_account applies, and it runs before Service::delete_player_atomic opens its transaction, so no over-cap ticket reaches a statement. The \"accounts delete ticket\" CapCase drives that op ITSELF against a pool that cannot connect and discriminates on STATUS: at the cap the ticket passes the guard, reaches the store and answers Internal; over the cap the guard answers Invalid first. A live pool could not decide this row — an unknown ticket is the same NotFound as a live one, by design, so the dead pool is what makes the cap executable" }),
        (key("accounts.findByHandle", "handle", Wire), Validated { cap: accountsapi::MAX_HANDLE_BYTES, basis: "accounts::handle_within_cap runs as the FIRST statement of Directory::find_by_handle, before the store round-trip — the same authority friends::Player::request checks before resolving a target and the one input_policies() names for friends' target_handle, so a caller of either op meets the identical bound" }),
        (key("accounts.link", "credential", External), Validated { cap: widest_credential_cap(), basis: "the same single authority loginFederated traverses: accounts::Service::verify_credential applies the RESOLVED provider's own CredentialVerifier::max_credential_bytes through accounts::credential_within_cap, before any verifier, JWKS or database work. The number stated is the maximum of the cap map the registry accounts really builds; the per-provider caps are exercised by the CapCases below, which call that same shared path, and the \"accounts link guest credential\" case drives accountsapi::Auth::link ITSELF against the resolved guest verifier's own bound, so deleting link's guards turns this row red" }),
        (key("accounts.link", "provider", External), Validated { cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES, basis: "accounts::provider_name_within_cap runs first in accounts::Service::verify_credential — the one helper link and loginFederated share — before the provider name is used as a registry lookup key. The \"accounts link provider name\" CapCase executes it through accountsapi::Auth::link itself, not through the helper below the guard" }),
        (key("accounts.login", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production login path" }),
        (key("accounts.login", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production login path" }),
        (key("accounts.loginFederated", "credential", External), Validated { cap: widest_credential_cap(), basis: "there is no single cap here: this ONE wire field carries every provider's credential and the bound applied is the RESOLVED provider's own CredentialVerifier::max_credential_bytes, checked by accounts::credential_within_cap inside accounts::Service::verify_credential (shared with link) after the (already length-capped) provider name resolves in the registry and before any verifier, JWKS or database work. The number stated is the maximum of the cap map the registry accounts really builds, computed here rather than written down, so it cannot name a bound no provider states. The per-provider caps are the SUBJECT of checks::CREDENTIAL_CAPS, which is diffed against that same registry before any assertion runs and requires each provider's own cap to be executed by its own CapCase below" }),
        (key("accounts.loginFederated", "provider", External), Validated { cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES, basis: "accounts::provider_name_within_cap runs first in accounts::Service::verify_credential — the one helper login_federated and link share — before the provider name is used as a registry lookup key" }),
        (key("accounts.playersById", "ids", Wire), Validated { cap: accountsapi::MAX_LOOKUP_IDS, basis: "Directory::players_by_id refuses a batch over accountsapi::MAX_LOOKUP_IDS before the store round-trip; the cap bounds ITEM COUNT rather than a single field's byte length, the only field-level policy row of that shape" }),
        (key("accounts.refresh", "refresh_token", External), Validated { cap: accountsapi::MAX_SESSION_TOKEN_BYTES, basis: "accounts::session_token_within_cap runs as the FIRST statement of accountsapi::Auth::refresh, before the rotation transaction is opened — the same contract cap verifySession applies to the access token, because one mint function produces both. The \"accounts refresh token\" CapCase drives the op itself against a pool that cannot connect, so the 401 it asserts is reachable only ahead of the store: with the guard deleted the over-cap case reaches the rotation and answers Internal instead" }),
        (key("accounts.register", "displayName", External), Validated { cap: accountsapi::MAX_DISPLAY_NAME_BYTES, basis: "accounts::display_name_within_cap validates the effective persisted display before Argon or SQL" }),
        (key("accounts.register", "email", External), Validated { cap: 320, basis: "accounts::email_within_cap is called by the production register path" }),
        (key("accounts.register", "password", External), Validated { cap: 1024, basis: "accounts::password_within_cap is called by the production register path" }),
        (key("accounts.verifySession", "token", Wire), Validated { cap: accountsapi::MAX_SESSION_TOKEN_BYTES, basis: "accounts::session_token_within_cap rejects before session SQL and gateway dispatch uses the same contract cap" }),
        (key("admin.adminData", "params.<key>", Wire), Opaque { rationale: "the operator's flattened query string on a READ-only fan-out; each provider indexes it by a known name (adminapi::param) and ignores the rest, so an unrecognized key is never persisted or interpolated" }),
        (key("admin.adminData", "params.<value>", Wire), Opaque { rationale: "read-path only, and nothing on this path writes it. Two consumer shapes, both bounded by construction: a lookup value reaching SQL is a bound parameter behind a uuid parse (characters/inventory `owner`, wallet `player`), where an over-long value fails the parse or matches no row; and a DISPLAY value that never reaches a statement at all (characters::admin/inventory::admin `owner_name`, which only titles the drill-down page and is HTML-escaped by the portal's minijinja autoescape)" }),
        (key("admin.adminSubmit", "id", Wire), Opaque { rationale: "the provider's own admin slug (adminapi::Item::id), selected by the portal from its resolved item set rather than parsed from operator text" }),
        (key("admin.adminSubmit", "params.<key>", Wire), Opaque { rationale: "the key set is NOT closed — admin::collect_submit_params copies the form's own declared Field/HiddenField names, then accepts ANY submitted name starting with the reserved _expected_ prefix, so the suffix is operator-authored and unbounded. It is opaque because no consumer ever iterates these keys: every provider reads the map by a name it constructed itself (adminapi::param / the module's own _expected_<field> literal), so an unrecognized key is inert — never persisted, interpolated or echoed" }),
        (key("admin.adminSubmit", "params.<value>", Wire), Validated { cap: apikeys::conformance::MAX_POLICY_BYTES, basis: "every module exposing adminapi::AdminSubmit constrains its declared form values in Rust before any statement carrying that value runs, mostly as byte caps mirroring a column CHECK: wallet via admin::CATALOG_CAPS + the currencies_*_len_check constraints (widest 64) and its validate_movement caps, apikeys via store::COLUMN_CAPS + the roles_/keys_*_len_check constraints — MAX_NAME_BYTES (128) on every role/key name and MAX_POLICY_BYTES (4096, the widest declared form value ANY implementor posts) on a role policy — and notifications via service::validate_new inside its single insert authority, MAX_TITLE_BYTES (200) and MAX_BODY_BYTES (4000) against notifications_title_len_check/notifications_body_len_check and, on its hidden _idem_send field, an EXACT shape rather than a ceiling — admin::rendered_key admits only the 48-byte minted key (admin-send-mail- + 32 hex) before any SQL, and service::send_operator_mail re-checks it at the insert authority so the shared dedup column's two key spaces stay disjoint — groups via admin::rendered_group and apply_submit's own player check, two EXACT uuid shapes (36 bytes) rather than ceilings, both refused before any statement reaches the store, and mail via service::validate_new inside its single enqueue authority, store::COLUMN_CAPS against mail_outbox_*_len_check, plus two EXACT shapes rather than ceilings: admin::is_outbox_id on the selected row id (36 bytes, the uuid spelling the selector rendered) and admin::rendered_key on the hidden _idem_test field (48 bytes, admin-send-test- + 32 hex), both refused before the value reaches a statement. Mail is the one implementor whose caps run with a transaction already open: enqueue_from_admin checks out a bounded tx before validate_new, but that tx has issued only SET LOCAL statement_timeout, so no operator value has reached a statement. Its sibling bound, MAX_DEDUP_KEY_BYTES (128) in validate_new, is NOT a submit-path cap and no form input can exercise it: it covers the OTHER half of that shared column, the fan-in's event_id, and is the only level that can word a verdict there because a btree index key has no column CHECK beneath it, so an over-long value would be an unmappable 54000 rather than a 23514. The 23514 mapping is NOT uniform and the difference is deliberate: wallet and apikeys map the CHECK back to the SAME operator-facing verdict because their writes can reach it, while notifications runs both caps in the same function that issues the INSERT, so nothing passing them reaches the CHECK and it stands only as the class fail-safe under them. Two declared form values carry no Rust byte cap. notifications' player_id is operator text bounded by the column's $1::uuid cast instead — the 22P02 comes back as Status::Invalid, never a row nobody owns. The other is the _action discriminant, and it is uncapped in ALL FIVE implementors by the same argument rather than by oversight: collect_submit_params takes it from the POSTED value, but every apply_submit matches it against a closed set of literal actions, and a value outside that set is never persisted, never interpolated into SQL and never forwarded — its only reach is being echoed back into the operator-facing rejection message. Its LENGTH rests on axum's default request-body limit, which the admin POST's axum::body::Bytes extractor honours and which nothing under core/ or modules/admin disables — a whole-request bound the HTTP layer owes every form field, not a per-module cap four modules should each grow. Two of the three claims are executable HERE: checks::ADMIN_SUBMIT_MODULES is diffed against modules/*/src before any assertion runs, so the implementor list cannot go stale, and every listed module must carry an input-byte-caps CapCase whose probe calls its real validator (today wallet::conformance -> validate_movement, apikeys::conformance -> store::validate_name/validate_policy, mail::conformance -> service::validate_new, and groups::conformance -> admin::apply_submit), so the caps themselves are exercised. The third — that the cap runs on the SUBMIT path, pre-SQL, rather than only as the column CHECK — is NOT decided by this gate: it is pinned by each module's own tests (wallet's direct admin::check_catalog_caps/check_decimals_range tests, apikeys' store_tests over_cap_* writers), because in those two, through apply_submit, the Rust verdict and the CHECK's mapping are byte-identical and no input can separate them. Mail's and groups' shape cases are the exception: each drives its own admin::apply_submit ITSELF against a store that cannot connect and discriminates on the verdict — Stale for a value the page did not render, Rejected for the operator's own bad input — so for those four the submit-path claim IS decided here" }),
        (key("apikeys.lookupKey", "key", Wire), Validated { cap: apikeysapi::MAX_KEY_BYTES, basis: "gateway::RealKeyVerifier::lookup rejects a presented key over apikeysapi::MAX_KEY_BYTES before any store round-trip; secrets are server-generated, so there is no caller-supplied creation path to cap" }),
        (key("characters.create", "class", External), Validated { cap: 64, basis: "characters::class_within_cap validates the defaulted persisted class before SQL" }),
        (key("characters.create", "name", External), Validated { cap: 128, basis: "characters::name_within_cap validates the persisted name before SQL" }),
        (key("characters.delete", "character_id", External), Opaque { rationale: "opaque character UUID resolved by the characters store, not player-authored free text" }),
        (key("characters.ownerOf", "character_id", Wire), Opaque { rationale: "opaque character UUID passed between domain capabilities" }),
        (key("friends.accept", "edge_id", External), Opaque { rationale: "opaque relation-edge UUID, bound as $1::uuid alongside the caller's player_id in store::view_edge/accept_tx; a value that is not a uuid raises 22P02, which friends::store::is_invalid_uuid folds into the same 404 an unknown edge id gets, so it is never persisted, interpolated or echoed" }),
        (key("friends.decline", "edge_id", External), Opaque { rationale: "opaque relation-edge UUID, bound as $1::uuid alongside the caller's player_id in store::view_edge/decline_tx; a value that is not a uuid raises 22P02, folded by friends::store::is_invalid_uuid into the same 404 an unknown edge id gets" }),
        (key("friends.list", "cursor", External), Validated { cap: friendsapi::MAX_CURSOR_BYTES, basis: "friends::service::decode_cursor checks MAX_CURSOR_BYTES before the base64 decode, before the keyset halves are parsed and before any store read, and Player::list/pending both call it ahead of Store::page. The \"friends list/pending cursor\" CapCase drives Player::list itself against a pool that cannot connect and discriminates on the VERDICT, both arms being Status::Invalid: at the cap the value cannot be a well-formed keyset and answers the malformed-cursor verdict, over the cap the cap verdict answers first" }),
        (key("friends.pending", "cursor", External), Validated { cap: friendsapi::MAX_CURSOR_BYTES, basis: "friends::service::decode_cursor is the SAME authority Player::list's row states — Player::pending calls the identical helper before any store read, so the cursor cap and its verdict discrimination are shared, not restated" }),
        (key("friends.remove", "edge_id", External), Opaque { rationale: "opaque relation-edge UUID, bound as $1::uuid alongside the caller's player_id in store::view_edge/delete_tx; a value that is not a uuid raises 22P02, folded by friends::store::is_invalid_uuid into the same 404 an unknown edge id gets" }),
        (key("friends.request", "target_handle", External), Validated { cap: accountsapi::MAX_HANDLE_BYTES, basis: "friends deliberately owns no second const for this bound: Player::request checks target_handle.len() against accountsapi::MAX_HANDLE_BYTES — the SAME authority accounts::handle_within_cap enforces for Directory::find_by_handle — before ever calling the directory. The \"friends request target handle\" CapCase drives Player::request itself against a resolved-but-failing Directory, discriminating on status: an at-cap handle reaches (and is refused by) the directory and answers Unavailable, never Invalid, so only the cap's own rejection registers as Invalid" }),
        (key("groups.create", "join_policy", External), Opaque { rationale: "a closed vocabulary, not free text: groups::service::validate_policy matches the posted value against the three groupsapi join-policy consts (open/request/invite) and answers a &'static str, so the value that reaches a statement is the compiled literal and never the caller's bytes. Anything outside the set is refused with Status::Invalid before any store work, and the rejection names the ALLOWED values rather than echoing the input" }),
        (key("groups.create", "name", External), Validated { cap: groupsapi::MAX_NAME_BYTES, basis: "groups::service::validate_name runs as Player::create's second statement — after the identity is read and before the transaction is opened, so no operator value has reached a statement. The \"groups create name\" CapCase drives Player::create ITSELF against a pool that cannot connect and discriminates on STATUS: at the cap the name passes the guard, reaches the store and answers Internal; over the cap the guard answers Invalid first. Delete validate_name and the over-cap name reaches the dead store too, so the case goes red rather than passing on the column below it" }),
        (key("groups.decide", "decision", External), Opaque { rationale: "a closed vocabulary, not free text: groups::service::validate_decision matches the posted value against the two groupsapi decision consts (accept/reject) and answers a &'static str, so the value that reaches a statement is the compiled literal and never the caller's bytes. Anything outside the set is refused with Status::Invalid before any store work, and the rejection names the ALLOWED values rather than echoing the input" }),
        (key("groups.decide", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for decide; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.decide", "subject_id", External), Opaque { rationale: "opaque player UUID naming the subject of the decision, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for decide; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.invite", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for invite; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.invite", "target_handle", External), Validated { cap: accountsapi::MAX_HANDLE_BYTES, basis: "groups deliberately owns no second const for this bound: Player::invite checks target_handle.len() against accountsapi::MAX_HANDLE_BYTES — the SAME authority accounts::handle_within_cap enforces for Directory::find_by_handle and the one friends::Player::request checks — before the roster read and before the directory call. The \"groups invite target handle\" CapCase drives Player::invite ITSELF against a pool that cannot connect, discriminating on status: at the cap the handle passes the guard and the op answers Internal off the dead store, so only the cap's own rejection registers as Invalid" }),
        (key("groups.join", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for join; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.leave", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for leave; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.listMine", "cursor", External), Validated { cap: groupsapi::MAX_CURSOR_BYTES, basis: "groups::service::decode_cursor applies MAX_CURSOR_BYTES as its first check after the empty-cursor arm — before the base64 decode, before the keyset halves are parsed and before any store read — and list_mine/members/pending all call it ahead of the page statement. The \"groups list/members/pending cursor\" CapCase drives Player::list_mine ITSELF against a pool that cannot connect and discriminates on the VERDICT rather than the status, because both arms are Status::Invalid: at the cap the filler cannot be a well-formed keyset and answers the malformed-cursor verdict, over the cap the cap verdict answers first" }),
        (key("groups.members", "cursor", External), Validated { cap: groupsapi::MAX_CURSOR_BYTES, basis: "groups::service::decode_cursor is the SAME authority list_mine's row states — page_group calls the identical helper before the visibility check and before any store read, so the cursor cap and its verdict discrimination are shared, not restated" }),
        (key("groups.members", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for the member page; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.pending", "cursor", External), Validated { cap: groupsapi::MAX_CURSOR_BYTES, basis: "the same groups::service::decode_cursor call page_group makes for the member page — one helper, one cap, two ops" }),
        (key("groups.pending", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for the pending page; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.respond", "decision", External), Opaque { rationale: "a closed vocabulary, not free text: groups::service::validate_decision matches the posted value against the two groupsapi decision consts (accept/reject) and answers a &'static str, so the value that reaches a statement is the compiled literal and never the caller's bytes. Anything outside the set is refused with Status::Invalid before any store work, and the rejection names the ALLOWED values rather than echoing the input" }),
        (key("groups.respond", "group_id", External), Opaque { rationale: "opaque group UUID, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for respond; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.roleOf", "group_id", Wire), Opaque { rationale: "opaque group UUID passed between domain capabilities, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for the roster predicate; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("groups.roleOf", "player_id", Wire), Opaque { rationale: "opaque player UUID passed between domain capabilities, bound as $1::uuid alongside the caller's own player_id in the statements groups::store runs for the roster predicate; every one of those helpers folds the 22P02 a non-uuid raises into the ABSENT-row answer (Ok(None)/empty rows), so a malformed id gets the same NotFound an unknown id gets and is never persisted, interpolated or echoed" }),
        (key("inventory.grant", "item_id", External), Opaque { rationale: "opaque catalog identifier accepted only when it exactly resolves to an existing inventory item" }),
        (key("inventory.listCharacter", "character_id", External), Opaque { rationale: "opaque character UUID authorized through characters::Ownership" }),
        (key("match.report", "Loser", External), Validated { cap: 128, basis: "match_module::validate_participant is called for every new loser before rating or SQL" }),
        (key("match.report", "ReportId", External), Validated { cap: 128, basis: "match_module::validate_report_id is called before the replay lookup" }),
        (key("match.report", "Winner", External), Validated { cap: 128, basis: "match_module::validate_participant is called for every new winner before rating or SQL" }),
        (key("notifications.delete", "notification_id", External), Opaque { rationale: "opaque notification UUID, bound as $1::uuid in the DELETE's own predicate alongside the caller's player_id; a value that is not a uuid raises 22P02, which notifications::is_invalid_uuid folds into the same 404 an unknown id gets, so it is never persisted, interpolated or echoed" }),
        (key("notifications.list", "cursor", External), Validated { cap: notificationsapi::MAX_CURSOR_BYTES, basis: "notifications::service::decode_cursor applies MAX_CURSOR_BYTES as its first check after the empty-cursor arm — before the base64 decode, before the keyset halves are parsed and before any store read — and notificationsapi::Player::list calls it ahead of Store::page_by_player. The \"notifications list cursor\" CapCase drives that op ITSELF against a pool that cannot connect, and discriminates on the VERDICT rather than the status because both arms are Status::Invalid: at the cap the value cannot be a well-formed keyset and answers the malformed-cursor verdict, over the cap the cap verdict answers first. Delete the cap and the over-cap value falls through to malformed; delete the decode_cursor call and both arms reach the dead store and answer Internal — either way the case goes red" }),
        (key("notifications.markRead", "notification_id", External), Opaque { rationale: "opaque notification UUID, bound as $1::uuid in the UPDATE's own predicate alongside the caller's player_id; a value that is not a uuid raises 22P02, which notifications::is_invalid_uuid folds into the same 404 an unknown id gets, so it is never persisted, interpolated or echoed" }),
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
                        name: "accounts link provider name",
                        cap: accounts::conformance::MAX_PROVIDER_NAME_BYTES,
                        probe: Arc::new(accounts::conformance::conformance_link_provider_rejected),
                    },
                    CapCase {
                        name: "accounts link guest credential",
                        cap: accounts::conformance::MAX_GUEST_CREDENTIAL_BYTES,
                        probe: Arc::new(
                            accounts::conformance::conformance_link_credential_rejected,
                        ),
                    },
                    CapCase {
                        name: "accounts delete ticket",
                        cap: accounts::conformance::MAX_DELETE_TICKET_BYTES,
                        probe: Arc::new(accounts::conformance::conformance_delete_ticket_rejected),
                    },
                    CapCase {
                        name: "accounts refresh token",
                        cap: accountsapi::MAX_SESSION_TOKEN_BYTES,
                        probe: Arc::new(accounts::conformance::conformance_refresh_token_rejected),
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

fn friends() -> Entry {
    Entry {
        module: "friends",
        stances: vec![
            (
                Convention::EnvValidation,
                na("friends parses no process environment"),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    // `accountsapi::MAX_HANDLE_BYTES` is friends' one borrowed cap — see
                    // the input_policies() row for why friends owns no second const for
                    // it. Driven through `Player::request` itself, discriminated by
                    // status against a resolved-but-failing directory so an at-cap value
                    // reaches (and is refused by) the directory, never the cap.
                    CapCase {
                        name: "friends request target handle",
                        cap: accountsapi::MAX_HANDLE_BYTES,
                        probe: Arc::new(friends::conformance::conformance_target_handle_rejected),
                    },
                    CapCase {
                        name: "friends list/pending cursor",
                        cap: friendsapi::MAX_CURSOR_BYTES,
                        probe: Arc::new(friends::conformance::conformance_cursor_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                // The directory IS friends' one synchronous external dependency: every
                // mutating op resolves it before any write. `directory_unavailable`
                // (modules/friends/src/service.rs) folds ANY directory error into
                // Status::Unavailable, never a page of blank names — this is the ONE
                // outage story friends has, and it is genuine, unlike a module with no
                // synchronous op at all (mail's na() explains that distinct case).
                Stance::Applies(Fixture::InfraOutage503(vec![OutageCase {
                    name: "friends request over a failing accounts directory capability",
                    probe: Arc::new(|| {
                        Box::pin(async {
                            match friends::conformance::conformance_directory_outage().await {
                                Err(error) if error.status.http() == 503 => {
                                    OutageClass::Unavailable
                                }
                                Err(error) => OutageClass::Other(format!(
                                    "unexpected error status {:?}: {}",
                                    error.status, error.msg
                                )),
                                Ok(_) => OutageClass::Other(
                                    "request succeeded with the directory capability down"
                                        .into(),
                                ),
                            }
                        })
                    }),
                }])),
            ),
            (
                Convention::ArgonParity,
                na("friends performs no password hashing"),
            ),
        ],
    }
}

fn groups() -> Entry {
    Entry {
        module: "groups",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![
                    // Only an UNSET variable takes the compiled default
                    // (groups::projection::retention_days_from_env). Blank rather than empty
                    // because std::env::set_var("") REMOVES the variable on Windows, where
                    // the case would then assert nothing.
                    EnvCase {
                        var: "GROUPS_RETENTION_DAYS",
                        bad_value: "   ",
                        expect: "GROUPS_RETENTION_DAYS must be a whole number of days",
                    },
                    EnvCase {
                        var: "GROUPS_RETENTION_DAYS",
                        bad_value: "0",
                        expect: "GROUPS_RETENTION_DAYS must be between 1 and",
                    },
                    // The ceiling: a retention beyond the range make_interval can subtract
                    // from now() passes startup and then raises 22008 on every sweep,
                    // pausing the subscription.
                    EnvCase {
                        var: "GROUPS_RETENTION_DAYS",
                        bad_value: "99999999",
                        expect: "GROUPS_RETENTION_DAYS must be between 1 and",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "groups create name",
                        cap: groupsapi::MAX_NAME_BYTES,
                        probe: Arc::new(groups::conformance::conformance_name_rejected),
                    },
                    CapCase {
                        name: "groups list/members/pending cursor",
                        cap: groupsapi::MAX_CURSOR_BYTES,
                        probe: Arc::new(groups::conformance::conformance_cursor_rejected),
                    },
                    CapCase {
                        name: "groups invite target handle",
                        cap: accountsapi::MAX_HANDLE_BYTES,
                        probe: Arc::new(groups::conformance::conformance_target_handle_rejected),
                    },
                    // The two admin-form values, the reason groups is in
                    // checks::ADMIN_SUBMIT_MODULES: both are EXACT uuid shapes rather than
                    // ceilings, and both cases drive admin::apply_submit itself.
                    CapCase {
                        name: "groups admin selected group id",
                        cap: groups::conformance::UUID_SHAPE_BYTES,
                        probe: Arc::new(groups::conformance::conformance_admin_group_rejected),
                    },
                    CapCase {
                        name: "groups admin promoted player id",
                        cap: groups::conformance::UUID_SHAPE_BYTES,
                        probe: Arc::new(groups::conformance::conformance_admin_player_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                // The accounts directory IS groups' one synchronous external dependency:
                // every member/pending page hydrates its handles through it. The case drives
                // the hydrate call those pages make, discriminating on status, so a
                // capability that cannot be reached surfaces 503 and never a page of blank
                // handles. `invite` and `join` fold through the SAME
                // service::directory_unavailable but re-check the caller's role in the
                // database first, so neither is reachable without a live store — a known gap
                // this zero-I/O case cannot close, left to the split-proof fleet.
                Stance::Applies(Fixture::InfraOutage503(vec![OutageCase {
                    name: "groups member page hydrate over a failing accounts directory capability",
                    probe: Arc::new(|| {
                        Box::pin(async {
                            match groups::conformance::conformance_directory_outage().await {
                                Err(error) if error.status.http() == 503 => {
                                    OutageClass::Unavailable
                                }
                                Err(error) => OutageClass::Other(format!(
                                    "unexpected error status {:?}: {}",
                                    error.status, error.msg
                                )),
                                Ok(_) => OutageClass::Other(
                                    "the page hydrated with the directory capability down"
                                        .into(),
                                ),
                            }
                        })
                    }),
                }])),
            ),
            (
                Convention::ArgonParity,
                na("groups performs no password hashing"),
            ),
        ],
    }
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
                Stance::Applies(Fixture::InputByteCaps(vec![
                    // The front's OWN enforcement of the two credential caps, which no
                    // other case executes: accounts' and apikeys' own cases probe their
                    // store-side checks, and apikeys' restates the comparison
                    // arithmetically. Both are reached from every credentialed request on
                    // both planes AND from the `/push` handshake frame, where the values
                    // arrive from a client that cannot set headers.
                    CapCase {
                        name: "gateway presented bearer",
                        cap: accountsapi::MAX_SESSION_TOKEN_BYTES,
                        probe: Arc::new(gateway::conformance::conformance_session_token_rejected),
                    },
                    CapCase {
                        name: "gateway presented api key",
                        cap: apikeysapi::MAX_KEY_BYTES,
                        probe: Arc::new(gateway::conformance::conformance_api_key_rejected),
                    },
                    // The field the gateway both NAMES and BOUNDS itself: an
                    // authenticated `/push` client picks its own group names, and each
                    // accepted one is stored per connection and per process.
                    CapCase {
                        name: "gateway push group name",
                        cap: gateway::conformance::MAX_GROUP_NAME_BYTES,
                        probe: Arc::new(gateway::conformance::conformance_group_name_rejected),
                    },
                ])),
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
                        name: "gateway bearer admission over a failing session verifier",
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

fn mail() -> Entry {
    Entry {
        module: "mail",
        stances: vec![
            (
                Convention::EnvValidation,
                // One case per MAIL_* variable, each set ALONE by the harness. Blank rather
                // than empty because std::env::set_var("") REMOVES the variable on Windows,
                // where the case would then assert the unset path — which is the compiled
                // default, not a failure.
                //
                // The five MAIL_SMTP_* cases carry VALID values on purpose: with one
                // variable set, MAIL_PROVIDER is never `smtp`, so what they execute is the
                // group refusal — a relay setting an operator believes is configured must
                // not boot into a channel that ignores it. Each names its own variable, so a
                // key dropped from config::SMTP_VARS turns only its own case red.
                Stance::Applies(Fixture::EnvValidation(vec![
                    EnvCase {
                        var: "MAIL_PROVIDER",
                        bad_value: "postbox",
                        expect: "invalid MAIL_PROVIDER: unknown provider",
                    },
                    // The smtp arm's own entry branch, reachable with this one variable
                    // because config::smtp_settings runs before the MAIL_FROM cross-check.
                    EnvCase {
                        var: "MAIL_PROVIDER",
                        bad_value: "smtp",
                        expect: "MAIL_PROVIDER=smtp but MAIL_SMTP_HOST is not set",
                    },
                    EnvCase {
                        var: "MAIL_FROM",
                        bad_value: "ops@",
                        expect: "invalid MAIL_FROM: is not a routable address",
                    },
                    // The cross-field rule, which no per-field parse can reach: a valid
                    // address with no provider is a channel that enqueues and never drains.
                    EnvCase {
                        var: "MAIL_FROM",
                        bad_value: "ops@example.com",
                        expect: "MAIL_FROM is set but MAIL_PROVIDER is not",
                    },
                    EnvCase {
                        var: "MAIL_SEND_TIMEOUT_MS",
                        bad_value: "   ",
                        expect: "invalid MAIL_SEND_TIMEOUT_MS: must be a whole number",
                    },
                    // The ceiling, derived from the drain's backoff cap: a send budget
                    // allowed to outlast the longest gap the retry ladder waits inverts the
                    // ladder, and the same value becomes a statement_timeout. The value is
                    // far above any plausible budget, so the case pins the bound's EXISTENCE
                    // rather than its current number.
                    EnvCase {
                        var: "MAIL_SEND_TIMEOUT_MS",
                        bad_value: "99999999999",
                        expect: "invalid MAIL_SEND_TIMEOUT_MS: must be between 1 and",
                    },
                    EnvCase {
                        var: "MAIL_MAX_ATTEMPTS",
                        bad_value: "   ",
                        expect: "invalid MAIL_MAX_ATTEMPTS: must be a whole number",
                    },
                    EnvCase {
                        var: "MAIL_MAX_ATTEMPTS",
                        bad_value: "0",
                        expect: "invalid MAIL_MAX_ATTEMPTS: must be between 1 and",
                    },
                    EnvCase {
                        var: "MAIL_RETENTION_DAYS",
                        bad_value: "   ",
                        expect: "invalid MAIL_RETENTION_DAYS: must be a whole number",
                    },
                    EnvCase {
                        var: "MAIL_RETENTION_DAYS",
                        bad_value: "99999999",
                        expect: "invalid MAIL_RETENTION_DAYS: must be between 1 and",
                    },
                    EnvCase {
                        var: "MAIL_SMTP_HOST",
                        bad_value: "relay.example.com",
                        expect: "MAIL_SMTP_HOST is set but MAIL_PROVIDER is not smtp",
                    },
                    EnvCase {
                        var: "MAIL_SMTP_PORT",
                        bad_value: "587",
                        expect: "MAIL_SMTP_PORT is set but MAIL_PROVIDER is not smtp",
                    },
                    EnvCase {
                        var: "MAIL_SMTP_USERNAME",
                        bad_value: "mailer",
                        expect: "MAIL_SMTP_USERNAME is set but MAIL_PROVIDER is not smtp",
                    },
                    EnvCase {
                        var: "MAIL_SMTP_PASSWORD",
                        bad_value: "unused",
                        expect: "MAIL_SMTP_PASSWORD is set but MAIL_PROVIDER is not smtp",
                    },
                    EnvCase {
                        var: "MAIL_SMTP_TLS",
                        bad_value: "starttls",
                        expect: "MAIL_SMTP_TLS is set but MAIL_PROVIDER is not smtp",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                // The first four drive service::validate_new, the one input policy the
                // enqueue authority runs for BOTH callers (the durable ingress and the
                // operator form) before any statement carries the value. The last two drive
                // admin::apply_submit itself — mail's contribution to the shared
                // `admin.adminSubmit params.<value>` verdict — against a store that cannot
                // connect, so an admitted length answers Internal and only the guard's own
                // verdict counts as a rejection.
                //
                // The recipient case is the one that needed a non-ASCII fixture: lettre
                // applies its length limits to the PUNYCODE form while the cap counts raw
                // UTF-8, so an ASCII probe would be decided by the parser (319 bytes is the
                // longest ASCII address it accepts) rather than by the cap.
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "mail request idempotency key",
                        cap: mailevents::MAX_IDEMPOTENCY_KEY_BYTES,
                        probe: Arc::new(mail::conformance::conformance_idempotency_key_rejected),
                    },
                    CapCase {
                        name: "mail request recipient",
                        cap: mailevents::MAX_ADDRESS_BYTES,
                        probe: Arc::new(mail::conformance::conformance_recipient_rejected),
                    },
                    CapCase {
                        name: "mail request subject",
                        cap: mailevents::MAX_SUBJECT_BYTES,
                        probe: Arc::new(mail::conformance::conformance_subject_rejected),
                    },
                    CapCase {
                        name: "mail request body",
                        cap: mailevents::MAX_BODY_BYTES,
                        probe: Arc::new(mail::conformance::conformance_body_rejected),
                    },
                    CapCase {
                        name: "mail request kind",
                        cap: mailevents::MAX_KIND_BYTES,
                        probe: Arc::new(mail::conformance::conformance_kind_rejected),
                    },
                    CapCase {
                        name: "mail admin send-test key",
                        cap: mail::conformance::TEST_KEY_BYTES,
                        probe: Arc::new(mail::conformance::conformance_test_key_rejected),
                    },
                    CapCase {
                        name: "mail admin selected message id",
                        cap: mail::conformance::OUTBOX_ID_SHAPE_BYTES,
                        probe: Arc::new(mail::conformance::conformance_outbox_id_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("mail publishes no synchronous op — no #[http] route, no capability trait — so it has no request whose answer could be a 503: its ingress is a durable subscription, where infrastructure trouble propagates and the plane retries, and a relay it cannot reach is a queued retry ending in a parked row an operator requeues"),
            ),
            (
                Convention::ArgonParity,
                na("mail performs no password hashing: MAIL_SMTP_PASSWORD is a credential it PRESENTS to a relay, held in memory and sent, never a stored verifier"),
            ),
        ],
    }
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

fn notifications() -> Entry {
    Entry {
        module: "notifications",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::Applies(Fixture::EnvValidation(vec![
                    // Only an UNSET variable takes the compiled default
                    // (notifications::projection::retention_days_from_env, the
                    // ASYNCEVENTS_HANDLER_TIMEOUT convention). A blank value is the
                    // divergence worth executing: `NOTIFICATIONS_RETENTION_DAYS=${RETENTION_DAYS}`
                    // with the outer variable unset expands to one, and audit's env_int would
                    // silently take its default here. Blank rather than empty because
                    // std::env::set_var("") REMOVES the variable on Windows, where the case
                    // would then assert nothing.
                    EnvCase {
                        var: "NOTIFICATIONS_RETENTION_DAYS",
                        bad_value: "   ",
                        expect: "NOTIFICATIONS_RETENTION_DAYS must be a whole number of days",
                    },
                    EnvCase {
                        var: "NOTIFICATIONS_RETENTION_DAYS",
                        bad_value: "0",
                        expect: "NOTIFICATIONS_RETENTION_DAYS must be between 1 and",
                    },
                    // The ceiling, which audit's twin does not have: a retention beyond the
                    // range make_interval can subtract from now() passes startup and then
                    // raises 22008 on every delivery, pausing the subscription. The value is
                    // far above any plausible ceiling so the case pins the bound's EXISTENCE
                    // rather than its current number.
                    EnvCase {
                        var: "NOTIFICATIONS_RETENTION_DAYS",
                        bad_value: "99999999",
                        expect: "NOTIFICATIONS_RETENTION_DAYS must be between 1 and",
                    },
                ])),
            ),
            (
                Convention::InputByteCaps,
                Stance::Applies(Fixture::InputByteCaps(vec![
                    CapCase {
                        name: "notifications message title",
                        cap: notificationsapi::MAX_TITLE_BYTES,
                        probe: Arc::new(notifications::conformance::conformance_title_rejected),
                    },
                    CapCase {
                        name: "notifications message body",
                        cap: notificationsapi::MAX_BODY_BYTES,
                        probe: Arc::new(notifications::conformance::conformance_body_rejected),
                    },
                    CapCase {
                        name: "notifications message kind",
                        cap: notificationsapi::MAX_KIND_BYTES,
                        probe: Arc::new(notifications::conformance::conformance_kind_rejected),
                    },
                    CapCase {
                        name: "notifications dedup key",
                        cap: notifications::conformance::MAX_DEDUP_KEY_BYTES,
                        probe: Arc::new(notifications::conformance::conformance_dedup_key_rejected),
                    },
                    CapCase {
                        name: "notifications list cursor",
                        cap: notificationsapi::MAX_CURSOR_BYTES,
                        probe: Arc::new(notifications::conformance::conformance_cursor_rejected),
                    },
                ])),
            ),
            (
                Convention::InfraOutage503,
                na("notifications has no external verifier to classify an outage of: its only dependency is the shared Postgres, where a failed read or write is a 500"),
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

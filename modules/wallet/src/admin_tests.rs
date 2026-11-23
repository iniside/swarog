//! The wallet control-plane provider: the fact, the tiles and the Transactions table (pure),
//! the cursor and the number format, the headline-currency config, the grant and revoke forms
//! and their field checks, and the panels and the moves against the live database.

use std::collections::BTreeMap;

use controlplane::{
    ActionPoint, Align, CellKind, EntityRef, FieldKind, FilterKind, FormSpec, InvokeCall, InvokeOutcome, Link, PanelBody,
    PanelQuery, Point, PrepareCall, Primitive, Provider as _, Role, Then, Tone,
};
use lifecycle::{Context, Module as _};
use opsapi::{Error, Status};
use testdb::test_pool;
use walletapi::{Balance, Currency, MAX_CURRENCY_CODE_BYTES, MAX_MOVEMENT_AMOUNT, MAX_REASON_BYTES};

use super::{
    balance_cells, create_form, currencies_body, fact_text, group_digits, grouped, headline_label, ledger_body, manifest,
    movement_field_error, panel_form, parse_amount, parse_cursor, parse_reason, purges_body, reason_filter, row_form, signed,
    spent_body, tiles_body, transactions_body, parse_new_currency, Direction, NewCurrency, WalletAdmin, ADMIN_KEY_PREFIX,
    CODE_TAKEN, NO_SUCH_PLAYER, REASON_ADMIN,
};
use crate::service::{
    IDEMPOTENCY_CONFLICT, MALFORMED_PLAYER_ID, MAX_CURRENCY_DISPLAY_NAME_BYTES, MAX_CURRENCY_KIND_BYTES, OUT_OF_RANGE,
    PLAYER_DELETED, UNKNOWN_CURRENCY,
};
use crate::store::{CurrencyStats, LedgerEntry, LedgerPage, LEDGER_PAGE_SIZE};
use crate::tests::{cleanup, unique_currency, unique_player, wired, with_directory, DB_LOCK};
use crate::WalletModule;

fn balance(currency: &str, amount: i64) -> Balance {
    Balance { currency: currency.to_string(), amount }
}

fn currency(code: &str, display_name: &str) -> Currency {
    Currency { code: code.to_string(), display_name: display_name.to_string(), kind: "soft".to_string(), decimals: 0 }
}

fn entry(seq: i64, delta: i64, balance_after: i64) -> LedgerEntry {
    LedgerEntry {
        seq,
        id: format!("00000000-0000-4000-8000-{seq:012}"),
        player_id: "11111111-1111-4111-8111-111111111111".to_string(),
        at: "2026-11-21 16:45".to_string(),
        currency: "gems".to_string(),
        delta,
        balance_after,
        reason: "admin.grant".to_string(),
    }
}

fn table(body: &PanelBody) -> &controlplane::DataTable {
    match &body.primitive {
        Primitive::DataTable(t) => t,
        _ => panic!("the Transactions panel answers a DataTable"),
    }
}

fn tiles(body: &PanelBody) -> Vec<(String, String)> {
    match &body.primitive {
        Primitive::KpiTiles(t) => t.iter().map(|k| (k.label.clone(), k.value.clone())).collect(),
        _ => panic!("the Wallet tiles answer KpiTiles"),
    }
}

fn query(contribution: &str, player: &str, cursor: Option<&str>) -> PanelQuery {
    PanelQuery {
        module: "wallet".to_string(),
        contribution: contribution.to_string(),
        entity: Some(EntityRef { kind: "player".to_string(), id: player.to_string() }),
        search: None,
        cursor: cursor.map(str::to_string),
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs: Vec::new(),
    }
}

#[test]
fn numbers_are_exact_grouped_integers_with_a_plus_on_a_credit() {
    assert_eq!(grouped(0), "0");
    assert_eq!(grouped(999), "999");
    assert_eq!(grouped(91_300), "91,300");
    assert_eq!(grouped(1_000_000_000_000_000), "1,000,000,000,000,000");
    assert_eq!(grouped(-1_200), "-1,200");
    assert_eq!(signed(500), "+500");
    assert_eq!(signed(-1_200), "-1,200");
    assert_eq!(signed(0), "0");
    assert_eq!(group_digits("0"), "0");
    assert_eq!(group_digits("12345678901234567890123"), "12,345,678,901,234,567,890,123", "past i64");
}

fn stats(code: &str, kind: &str, supply: &str, holders: i64) -> CurrencyStats {
    CurrencyStats {
        code: code.to_string(),
        display_name: code.to_uppercase(),
        kind: kind.to_string(),
        supply: supply.to_string(),
        holders,
    }
}

#[test]
fn currencies_draw_code_name_kind_supply_holders_and_a_dash_cap_with_hard_accented() {
    let body = currencies_body(&[stats("gems", "hard", "41800000", 182_000), stats("gold", "soft", "0", 0)]);
    let t = table(&body);
    let cols: Vec<(&str, CellKind, Align)> = t.columns.iter().map(|c| (c.label.as_str(), c.kind, c.align)).collect();
    assert_eq!(
        cols,
        [
            ("Code", CellKind::Ref, Align::Left),
            ("Name", CellKind::Text, Align::Left),
            ("Kind", CellKind::Label, Align::Left),
            ("Supply", CellKind::Number, Align::Right),
            ("Holders", CellKind::Number, Align::Right),
            ("Cap", CellKind::Dim, Align::Left),
        ]
    );
    let drawn: Vec<Vec<(&str, Option<Tone>)>> =
        t.rows.iter().map(|r| r.cells.iter().map(|c| (c.value.as_str(), c.tone)).collect()).collect();
    assert_eq!(
        drawn,
        [
            [
                ("gems", None),
                ("GEMS", None),
                ("hard", Some(Tone::Accent)),
                ("41,800,000", None),
                ("182,000", None),
                ("—", None),
            ],
            [("gold", None), ("GOLD", None), ("soft", None), ("0", None), ("0", None), ("—", None)],
        ]
    );
    assert!(t.page.next_cursor.is_none() && t.page.total.is_none(), "the catalog has no pager");
    assert!(t.filters.is_empty() && t.search_placeholder.is_none());
    assert!(table(&currencies_body(&[])).rows.is_empty(), "an empty catalog is the portal's empty state");
}

#[test]
fn fact_names_the_headline_currency_balance_and_zero_when_none_is_held() {
    let held = [balance("gold", 7), balance("gems", 12_500)];
    assert_eq!(fact_text("gems", &held), "12,500 gems");
    assert_eq!(fact_text("gold", &held), "7 gold", "the headline is the configured code, not gems");
    assert_eq!(fact_text("embers", &held), "0 embers");
}

#[test]
fn tiles_are_one_per_catalog_currency_in_catalog_order() {
    let body = tiles_body(&[currency("gems", "Gems"), currency("gold", "Gold")], &[balance("gold", 3_120)]);
    assert_eq!(
        tiles(&body),
        [("Gems (gems)".to_string(), "0".to_string()), ("Gold (gold)".to_string(), "3,120".to_string())]
    );
    match &body.primitive {
        Primitive::KpiTiles(t) => assert!(t.iter().all(|k| k.delta.is_none() && k.series.is_empty() && k.link.is_none())),
        _ => unreachable!(),
    }
    assert!(tiles(&tiles_body(&[], &[])).is_empty(), "an empty catalog draws no tile");
}

#[test]
fn transactions_map_each_movement_to_the_six_cells_and_the_cursor_to_the_last_seq() {
    let page = LedgerPage { rows: vec![entry(9, -1_200, 91_300), entry(4, 500, 92_500)], next_after: Some(4) };
    let body = transactions_body(&page);
    let t = table(&body);
    let heads: Vec<(&str, CellKind, bool, bool)> =
        t.columns.iter().map(|c| (c.label.as_str(), c.kind, matches!(c.align, Align::Right), c.wrap)).collect();
    assert_eq!(
        heads,
        [
            ("Time", CellKind::Mono, false, false),
            ("Currency", CellKind::Ref, false, false),
            ("Amount", CellKind::Mono, true, false),
            ("Balance", CellKind::Mono, true, false),
            ("Reason", CellKind::Dim, false, true),
            ("Tx", CellKind::MonoMuted, false, false),
        ]
    );
    let values: Vec<Vec<&str>> = t.rows.iter().map(|r| r.cells.iter().map(|c| c.value.as_str()).collect()).collect();
    assert_eq!(
        values[0],
        ["2026-11-21 16:45", "gems", "-1,200", "91,300", "admin.grant", "00000000-0000-4000-8000-000000000009"]
    );
    assert_eq!(values[1][2], "+500");
    assert_eq!(t.rows[0].cells[2].tone, Some(Tone::Err), "a debit is red");
    assert_eq!(t.rows[1].cells[2].tone, Some(Tone::Ok), "a credit is green");
    assert_eq!(t.page.next_cursor.as_deref(), Some("4"));
    assert_eq!(t.page.size, LEDGER_PAGE_SIZE as u32);
    assert!(t.page.total.is_none() && t.filters.is_empty() && t.search_placeholder.is_none());

    let last = transactions_body(&LedgerPage { rows: Vec::new(), next_after: None });
    assert!(table(&last).page.next_cursor.is_none() && table(&last).rows.is_empty());
}

#[test]
fn ledger_adds_a_linked_player_cell_the_selects_the_search_and_the_counted_foot() {
    let page = LedgerPage { rows: vec![entry(9, -1_200, 91_300), entry(4, 500, 92_500)], next_after: Some(4) };
    let codes = ["gems".to_string(), "gold".to_string()];
    let body = ledger_body(&page, 2_400_000, &codes);
    let t = table(&body);
    let heads: Vec<(&str, CellKind)> = t.columns.iter().map(|c| (c.label.as_str(), c.kind)).collect();
    assert_eq!(
        heads,
        [
            ("Time", CellKind::Mono),
            ("Player", CellKind::User),
            ("Currency", CellKind::Ref),
            ("Amount", CellKind::Mono),
            ("Balance", CellKind::Mono),
            ("Reason", CellKind::Dim),
            ("Tx", CellKind::MonoMuted),
        ]
    );
    assert!(t.columns[3].align == Align::Right && t.columns[4].align == Align::Right && t.columns[5].wrap);
    let player = &t.rows[0].cells[1];
    let pid = "11111111-1111-4111-8111-111111111111";
    assert_eq!(player.value, pid, "the value is the id, so the portal draws accounts' label for it");
    assert_eq!(
        player.link,
        Some(Link::Entity(EntityRef { kind: "player".to_string(), id: pid.to_string() }))
    );
    assert!(player.label.is_none(), "a cell holds a link or a label, never both");
    let values: Vec<&str> = t.rows[0].cells.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(
        values,
        ["2026-11-21 16:45", pid, "gems", "-1,200", "91,300", "admin.grant", "00000000-0000-4000-8000-000000000009"]
    );
    assert_eq!((t.rows[0].cells[3].tone, t.rows[1].cells[3].tone), (Some(Tone::Err), Some(Tone::Ok)));
    assert_eq!(t.rows[1].cells[3].value, "+500");
    let filters: Vec<(&str, &str, &FilterKind)> =
        t.filters.iter().map(|f| (f.key.as_str(), f.label.as_str(), &f.kind)).collect();
    let reasons = FilterKind::Select(vec![crate::projection::REASON.to_string(), REASON_ADMIN.to_string()]);
    assert_eq!(
        filters,
        [("currency", "Currency", &FilterKind::Select(codes.to_vec())), ("reason", "Reason", &reasons)]
    );
    assert_eq!(t.search_placeholder.as_deref(), Some("Player or tx id"));
    assert_eq!(t.page.next_cursor.as_deref(), Some("4"));
    assert_eq!((t.page.size, t.page.total), (LEDGER_PAGE_SIZE as u32, Some(2_400_000)));
    let noun = t.item_noun.as_ref().expect("the foot counts transactions");
    assert_eq!((noun.of(1), noun.of(2)), ("transaction", "transactions"));
}

#[test]
fn the_reason_select_matches_a_system_reason_or_the_admin_key_prefix() {
    assert_eq!(reason_filter(None).unwrap(), (None, None));
    assert_eq!(reason_filter(Some("starter-grant")).unwrap(), (Some("starter-grant"), None));
    assert_eq!(reason_filter(Some(REASON_ADMIN)).unwrap(), (None, Some(ADMIN_KEY_PREFIX)));
    for bad in ["admin.grant", "Admin", "starter"] {
        assert_eq!(reason_filter(Some(bad)).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

#[test]
fn cursor_is_a_decimal_seq_and_anything_else_is_the_callers_error() {
    assert_eq!(parse_cursor("42").unwrap(), 42);
    for bad in ["", "-1", "4x", "1.5", "99999999999999999999"] {
        assert_eq!(parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

#[test]
fn manifest_declares_the_tab_at_30_its_tiles_the_fact_at_50_and_the_economy_panels_at_10_20_and_30() {
    let m = manifest("gems");
    assert!(m.writes, "grant and revoke write");
    assert!(m.entity_kinds.is_empty());
    let decls: Vec<(&str, i32, &str, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str(), c.icon.as_str())).collect();
    assert_eq!(
        decls,
        [
            ("wallet", 30, "Transactions", ""),
            ("wallet.tiles", 0, "Wallet", ""),
            ("fact.balance", 50, "Wallet", "coins"),
            ("currencies", 10, "Currencies", ""),
            ("grant", 20, "Grant or revoke", ""),
            ("ledger", 30, "Ledger", ""),
            ("column.balance", 30, "Gems", ""),
            ("tile.spent", 70, "Gems spent (24 h)", ""),
            ("attention.purges", 62, "Wallet purges pending", ""),
        ]
    );
    for c in &m.contributions[3..6] {
        assert!(matches!(&c.point, Point::Page(page) if page == "economy"), "{}", c.id);
    }
    for c in &m.contributions {
        let want = (c.id == "tile.spent").then_some(60);
        assert_eq!(c.cache_ttl_s, want, "{}: only the 24 h spent tile may be stale; no copy else outlives a grant", c.id);
    }
    assert!(m.sections.is_empty(), "the Economy section is controlplane's fixed one");
    assert!(controlplane::SECTIONS.contains(&("economy", "Economy", 30)));
    let nav: Vec<(&str, &str, &str, &str, i32, &str, bool)> = m
        .entries
        .iter()
        .map(|e| {
            let counted = e.counter.is_some();
            (e.section.as_str(), e.id.as_str(), e.label.as_str(), e.icon.as_str(), e.order, e.page.as_str(), counted)
        })
        .collect();
    assert_eq!(nav, [("economy", "economy", "Currencies & ledger", "coins", 10, "economy", false)]);
    assert_eq!(m.pages.len(), 1);
    let page = &m.pages[0];
    assert_eq!(
        (page.id.as_str(), page.title.as_str(), page.subtitle.as_str()),
        ("economy", "Currencies & ledger", "wallet module")
    );
    assert_eq!(
        page.layout,
        controlplane::Layout::Rows(vec![
            controlplane::Layout::Grid21(vec!["currencies".to_string()], vec!["grant".to_string()]),
            controlplane::Layout::Stack(vec!["ledger".to_string()]),
        ])
    );
    assert_eq!(page.params, ["currency", "reason"], "the Ledger's selects reach it as params");
    assert!(matches!(
        &m.contributions[0].point,
        Point::EntityTab { kind, tab, tab_label, columns: 1, params }
            if kind == "player" && tab == "wallet" && tab_label == "Wallet" && params.is_empty()
    ));
    assert!(matches!(
        &m.contributions[1].point,
        Point::EntityTabTile { kind, tab } if kind == "player" && tab == "wallet"
    ));
    assert!(matches!(&m.contributions[2].point, Point::EntitySummary(kind) if kind == "player"));
    assert!(matches!(
        &m.contributions[6].point,
        Point::EntityListColumn { kind, label, align: Align::Right } if kind == "player" && label == "Gems"
    ));
    assert_eq!(m.contributions[7].point, Point::DashboardTile);
    assert_eq!(m.contributions[8].point, Point::DashboardAttention);
    let other = manifest("gold");
    assert_eq!((other.contributions[6].title.as_str(), other.contributions[7].title.as_str()), ("Gold", "Gold spent (24 h)"));

    let row = ActionPoint::RowAction("player".to_string());
    let form = ActionPoint::PanelForm("grant".to_string());
    let tool = ActionPoint::PanelTool("currencies".to_string());
    let actions: Vec<(&str, &ActionPoint, &str, &str, bool)> =
        m.actions.iter().map(|a| (a.id.as_str(), &a.point, a.label.as_str(), a.icon.as_str(), a.prepare)).collect();
    assert_eq!(
        actions,
        [
            ("currency.grant", &row, "Grant currency…", "gift", true),
            ("currency.revoke", &row, "Revoke currency…", "coins", true),
            ("ledger.grant", &form, "Grant", "", false),
            ("ledger.revoke", &form, "Revoke", "", false),
            ("currency.create", &tool, "New currency", "plus", false),
        ]
    );
    for a in &m.actions {
        assert!(a.implemented && !a.danger && a.min_role == Role::Operator, "{}", a.id);
        assert_eq!(a.emphasis, controlplane::Emphasis::Default, "{}", a.id);
        assert_eq!(a.when_empty, a.id == "currency.create", "only the create tool also draws on an empty catalog: {}", a.id);
    }
    assert!(m.actions[..4].iter().all(|a| a.order == 40), "the row menu's wallet group");
    assert_eq!(m.actions[4].order, 10);
    assert_eq!(m.actions[4].then, Then::Form(create_form()));
    assert!(controlplane::form_spec_errors(&create_form()).is_empty());
    for (i, dir) in [Direction::Grant, Direction::Revoke].into_iter().enumerate() {
        assert_eq!(m.actions[i].then, Then::Form(row_form(dir, &[])), "the row pair declares its modal without modes");
    }
    assert!(m.actions[2..4].iter().all(|a| a.then == Then::Invoke));
    let spec = panel_form(&[]);
    assert!(controlplane::form_spec_errors(&spec).is_empty());
    assert!(controlplane::form_mode_errors(&m, &form, &spec).is_empty(), "the panel's modes are its PanelForm actions");
    assert!(!m.is_mode_action("currency.grant") && !m.is_mode_action("currency.revoke"));
}

fn form_of(then: Then) -> FormSpec {
    match then {
        Then::Form(spec) => spec,
        other => panic!("expected a form, got {other:?}"),
    }
}

/// (key, label, kind, required, mono, has help) of each field.
fn shape(spec: &FormSpec) -> Vec<(&str, &str, FieldKind, bool, bool, bool)> {
    spec.fields
        .iter()
        .map(|f| (f.key.as_str(), f.label.as_str(), f.kind.clone(), f.required, f.mono, f.help.is_some()))
        .collect()
}

#[test]
fn row_forms_ask_amount_reason_and_request_id_with_no_modes() {
    let codes = ["gems".to_string(), "gold".to_string()];
    for (dir, title, submit) in
        [(Direction::Grant, "Grant currency", "Grant"), (Direction::Revoke, "Revoke currency", "Revoke")]
    {
        let spec = row_form(dir, &codes);
        assert_eq!(
            shape(&spec),
            [
                ("amount", "Amount", FieldKind::Money(codes.to_vec()), true, false, false),
                ("reason", "Reason", FieldKind::String, true, false, true),
                ("request_id", "Request id", FieldKind::RequestId, false, true, false),
            ]
        );
        assert_eq!(spec.fields[1].help.as_deref(), Some("Shown in the ledger and audit log."));
        assert!(spec.fields.iter().all(|f| f.default.is_none()), "the portal fills the request id");
        assert_eq!((spec.title.as_deref(), spec.submit_label.as_str()), (Some(title), submit));
        assert_eq!(spec.note.as_deref(), Some("Idempotent per request id."));
        assert!(spec.modes.is_empty());
        assert!(controlplane::form_spec_errors(&spec).is_empty());
    }
}

#[test]
fn the_panel_form_asks_the_player_first_and_switches_grant_and_revoke() {
    let codes = ["gems".to_string()];
    let spec = panel_form(&codes);
    let fields = shape(&spec);
    assert_eq!(fields[0], ("player", "Player", FieldKind::Ref("player".to_string()), true, false, false));
    assert_eq!(fields[1..], shape(&row_form(Direction::Grant, &codes))[..]);
    assert!(spec.title.is_none(), "a panel form has no title");
    assert_eq!(spec.note.as_deref(), Some("Idempotent per request id."));
    let modes: Vec<(&str, &str, &str, &str)> = spec
        .modes
        .iter()
        .map(|m| (m.id.as_str(), m.label.as_str(), m.action.as_str(), m.submit_label.as_str()))
        .collect();
    assert_eq!(
        modes,
        [("grant", "Grant", "ledger.grant", "Grant"), ("revoke", "Revoke", "ledger.revoke", "Revoke")]
    );
}

#[test]
fn the_amount_is_a_whole_positive_number_within_the_movement_cap() {
    let money =
        |amount: &str, currency: &str| serde_json::json!({ "amount": amount, "currency": currency }).to_string();
    assert_eq!(parse_amount(&money("500", "gems")), Ok((500, "gems".to_string())));
    assert_eq!(parse_amount(&money(&MAX_MOVEMENT_AMOUNT.to_string(), "gems")).unwrap().0, MAX_MOVEMENT_AMOUNT);
    for bad in ["0", "-5", "1.5", "", "1e3", &(MAX_MOVEMENT_AMOUNT + 1).to_string(), "99999999999999999999"] {
        let err = parse_amount(&money(bad, "gems")).unwrap_err();
        assert!(err.starts_with("Amount must be a whole number"), "{bad:?}: {err}");
    }
    assert_eq!(parse_amount("").unwrap_err(), "Amount is required");
    assert!(parse_amount("500").is_err() && parse_amount(r#"{"amount":"5"}"#).is_err());
    assert!(parse_amount(r#"{"amount":"5","currency":"gems","x":1}"#).is_err(), "unknown keys are refused");
    assert_eq!(parse_amount(&money("5", "")).unwrap_err(), "Amount must name a currency");
    let long = "c".repeat(walletapi::MAX_CURRENCY_CODE_BYTES + 1);
    assert_eq!(parse_amount(&money("5", &long)).unwrap_err(), "Currency is not in the catalog");
}

#[test]
fn the_reason_is_the_trimmed_text_within_the_reason_cap() {
    assert_eq!(parse_reason("  Compensation for outage  "), Ok("Compensation for outage".to_string()));
    assert_eq!(parse_reason(" \t ").unwrap_err(), "Reason is required");
    assert!(parse_reason(&"r".repeat(MAX_REASON_BYTES)).is_ok());
    assert!(parse_reason(&"r".repeat(MAX_REASON_BYTES + 1)).is_err());
}

#[test]
fn service_refusals_map_to_their_field_and_the_rest_pass_through() {
    let amount = |e: Error| movement_field_error(&e).map(|(k, _)| k);
    assert_eq!(amount(Error::conflict(OUT_OF_RANGE)), Some("amount"));
    assert_eq!(amount(Error::invalid(UNKNOWN_CURRENCY)), Some("amount"));
    assert_eq!(amount(Error::invalid(MALFORMED_PLAYER_ID)), Some("player"));
    assert_eq!(amount(Error::conflict(IDEMPOTENCY_CONFLICT)), None, "the portal redraws the form locked");
    assert_eq!(amount(Error::conflict(PLAYER_DELETED)), Some("player"), "not the changed-since banner");
    assert_eq!(amount(Error::invalid(OUT_OF_RANGE)), None, "the status is part of the match");
}

#[test]
fn an_empty_or_over_cap_headline_currency_fails_register() {
    let ctx = Context::new();
    for bad in [String::new(), "x".repeat(walletapi::MAX_CURRENCY_CODE_BYTES + 1)] {
        let err = WalletModule::new().with_headline_currency(bad.clone()).register(&ctx).unwrap_err();
        assert!(err.to_string().contains("headline currency"), "{bad:?}: {err}");
    }
}

/// The panels against the live database: the fact headlines the configured currency, the
/// tiles carry the player's balance, and the Transactions pager walks the keyset.
#[tokio::test]
async fn panels_read_the_players_balances_and_ledger() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    sqlx::query("INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid, $2, 12500)")
        .bind(&pid)
        .bind(&code)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO wallet.ledger (idempotency_key, player_id, currency, delta, balance_after, reason) \
         SELECT 'admin-tests-' || $1 || '-' || g::text, $1::uuid, $2, 500, 500 * g, 'admin.grant' \
           FROM generate_series(1, $3) AS g",
    )
    .bind(&pid)
    .bind(&code)
    .bind(LEDGER_PAGE_SIZE + 1)
    .execute(&pool)
    .await
    .unwrap();
    let admin = WalletAdmin::new(svc, code.clone());

    let fact = admin.panel(query("fact.balance", &pid, None)).await.unwrap();
    match fact.primitive {
        Primitive::Cell(c) => assert_eq!(c.value, format!("12,500 {code}")),
        _ => panic!("the fact answers a Cell"),
    }

    let drawn = tiles(&admin.panel(query("wallet.tiles", &pid, None)).await.unwrap());
    assert!(drawn.contains(&(format!("{code} ({code})"), "12,500".to_string())), "{drawn:?}");

    let first = admin.panel(query("wallet", &pid, None)).await.unwrap();
    let t = table(&first);
    assert_eq!(t.rows.len(), LEDGER_PAGE_SIZE as usize);
    let cursor = t.page.next_cursor.clone().expect("one row is left");
    let second = admin.panel(query("wallet", &pid, Some(&cursor))).await.unwrap();
    let rest = table(&second);
    assert_eq!(rest.rows.len(), 1);
    assert_eq!(rest.rows[0].cells[3].value, "500", "the last page holds the oldest movement");
    assert!(rest.page.next_cursor.is_none());

    let err = admin.panel(query("wallet", &pid, Some("nope"))).await.unwrap_err();
    assert_eq!(err.status, Status::Invalid);

    cleanup(&pool, &[&pid], &[&code]).await;
}

/// The Ledger reads every player's movements: the search narrows to a player or one tx, the
/// Reason select to a system reason or to the `admin:` key prefix, the Currency select to one
/// code, and the foot counts what they select.
#[tokio::test]
async fn the_ledger_narrows_by_search_reason_and_currency_and_counts_the_selection() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (a, b) = (unique_player(&pool).await, unique_player(&pool).await);
    let (one, two) = (unique_currency(&pool).await, unique_currency(&pool).await);
    // a: two admin grants in `one`, a starter grant in `two`, a key-less-prefix movement whose
    // reason merely reads like an admin one; b: one admin grant.
    let rows = [
        (&a, format!("{ADMIN_KEY_PREFIX}ledger-{a}-1"), &one, "CS ticket 1"),
        (&a, format!("{ADMIN_KEY_PREFIX}ledger-{a}-2"), &one, "CS ticket 2"),
        (&a, format!("starter:{a}"), &two, crate::projection::REASON),
        (&a, format!("other-{a}"), &one, "admin.grant"),
        (&b, format!("{ADMIN_KEY_PREFIX}ledger-{b}-1"), &one, "CS ticket 3"),
    ];
    for (pid, key, code, reason) in &rows {
        sqlx::query(
            "INSERT INTO wallet.ledger (idempotency_key, player_id, currency, delta, balance_after, reason) \
             VALUES ($1, $2::uuid, $3, 10, 10, $4)",
        )
        .bind(key)
        .bind(pid)
        .bind(code)
        .bind(reason)
        .execute(&pool)
        .await
        .unwrap();
    }
    let admin = WalletAdmin::new(svc, one.clone());
    let read = |search: Option<&str>, params: &[(&str, &str)]| {
        let mut q = query("ledger", &a, None);
        q.entity = None;
        q.search = search.map(str::to_string);
        q.params = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        admin.panel(q)
    };
    let reasons =
        |body: &PanelBody| -> Vec<String> { table(body).rows.iter().map(|r| r.cells[5].value.clone()).collect() };

    let all = read(Some(&format!("  {a} ")), &[]).await.unwrap();
    let t = table(&all);
    assert_eq!(
        reasons(&all),
        ["admin.grant", crate::projection::REASON, "CS ticket 2", "CS ticket 1"],
        "newest first"
    );
    assert_eq!(t.page.total, Some(4));
    assert!(t.rows.iter().all(|r| r.cells[1].value == a), "the search narrows to the player");

    let admins = read(Some(&a), &[("reason", REASON_ADMIN)]).await.unwrap();
    assert_eq!(reasons(&admins), ["CS ticket 2", "CS ticket 1"], "admin is the key prefix, not the reason text");
    assert_eq!(table(&admins).page.total, Some(2));

    let starter = read(Some(&a), &[("reason", crate::projection::REASON)]).await.unwrap();
    assert_eq!(reasons(&starter), [crate::projection::REASON]);

    let in_two = read(Some(&a), &[("currency", two.as_str())]).await.unwrap();
    assert_eq!(reasons(&in_two), [crate::projection::REASON]);
    assert_eq!(table(&in_two).page.total, Some(1));

    let tx = t.rows[3].cells[6].value.clone();
    let by_tx = read(Some(&tx), &[]).await.unwrap();
    assert_eq!(reasons(&by_tx), ["CS ticket 1"], "a tx id selects its one movement");
    assert_eq!(table(&by_tx).page.total, Some(1));

    let unknown = read(Some("not-an-id"), &[]).await.unwrap();
    assert!(table(&unknown).rows.is_empty() && table(&unknown).page.total == Some(0), "a malformed id is a miss");

    let everyone = read(None, &[("currency", one.as_str())]).await.unwrap();
    let players: Vec<&str> = table(&everyone).rows.iter().map(|r| r.cells[1].value.as_str()).collect();
    assert_eq!(players, [b.as_str(), a.as_str(), a.as_str(), a.as_str()], "no search reads every player");
    assert_eq!(table(&everyone).page.total, Some(4));

    assert_eq!(read(None, &[("reason", "bogus")]).await.unwrap_err().status, Status::Invalid);

    cleanup(&pool, &[&a, &b], &[&one, &two]).await;
}

#[tokio::test]
async fn currencies_sum_every_balance_and_count_only_positive_holders() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let (a, b, c) = (unique_player(&pool).await, unique_player(&pool).await, unique_player(&pool).await);
    let held = unique_currency(&pool).await;
    let unheld = unique_currency(&pool).await;
    sqlx::query("UPDATE wallet.currencies SET kind = 'hard' WHERE code = $1")
        .bind(&held)
        .execute(&pool)
        .await
        .unwrap();
    // Two balances at the per-row ceiling and one at zero: a zero balance adds no supply and
    // is no holder.
    for (pid, amount) in [(&a, 1_000_000_000_000_000_i64), (&b, 1_000_000_000_000_000), (&c, 0)] {
        sqlx::query("INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid, $2, $3)")
            .bind(pid)
            .bind(&held)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
    }
    let admin = WalletAdmin::new(svc, held.clone());

    let mut q = query("currencies", &a, None);
    q.entity = None;
    let body = admin.panel(q).await.unwrap();
    let rows: Vec<Vec<String>> =
        table(&body).rows.iter().map(|r| r.cells.iter().map(|c| c.value.clone()).collect()).collect();
    let row = |code: &str| rows.iter().find(|r| r[0] == code).cloned().unwrap_or_else(|| panic!("{code}: {rows:?}"));
    assert_eq!(row(&held), [held.as_str(), held.as_str(), "hard", "2,000,000,000,000,000", "2", "—"]);
    assert_eq!(row(&unheld), [unheld.as_str(), unheld.as_str(), "soft", "0", "0", "—"]);
    let mut codes: Vec<&String> = rows.iter().map(|r| &r[0]).collect();
    let drawn = codes.clone();
    codes.sort();
    assert_eq!(drawn, codes, "the catalog in code order");

    cleanup(&pool, &[&a, &b, &c], &[&held, &unheld]).await;
}

fn money(amount: i64, currency: &str) -> String {
    serde_json::json!({ "amount": amount.to_string(), "currency": currency }).to_string()
}

fn call(action: &str, entity: Option<&str>, rid: Option<&str>, input: &[(&str, &str)]) -> InvokeCall {
    InvokeCall {
        module: "wallet".to_string(),
        action: action.to_string(),
        entity: entity.map(|id| EntityRef { kind: "player".to_string(), id: id.to_string() }),
        input: input.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        expected_revision: None,
        reason: None,
        operator: "ops-tester".to_string(),
        item: None,
        request_id: rid.map(str::to_string),
    }
}

fn field_errors(outcome: InvokeOutcome) -> BTreeMap<String, String> {
    match outcome {
        InvokeOutcome::FieldErrors(errors) => errors,
        other => panic!("expected field errors, got {other:?}"),
    }
}

/// The player's ledger as (idempotency key, delta, reason), oldest first.
async fn ledger_of(pool: &sqlx::PgPool, pid: &str) -> Vec<(String, i64, String)> {
    sqlx::query_as("SELECT idempotency_key, delta, reason FROM wallet.ledger WHERE player_id = $1::uuid ORDER BY seq")
        .bind(pid)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A row grant credits under `admin:<request id>` with the operator's text alone as the
/// ledger reason; a repeat of the submit applies nothing, the same request id with a changed
/// reason is the service's 409, and a revoke past the balance is the Amount field's error.
#[tokio::test]
async fn row_grant_and_revoke_move_currency_once_per_request_id() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    let rid = format!("currency.grant:{}", unique_player(&pool).await);
    with_directory(&svc, Some(&[pid.as_str()]));
    let admin = WalletAdmin::new(svc, code.clone());
    let amount = money(1_500, &code);
    let grant = call("currency.grant", Some(&pid), Some(&rid), &[("amount", &amount), ("reason", "  Outage refund  ")]);

    match admin.invoke(grant.clone()).await.unwrap() {
        InvokeOutcome::Ok { message, .. } => assert_eq!(message, format!("Granted 1,500 {code}; balance 1,500")),
        other => panic!("expected Ok, got {other:?}"),
    }
    let key = format!("{ADMIN_KEY_PREFIX}{rid}");
    assert_eq!(ledger_of(&pool, &pid).await, [(key.clone(), 1_500, "Outage refund".to_string())]);

    assert!(matches!(admin.invoke(grant.clone()).await.unwrap(), InvokeOutcome::Ok { .. }));
    assert_eq!(ledger_of(&pool, &pid).await.len(), 1, "a repeat of one submit is not applied twice");

    let changed = call("currency.grant", Some(&pid), Some(&rid), &[("amount", &amount), ("reason", "Other")]);
    let err = admin.invoke(changed).await.unwrap_err();
    assert_eq!((err.status, err.msg.as_str()), (Status::Conflict, IDEMPOTENCY_CONFLICT));

    let rid2 = format!("currency.revoke:{}", unique_player(&pool).await);
    let too_much = money(1_501, &code);
    let revoke = call("currency.revoke", Some(&pid), Some(&rid2), &[("amount", &too_much), ("reason", "Chargeback")]);
    assert!(field_errors(admin.invoke(revoke).await.unwrap()).contains_key("amount"));
    // The refused debit released its key: the same request id with a fundable amount applies.
    let fundable = money(500, &code);
    let revoke = call("currency.revoke", Some(&pid), Some(&rid2), &[("amount", &fundable), ("reason", "Chargeback")]);
    assert!(matches!(admin.invoke(revoke).await.unwrap(), InvokeOutcome::Ok { .. }));
    let rid2_key = format!("{ADMIN_KEY_PREFIX}{rid2}");
    assert_eq!(ledger_of(&pool, &pid).await.iter().filter(|(k, _, _)| *k == rid2_key).count(), 1);
    let rid3 = format!("currency.revoke:{}", unique_player(&pool).await);
    let some = money(500, &code);
    let revoke = call("currency.revoke", Some(&pid), Some(&rid3), &[("amount", &some), ("reason", "Chargeback")]);
    match admin.invoke(revoke).await.unwrap() {
        InvokeOutcome::Ok { message, .. } => assert_eq!(message, format!("Revoked 500 {code}; balance 500")),
        other => panic!("expected Ok, got {other:?}"),
    }
    assert_eq!(ledger_of(&pool, &pid).await[2], (format!("{ADMIN_KEY_PREFIX}{rid3}"), -500, "Chargeback".to_string()));

    cleanup(&pool, &[&pid], &[&code]).await;
}

/// The refusals that come before a write: an unknown currency and a bad amount are Amount
/// errors, a missing request id or reason refuses, an over-cap input value is Invalid, and
/// a row action needs its player entity. None writes a ledger row.
#[tokio::test]
async fn grant_refusals_write_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    with_directory(&svc, Some(&[pid.as_str()]));
    let admin = WalletAdmin::new(svc, code.clone());
    let rid = |tag: &str| format!("currency.grant:{tag}:{pid}");
    let ok = money(10, &code);
    let reason = ("reason", "Gift");

    let unknown = money(10, "no-such-currency");
    let outcome = admin.invoke(call("currency.grant", Some(&pid), Some(&rid("unk")), &[("amount", &unknown), reason]));
    assert_eq!(field_errors(outcome.await.unwrap())["amount"], "Currency is not in the catalog");

    let zero = money(0, &code);
    let outcome = admin.invoke(call("currency.grant", Some(&pid), Some(&rid("zero")), &[("amount", &zero)]));
    let errors = field_errors(outcome.await.unwrap());
    assert!(errors.contains_key("amount") && errors.contains_key("reason"), "{errors:?}");

    let err = admin.invoke(call("currency.grant", Some(&pid), None, &[("amount", &ok), reason])).await.unwrap_err();
    assert_eq!(err.status, Status::Invalid, "a submit without a request id");

    let long = "x".repeat(controlplane::MAX_INPUT_VALUE_BYTES + 1);
    let input = [("amount", ok.as_str()), ("reason", long.as_str())];
    let outcome = admin.invoke(call("currency.grant", Some(&pid), Some(&rid("long")), &input));
    assert_eq!(outcome.await.unwrap_err().status, Status::Invalid, "an input value over the portal's cap");

    let outcome = admin.invoke(call("currency.grant", None, Some(&rid("none")), &[("amount", &ok), reason]));
    let err = outcome.await.unwrap_err();
    assert_eq!(err.status, Status::Invalid, "a row action needs its player");

    let err = admin.invoke(call("currency.burn", Some(&pid), Some(&rid("x")), &[])).await.unwrap_err();
    assert_eq!(err.status, Status::NotFound);

    assert!(ledger_of(&pool, &pid).await.is_empty());
    cleanup(&pool, &[&pid], &[&code]).await;
}

/// The Economy form names its player in the input, its mode picks the direction, and its
/// Money options are the live catalog; the row modal is prepared with the same options.
#[tokio::test]
async fn the_panel_form_takes_its_player_from_the_input_and_lists_the_catalog() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    with_directory(&svc, Some(&[pid.as_str()]));
    let admin = WalletAdmin::new(svc, code.clone());
    let amount = money(40, &code);

    let rid = format!("grant:{pid}");
    let input = [("player", pid.as_str()), ("amount", amount.as_str()), ("reason", "Event prize")];
    let outcome = admin.invoke(call("ledger.grant", None, Some(&rid), &input)).await.unwrap();
    assert!(matches!(outcome, InvokeOutcome::Ok { .. }), "{outcome:?}");
    assert_eq!(ledger_of(&pool, &pid).await, [(format!("{ADMIN_KEY_PREFIX}{rid}"), 40, "Event prize".to_string())]);

    let rid2 = format!("grant:revoke:{pid}");
    let outcome = admin
        .invoke(call("ledger.revoke", None, Some(&rid2), &[("player", &pid), ("amount", &amount), ("reason", "Undo")]))
        .await
        .unwrap();
    assert!(matches!(outcome, InvokeOutcome::Ok { .. }), "{outcome:?}");
    assert_eq!(ledger_of(&pool, &pid).await[1].1, -40);

    let rid3 = format!("grant:bad:{pid}");
    let outcome = admin.invoke(call("ledger.grant", None, Some(&rid3), &[("amount", &amount), ("reason", "x")]));
    assert_eq!(field_errors(outcome.await.unwrap())["player"], "Player is required");
    let input = [("player", "not-a-uuid"), ("amount", amount.as_str()), ("reason", "x")];
    let outcome = admin.invoke(call("ledger.grant", None, Some(&rid3), &input));
    assert_eq!(field_errors(outcome.await.unwrap())["player"], "Player is not a valid player id");

    let mut q = query("grant", &pid, None);
    q.entity = None;
    let panel = admin.panel(q).await.unwrap();
    let Primitive::Form(spec) = panel.primitive else { panic!("the grant panel answers a Form") };
    let FieldKind::Money(options) = &spec.fields[1].kind else { panic!("Amount is Money") };
    assert!(options.contains(&code), "{options:?}");
    assert_eq!(spec.modes.len(), 2);

    let prepare = |action: &str, entity: Option<EntityRef>| PrepareCall {
        module: "wallet".to_string(),
        action: action.to_string(),
        entity,
        item: None,
        operator: "ops-tester".to_string(),
    };
    let player = || Some(EntityRef { kind: "player".to_string(), id: pid.clone() });
    let spec = form_of(admin.prepare(prepare("currency.revoke", player())).await.unwrap());
    assert_eq!(spec.title.as_deref(), Some("Revoke currency"));
    let FieldKind::Money(options) = &spec.fields[0].kind else { panic!("Amount is Money") };
    assert!(options.contains(&code), "{options:?}");
    assert_eq!(admin.prepare(prepare("currency.grant", None)).await.unwrap_err().status, Status::Invalid);
    assert_eq!(admin.prepare(prepare("ledger.grant", player())).await.unwrap_err().status, Status::NotFound);

    cleanup(&pool, &[&pid], &[&code]).await;
}

/// The owner's ruling: a grant or revoke to a uuid no player holds is refused before the
/// write — the Player field's error on the panel form, `NotFound` on a row action — and an
/// accounts outage is `Unavailable`, never "no such player". None writes a ledger row.
#[tokio::test]
async fn a_move_to_a_player_that_does_not_exist_writes_nothing() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let ghost = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    with_directory(&svc, Some(&[]));
    let admin = WalletAdmin::new(svc, code.clone());
    let amount = money(10, &code);

    let input = [("player", ghost.as_str()), ("amount", amount.as_str()), ("reason", "Typo")];
    let outcome = admin.invoke(call("ledger.grant", None, Some(&format!("grant:{ghost}")), &input));
    assert_eq!(field_errors(outcome.await.unwrap())["player"], NO_SUCH_PLAYER);

    let row = call("currency.grant", Some(&ghost), Some(&format!("row:{ghost}")), &[("amount", &amount), ("reason", "Typo")]);
    let err = admin.invoke(row).await.unwrap_err();
    assert_eq!((err.status, err.msg.as_str()), (Status::NotFound, NO_SUCH_PLAYER));

    let (_ctx, down) = wired(&pool).await;
    with_directory(&down, None);
    let down = WalletAdmin::new(down, code.clone());
    let outcome = down.invoke(call("ledger.grant", None, Some(&format!("down:{ghost}")), &input));
    assert_eq!(outcome.await.unwrap_err().status, Status::Unavailable, "an outage is not a missing player");

    assert!(ledger_of(&pool, &ghost).await.is_empty());
    cleanup(&pool, &[&ghost], &[&code]).await;
}

/// A player deleted between the directory check and the write is refused as that player:
/// the Player field's error on the panel form, `NotFound` on a row action — never the 409 the
/// portal draws as "changed since you opened it".
#[tokio::test]
async fn a_move_to_a_deleted_player_is_the_players_refusal() {
    let Some(pool) = test_pool().await else { return };
    let _lock = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    with_directory(&svc, Some(&[pid.as_str()]));
    let admin = WalletAdmin::new(svc, code.clone());
    sqlx::query("INSERT INTO wallet.purged_players (player_id) VALUES ($1::uuid)")
        .bind(&pid)
        .execute(&pool)
        .await
        .unwrap();
    let amount = money(10, &code);

    let input = [("player", pid.as_str()), ("amount", amount.as_str()), ("reason", "Gift")];
    let outcome = admin.invoke(call("ledger.grant", None, Some(&format!("grant:{pid}")), &input));
    assert_eq!(field_errors(outcome.await.unwrap())["player"], "Player has been deleted.");
    let row = call("currency.grant", Some(&pid), Some(&format!("row:{pid}")), &[("amount", &amount), ("reason", "Gift")]);
    let err = admin.invoke(row).await.unwrap_err();
    assert_eq!((err.status, err.msg.as_str()), (Status::NotFound, "Player has been deleted."));

    assert!(ledger_of(&pool, &pid).await.is_empty());
    let _ = sqlx::query("DELETE FROM wallet.purged_players WHERE player_id = $1::uuid").bind(&pid).execute(&pool).await;
    cleanup(&pool, &[&pid], &[&code]).await;
}

/// The panel form trims its Player input: a padded uuid moves the same balance as the bare one.
#[tokio::test]
async fn a_padded_player_uuid_moves_the_same_balance_as_the_bare_one() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let pid = unique_player(&pool).await;
    let code = unique_currency(&pool).await;
    with_directory(&svc, Some(&[pid.as_str()]));
    let admin = WalletAdmin::new(svc.clone(), code.clone());
    let amount = money(25, &code);

    let padded = format!("  {pid}\t");
    for (tag, player) in [("bare", pid.as_str()), ("padded", padded.as_str())] {
        let input = [("player", player), ("amount", amount.as_str()), ("reason", "Gift")];
        let outcome = admin.invoke(call("ledger.grant", None, Some(&format!("{tag}:{pid}")), &input)).await.unwrap();
        assert!(matches!(outcome, InvokeOutcome::Ok { .. }), "{tag}: {outcome:?}");
    }
    let held: Vec<(String, i64)> =
        sqlx::query_as("SELECT player_id::text, amount FROM wallet.balances WHERE currency = $1")
            .bind(&code)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(held, [(pid.clone(), 50)], "both grants land on the one balance");

    cleanup(&pool, &[&pid], &[&code]).await;
}

fn create_input(code: &str, name: &str, kind: &str, decimals: &str) -> BTreeMap<String, String> {
    [("code", code), ("name", name), ("kind", kind), ("decimals", decimals)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The create form trims its text, holds Code, Name and Kind to their byte caps (each at its
/// cap passes, one byte over is that field's error alone), requires every field, and reads
/// Decimals with the Int codec as a whole number from 0 to 18.
#[test]
fn the_create_form_holds_each_field_to_its_bound() {
    let parsed = parse_new_currency(&create_input(" ruby ", " Ruby ", " hard ", " 2 ")).unwrap();
    let want = NewCurrency {
        code: "ruby".to_string(),
        display_name: "Ruby".to_string(),
        kind: "hard".to_string(),
        decimals: 2,
    };
    assert_eq!(parsed, want);
    let caps = [("code", MAX_CURRENCY_CODE_BYTES), ("name", MAX_CURRENCY_DISPLAY_NAME_BYTES), ("kind", MAX_CURRENCY_KIND_BYTES)];
    for (key, cap) in caps {
        let mut input = create_input("ruby", "Ruby", "hard", "0");
        input.insert(key.to_string(), "a".repeat(cap));
        assert!(parse_new_currency(&input).is_ok(), "{key} at its cap");
        input.insert(key.to_string(), "a".repeat(cap + 1));
        let errors = parse_new_currency(&input).unwrap_err();
        assert_eq!(errors.keys().map(String::as_str).collect::<Vec<_>>(), [key], "{key} one byte over its cap");
    }
    for decimals in ["0", "18"] {
        assert!(parse_new_currency(&create_input("ruby", "Ruby", "hard", decimals)).is_ok(), "{decimals}");
    }
    for decimals in ["-1", "19", "1.5", "two", "4294967296"] {
        let errors = parse_new_currency(&create_input("ruby", "Ruby", "hard", decimals)).unwrap_err();
        assert_eq!(errors["decimals"], "Decimals must be a whole number from 0 to 18", "{decimals}");
        assert_eq!(errors.len(), 1, "{decimals}");
    }
    let errors = parse_new_currency(&create_input("  ", "", " ", "")).unwrap_err();
    let required: BTreeMap<String, String> = [
        ("code", "Code is required"),
        ("decimals", "Decimals is required"),
        ("kind", "Kind is required"),
        ("name", "Name is required"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(errors, required);
    assert_eq!(parse_new_currency(&BTreeMap::new()).unwrap_err(), required, "a missing field is an empty one");
}

/// The create form's conformance probes run through `WalletAdmin::invoke`: at each cap the
/// create reaches the (dead) store, one byte over it is the field's own refusal.
#[test]
fn the_create_probes_refuse_one_byte_over_each_cap() {
    let probes = [
        (crate::conformance::conformance_create_code_rejected as fn(usize) -> bool, MAX_CURRENCY_CODE_BYTES),
        (crate::conformance::conformance_create_name_rejected, crate::conformance::CREATE_NAME_BYTES),
        (crate::conformance::conformance_create_kind_rejected, crate::conformance::CREATE_KIND_BYTES),
    ];
    for (probe, cap) in probes {
        assert!(!probe(cap), "at the cap {cap}");
        assert!(probe(cap + 1), "over the cap {cap}");
    }
}

/// The catalog row `code` names, as (display name, kind, decimals).
async fn catalog_row(pool: &sqlx::PgPool, code: &str) -> Option<(String, String, i32)> {
    sqlx::query_as("SELECT display_name, kind, decimals FROM wallet.currencies WHERE code = $1")
        .bind(code)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// A create writes one catalog row with the form's fields. The same code again is the Code
/// field's error and leaves the first row as it was: never an overwrite, never a silent
/// success. A form refused on its fields writes nothing.
#[tokio::test]
async fn create_adds_a_currency_and_refuses_an_existing_code() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let code = format!("n{}", &unique_player(&pool).await[..12]);
    let other = format!("{code}x");
    let admin = WalletAdmin::new(svc, code.clone());
    let create = |code: &str, name: &str, kind: &str, decimals: &str| {
        call("currency.create", None, None, &[("code", code), ("name", name), ("kind", kind), ("decimals", decimals)])
    };

    match admin.invoke(create(&code, "Ruby", "hard", "2")).await.unwrap() {
        InvokeOutcome::Ok { message, .. } => assert_eq!(message, format!("Created currency {code}")),
        other => panic!("expected Ok, got {other:?}"),
    }
    let ruby = Some(("Ruby".to_string(), "hard".to_string(), 2));
    assert_eq!(catalog_row(&pool, &code).await, ruby);

    let again = admin.invoke(create(&code, "Rubies", "soft", "0")).await.unwrap();
    assert_eq!(field_errors(again), BTreeMap::from([("code".to_string(), CODE_TAKEN.to_string())]));
    assert_eq!(catalog_row(&pool, &code).await, ruby, "an existing code is never overwritten");

    let refused = admin.invoke(create(&other, "Other", "soft", "19")).await.unwrap();
    assert!(field_errors(refused).contains_key("decimals"));
    assert_eq!(catalog_row(&pool, &other).await, None, "a refused form writes nothing");

    cleanup(&pool, &[], &[&code, &other]).await;
}

#[test]
fn the_headline_label_upper_cases_the_codes_first_letter() {
    assert_eq!(headline_label("gems"), "Gems");
    assert_eq!(headline_label("Gold"), "Gold");
    assert_eq!(headline_label(""), "");
}

#[test]
fn the_balance_column_draws_every_ref_grouped_and_zero_without_a_row() {
    let refs = ["a".to_string(), "b".to_string()];
    let body = balance_cells(&refs, &BTreeMap::from([("a".to_string(), 91_300)]));
    let Primitive::ColumnCells(cells) = body.primitive else { panic!("the column answers ColumnCells") };
    let drawn: Vec<(&str, &str, CellKind)> = cells.iter().map(|(r, c)| (r.as_str(), c.value.as_str(), c.kind)).collect();
    assert_eq!(drawn, [("a", "91,300", CellKind::Number), ("b", "0", CellKind::Number)]);
}

#[test]
fn the_spent_tile_is_the_grouped_total_with_no_delta_spark_or_link() {
    let Primitive::KpiTile(tile) = spent_body("gems", "1940000").primitive else { panic!("the tile answers a KpiTile") };
    assert_eq!((tile.label.as_str(), tile.value.as_str()), ("Gems spent (24 h)", "1,940,000"));
    assert!(tile.delta.is_none() && tile.series.is_empty() && tile.link.is_none() && tile.unit.is_none());
}

fn attention(body: PanelBody) -> Vec<controlplane::AttentionItem> {
    match body.primitive {
        Primitive::AttentionItems(items) => items,
        _ => panic!("the purges item answers AttentionItems"),
    }
}

/// The drawing is `controlplane::purge_backlog_body`'s; this pins what wallet passes it.
#[test]
fn purges_name_wallet_and_link_its_own_sweep() {
    let items = attention(purges_body(3, "2026-11-19"));
    assert_eq!(items[0].title, "Wallet purges pending");
    assert_eq!(
        items[0].link,
        Link::Page {
            module: controlplane::EVENT_LOG_PAGE_MODULE.to_string(),
            page: controlplane::EVENT_LOG_PAGE.to_string(),
            params: BTreeMap::from([(
                controlplane::PAGE_PARAM_SEARCH.to_string(),
                "wallet.purge-on-scheduler.v1".to_string(),
            )]),
        }
    );
}

/// The balance column, the spent tile and the purges item against the live database.
#[tokio::test]
async fn the_column_and_tile_read_the_headline_currency_and_the_attention_the_backlog() {
    let Some(pool) = test_pool().await else { return };
    let _guard = DB_LOCK.lock().await;
    let (_ctx, svc) = wired(&pool).await;
    let (held, empty, purged) = (unique_player(&pool).await, unique_player(&pool).await, unique_player(&pool).await);
    let (code, other) = (unique_currency(&pool).await, unique_currency(&pool).await);
    for (pid, currency, amount) in [(&held, &code, 91_300_i64), (&held, &other, 5), (&empty, &other, 7)] {
        sqlx::query("INSERT INTO wallet.balances (player_id, currency, amount) VALUES ($1::uuid, $2, $3)")
            .bind(pid)
            .bind(currency)
            .bind(amount)
            .execute(&pool)
            .await
            .unwrap();
    }
    // Spent: the two debits of the headline currency inside the window; not the credit, the
    // debit 25 hours ago or the other currency's debit.
    for (i, (currency, delta, hours_ago)) in
        [(&code, -300, 0), (&code, -200, 1), (&code, 1000, 0), (&code, -50, 25), (&other, -7, 0)].into_iter().enumerate()
    {
        sqlx::query(
            "INSERT INTO wallet.ledger (idempotency_key, player_id, currency, delta, balance_after, reason, at) \
             VALUES ('admin-tests-spent-' || $1 || '-' || $2, $1::uuid, $3, $4, 0, 'admin.revoke', \
                     now() - make_interval(hours => $5))",
        )
        .bind(&held)
        .bind(i.to_string())
        .bind(currency)
        .bind(delta)
        .bind(hours_ago)
        .execute(&pool)
        .await
        .unwrap();
    }
    let admin = WalletAdmin::new(svc.clone(), code.clone());

    let mut column = query("column.balance", &held, None);
    column.entity = None;
    column.refs = vec![held.clone(), empty.clone()];
    let Primitive::ColumnCells(cells) = admin.panel(column.clone()).await.unwrap().primitive else {
        panic!("the column answers ColumnCells")
    };
    let drawn: Vec<(&str, &str)> = cells.iter().map(|(r, c)| (r.as_str(), c.value.as_str())).collect();
    let mut want = [(held.as_str(), "91,300"), (empty.as_str(), "0")];
    want.sort();
    assert_eq!(drawn, want, "the headline currency only; a ref with no row of it draws 0");

    // Malformed refs over the cap read the cap's refusal: it is checked before any SQL.
    column.refs = vec!["nope".to_string(); controlplane::MAX_PANEL_REFS + 1];
    let err = admin.panel(column.clone()).await.unwrap_err();
    assert_eq!(err.status, Status::Invalid);
    assert!(err.msg.contains("over the cap"), "{}", err.msg);
    column.refs = vec![held.clone(), "nope".to_string()];
    let err = admin.panel(column.clone()).await.unwrap_err();
    assert_eq!((err.status, err.msg.as_str()), (Status::Invalid, MALFORMED_PLAYER_ID));
    // Refused even when no balance row holds the headline currency, so no join casts a ref.
    let unheld_code = unique_currency(&pool).await;
    let unheld = WalletAdmin::new(svc, unheld_code.clone());
    let err = unheld.panel(column).await.unwrap_err();
    assert_eq!((err.status, err.msg.as_str()), (Status::Invalid, MALFORMED_PLAYER_ID));

    let Primitive::KpiTile(tile) = admin.panel(query("tile.spent", &held, None)).await.unwrap().primitive else {
        panic!("the tile answers a KpiTile")
    };
    assert_eq!((tile.label, tile.value), (format!("{} spent (24 h)", headline_label(&code)), "500".to_string()));

    sqlx::query("INSERT INTO wallet.purged_players (player_id, handle) VALUES ($1::uuid, 'admin-tests')")
        .bind(&purged)
        .execute(&pool)
        .await
        .unwrap();
    let items = attention(admin.panel(query("attention.purges", &held, None)).await.unwrap());
    sqlx::query("DELETE FROM wallet.purged_players WHERE player_id = $1::uuid")
        .bind(&purged)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(items.len(), 1, "a pending tombstone draws the item");
    assert!(items[0].text.contains("awaiting sweep · oldest "), "{}", items[0].text);

    cleanup(&pool, &[&held, &empty], &[&code, &other, &unheld_code]).await;
}

//! The wallet control-plane provider: the fact, the tiles and the Transactions table (pure),
//! the cursor and the number format, the headline-currency config, and the panels against the
//! live database.

use std::collections::BTreeMap;

use controlplane::{Align, CellKind, EntityRef, PanelBody, PanelQuery, Point, Primitive, Provider as _, Tone};
use lifecycle::{Context, Module as _};
use opsapi::Status;
use testdb::test_pool;
use walletapi::{Balance, Currency};

use super::{fact_text, grouped, manifest, parse_cursor, signed, tiles_body, transactions_body, WalletAdmin};
use crate::store::{LedgerEntry, LedgerPage, LEDGER_PAGE_SIZE};
use crate::tests::{cleanup, unique_currency, unique_player, wired};
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
fn cursor_is_a_decimal_seq_and_anything_else_is_the_callers_error() {
    assert_eq!(parse_cursor("42").unwrap(), 42);
    for bad in ["", "-1", "4x", "1.5", "99999999999999999999"] {
        assert_eq!(parse_cursor(bad).unwrap_err().status, Status::Invalid, "{bad:?}");
    }
}

#[test]
fn manifest_declares_the_tab_at_30_its_tiles_and_the_fact_at_50() {
    let m = manifest();
    assert!(!m.writes && m.actions.is_empty() && m.entity_kinds.is_empty());
    let decls: Vec<(&str, i32, &str, &str)> =
        m.contributions.iter().map(|c| (c.id.as_str(), c.order, c.title.as_str(), c.icon.as_str())).collect();
    assert_eq!(
        decls,
        [("wallet", 30, "Transactions", ""), ("wallet.tiles", 0, "Wallet", ""), ("fact.balance", 50, "Wallet", "coins")]
    );
    assert!(matches!(
        &m.contributions[0].point,
        Point::EntityTab { kind, tab, tab_label, columns: 1 } if kind == "player" && tab == "wallet" && tab_label == "Wallet"
    ));
    assert!(matches!(
        &m.contributions[1].point,
        Point::EntityTabTile { kind, tab } if kind == "player" && tab == "wallet"
    ));
    assert!(matches!(&m.contributions[2].point, Point::EntitySummary(kind) if kind == "player"));
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

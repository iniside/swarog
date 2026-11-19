//! The accounts control-plane provider against the live database.

use std::collections::BTreeMap;
use std::sync::Arc;

use accountsapi::Auth as _;
use controlplane::{CellKind, PanelQuery, Primitive, Provider as _};
use testdb::test_pool;

use crate::admin::AccountsAdmin;
use crate::tests::{cleanup_player, suffix, wired};

/// A canonical uuid no registration mints: registrations are v4, this is v0.
const NO_PLAYER: &str = "00000000-0000-0000-0000-000000000000";

fn label_query(refs: Vec<String>) -> PanelQuery {
    PanelQuery {
        module: "accounts".to_string(),
        contribution: "label".to_string(),
        entity: None,
        search: None,
        cursor: None,
        before: None,
        queue_state: None,
        range: None,
        params: BTreeMap::new(),
        refs,
    }
}

#[tokio::test]
async fn player_label_answers_each_handle_and_leaves_out_a_missing_player() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let mut ids = Vec::new();
    for name in ["LabelA", "LabelB"] {
        let email = format!("{name}-{}@test.local", suffix());
        ids.push(svc.register(email, "pw".into(), name.into()).await.unwrap().player_id);
    }
    let handles: Vec<Option<String>> = {
        let mut out = Vec::new();
        for id in &ids {
            out.push(svc.store.player_summary(id).await.unwrap().map(|p| p.handle));
        }
        out
    };
    let refs = vec![ids[0].clone(), NO_PLAYER.to_string(), "not-a-uuid".to_string(), ids[1].clone()];
    let answer = AccountsAdmin::new(Arc::clone(&svc)).panel(label_query(refs)).await;
    for id in &ids {
        cleanup_player(&pool, id).await;
    }

    let Primitive::ColumnCells(cells) = answer.unwrap().primitive else {
        panic!("a player label answers ColumnCells");
    };
    let drawn: BTreeMap<&str, (&str, CellKind)> =
        cells.iter().map(|(id, c)| (id.as_str(), (c.value.as_str(), c.kind))).collect();
    let expected: BTreeMap<&str, (&str, CellKind)> = ids
        .iter()
        .zip(&handles)
        .map(|(id, h)| (id.as_str(), (h.as_deref().unwrap(), CellKind::Text)))
        .collect();
    assert_eq!(drawn, expected, "one handle per known player; a missing or malformed id draws nothing");
    assert!(expected.values().all(|(h, _)| h.contains('#')), "a label is the Name#1234 handle");
}

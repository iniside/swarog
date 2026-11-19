//! The accounts control-plane provider against the live database.

use std::collections::BTreeMap;
use std::sync::Arc;

use accountsapi::Auth as _;
use controlplane::{CellKind, EntityRef, Link, PanelQuery, Primitive, Provider as _, SearchQuery};
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
    let keys: Vec<&str> = drawn.keys().copied().collect();
    let mut known: Vec<&str> = ids.iter().map(String::as_str).collect();
    known.sort();
    assert_eq!(keys, known, "one label per known player; a missing or malformed id draws nothing");
    for (id, name) in ids.iter().zip(["LabelA", "LabelB"]) {
        let (value, kind) = drawn[id.as_str()];
        assert_eq!(kind, CellKind::Text, "a label is a text cell");
        let digits = value
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('#'))
            .unwrap_or_else(|| {
                panic!("label {value:?} for {id} is not {name}#...: keyed to the wrong player")
            });
        assert!(
            digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()),
            "label {value:?} is the registered name plus '#' and exactly 4 digits",
        );
    }
}

#[tokio::test]
async fn player_search_hit_links_to_the_player_entity() {
    let Some(pool) = test_pool().await else { return };
    let (_ctx, svc) = wired(&pool).await;
    let email = format!("Seeker-{}@test.local", suffix());
    let id = svc.register(email, "pw".into(), "Seeker".into()).await.unwrap().player_id;
    let query = SearchQuery { module: "accounts".to_string(), kind: "player".to_string(), q: id.clone(), limit: 10 };
    let hits = AccountsAdmin::new(Arc::clone(&svc)).search(query).await;
    cleanup_player(&pool, &id).await;

    let hits = hits.unwrap();
    let hit = hits
        .iter()
        .find(|h| h.subtitle.ends_with(&id))
        .unwrap_or_else(|| panic!("searching by id finds player {id}: {hits:?}"));
    assert_eq!(
        hit.link,
        Link::Entity(EntityRef { kind: "player".to_string(), id: id.clone() }),
        "a player hit opens that player's entity page",
    );
}

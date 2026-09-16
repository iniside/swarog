use anyhow::{bail, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::PgPool;
use storageapi::MAX_OBJECTS_PER_OWNER;

use crate::{count_of, cursor_at_or_past, friends_call, friends_player, pass_nonce,
    player_call, poll_count, Ctx, FriendPlayer, Proof};

async fn put(ctx: &Ctx, front: &str, token: &str, key: &str, version: i64, acl: &str, value: &str) -> Result<(u16, Value)> {
    friends_call(ctx, front, token, Method::POST, &format!("/storage/proof/{key}"),
        Some(json!({"value": value, "expected_version": version, "acl": acl}))).await
}

async fn get(ctx: &Ctx, front: &str, token: &str, key: &str, owner: &str) -> Result<(u16, Value)> {
    friends_call(ctx, front, token, Method::POST, &format!("/storage/proof/{key}/get"),
        Some(json!({"owner_id": owner}))).await
}

pub(super) async fn assertions(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    if let Err(e) = checks(ctx, pool, p, front, tag).await {
        p.check(&format!("[ST0{tag}] storage pass completes"), false, format!("{e:#}"));
    }
    Ok(())
}

async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let a = friends_player(ctx, front, &format!("st-a-{nonce}@test.local"), &format!("StA{nonce}")).await?;
    let b = friends_player(ctx, front, &format!("st-b-{nonce}@test.local"), &format!("StB{nonce}")).await?;
    let (created, first) = put(ctx, front, &a.token, "public", 0, "public_read", r#"{"save":1}"#).await?;
    let (updated, second) = put(ctx, front, &a.token, "public", 1, "", r#"{"save":2}"#).await?;
    let (stale, _) = put(ctx, front, &a.token, "public", 1, "private", r#"{"save":3}"#).await?;
    let (stranger, _) = put(ctx, front, &b.token, "public", 1, "private", r#"{"save":4}"#).await?;
    let (duplicate, _) = put(ctx, front, &a.token, "public", 0, "private", r#"{"save":5}"#).await?;
    let (read, saved) = get(ctx, front, &a.token, "public", &a.player_id).await?;
    let saved_value = saved["value"].as_str().and_then(|value| serde_json::from_str::<Value>(value).ok());
    p.check(&format!("[ST1{tag}] CAS increments; stale/create owner 409, stranger 404, rejected writes preserve object"),
        created == 200 && first["version"] == 1 && updated == 200 && second["version"] == 2
            && stale == 409 && stranger == 404 && duplicate == 409 && read == 200
            && saved["version"] == 2 && saved["acl"] == "public_read"
            && saved["owner_id"] == a.player_id && saved_value == Some(json!({"save": 2})),
        format!("create={created} update={updated} stale={stale} stranger={stranger} duplicate={duplicate} saved={saved}"));

    let (private_create, _) = put(ctx, front, &a.token, "private", 0, "private", "{}").await?;
    let (public_read, public) = get(ctx, front, &b.token, "public", &a.player_id).await?;
    let (private_read, _) = get(ctx, front, &b.token, "private", &a.player_id).await?;
    let (own_read, own) = get(ctx, front, &a.token, "private", &a.player_id).await?;
    let (stranger_delete, _) = friends_call(ctx, front, &b.token, Method::DELETE, "/storage/proof/public", None).await?;
    let (still_readable, _) = get(ctx, front, &a.token, "public", "").await?;
    p.check(&format!("[ST2{tag}] public read crosses owners; private read and public delete are 404 for stranger"),
        private_create == 200 && public_read == 200 && public["owner_id"] == a.player_id
            && public["version"] == 2 && public["acl"] == "public_read"
            && private_read == 404 && own_read == 200 && own["acl"] == "private"
            && stranger_delete == 404 && still_readable == 200,
        format!("public={public_read} private={private_read} own={own_read} delete={stranger_delete} survivor={still_readable}"));

    let (page_code, page) = friends_call(ctx, front, &a.token, Method::POST, "/storage/list",
        Some(json!({"collection": "proof", "cursor": "", "limit": 1}))).await?;
    let cursor = page["next_cursor"].as_str().unwrap_or_default();
    let (next_code, next) = friends_call(ctx, front, &a.token, Method::POST, "/storage/list",
        Some(json!({"collection": "proof", "cursor": cursor, "limit": 1}))).await?;
    let (replay_code, replay) = friends_call(ctx, front, &b.token, Method::POST, "/storage/list",
        Some(json!({"collection": "proof", "cursor": cursor, "limit": 1}))).await?;
    let keys = [page["items"][0]["key"].as_str().unwrap_or_default(), next["items"][0]["key"].as_str().unwrap_or_default()];
    p.check(&format!("[ST3{tag}] paging visits both objects once; cursor replay cannot list another owner"),
        page_code == 200 && !cursor.is_empty() && page["items"].as_array().map(Vec::len) == Some(1)
            && next_code == 200 && next["items"].as_array().map(Vec::len) == Some(1)
            && keys.contains(&"public") && keys.contains(&"private") && next["next_cursor"] == ""
            && replay_code == 200 && replay["items"] == json!([]) && replay["next_cursor"] == "",
        format!("first={page} second={next} replay={replay}"));

    let (deleted, _) = friends_call(ctx, front, &a.token, Method::DELETE, "/storage/proof/private", None).await?;
    let (missing, _) = get(ctx, front, &a.token, "private", "").await?;
    let (recreated, incarnation) = put(ctx, front, &a.token, "private", 0, "", "{}").await?;
    p.check(&format!("[ST4{tag}] delete removes address; recreate starts version 1"),
        deleted == 204 && missing == 404 && recreated == 200 && incarnation["version"] == 1,
        format!("delete={deleted} get={missing} recreate={recreated} object={incarnation}"));

    let legal = json!({"collection":"proof", "key":"quic-legal", "value":"{}", "expected_version":0, "acl":"private"});
    let legal_response = player_call(ctx, Some(&a.token), "storage.put", &legal.to_string()).await?;
    let mut rejected = true;
    for key in ["illegal/key", "illegal%key", "illegal|key"] {
        let mut payload = legal.clone();
        payload["key"] = json!(key);
        let response = player_call(ctx, Some(&a.token), "storage.put", &payload.to_string()).await?;
        rejected &= response["status"] == "Invalid";
    }
    let illegal_rows = count_of(pool,
        "SELECT count(*) FROM storage.objects WHERE owner_id::text = $1 AND key IN ('illegal/key','illegal%key','illegal|key')",
        &a.player_id).await;
    let (legal_read, legal_object) = get(ctx, front, &a.token, "quic-legal", "").await?;
    p.check(&format!("[ST5{tag}] legal QUIC write is HTTP-readable; / % | keys reach service and return Invalid"),
        legal_response["status"] == "Ok" && legal_read == 200 && legal_object["version"] == 1
            && rejected && illegal_rows == 0,
        format!("legal={legal_response} read={legal_read} rejected={rejected} illegal_rows={illegal_rows}"));

    let c = friends_player(ctx, front, &format!("st-cap-{nonce}@test.local"), &format!("StC{nonce}")).await?;
    sqlx::query("INSERT INTO storage.objects (owner_id, collection, key, value) \
        SELECT $1::uuid, 'cap-seed', i::text, '{}'::jsonb FROM generate_series(1, $2::bigint) i")
        .bind(&c.player_id).bind(MAX_OBJECTS_PER_OWNER - 1).execute(pool).await?;
    let before = count_of(pool, "SELECT count(*) FROM storage.objects WHERE owner_id::text = $1", &c.player_id).await;
    let (left, right) = tokio::join!(put(ctx, front, &c.token, "cap-left", 0, "", "{}"), put(ctx, front, &c.token, "cap-right", 0, "", "{}"));
    let (left, left_body) = left?;
    let (right, right_body) = right?;
    let (over, _) = put(ctx, front, &c.token, "cap-over", 0, "", "{}").await?;
    let after = count_of(pool, "SELECT count(*) FROM storage.objects WHERE owner_id::text = $1", &c.player_id).await;
    let inserted = count_of(pool, "SELECT count(*) FROM storage.objects WHERE owner_id::text = $1 AND collection = 'proof'", &c.player_id).await;
    p.check(&format!("[ST6{tag}] cap permits exactly the last slot, refuses MAX+1, counts across collections"),
        before == MAX_OBJECTS_PER_OWNER - 1 && after == MAX_OBJECTS_PER_OWNER && inserted == 1 && over == 409
            && ((left == 200 && left_body["version"] == 1 && right == 409)
                || (right == 200 && right_body["version"] == 1 && left == 409)),
        format!("before={before} left={left} right={right} over={over} after={after} inserted={inserted}"));
    sqlx::query("DELETE FROM storage.objects WHERE owner_id::text = $1").bind(&c.player_id).execute(pool).await?;
    Ok(())
}

pub(super) async fn seed_purge(ctx: &Ctx, pool: &PgPool, front: &str, s: &FriendPlayer, t: &FriendPlayer) -> Result<Vec<(String, String, String, i64, String)>> {
    for player in [s, t] {
        for (collection, key, acl) in [("purge-one", "private", "private"), ("purge-two", "public", "public_read")] {
            let (code, body) = friends_call(ctx, front, &player.token, Method::POST,
                &format!("/storage/{collection}/{key}"),
                Some(json!({"value":"{\"seed\":1}", "expected_version":0, "acl":acl}))).await?;
            if code != 200 || body["version"] != 1 { bail!("storage purge fixture rejected: {code} {body}"); }
        }
    }
    let before = snapshot(pool, &s.player_id).await?;
    let survivor = snapshot(pool, &t.player_id).await?;
    if before.len() != 2 || survivor != before { bail!("storage purge fixtures differ: {before:?} {survivor:?}"); }
    Ok(before)
}

async fn snapshot(pool: &PgPool, owner: &str) -> Result<Vec<(String, String, String, i64, String)>> {
    Ok(sqlx::query_as("SELECT collection, key, value::text, version, acl FROM storage.objects WHERE owner_id::text = $1 ORDER BY collection, key")
        .bind(owner).fetch_all(pool).await?)
}

pub(super) async fn check_purge(pool: &PgPool, p: &mut Proof, (s, t): (&FriendPlayer, &FriendPlayer),
    before: Vec<(String, String, String, i64, String)>, (planted, swept): (bool, bool),
    (deleted, fire): (&str, &str), tag: &str) -> Result<()> {
    let after = snapshot(pool, &s.player_id).await?;
    let survivor = snapshot(pool, &t.player_id).await?;
    let tomb = cursor_at_or_past(pool, "storage.player-deleted.v1", deleted).await;
    let sweep = cursor_at_or_past(pool, "storage.purge-on-scheduler.v1", fire).await;
    p.check(&format!("[DL12{tag}] storage: two seeded objects purged across collections, identical survivor untouched, both cursors advanced"),
        before.len() == 2 && after.is_empty() && survivor == before && planted && swept && tomb == 1 && sweep == 1,
        format!("before={before:?} after={after:?} survivor={survivor:?} planted={planted} swept={swept} tomb={tomb} sweep={sweep}"));
    Ok(())
}

pub(super) async fn check_guard(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, w: &FriendPlayer, tag: &str) -> Result<()> {
    let planted = poll_count(pool, "SELECT count(*) FROM storage.purged_players WHERE player_id::text = $1", &w.player_id, 1).await;
    let (code, _) = put(ctx, front, &w.token, "guard", 0, "", "{}").await?;
    let rows = count_of(pool, "SELECT count(*) FROM storage.objects WHERE owner_id::text = $1", &w.player_id).await;
    p.check(&format!("[DL13{tag}] storage tombstone refuses still-authenticated create with 409 and writes no row"),
        planted && code == 409 && rows == 0,
        format!("planted={planted} code={code} rows={rows}"));
    Ok(())
}

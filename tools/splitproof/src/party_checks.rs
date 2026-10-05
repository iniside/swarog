//! `[PY1]`-`[PY7]` (party ops invoked over `/push`) and `[DL14]` (the party purge of a
//! deleted player), run once per topology. In the split every op is dispatched Remote from
//! gateway-svc to party-svc, whose `party.state`/`party.invite` pushes cross back over the
//! backplane to the socket on gateway-svc; in the monolith all of it is Local.
//!
//! Both fleets run party with `PARTY_LEASE_SECS=10`, so every wait below is a poll against a
//! deadline that keeps the members it needs alive with `party.heartbeat`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use partyapi::player_rpc::{
    METHOD_ACCEPT, METHOD_CREATE, METHOD_HEARTBEAT, METHOD_INVITE, METHOD_KICK, METHOD_LEAVE,
};
use partyapi::{NOT_READY, PUSH_INVITE_TOPIC, PUSH_STATE_TOPIC};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::pushws::{Frame, PushClient};
use crate::{count_of, cursor_at_or_past, friends_call, friends_player, pass_nonce, wallet_form,
    Ctx, FriendPlayer, Proof};

const INVOKE_BUDGET: Duration = Duration::from_secs(15);
/// `[PY5]`: the 10 s proof lease plus the 5 s sweep interval, with headroom for one slow pass.
const LEASE_EXPIRY_DEADLINE: Duration = Duration::from_secs(30);
/// The least lease left before `[PY2]`-`[PY4]`; below it the step could race the sweep.
const MIN_LEASE_AHEAD_SECS: f64 = 5.0;

/// One `/push` socket with its own invoke-id sequence. Every frame read while an invoke
/// awaits its result is buffered by `PushClient`, so a push is never lost to an invoke.
struct Sock {
    client: PushClient,
    next_id: i64,
}

impl Sock {
    async fn open(ws: &str, token: &str) -> Result<Sock> {
        let mut client = PushClient::connect(ws, token, "dev-key-client").await?;
        match client.next_frame(Duration::from_secs(10)).await {
            Ok(Some(Frame::Ack { .. })) => Ok(Sock { client, next_id: 1 }),
            other => bail!("/push opened without an ack: {other:?}"),
        }
    }

    async fn call(&mut self, method: &str, payload: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.client.invoke(id, method, payload, INVOKE_BUDGET).await
    }

    /// The first `party.state` whose view satisfies `want`, as `(hub, view)`.
    async fn await_state(&mut self, budget: Duration, want: impl Fn(&Value) -> bool) -> Option<(String, Value)> {
        let frame = self.client.await_frame(budget, |f| state_of(f).is_some_and(|(_, v)| want(&v))).await?;
        state_of(&frame)
    }
}

fn state_of(frame: &Frame) -> Option<(String, Value)> {
    match frame {
        Frame::Message { hub, topic, payload } if topic == PUSH_STATE_TOPIC => {
            Some((hub.clone(), serde_json::from_slice(payload).ok()?))
        }
        _ => None,
    }
}

fn status(r: &Value) -> &str {
    r.get("status").and_then(Value::as_str).unwrap_or_default()
}

fn err(r: &Value) -> &str {
    r.get("err").and_then(Value::as_str).unwrap_or_default()
}

fn text<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

fn int(v: &Value, pointer: &str) -> i64 {
    v.pointer(pointer).and_then(Value::as_i64).unwrap_or(-1)
}

/// The sorted `player_id`s of a `PartyView`'s members.
fn roster(view: &Value, pointer: &str) -> Vec<String> {
    let mut ids: Vec<String> = view
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|members| {
            members
                .iter()
                .filter_map(|m| m.get("player_id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn sorted(ids: &[&str]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    ids.sort();
    ids
}

/// Seconds from the database clock to `lease_until`; `-1` for an empty or unparseable value.
/// Measured on the database because party stamps the lease with `clock_timestamp()`.
async fn lease_ahead(pool: &PgPool, lease_until: &str) -> f64 {
    if lease_until.is_empty() {
        return -1.0;
    }
    sqlx::query_scalar("SELECT extract(epoch FROM ($1::timestamptz - clock_timestamp()))::float8")
        .bind(lease_until)
        .fetch_one(pool)
        .await
        .unwrap_or(-1.0)
}

/// Both players heartbeat; `a` must hold a lease at least [`MIN_LEASE_AHEAD_SECS`] ahead and
/// `b` likewise when `b_member`, else an EMPTY `lease_until` (it is in no party yet). Also
/// answers `b`'s `lease_until` as returned.
async fn heartbeat_gate(pool: &PgPool, a: &mut Sock, b: &mut Sock, b_member: bool) -> Result<(bool, String, String)> {
    let ha = a.call(METHOD_HEARTBEAT, json!({})).await?;
    let hb = b.call(METHOD_HEARTBEAT, json!({})).await?;
    let ahead_a = lease_ahead(pool, text(&ha, "/value/lease_until")).await;
    let ahead_b = lease_ahead(pool, text(&hb, "/value/lease_until")).await;
    let b_ok = if b_member {
        ahead_b >= MIN_LEASE_AHEAD_SECS
    } else {
        status(&hb) == "Ok" && text(&hb, "/value/lease_until").is_empty()
    };
    let ok = status(&ha) == "Ok" && status(&hb) == "Ok" && ahead_a >= MIN_LEASE_AHEAD_SECS && b_ok;
    let b_lease = text(&hb, "/value/lease_until").to_string();
    Ok((ok, format!("lease_a={ahead_a:.1}s lease_b={ahead_b:.1}s b_member={b_member}"), b_lease))
}

pub(super) async fn checks(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, admin: &reqwest::Client, tag: &str) -> Result<()> {
    let ws = format!("ws://{}", front.trim_start_matches("http://"));
    let nonce = pass_nonce(tag);
    let a = friends_player(ctx, front, &format!("py-a-{nonce}@test.local"), &format!("PyA{nonce}")).await?;
    let b = friends_player(ctx, front, &format!("py-b-{nonce}@test.local"), &format!("PyB{nonce}")).await?;
    let mut sa = Sock::open(&ws, &a.token).await?;
    let mut sb = Sock::open(&ws, &b.token).await?;

    // [PY1] create answers the leader-only roster, and the same revision is pushed to the
    // leader's socket, where the gateway labels it hub `party`.
    let created = sa.call(METHOD_CREATE, json!({})).await?;
    let party_id = text(&created, "/value/party_id").to_string();
    let rev1 = int(&created, "/value/revision");
    let py1_push = sa.await_state(Duration::from_secs(10), |v| v["revision"] == rev1).await;
    let py1_rows = count_of(pool, "SELECT count(*) FROM party.parties WHERE id::text = $1", &party_id).await;
    p.check(&format!("[PY1{tag}] party.create over /push -> leader-only party, same revision pushed, gateway hub label `party`"),
        status(&created) == "Ok" && !party_id.is_empty() && rev1 > 0
            && text(&created, "/value/leader_id") == a.player_id
            && roster(&created, "/value/members") == sorted(&[&a.player_id])
            && text(&created, "/value/members/0/ready") == NOT_READY
            && py1_push.as_ref().is_some_and(|(hub, v)| hub == "party" && v["party_id"] == party_id.as_str())
            && py1_rows == 1,
        format!("create={created} push={py1_push:?} rows={py1_rows}"));

    // [PY2] the invite is addressed by B's handle and reaches B's socket as `party.invite`.
    let (gate2, gate2_detail, _) = heartbeat_gate(pool, &mut sa, &mut sb, false).await?;
    let invited = sa.call(METHOD_INVITE, json!({ "target_handle": b.handle })).await?;
    let invite_id = text(&invited, "/value/invite_id").to_string();
    let py2_frame = sb.client
        .await_frame(Duration::from_secs(10), |f| matches!(f, Frame::Message { topic, .. } if topic == PUSH_INVITE_TOPIC))
        .await;
    let (py2_hub, py2_view) = match &py2_frame {
        Some(Frame::Message { hub, payload, .. }) => (hub.clone(), serde_json::from_slice::<Value>(payload).unwrap_or(Value::Null)),
        _ => (String::new(), Value::Null),
    };
    p.check(&format!("[PY2{tag}] leader invites by handle over /push -> invitee's socket gets gateway hub label `party`, topic `{PUSH_INVITE_TOPIC}`, same invite"),
        gate2 && status(&invited) == "Ok" && !invite_id.is_empty()
            && text(&invited, "/value/party_id") == party_id
            && text(&invited, "/value/inviter_handle") == a.handle
            && py2_hub == "party" && py2_view["invite_id"] == invite_id.as_str()
            && py2_view["party_id"] == party_id.as_str() && py2_view["inviter_handle"] == a.handle.as_str(),
        format!("{gate2_detail} invite={invited} hub={py2_hub:?} pushed={py2_view}"));

    // [PY3] accept stamps above create, and BOTH sockets see the two-member snapshot at the
    // accept's own revision.
    let (gate3, gate3_detail, _) = heartbeat_gate(pool, &mut sa, &mut sb, false).await?;
    let accepted = sb.call(METHOD_ACCEPT, json!({ "invite_id": invite_id })).await?;
    let rev3 = int(&accepted, "/value/revision");
    let both = sorted(&[&a.player_id, &b.player_id]);
    let two = |v: &Value| roster(v, "/members") == both;
    let py3_a = sa.await_state(Duration::from_secs(10), two).await;
    let py3_b = sb.await_state(Duration::from_secs(10), two).await;
    let pushed_rev = |s: &Option<(String, Value)>| s.as_ref().map(|(_, v)| int(v, "/revision"));
    let pushed_hub = |s: &Option<(String, Value)>| s.as_ref().map(|(h, _)| h.clone());
    p.check(&format!("[PY3{tag}] invitee accepts over /push -> both sockets get the 2-member `{PUSH_STATE_TOPIC}` at the accept's revision"),
        gate3 && status(&accepted) == "Ok" && text(&accepted, "/value/party_id") == party_id
            && roster(&accepted, "/value/members") == both && rev3 > rev1
            && pushed_rev(&py3_a) == Some(rev3) && pushed_rev(&py3_b) == Some(rev3)
            && pushed_hub(&py3_a).as_deref() == Some("party") && pushed_hub(&py3_b).as_deref() == Some("party"),
        format!("{gate3_detail} accept={accepted} rev1={rev1} push_a={py3_a:?} push_b={py3_b:?}"));

    // [PY4] a member kicking the leader is the leader-only NotFound, and changes nothing: the
    // leader is still a member and the party's revision is still the accept's.
    let (gate4, gate4_detail, b_lease) = heartbeat_gate(pool, &mut sa, &mut sb, true).await?;
    let kicked = sb.call(METHOD_KICK, json!({ "player_id": a.player_id })).await?;
    let py4_members = count_of(pool, "SELECT count(*) FROM party.members WHERE party_id::text = $1", &party_id).await;
    let py4_rev = count_of(pool, "SELECT revision FROM party.parties WHERE id::text = $1", &party_id).await;
    p.check(&format!("[PY4{tag}] non-leader party.kick of the leader over /push -> NotFound `party not found`, nothing stamped"),
        gate4 && status(&kicked) == "NotFound" && err(&kicked) == "party not found"
            && py4_members == 2 && py4_rev == rev3,
        format!("{gate4_detail} kick={kicked} members={py4_members} revision={py4_rev} accept_rev={rev3}"));

    // [PY5] B goes silent while A keeps heartbeating: the lease sweep removes B, A's socket
    // gets the 1-member snapshot ABOVE the accept's revision (so no earlier 1-member
    // snapshot can satisfy it), and B's socket gets
    // the "no party" view stamped with that same revision.
    //
    // The lower bound: B's stored lease must still be the one the gate4 heartbeat answered
    // (the failed kick renewed nothing), and every sample pairs B's presence with the
    // database clock, so B absent while that clock is still before its lease FAILS — only
    // the lease sweep may remove it, and only once the lease has passed.
    let lease_unchanged = sqlx::query_scalar::<_, bool>(
        "SELECT lease_until = $2::timestamptz FROM party.members WHERE player_id::text = $1")
        .bind(&b.player_id).bind(&b_lease).fetch_optional(pool).await.ok().flatten() == Some(true);
    let mut early_removal = false;
    let mut absent_after_expiry = false;
    let silent_since = Instant::now();
    let deadline = silent_since + LEASE_EXPIRY_DEADLINE;
    let only_a = sorted(&[&a.player_id]);
    let mut py5_a = None;
    let mut heartbeats_ok = true;
    loop {
        let (present, before_expiry) = b_sample(pool, &b.player_id, &b_lease).await;
        if !present {
            early_removal |= before_expiry;
            absent_after_expiry |= !before_expiry;
        }
        if (py5_a.is_some() && !present) || Instant::now() >= deadline {
            break;
        }
        if py5_a.is_some() {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let hb = sa.call(METHOD_HEARTBEAT, json!({})).await?;
        heartbeats_ok &= status(&hb) == "Ok" && !text(&hb, "/value/lease_until").is_empty();
        py5_a = sa
            .await_state(Duration::from_secs(1), |v| int(v, "/revision") > rev3 && roster(v, "/members") == only_a)
            .await;
    }
    let removed_after = silent_since.elapsed();
    let rev5 = pushed_rev(&py5_a).unwrap_or(-1);
    let py5_b = sb
        .await_state(Duration::from_secs(10), |v| v["party_id"] == "" && int(v, "/revision") == rev5)
        .await;
    let b_rows = count_of(pool, "SELECT count(*) FROM party.members WHERE player_id::text = $1", &b.player_id).await;
    let b_rev = count_of(pool, "SELECT revision FROM party.player_revisions WHERE player_id::text = $1", &b.player_id).await;
    p.check(&format!("[PY5{tag}] a silent member is swept within lease+sweep and never before its lease expires -> leader's socket gets the 1-member snapshot, the swept socket the no-party view, same revision"),
        heartbeats_ok && lease_unchanged && !early_removal && absent_after_expiry && rev5 > rev3
            && pushed_hub(&py5_a).as_deref() == Some("party")
            && py5_a.as_ref().is_some_and(|(_, v)| v["leader_id"] == a.player_id.as_str())
            && pushed_hub(&py5_b).as_deref() == Some("party")
            && py5_b.as_ref().is_some_and(|(_, v)| v["members"] == json!([]) && v["leader_id"] == "")
            && b_rows == 0 && b_rev == rev5,
        format!("lease_b={b_lease} lease_unchanged={lease_unchanged} early_removal={early_removal} absent_after_expiry={absent_after_expiry} after={removed_after:?} heartbeats_ok={heartbeats_ok} push_a={py5_a:?} push_b={py5_b:?} b_rows={b_rows} b_rev={b_rev} accept_rev={rev3}"));

    // [PY6] the last member leaving dissolves the party; the answer is the "no party" view
    // carrying the leave's own stamp, which is also A's recorded revision.
    let left = sa.call(METHOD_LEAVE, json!({})).await?;
    let rev6 = int(&left, "/value/revision");
    let py6_rows = count_of(pool, "SELECT count(*) FROM party.parties WHERE id::text = $1", &party_id).await;
    let py6_members = count_of(pool, "SELECT count(*) FROM party.members WHERE party_id::text = $1", &party_id).await;
    let a_rev = count_of(pool, "SELECT revision FROM party.player_revisions WHERE player_id::text = $1", &a.player_id).await;
    let py6_push = sa
        .await_state(Duration::from_secs(10), |v| v["party_id"] == "" && int(v, "/revision") == rev6)
        .await;
    p.check(&format!("[PY6{tag}] last member party.leave over /push -> no-party view above the sweep's revision answered AND pushed to the leaver, party row gone"),
        status(&left) == "Ok" && text(&left, "/value/party_id").is_empty()
            && rev6 > rev5 && py6_rows == 0 && py6_members == 0 && a_rev == rev6
            && pushed_hub(&py6_push).as_deref() == Some("party")
            && py6_push.as_ref().is_some_and(|(_, v)| v["members"] == json!([]) && v["leader_id"] == ""),
        format!("leave={left} sweep_rev={rev5} parties={py6_rows} members={py6_members} a_rev={a_rev} push={py6_push:?}"));
    sa.client.disconnect().await;
    sb.client.disconnect().await;

    // [PY7] a notifications nudge and a party snapshot share one socket, each with its own
    // gateway hub label. The socket is opened only once every wallet.changed for A that
    // registration caused has been consumed by notifications (the nudge leaves inside that
    // delivery, before its checkpoint), so the only nudge it can see is this grant's.
    let quiesced = nudges_quiesced(pool, &a.player_id).await;
    let mut s7 = Sock::open(&ws, &a.token).await?;
    let credits_before = count_of(pool,
        "SELECT count(*) FROM notifications.messages WHERE player_id::text = $1 AND kind = 'wallet.credit'",
        &a.player_id).await;
    let (csrf, idem, _) = wallet_form(admin, front).await?;
    let grant = admin
        .post(format!("{front}/admin/wallet"))
        .form(&[("_csrf", csrf.as_str()), ("_idem_grant", idem.as_str()), ("_action", "grant"),
            ("player_id", a.player_id.as_str()), ("currency", "gold"), ("amount", "7"),
            ("reason", "splitproof party shared socket")])
        .send()
        .await?
        .status()
        .as_u16();
    let created7 = s7.call(METHOD_CREATE, json!({})).await?;
    let party7 = text(&created7, "/value/party_id").to_string();
    let rev7 = int(&created7, "/value/revision");
    // ONE read loop for both frames: they arrive in either order, and a waiter for one
    // discards the other.
    let mut py7_state = None;
    let mut nudge_hub = None;
    let frame_deadline = Instant::now() + Duration::from_secs(30);
    while (py7_state.is_none() || nudge_hub.is_none()) && Instant::now() < frame_deadline {
        let left = frame_deadline.saturating_duration_since(Instant::now());
        let Ok(Some(frame)) = s7.client.next_frame(left).await else { break };
        match &frame {
            Frame::Message { hub, topic, .. } if topic == notificationsapi::PUSH_NEW_TOPIC => {
                nudge_hub = Some(hub.clone());
            }
            _ => {
                if let Some(state) = state_of(&frame).filter(|(_, v)| v["party_id"] == party7.as_str()) {
                    py7_state = Some(state);
                }
            }
        }
    }
    // The nudge leaves inside the delivery transaction, so the row may commit after it.
    let credit_deadline = Instant::now() + Duration::from_secs(15);
    let mut credits_after = credits_before;
    while credits_after <= credits_before && Instant::now() < credit_deadline {
        credits_after = count_of(pool,
            "SELECT count(*) FROM notifications.messages WHERE player_id::text = $1 AND kind = 'wallet.credit'",
            &a.player_id).await;
        if credits_after <= credits_before {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let cleanup = s7.call(METHOD_LEAVE, json!({})).await?;
    s7.client.disconnect().await;
    p.check(&format!("[PY7{tag}] this grant's `notifications` nudge and a `party` snapshot share one socket, each with its own gateway hub label"),
        quiesced && grant == 303 && credits_before >= 0 && credits_after > credits_before && status(&created7) == "Ok"
            && pushed_hub(&py7_state).as_deref() == Some("party") && pushed_rev(&py7_state) == Some(rev7)
            && nudge_hub.as_deref() == Some("notifications") && status(&cleanup) == "Ok",
        format!("quiesced={quiesced} grant={grant} credits={credits_before}->{credits_after} create={created7} state={py7_state:?} nudge_hub={nudge_hub:?} cleanup={}", status(&cleanup)));
    Ok(())
}

/// `(B is a member, the database clock is still before lease)` from ONE statement.
async fn b_sample(pool: &PgPool, player_id: &str, lease: &str) -> (bool, bool) {
    sqlx::query_as::<_, (bool, bool)>(
        "SELECT EXISTS (SELECT 1 FROM party.members WHERE player_id::text = $1), \
                clock_timestamp() < $2::timestamptz")
        .bind(player_id).bind(lease).fetch_one(pool).await.unwrap_or((true, false))
}

/// Polls `subscription` to at or past `event_id`, against a deadline.
async fn poll_cursor(pool: &PgPool, subscription: &str, event_id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if cursor_at_or_past(pool, subscription, event_id).await == 1 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

/// True once wallet has consumed `player`'s `player.registered` (so any starter grant it
/// causes is already appended) and notifications has consumed `player`'s latest
/// `wallet.changed`, if there is one.
async fn nudges_quiesced(pool: &PgPool, player: &str) -> bool {
    let registered: Option<String> = sqlx::query_scalar(
        "SELECT event_id FROM asyncevents.events WHERE topic = 'player.registered' AND payload->>'player_id' = $1")
        .bind(player).fetch_optional(pool).await.ok().flatten();
    let Some(registered) = registered else { return false };
    if !poll_cursor(pool, "wallet.player-registered.v1", &registered).await {
        return false;
    }
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT event_id FROM asyncevents.events WHERE topic = 'wallet.changed' AND payload->>'player_id' = $1 \
          ORDER BY generation DESC, producer_xid DESC, tie_breaker DESC LIMIT 1")
        .bind(player).fetch_optional(pool).await.ok().flatten();
    match latest {
        Some(event_id) => poll_cursor(pool, "notifications.wallet-changed.v1", &event_id).await,
        None => true,
    }
}

async fn http(ctx: &Ctx, front: &str, who: &FriendPlayer, path: &str, body: Value) -> Result<(u16, Value)> {
    friends_call(ctx, front, &who.token, Method::POST, path, Some(body)).await
}

/// `[DL14]`, with its transport errors converted into ONE named failing check (the
/// `deletion_assertions` reason: the parity pass runs before `[W2]`).
pub(super) async fn deletion_assertion(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    if let Err(e) = deletion_check(ctx, pool, p, front, tag).await {
        p.check(&format!("[DL14{tag}] the party purge pass ran to completion"), false, format!("{e:#}"));
    }
    Ok(())
}

/// `[DL14]`: party keeps no `purged_players` tombstone, so its consumer is proven by what only
/// `party.player-deleted.v1` can do. Two invites involving C — C's to E, and E's to C — are
/// deleted while still UNEXPIRED: the invite sweep deletes only `expires_at < now()`, and the
/// one dissolution path that drops live invites is ruled out by both inviting parties still
/// existing (D and E heartbeat throughout). The membership half alone would not discriminate,
/// since the 10 s lease sweep also removes C; C's missing `player_revisions` row does, since a
/// sweep removal upserts it and only the purge deletes it.
async fn deletion_check(ctx: &Ctx, pool: &PgPool, p: &mut Proof, front: &str, tag: &str) -> Result<()> {
    let nonce = pass_nonce(tag);
    let c = friends_player(ctx, front, &format!("pd-c-{nonce}@test.local"), &format!("PdC{nonce}")).await?;
    let d = friends_player(ctx, front, &format!("pd-d-{nonce}@test.local"), &format!("PdD{nonce}")).await?;
    let e = friends_player(ctx, front, &format!("pd-e-{nonce}@test.local"), &format!("PdE{nonce}")).await?;

    let (c_create, c_party) = http(ctx, front, &c, "/party/create", json!({})).await?;
    let party_c = text(&c_party, "/party_id").to_string();
    let (d_invite, d_inv) = http(ctx, front, &c, "/party/invite", json!({ "target_handle": d.handle })).await?;
    let (d_accept, _) = http(ctx, front, &d, "/party/accept", json!({ "invite_id": text(&d_inv, "/invite_id") })).await?;
    let (e_invite, e_inv) = http(ctx, front, &c, "/party/invite", json!({ "target_handle": e.handle })).await?;
    let (e_create, e_party) = http(ctx, front, &e, "/party/create", json!({})).await?;
    let party_e = text(&e_party, "/party_id").to_string();
    let (c_invite, c_inv) = http(ctx, front, &e, "/party/invite", json!({ "target_handle": c.handle })).await?;
    let inv_ce = text(&e_inv, "/invite_id").to_string();
    let inv_ec = text(&c_inv, "/invite_id").to_string();
    let fixture = [c_create, d_invite, e_invite, e_create, c_invite] == [201; 5] && d_accept == 200;
    if !fixture || party_c.is_empty() || party_e.is_empty() || inv_ce.is_empty() || inv_ec.is_empty() {
        bail!("party purge fixture rejected: create={c_create} invite_d={d_invite} accept={d_accept} \
            invite_e={e_invite} create_e={e_create} invite_c={c_invite}");
    }
    let c_revision_before = count_of(pool, "SELECT count(*) FROM party.player_revisions WHERE player_id::text = $1", &c.player_id).await;
    // The least TTL left on either invite, read before the delete: their disappearance must
    // be observed inside it.
    let ttl_left: f64 = sqlx::query_scalar(
        "SELECT coalesce(min(extract(epoch FROM (expires_at - clock_timestamp())))::float8, -1) \
           FROM party.invites WHERE id::text IN ($1, $2)")
        .bind(&inv_ce).bind(&inv_ec).fetch_one(pool).await.context("read the invite TTLs")?;
    let ttl_clock = Instant::now();
    let invites_before = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM party.invites WHERE id::text IN ($1, $2)")
        .bind(&inv_ce).bind(&inv_ec).fetch_one(pool).await.unwrap_or(-1);

    let (begin_code, begin) = friends_call(ctx, front, &c.token, Method::POST, "/accounts/delete/begin", None).await?;
    let ticket = text(&begin, "/ticket").to_string();
    let (delete_code, _) = friends_call(ctx, front, &c.token, Method::POST, "/accounts/delete",
        Some(json!({ "ticket": ticket }))).await?;
    let deleted_event: String = sqlx::query_scalar(
        "SELECT event_id FROM asyncevents.events WHERE topic = 'player.deleted' AND payload->>'player_id' = $1")
        .bind(&c.player_id).fetch_optional(pool).await.ok().flatten().unwrap_or_default();

    let deadline = Instant::now() + Duration::from_secs(40);
    let mut purged = None;
    let mut keepalive_ok = true;
    while purged.is_none() && Instant::now() < deadline {
        for who in [&d, &e] {
            let (code, _) = http(ctx, front, who, "/party/heartbeat", json!({})).await?;
            keepalive_ok &= code == 200;
        }
        let (invites, members, revisions): (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM party.invites WHERE id::text IN ($1, $2)), \
                    (SELECT count(*) FROM party.members WHERE player_id::text = $3), \
                    (SELECT count(*) FROM party.player_revisions WHERE player_id::text = $3)")
            .bind(&inv_ce).bind(&inv_ec).bind(&c.player_id).fetch_one(pool).await.unwrap_or((-1, -1, -1));
        if invites == 0 && members == 0 && revisions == 0 {
            purged = Some(ttl_clock.elapsed());
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let leader: Option<String> = sqlx::query_scalar("SELECT leader_id::text FROM party.parties WHERE id::text = $1")
        .bind(&party_c).fetch_optional(pool).await.ok().flatten();
    let c_roster = count_of(pool, "SELECT count(*) FROM party.members WHERE party_id::text = $1", &party_c).await;
    let e_alive = count_of(pool, "SELECT count(*) FROM party.parties WHERE id::text = $1", &party_e).await;
    let cursor = cursor_at_or_past(pool, "party.player-deleted.v1", &deleted_event).await;
    let inside_ttl = purged.is_some_and(|t| t.as_secs_f64() < ttl_left);
    p.check(&format!("[DL14{tag}] party: both UNEXPIRED invites involving the deleted player gone, its member row and revision row gone, the other member leads, cursor advanced"),
        begin_code == 200 && delete_code == 200 && !deleted_event.is_empty()
            && invites_before == 2 && c_revision_before == 1 && ttl_left > 60.0 && inside_ttl
            && keepalive_ok && leader.as_deref() == Some(d.player_id.as_str()) && c_roster == 1
            && e_alive == 1 && cursor == 1,
        format!("begin={begin_code} delete={delete_code} event={deleted_event} invites_before={invites_before} \
            c_rev_before={c_revision_before} ttl_left={ttl_left:.1}s purged_after={purged:?} keepalive={keepalive_ok} \
            leader={leader:?} d={} c_roster={c_roster} e_party={e_alive} cursor={cursor}", d.player_id));

    for who in [&d, &e] {
        http(ctx, front, who, "/party/leave", json!({})).await?;
    }
    Ok(())
}

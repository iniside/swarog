use std::sync::{Arc, OnceLock};

use accountsapi::{Directory, PlayerSummary, MAX_HANDLE_BYTES};
use async_trait::async_trait;
use opsapi::{Error, Identity};
use partyapi::{
    HeartbeatView, InviteList, InviteView, MemberView, PartyView, Player, INVITE_TTL_SECS,
    MAX_MEMBERS, NOT_READY, PUSH_INVITE_TOPIC, PUSH_STATE_TOPIC, READY,
};
use push::{Message, Push, Target};
use sqlx::{PgConnection, Postgres, Transaction};

use crate::internal;
use crate::store::{is_unique_violation, InviteRow, Removal, Store, ViewRow};

/// The ONE answer for a party, invite or member the caller may not see or act on: a 403
/// would confirm the id names something real.
pub(crate) const NOT_FOUND: &str = "party not found";
pub(crate) const INVITE_NOT_FOUND: &str = "invite not found";
pub(crate) const NO_SUCH_PLAYER: &str = "no player with that handle";
pub(crate) const ALREADY_IN_PARTY: &str = "already in a party";

/// Attempts at "read my party, lock it, re-read it" before answering `Conflict`. A retry is
/// a fresh transaction, so no transaction ever holds two party locks.
const MEMBERSHIP_ATTEMPTS: usize = 3;

pub(crate) fn is_uuid_text(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// A caller-supplied id, lowercased so it compares equal to the database's spelling.
fn uuid_arg(name: &str, value: &str) -> Result<String, Error> {
    if !is_uuid_text(value) {
        return Err(Error::invalid(format!("{name} is not a valid uuid")));
    }
    Ok(value.to_ascii_lowercase())
}

fn validate_state(state: &str) -> Result<&'static str, Error> {
    match state {
        READY => Ok(READY),
        NOT_READY => Ok(NOT_READY),
        _ => Err(Error::invalid(format!(
            "state must be {READY} or {NOT_READY}"
        ))),
    }
}

/// Every directory failure on the invite target is `Unavailable` (503): the handle was
/// already checked against [`MAX_HANDLE_BYTES`], so an `Err` is the directory being
/// unreachable; a miss is `Ok(None)`.
fn directory_unavailable(e: Error) -> Error {
    Error::unavailable(format!("player directory unavailable: {}", e.msg))
}

fn write_error(e: sqlx::Error) -> Error {
    if is_unique_violation(&e) {
        Error::conflict(ALREADY_IN_PARTY)
    } else {
        internal(e)
    }
}

fn no_party(revision: i64) -> PartyView {
    PartyView {
        party_id: String::new(),
        revision,
        leader_id: String::new(),
        members: Vec::new(),
    }
}

fn handle_of(summaries: &[PlayerSummary], id: &str) -> String {
    summaries
        .iter()
        .find(|s| s.player_id.eq_ignore_ascii_case(id))
        .map(|s| s.handle.clone())
        .unwrap_or_default()
}

fn view_of(row: &ViewRow, summaries: &[PlayerSummary]) -> PartyView {
    PartyView {
        party_id: row.party_id.clone(),
        revision: row.revision,
        leader_id: row.leader_id.clone(),
        members: row
            .member_ids
            .iter()
            .zip(row.readies.iter())
            .map(|(id, ready)| MemberView {
                player_id: id.clone(),
                handle: handle_of(summaries, id),
                ready: ready.clone(),
            })
            .collect(),
    }
}

/// What a committed stamping op owes after commit: the party whose members get a
/// `party.state` push, and the removal (if any) whose players get a "no party" view.
struct Fanout {
    party_id: Option<String>,
    removal: Option<Removal>,
}

pub struct Service {
    pub(crate) store: Store,
    push: Arc<Push>,
    lease_secs: i64,
    /// Resolved in `init` (phase 2); in a split process a `remote::Stub` swaps an
    /// edge-backed client under the same key.
    pub(crate) directory: OnceLock<Arc<dyn Directory>>,
}

impl Service {
    pub fn new(pool: sqlx::PgPool, push: Arc<Push>, lease_secs: i64) -> Service {
        Service {
            store: Store { pool },
            push,
            lease_secs,
            directory: OnceLock::new(),
        }
    }

    fn directory(&self) -> &Arc<dyn Directory> {
        self.directory
            .get()
            .expect("party.init must resolve the accounts directory before any op")
    }

    fn caller(identity: &Identity) -> Result<String, Error> {
        let id = identity
            .player_id()
            .ok_or_else(|| Error::invalid("missing player identity"))?;
        if !is_uuid_text(id) {
            return Err(Error::invalid("player identity is not a valid uuid"));
        }
        Ok(id.to_ascii_lowercase())
    }

    /// Opens a transaction holding the lock of the caller's party, re-read under that lock;
    /// `None` when the caller is in no party. Under the lock the caller's membership of that
    /// party cannot change, since every removal from it takes the same lock.
    async fn begin_locked(
        &self,
        me: &str,
    ) -> Result<Option<(Transaction<'static, Postgres>, String)>, Error> {
        for _ in 0..MEMBERSHIP_ATTEMPTS {
            let mut tx = self.store.pool.begin().await.map_err(internal)?;
            let Some(party_id) = self.store.party_of_tx(&mut tx, me).await.map_err(internal)?
            else {
                tx.rollback().await.map_err(internal)?;
                return Ok(None);
            };
            self.store
                .lock_party_tx(&mut tx, &party_id)
                .await
                .map_err(internal)?;
            let again = self.store.party_of_tx(&mut tx, me).await.map_err(internal)?;
            if again.as_deref() == Some(party_id.as_str()) {
                return Ok(Some((tx, party_id)));
            }
            tx.rollback().await.map_err(internal)?;
        }
        Err(Error::conflict("party membership changed concurrently; retry"))
    }

    /// The invite target's id. Every directory error is `Unavailable`; a miss is `NotFound`.
    pub(crate) async fn resolve_target(&self, handle: String) -> Result<String, Error> {
        let target = self
            .directory()
            .find_by_handle(handle)
            .await
            .map_err(directory_unavailable)?
            .ok_or_else(|| Error::not_found(NO_SUCH_PLAYER))?;
        Ok(target.player_id.to_ascii_lowercase())
    }

    /// ONE batched lookup, after commit and outside any transaction. A failure answers no
    /// summaries: the mutation already committed, so the handles are empty, never an error.
    async fn summaries(&self, ids: Vec<String>) -> Vec<PlayerSummary> {
        if ids.is_empty() {
            return Vec::new();
        }
        match self.directory().players_by_id(ids).await {
            Ok(summaries) => summaries,
            Err(e) => {
                tracing::warn!(reason = %e.msg, "party: handle lookup failed; answering empty handles");
                Vec::new()
            }
        }
    }

    fn send(&self, player_id: &str, topic: &str, payload: &impl serde::Serialize) {
        let bytes = match serde_json::to_vec(payload) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(reason = %e, topic, "party: push payload not encoded");
                return;
            }
        };
        let target = Target::Player(player_id.to_string());
        if let Err(e) = self.push.send(&target, &Message::new(topic, bytes)) {
            tracing::debug!(reason = %e, topic, "party: push not delivered");
        }
    }

    fn push_state(&self, party: Option<&PartyView>, removal: Option<&Removal>) {
        if let Some(view) = party {
            for member in &view.members {
                self.send(&member.player_id, PUSH_STATE_TOPIC, view);
            }
        }
        if let Some(removal) = removal {
            let gone = no_party(removal.revision);
            for player_id in &removal.removed {
                self.send(player_id, PUSH_STATE_TOPIC, &gone);
            }
        }
    }

    /// After commit: the party's snapshot for its members' push, with handles from ONE
    /// directory call, then the pushes.
    pub(crate) async fn publish(&self, party_id: Option<&str>, removal: Option<&Removal>) {
        let row = match party_id {
            Some(pid) if !removal.is_some_and(|r| r.dissolved) => {
                match self.store.party_view(pid).await {
                    Ok(row) => row,
                    Err(e) => {
                        tracing::warn!(error = %e, "party: snapshot for push not loaded");
                        None
                    }
                }
            }
            _ => None,
        };
        let summaries = match &row {
            Some(row) => self.summaries(row.member_ids.clone()).await,
            None => Vec::new(),
        };
        let view = row.as_ref().map(|row| view_of(row, &summaries));
        self.push_state(view.as_ref(), removal);
    }

    /// The caller's response view and the members' push, from at most two single-statement
    /// snapshots and ONE directory call.
    async fn respond(&self, me: &str, fan: Fanout) -> Result<PartyView, Error> {
        let mine = self.store.player_view(me).await.map_err(internal)?;
        let dissolved = fan.removal.as_ref().is_some_and(|r| r.dissolved);
        let party = match fan.party_id.as_deref() {
            Some(pid) if pid == mine.party_id => Some(mine.clone()),
            Some(pid) if !dissolved => match self.store.party_view(pid).await {
                Ok(row) => row,
                Err(e) => {
                    tracing::warn!(error = %e, "party: snapshot for push not loaded");
                    None
                }
            },
            _ => None,
        };
        let mut ids = mine.member_ids.clone();
        if let Some(row) = &party {
            for id in &row.member_ids {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        let summaries = self.summaries(ids).await;
        let party_view = party.as_ref().map(|row| view_of(row, &summaries));
        self.push_state(party_view.as_ref(), fan.removal.as_ref());
        Ok(view_of(&mine, &summaries))
    }

    /// Applies the leave rule for a deleted player on the delivery connection: stamps the
    /// party, drops every invite to or from the player and the player's revision row. No
    /// push — this runs inside the delivery transaction.
    pub(crate) async fn purge_player_tx(
        &self,
        conn: &mut PgConnection,
        player_id: &str,
    ) -> Result<(), bus::Error> {
        if !is_uuid_text(player_id) {
            tracing::warn!(player_id, "party: player.deleted names no uuid; nothing to purge");
            return Ok(());
        }
        let player_id = player_id.to_ascii_lowercase();
        let db = bus::Error::transport;
        if let Some(party_id) = self.store.party_of_tx(conn, &player_id).await.map_err(db)? {
            self.store
                .lock_party_tx(conn, &party_id)
                .await
                .map_err(db)?;
            if self
                .store
                .remove_member_tx(conn, &party_id, &player_id)
                .await
                .map_err(db)?
                .is_none()
            {
                return Err(bus::Error::transport(std::io::Error::other(
                    "party: deleted player's membership changed concurrently; redeliver",
                )));
            }
        }
        self.store
            .delete_invites_involving_tx(conn, &player_id)
            .await
            .map_err(db)?;
        self.store
            .delete_player_revision_tx(conn, &player_id)
            .await
            .map_err(db)?;
        Ok(())
    }

    /// One sweep step: removes the party's expired members under its lock and publishes.
    /// `false` when a re-check under the lock found none.
    pub(crate) async fn sweep_party(&self, party_id: &str) -> Result<bool, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        self.store.lock_party_tx(&mut tx, party_id).await?;
        let Some(removal) = self.store.remove_expired_tx(&mut tx, party_id).await? else {
            tx.rollback().await?;
            return Ok(false);
        };
        tx.commit().await?;
        self.publish(Some(party_id), Some(&removal)).await;
        Ok(true)
    }

    async fn leader_locked(&self, me: &str) -> Result<(Transaction<'static, Postgres>, String), Error> {
        let Some((mut tx, party_id)) = self.begin_locked(me).await? else {
            return Err(Error::not_found(NOT_FOUND));
        };
        let led = self
            .store
            .led_party_tx(&mut tx, me)
            .await
            .map_err(internal)?;
        if led.as_deref() != Some(party_id.as_str()) {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(NOT_FOUND));
        }
        Ok((tx, party_id))
    }
}

#[async_trait]
impl Player for Service {
    async fn create(&self, identity: Identity) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        let (party_id, _) = self
            .store
            .create_tx(&mut tx, &me, self.lease_secs)
            .await
            .map_err(write_error)?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: None,
            },
        )
        .await
    }

    async fn get(&self, identity: Identity) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        self.store
            .renew(&me, self.lease_secs)
            .await
            .map_err(internal)?;
        let row = self.store.player_view(&me).await.map_err(internal)?;
        let summaries = self.summaries(row.member_ids.clone()).await;
        Ok(view_of(&row, &summaries))
    }

    async fn invite(&self, identity: Identity, target_handle: String) -> Result<InviteView, Error> {
        let me = Service::caller(&identity)?;
        if target_handle.len() > MAX_HANDLE_BYTES {
            return Err(Error::invalid(format!(
                "handle exceeds {MAX_HANDLE_BYTES} bytes"
            )));
        }
        // Pre-checked before the directory call so a caller who may not invite cannot use
        // this op as a handle-existence probe; re-made under the party lock below.
        let party_id = self
            .store
            .led_party(&me)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(NOT_FOUND))?;
        let target = self.resolve_target(target_handle).await?;
        if target == me {
            return Err(Error::invalid("cannot invite yourself"));
        }

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_party_tx(&mut tx, &party_id)
            .await
            .map_err(internal)?;
        let led = self
            .store
            .led_party_tx(&mut tx, &me)
            .await
            .map_err(internal)?;
        if led.as_deref() != Some(party_id.as_str()) {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(NOT_FOUND));
        }
        if self
            .store
            .is_member_tx(&mut tx, &party_id, &target)
            .await
            .map_err(internal)?
        {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict("player is already in this party"));
        }
        self.store
            .renew_tx(&mut tx, &me, self.lease_secs)
            .await
            .map_err(internal)?;
        let row = self
            .store
            .upsert_invite_tx(&mut tx, &party_id, &me, &target, INVITE_TTL_SECS)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;

        let summaries = self.summaries(vec![row.inviter_id.clone()]).await;
        let view = InviteView {
            invite_id: row.invite_id,
            party_id: row.party_id,
            inviter_handle: handle_of(&summaries, &row.inviter_id),
            expires_at: row.expires_at,
        };
        self.send(&target, PUSH_INVITE_TOPIC, &view);
        Ok(view)
    }

    async fn invites(&self, identity: Identity) -> Result<InviteList, Error> {
        let me = Service::caller(&identity)?;
        self.store
            .renew(&me, self.lease_secs)
            .await
            .map_err(internal)?;
        let rows: Vec<InviteRow> = self.store.invites_of(&me).await.map_err(internal)?;
        let mut ids: Vec<String> = Vec::with_capacity(rows.len());
        for row in &rows {
            if !ids.contains(&row.inviter_id) {
                ids.push(row.inviter_id.clone());
            }
        }
        let summaries = self.summaries(ids).await;
        Ok(InviteList {
            invites: rows
                .into_iter()
                .map(|row| InviteView {
                    inviter_handle: handle_of(&summaries, &row.inviter_id),
                    invite_id: row.invite_id,
                    party_id: row.party_id,
                    expires_at: row.expires_at,
                })
                .collect(),
        })
    }

    async fn accept(&self, identity: Identity, invite_id: String) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let invite_id = uuid_arg("invite_id", &invite_id)?;
        let party_id = self
            .store
            .pending_invite_party(&invite_id, &me)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::not_found(INVITE_NOT_FOUND))?;

        let mut tx = self.store.pool.begin().await.map_err(internal)?;
        self.store
            .lock_party_tx(&mut tx, &party_id)
            .await
            .map_err(internal)?;
        if !self
            .store
            .consume_invite_tx(&mut tx, &invite_id, &me, &party_id)
            .await
            .map_err(internal)?
        {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(INVITE_NOT_FOUND));
        }
        let count = self
            .store
            .count_members_tx(&mut tx, &party_id)
            .await
            .map_err(internal)?;
        if count >= MAX_MEMBERS {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::conflict(format!("party is full ({MAX_MEMBERS} members)")));
        }
        self.store
            .join_tx(&mut tx, &party_id, &me, self.lease_secs)
            .await
            .map_err(write_error)?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: None,
            },
        )
        .await
    }

    async fn decline(&self, identity: Identity, invite_id: String) -> Result<(), Error> {
        let me = Service::caller(&identity)?;
        let invite_id = uuid_arg("invite_id", &invite_id)?;
        self.store
            .renew(&me, self.lease_secs)
            .await
            .map_err(internal)?;
        if !self.store.decline(&invite_id, &me).await.map_err(internal)? {
            return Err(Error::not_found(INVITE_NOT_FOUND));
        }
        Ok(())
    }

    async fn leave(&self, identity: Identity) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let Some((mut tx, party_id)) = self.begin_locked(&me).await? else {
            let row = self.store.player_view(&me).await.map_err(internal)?;
            return Ok(view_of(&row, &[]));
        };
        let removal = self
            .store
            .remove_member_tx(&mut tx, &party_id, &me)
            .await
            .map_err(internal)?
            .ok_or_else(|| internal("membership vanished while the party was locked"))?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: Some(removal),
            },
        )
        .await
    }

    async fn kick(&self, identity: Identity, player_id: String) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let target = uuid_arg("player_id", &player_id)?;
        if target == me {
            return Err(Error::invalid("cannot kick yourself; use leave"));
        }
        let (mut tx, party_id) = self.leader_locked(&me).await?;
        let Some(removal) = self
            .store
            .remove_member_tx(&mut tx, &party_id, &target)
            .await
            .map_err(internal)?
        else {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(NOT_FOUND));
        };
        self.store
            .renew_tx(&mut tx, &me, self.lease_secs)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: Some(removal),
            },
        )
        .await
    }

    async fn promote(&self, identity: Identity, player_id: String) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let target = uuid_arg("player_id", &player_id)?;
        if target == me {
            return Err(Error::invalid("cannot promote yourself"));
        }
        let (mut tx, party_id) = self.leader_locked(&me).await?;
        if !self
            .store
            .is_member_tx(&mut tx, &party_id, &target)
            .await
            .map_err(internal)?
        {
            tx.rollback().await.map_err(internal)?;
            return Err(Error::not_found(NOT_FOUND));
        }
        self.store
            .renew_tx(&mut tx, &me, self.lease_secs)
            .await
            .map_err(internal)?;
        self.store
            .stamp_tx(&mut tx, &party_id, Some(&target))
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: None,
            },
        )
        .await
    }

    async fn set_ready(&self, identity: Identity, state: String) -> Result<PartyView, Error> {
        let me = Service::caller(&identity)?;
        let state = validate_state(&state)?;
        let Some((mut tx, party_id)) = self.begin_locked(&me).await? else {
            return Err(Error::not_found(NOT_FOUND));
        };
        if !self
            .store
            .set_ready_tx(&mut tx, &party_id, &me, state, self.lease_secs)
            .await
            .map_err(internal)?
        {
            tx.rollback().await.map_err(internal)?;
            return Err(internal("membership vanished while the party was locked"));
        }
        self.store
            .stamp_tx(&mut tx, &party_id, None)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        self.respond(
            &me,
            Fanout {
                party_id: Some(party_id),
                removal: None,
            },
        )
        .await
    }

    async fn heartbeat(&self, identity: Identity) -> Result<HeartbeatView, Error> {
        let me = Service::caller(&identity)?;
        self.store
            .renew(&me, self.lease_secs)
            .await
            .map_err(internal)?;
        let row = self.store.player_view(&me).await.map_err(internal)?;
        Ok(HeartbeatView {
            lease_until: row.lease_until,
            revision: row.revision,
        })
    }
}

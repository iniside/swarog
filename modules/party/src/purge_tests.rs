//! `party.player-deleted.v1`: the durable purge reaction, driven through the REAL plane
//! (`TestTransport::deliver_all` runs the registered subscription), not by calling the handler.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use accountsapi::Directory;
use asyncevents::testing::TestTransport;
use bus::AnyTx;
use lifecycle::{Context, Module};
use partyapi::Player as _;
use sqlx::PgPool;

use crate::tests::*;
use crate::Party;

/// The real module over the fixture's pool, sink and directory, with the plane's test
/// transport carrying `on_tx`. The subscription is registered (reconciled) before any event
/// is emitted, so `AfterRegistration` cannot skip it.
async fn wired(fx: &Fixture) -> (Context, Party, TestTransport) {
    let transport = asyncevents::testing::transport(fx.pool.clone());
    let ctx = Context::with_db_and_transport(fx.pool.clone(), transport.handle());
    ctx.push().install(fx.sink.clone());
    ctx.registry().provide::<dyn Directory>(
        registry::key("accounts", "directory"),
        fx.dir.clone() as Arc<dyn Directory>,
    );
    let module = Party::new();
    module.register(&ctx).unwrap();
    module.init(&ctx).unwrap();
    transport.deliver_all().await.unwrap();
    (ctx, module, transport)
}

async fn emit_deleted(ctx: &Context, pool: &PgPool, player: &str) {
    let mut tx = pool.begin().await.unwrap();
    ctx.bus()
        .emit_tx(
            AnyTx::new(&mut *tx),
            &accountsevents::PLAYER_DELETED,
            &accountsevents::PlayerDeleted {
                player_id: player.to_string(),
                handle: "Gone#0000".to_string(),
            },
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Delivery is frontier-bounded, so the round trip polls the OBSERVED effect with a hang guard.
async fn deliver_until<F, Fut>(transport: &TestTransport, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        transport.deliver_all().await.unwrap();
        if done().await {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the purge was never delivered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Once-wrong branch: the purge reaction end to end. Deleting a LEADER removes the
/// membership (leadership handed on and the party stamped), drops invites both TO and FROM
/// the player, deletes the player's `player_revisions` row (the removal stamped it first, so
/// only an explicit DELETE leaves no residue), and pushes NOTHING (it runs in the delivery tx).
#[tokio::test(flavor = "multi_thread")]
async fn player_deleted_removes_membership_and_invites_stamps_the_party_and_leaves_no_revision_row() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.unset();
    Fixture::new(&pool).await.run(|fx| async move {
        let (ctx, _module, transport) = wired(&fx).await;
        let [doomed, other, rival, invitee] = fx.players();
        let party = create_party(&fx, &doomed).await;
        let before = join(&fx, &doomed, &other).await.revision;
        let from_doomed = invite_id(&fx, &doomed, &invitee).await;
        let rival_party = create_party(&fx, &rival).await;
        let to_doomed = invite_id(&fx, &rival, &doomed).await;
        assert!(player_rev(&fx.pool, &doomed).await.is_some());
        fx.sink.clear();

        emit_deleted(&ctx, &fx.pool, &doomed).await;
        deliver_until(&transport, || async { party_of(&fx.pool, &doomed).await.is_none() }).await;

        assert_eq!(members_of(&fx.pool, &party).await, vec![other.clone()]);
        let (revision, leader) = party_row(&fx.pool, &party).await.unwrap();
        assert_eq!(leader, other, "the deleted leader's party is handed on");
        assert!(revision > before, "the purge removal stamps the party");
        assert_eq!(player_rev(&fx.pool, &doomed).await, None, "no player_revisions residue");
        assert!(!invite_exists(&fx.pool, &from_doomed).await, "invites FROM the player are gone");
        assert!(!invite_exists(&fx.pool, &to_doomed).await, "invites TO the player are gone");
        assert_eq!(members_of(&fx.pool, &rival_party).await, vec![rival.clone()], "other parties are untouched");
        assert_eq!(fx.sink.total(), 0, "the purge stamps without pushing");
    }).await;
}

/// Once-wrong branch: the last member's deletion dissolves the party and its invites; a
/// player in NO party still loses their invites and revision row.
#[tokio::test(flavor = "multi_thread")]
async fn player_deleted_dissolves_a_sole_party_and_cleans_a_player_in_no_party() {
    let Some(pool) = test_pool().await else { return };
    let _db = DB_LOCK.lock().await;
    let _env = ENV_LOCK.lock().await;
    let guard = EnvGuard::take();
    guard.unset();
    Fixture::new(&pool).await.run(|fx| async move {
        let (ctx, _module, transport) = wired(&fx).await;
        let [solo, invitee, leaver, host] = fx.players();
        let party = create_party(&fx, &solo).await;
        let pending = invite_id(&fx, &solo, &invitee).await;

        create_party(&fx, &host).await;
        join(&fx, &host, &leaver).await;
        fx.svc.leave(who(&leaver)).await.unwrap();
        let invite_to_leaver = invite_id(&fx, &host, &leaver).await;
        assert!(player_rev(&fx.pool, &leaver).await.is_some());

        emit_deleted(&ctx, &fx.pool, &solo).await;
        emit_deleted(&ctx, &fx.pool, &leaver).await;
        deliver_until(&transport, || async {
            party_row(&fx.pool, &party).await.is_none() && player_rev(&fx.pool, &leaver).await.is_none()
        })
        .await;

        assert!(members_of(&fx.pool, &party).await.is_empty());
        assert!(!invite_exists(&fx.pool, &pending).await, "dissolution drops the party's invites");
        assert_eq!(player_rev(&fx.pool, &solo).await, None);
        assert!(!invite_exists(&fx.pool, &invite_to_leaver).await, "invites to a player in no party are dropped");
    }).await;
}

/// Once-wrong branch: the handler's guards — a `player_id` that is no uuid is a logged no-op
/// (not an error that would poison the subscription), and a redelivery of an already-applied
/// purge is idempotent.
#[tokio::test(flavor = "multi_thread")]
async fn purge_ignores_a_non_uuid_id_and_is_idempotent_on_redelivery() {
    let Some(pool) = test_pool().await else { return };
    Fixture::new(&pool).await.run(|fx| async move {
        let [leader, member] = fx.players();
        let party = create_party(&fx, &leader).await;
        join(&fx, &leader, &member).await;

        let mut conn = fx.pool.acquire().await.unwrap();
        fx.svc.purge_player_tx(&mut conn, "not-a-uuid").await.expect("a non-uuid id is a no-op");
        fx.svc.purge_player_tx(&mut conn, &member).await.unwrap();
        fx.svc.purge_player_tx(&mut conn, &member).await.expect("a redelivery is a no-op");
        assert_eq!(members_of(&fx.pool, &party).await, vec![leader.clone()]);
        assert_eq!(player_rev(&fx.pool, &member).await, None);
    }).await;
}

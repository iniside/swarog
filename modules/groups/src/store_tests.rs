//! Live-Postgres store-level tests: the `memberships_role_check` constraint
//! (`SCHEMA_DDL`), exercised as the EQUIVALENCE it is declared to be — both the
//! forward direction (a pending row may not carry a role) and the reverse (a member
//! row may not carry an empty one) — not merely one implication. A malformed insert
//! attempted through raw SQL, never through `Store`, because the constraint is the
//! thing under test: `Store::insert_membership_tx` would happily hand the database
//! whatever `state`/`role` pair its caller supplies, and the schema is the one
//! authority that must refuse the illegal combinations.

use crate::tests::{ensure_schema, test_pool, unique_uuid};

/// The 23514 SQLSTATE ("check_violation") — the one signal a constraint rejection
/// raises.
fn is_check_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23514"))
}

async fn try_insert(pool: &sqlx::PgPool, group_id: &str, state: &str, role: &str) -> Result<(), sqlx::Error> {
    let player_id = unique_uuid(pool).await;
    sqlx::query(
        "INSERT INTO groups.memberships (group_id, player_id, state, role) \
         VALUES ($1::uuid, $2::uuid, $3, $4)",
    )
    .bind(group_id)
    .bind(&player_id)
    .bind(state)
    .bind(role)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The FORWARD half: a `requested` row (a pending state) may not carry a role.
/// `groupsapi::STATE_REQUESTED`'s only legal role is the empty string.
#[tokio::test]
async fn memberships_role_check_rejects_a_pending_row_carrying_a_role() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let group_id = unique_uuid(&pool).await;

    let err = try_insert(&pool, &group_id, groupsapi::STATE_REQUESTED, groupsapi::ROLE_ADMIN)
        .await
        .expect_err("a pending row with a non-empty role must violate the constraint");
    assert!(
        is_check_violation(&err),
        "expected a 23514 check violation, got: {err}"
    );

    // Nothing landed — the illegal row must not be visible under any spelling.
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(&group_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

/// The REVERSE half of the SAME equivalence: a `member` row may not carry an EMPTY
/// role. Proving only the forward half would leave `state='member' AND role=''`
/// unchecked — exactly the shape that would make `role_of` answer `""` for a real
/// member (see `service::wire_state`'s neighbouring invariant).
#[tokio::test]
async fn memberships_role_check_rejects_a_member_row_with_no_role() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let group_id = unique_uuid(&pool).await;

    let err = try_insert(&pool, &group_id, groupsapi::STATE_MEMBER, "")
        .await
        .expect_err("a member row with an empty role must violate the constraint");
    assert!(
        is_check_violation(&err),
        "expected a 23514 check violation, got: {err}"
    );

    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(&group_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

/// The admitted combinations, proven alongside the rejections above so the equivalence
/// is shown both ways: a pending row with NO role and a member row WITH a role both
/// commit cleanly.
#[tokio::test]
async fn memberships_role_check_admits_the_legal_combinations() {
    let Some(pool) = test_pool().await else { return };
    ensure_schema(&pool).await;
    let group_id = unique_uuid(&pool).await;

    try_insert(&pool, &group_id, groupsapi::STATE_INVITED, "")
        .await
        .expect("a pending row with no role is legal");
    try_insert(&pool, &group_id, groupsapi::STATE_MEMBER, groupsapi::ROLE_MEMBER)
        .await
        .expect("a member row with a role is legal");

    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(&group_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 2);

    let _ = sqlx::query("DELETE FROM groups.memberships WHERE group_id = $1::uuid")
        .bind(&group_id)
        .execute(&pool)
        .await;
}

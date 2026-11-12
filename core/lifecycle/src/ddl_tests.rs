use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::{Connection, PgConnection, PgPool};
use testdb::test_pool;

use super::*;

/// Every DB test takes `read()`; the one test that holds the GLOBAL
/// [`DDL_LOCK_KEY`] on a raw connection takes `write()`, so no other `Ddl` run
/// in this process queues behind that hold. A tokio lock because the guard is
/// held across awaits, and it cannot poison.
static DDL_KEY_GATE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

static SCHEMA_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Hang-guard only: generous headroom, never the proof of anything.
const HANG_GUARD: Duration = Duration::from_secs(60);

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

async fn bounded<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG_GUARD, fut)
        .await
        .expect("hang guard: the operation did not finish")
}

/// A schema unique to this test and process, dropped on entry (stale leftovers)
/// and by [`Schema::cleanup`].
struct Schema {
    name: String,
}

impl Schema {
    async fn fresh(pool: &PgPool, tag: &str) -> Schema {
        let name = format!(
            "ddl_t_{tag}_{}_{}",
            std::process::id(),
            SCHEMA_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let schema = Schema { name };
        schema.cleanup(pool).await;
        schema
    }

    /// Callers must have ended any open transaction touching the schema first:
    /// `DROP SCHEMA` would otherwise wait on its locks.
    async fn cleanup(&self, pool: &PgPool) {
        sqlx::query(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name))
            .execute(pool)
            .await
            .expect("drop test schema");
    }

    /// The module-shaped DDL under test: a table, a trigger function, an index
    /// and a trigger.
    fn spec(&self, pool: &PgPool) -> Ddl {
        let s = &self.name;
        Ddl::new(pool)
            .sql(leak(format!(
                "CREATE SCHEMA IF NOT EXISTS {s}; \
                 CREATE TABLE IF NOT EXISTS {s}.t \
                 (id bigint PRIMARY KEY, v bigint NOT NULL DEFAULT 0); \
                 CREATE OR REPLACE FUNCTION {s}.touch() RETURNS trigger \
                 LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;"
            )))
            .index(
                leak(format!("{s}.t_v_idx")),
                leak(format!("CREATE INDEX t_v_idx ON {s}.t (v)")),
            )
            .trigger(
                leak(format!("{s}.t")),
                "t_touch",
                leak(format!(
                    "CREATE TRIGGER t_touch BEFORE INSERT ON {s}.t \
                     FOR EACH ROW EXECUTE FUNCTION {s}.touch()"
                )),
            )
    }

    fn index(&self) -> String {
        format!("{}.t_v_idx", self.name)
    }

    fn table(&self) -> String {
        format!("{}.t", self.name)
    }

    async fn index_oid(&self, pool: &PgPool) -> Option<i64> {
        sqlx::query_scalar("SELECT to_regclass($1)::oid::int8")
            .bind(self.index())
            .fetch_one(pool)
            .await
            .expect("index oid")
    }

    async fn index_count(&self, pool: &PgPool) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = 't_v_idx'",
        )
        .bind(&self.name)
        .fetch_one(pool)
        .await
        .expect("index count")
    }

    async fn trigger_count(&self, pool: &PgPool) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM pg_trigger \
             WHERE tgrelid = $1::regclass AND tgname = 't_touch' AND NOT tgisinternal",
        )
        .bind(self.table())
        .fetch_one(pool)
        .await
        .expect("trigger count")
    }

    /// Opens a transaction with an uncommitted INSERT: it holds ROW EXCLUSIVE
    /// on the table until the returned transaction ends.
    async fn open_writer(&self, pool: &PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut tx = pool.begin().await.expect("begin writer tx");
        sqlx::query(&format!("INSERT INTO {} (id) VALUES (1)", self.table()))
            .execute(&mut *tx)
            .await
            .expect("uncommitted insert");
        tx
    }
}

fn has_lock_timeout_code(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        matches!(
            e.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::Database(db)) if db.code().as_deref() == Some("55P03")
        )
    })
}

/// The steady state takes no lock on module tables: with the index and trigger
/// present, a `Ddl` run finishes while another transaction holds ROW EXCLUSIVE
/// on the table. The raw `CREATE INDEX IF NOT EXISTS` form it replaces
/// (SHARE lock before the existence check) fails the same setup with `55P03`.
#[tokio::test]
async fn steady_state_run_takes_no_table_lock() {
    let _gate = DDL_KEY_GATE.read().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let schema = Schema::fresh(&pool, "steady").await;
    bounded(schema.spec(&pool).run()).await.unwrap();
    let oid_before = schema.index_oid(&pool).await.expect("index created");

    let writer = schema.open_writer(&pool).await;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE relation = $1::regclass \
         AND mode = 'RowExclusiveLock' AND granted",
    )
    .bind(schema.table())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(held >= 1, "the writer transaction must hold ROW EXCLUSIVE");

    // Positive control: the raw form is blocked by the same open writer.
    let mut raw = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '500ms'")
        .execute(&mut *raw)
        .await
        .unwrap();
    let raw_err = bounded(sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS t_v_idx ON {} (v)",
        schema.table()
    ))
    .execute(&mut *raw))
    .await
    .expect_err("raw CREATE INDEX IF NOT EXISTS must wait on the open writer");
    assert!(
        matches!(&raw_err, sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03")),
        "expected 55P03, got: {raw_err}"
    );
    drop(raw);

    // `writer` is still open here: the rerun succeeds despite its lock.
    bounded(schema.spec(&pool).run_with_lock_timeout("2s"))
        .await
        .expect("steady-state Ddl must not wait on the open writer");
    assert_eq!(schema.index_oid(&pool).await, Some(oid_before));
    assert_eq!(schema.trigger_count(&pool).await, 1);

    writer.rollback().await.unwrap();
    schema.cleanup(&pool).await;
}

/// A missing index and trigger are recreated by the next run.
#[tokio::test]
async fn missing_index_and_trigger_are_recreated() {
    let _gate = DDL_KEY_GATE.read().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let schema = Schema::fresh(&pool, "create").await;
    bounded(schema.spec(&pool).run()).await.unwrap();
    assert_eq!(schema.index_count(&pool).await, 1);
    assert_eq!(schema.trigger_count(&pool).await, 1);

    sqlx::raw_sql(&format!(
        "DROP INDEX {}; DROP TRIGGER t_touch ON {}",
        schema.index(),
        schema.table()
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(schema.index_count(&pool).await, 0);
    assert_eq!(schema.trigger_count(&pool).await, 0);

    bounded(schema.spec(&pool).run()).await.unwrap();
    assert_eq!(schema.index_count(&pool).await, 1);
    assert_eq!(schema.trigger_count(&pool).await, 1);

    schema.cleanup(&pool).await;
}

/// Parallel first creation of one spec on a fresh schema all succeed and leave
/// each object exactly once (the advisory lock serializes probe -> create).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_creation_all_succeed() {
    let _gate = DDL_KEY_GATE.read().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let schema = Schema::fresh(&pool, "race").await;

    let runs: Vec<_> = (0..8)
        .map(|_| tokio::spawn(schema.spec(&pool).run()))
        .collect();
    for run in runs {
        bounded(run).await.unwrap().unwrap();
    }
    assert_eq!(schema.index_count(&pool).await, 1);
    assert_eq!(schema.trigger_count(&pool).await, 1);

    schema.cleanup(&pool).await;
}

/// A locking `sql` step behind a conflicting open transaction fails with
/// `55P03` naming the step and the bound, instead of waiting.
#[tokio::test]
async fn sql_step_lock_wait_is_bounded_and_named() {
    let _gate = DDL_KEY_GATE.read().await;
    let Some(pool) = test_pool().await else {
        return;
    };
    let schema = Schema::fresh(&pool, "bound").await;
    bounded(schema.spec(&pool).run()).await.unwrap();

    let writer = schema.open_writer(&pool).await;
    let lock_sql = leak(format!(
        "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
        schema.table()
    ));
    let err = bounded(Ddl::new(&pool).sql(lock_sql).run_with_lock_timeout("300ms"))
        .await
        .expect_err("the lock wait must fail, not hang");
    let msg = format!("{err:#}");
    assert!(has_lock_timeout_code(&err), "no 55P03 in chain: {msg}");
    assert!(
        msg.contains("lock not acquired within 300ms"),
        "missing lock_timeout wording: {msg}"
    );
    assert!(
        msg.contains(&format!("at sql `{lock_sql}`")),
        "missing step label: {msg}"
    );

    writer.rollback().await.unwrap();
    schema.cleanup(&pool).await;
}

/// Holding [`DDL_LOCK_KEY`] makes the next run fail at the advisory-lock step,
/// and the run succeeds once the key is released.
#[tokio::test]
async fn advisory_lock_wait_is_bounded_and_named() {
    let _gate = DDL_KEY_GATE.write().await;
    let Some(pool) = test_pool().await else {
        return;
    };

    // A dedicated connection: dropping it on a panic closes the session and
    // releases the lock, which a pooled connection would not.
    let mut holder = PgConnection::connect(&testdb::dsn())
        .await
        .expect("holder connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(DDL_LOCK_KEY)
        .execute(&mut holder)
        .await
        .expect("hold DDL lock key");

    let result = bounded(
        Ddl::new(&pool)
            .sql("SELECT 1")
            .run_with_lock_timeout("300ms"),
    )
    .await;

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(DDL_LOCK_KEY)
        .execute(&mut holder)
        .await
        .expect("release DDL lock key");
    holder.close().await.unwrap();

    let err = result.expect_err("the advisory lock wait must fail, not hang");
    let msg = format!("{err:#}");
    assert!(has_lock_timeout_code(&err), "no 55P03 in chain: {msg}");
    assert!(
        msg.contains(&format!(
            "lock not acquired within 300ms at {ADVISORY_LOCK_STEP}"
        )),
        "missing advisory step label: {msg}"
    );

    bounded(Ddl::new(&pool).sql("SELECT 1").run_with_lock_timeout("2s"))
        .await
        .expect("the run succeeds once the key is free");
}

async fn lazy_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@localhost/unused")
        .expect("lazy pool")
}

#[tokio::test]
#[should_panic(expected = "must contain `INDEX A_IDX ON S.`")]
async fn index_name_not_in_create_sql_panics() {
    Ddl::new(&lazy_pool().await).index("s.a_idx", "CREATE INDEX b_idx ON s.t (v)");
}

#[tokio::test]
#[should_panic(expected = "must contain `INDEX A_IDX ON S.`")]
async fn index_if_not_exists_form_panics() {
    Ddl::new(&lazy_pool().await).index("s.a_idx", "CREATE INDEX IF NOT EXISTS a_idx ON s.t (v)");
}

#[tokio::test]
#[should_panic(expected = "must contain `INDEX A_IDX ON S.`")]
async fn index_schema_mismatch_panics() {
    Ddl::new(&lazy_pool().await).index("s.a_idx", "CREATE INDEX a_idx ON other.t (v)");
}

#[tokio::test]
#[should_panic(expected = "must be `<schema>.<index>`")]
async fn unqualified_index_name_panics() {
    Ddl::new(&lazy_pool().await).index("a_idx", "CREATE INDEX a_idx ON s.t (v)");
}

#[tokio::test]
async fn partial_unique_index_form_is_accepted() {
    let ddl = Ddl::new(&lazy_pool().await)
        .index(
            "s.u_idx",
            "CREATE UNIQUE INDEX u_idx ON s.t (v) WHERE v > 0",
        )
        .index(
            "s.m_idx",
            "create unique\n    index   m_idx\n  on s.t (v)\n WHERE v > 0",
        );
    assert_eq!(ddl.steps.len(), 2);
}

#[tokio::test]
#[should_panic(expected = "must contain `TRIGGER T_TOUCH`")]
async fn trigger_name_mismatch_panics() {
    Ddl::new(&lazy_pool().await).trigger(
        "s.t",
        "t_touch",
        "CREATE TRIGGER other BEFORE INSERT ON s.t FOR EACH ROW EXECUTE FUNCTION s.touch()",
    );
}

#[tokio::test]
#[should_panic(expected = "must contain `ON S.T`")]
async fn trigger_table_mismatch_panics() {
    Ddl::new(&lazy_pool().await).trigger(
        "s.t",
        "t_touch",
        "CREATE TRIGGER t_touch BEFORE INSERT ON s.other FOR EACH ROW EXECUTE FUNCTION s.touch()",
    );
}

#[tokio::test]
async fn matching_trigger_is_accepted() {
    let ddl = Ddl::new(&lazy_pool().await).trigger(
        "s.t",
        "t_touch",
        "create trigger t_touch\n  before insert on s.t\n  for each row execute function s.touch()",
    );
    assert_eq!(ddl.steps.len(), 1);
}

#[test]
fn lock_timeout_message_names_step_and_bound() {
    let msg = lock_timeout_message("index s.t_v_idx", "60s");
    assert!(
        msg.starts_with("module DDL: lock not acquired within 60s at index s.t_v_idx"),
        "{msg}"
    );
    assert!(msg.contains("pg_blocking_pids"), "{msg}");
}

#[test]
fn sql_label_collapses_whitespace_without_truncating_short_text() {
    let label = Step::Sql("CREATE   TABLE\n\t x\n (id int)").label();
    assert_eq!(label, "sql `CREATE TABLE x (id int)`");
}

#[test]
fn sql_label_whitespace_runs_do_not_count_toward_the_excerpt() {
    let text = leak(format!("a{}b", " ".repeat(500)));
    assert_eq!(Step::Sql(text).label(), "sql `a b`");
}

#[test]
fn sql_label_of_exactly_the_excerpt_length_is_not_marked_truncated() {
    let text = leak(format!("{}éé", "a".repeat(SQL_EXCERPT_CHARS - 2)));
    assert_eq!(text.chars().count(), SQL_EXCERPT_CHARS);
    assert_eq!(Step::Sql(text).label(), format!("sql `{text}`"));
}

#[test]
fn sql_label_truncates_on_a_char_boundary_with_a_multibyte_char_at_the_edge() {
    let text = leak(format!("{}éétail", "a".repeat(SQL_EXCERPT_CHARS - 2)));
    let kept = format!("{}éé", "a".repeat(SQL_EXCERPT_CHARS - 2));
    assert_eq!(Step::Sql(text).label(), format!("sql `{kept}…`"));

    let text = leak(format!("{}é", "a".repeat(SQL_EXCERPT_CHARS)));
    let kept = "a".repeat(SQL_EXCERPT_CHARS);
    assert_eq!(Step::Sql(text).label(), format!("sql `{kept}…`"));
}

#[test]
fn index_and_trigger_labels_name_the_object() {
    let index = Step::Index {
        qualified_name: "s.t_v_idx",
        create_sql: "CREATE INDEX t_v_idx ON s.t (v)",
    };
    assert_eq!(index.label(), "index s.t_v_idx");
    let trigger = Step::Trigger {
        table: "s.t",
        name: "t_touch",
        create_sql: "CREATE TRIGGER t_touch BEFORE INSERT ON s.t FOR EACH ROW EXECUTE FUNCTION s.touch()",
    };
    assert_eq!(trigger.label(), "trigger t_touch on s.t");
}

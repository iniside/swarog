use anyhow::Context as _;
use sqlx::PgPool;

use crate::app::MODULE_MIGRATE_LOCK_TIMEOUT;

/// Transaction-scoped advisory-lock key serializing every [`Ddl::run`] against
/// one shared DB, so probe→create and parallel first creation never race. It
/// differs from `MODULE_MIGRATE_LOCK_KEY` because `App::migrate` holds that one
/// as a SESSION lock on a separate connection while module `migrate`s run: the
/// same key here would always fail with `55P03` once the `lock_timeout` expires.
/// ASCII `"lifeddl!"` as a positive i64.
pub(crate) const DDL_LOCK_KEY: i64 = 0x6C69_6665_6464_6C21;

/// Step label a `55P03` on the advisory-lock acquisition carries.
pub(crate) const ADVISORY_LOCK_STEP: &str = "advisory DDL lock";

/// How many characters of an `sql` step's whitespace-collapsed text its label
/// quotes.
const SQL_EXCERPT_CHARS: usize = 120;

/// A module's boot-time DDL, run create-only in ONE transaction under
/// `lock_timeout` and [`DDL_LOCK_KEY`].
///
/// [`Ddl::index`] and [`Ddl::trigger`] probe the catalog and create only what is
/// missing, so with the object present they take no lock on module tables.
/// They never alter an existing object: a changed index or trigger
/// definition follows the wipe policy (`.agents/shared/core-rules.md`
/// **No Dual-Write, No Topology Branch, Wipe**). Every lock wait in the run —
/// including an `INSERT … ON CONFLICT DO NOTHING` seed waiting on an in-flight
/// writer of the same row — is bounded by the `lock_timeout` and fails with
/// SQLSTATE `55P03` naming the step.
///
/// The name/text tripwires in [`Ddl::index`] and [`Ddl::trigger`] panic at the
/// builder call, before any SQL runs.
pub struct Ddl {
    pool: PgPool,
    steps: Vec<Step>,
}

pub(crate) enum Step {
    Sql(&'static str),
    Index {
        qualified_name: &'static str,
        create_sql: &'static str,
    },
    Trigger {
        table: &'static str,
        name: &'static str,
        create_sql: &'static str,
    },
}

impl Step {
    /// The step as a `55P03` or failure context names it.
    pub(crate) fn label(&self) -> String {
        match self {
            Step::Sql(sql) => {
                let collapsed = collapse_whitespace(sql);
                let mut excerpt: String = collapsed.chars().take(SQL_EXCERPT_CHARS).collect();
                if excerpt.len() < collapsed.len() {
                    excerpt.push('…');
                }
                format!("sql `{excerpt}`")
            }
            Step::Index { qualified_name, .. } => format!("index {qualified_name}"),
            Step::Trigger { table, name, .. } => format!("trigger {name} on {table}"),
        }
    }
}

/// The context for a `55P03` raised while running `step` (a [`Step::label`] or
/// [`ADVISORY_LOCK_STEP`]).
pub(crate) fn lock_timeout_message(step: &str, lock_timeout: &str) -> String {
    format!(
        "module DDL: lock not acquired within {lock_timeout} at {step} — an open \
         transaction holds a conflicting lock; see pg_locks / pg_blocking_pids"
    )
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_lock_timeout(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03"))
}

fn step_error(err: sqlx::Error, step: &str, lock_timeout: &str) -> anyhow::Error {
    if is_lock_timeout(&err) {
        anyhow::Error::new(err).context(lock_timeout_message(step, lock_timeout))
    } else {
        anyhow::Error::new(err).context(format!("module DDL failed at {step}"))
    }
}

impl Ddl {
    pub fn new(pool: &PgPool) -> Self {
        Ddl {
            pool: pool.clone(),
            steps: Vec::new(),
        }
    }

    /// Idempotent statements: `CREATE SCHEMA/TABLE IF NOT EXISTS`,
    /// `CREATE OR REPLACE FUNCTION`, `ON CONFLICT DO NOTHING` seeds.
    /// Multi-statement text is allowed.
    pub fn sql(mut self, sql: &'static str) -> Self {
        self.steps.push(Step::Sql(sql));
        self
    }

    /// Runs `create_sql` only when `to_regclass(qualified_name)` is NULL.
    ///
    /// # Panics
    /// When `qualified_name` is not `<schema>.<index>` or `create_sql` does not
    /// contain `INDEX <index> ON <schema>.` (ASCII case-insensitive, whitespace
    /// collapsed): a mismatch would skip the creation forever.
    pub fn index(mut self, qualified_name: &'static str, create_sql: &'static str) -> Self {
        let (schema, index) = qualified_name
            .split_once('.')
            .filter(|(s, i)| !s.is_empty() && !i.is_empty() && !i.contains('.'))
            .unwrap_or_else(|| {
                panic!("Ddl::index: {qualified_name:?} must be `<schema>.<index>`")
            });
        let needle = format!("INDEX {index} ON {schema}.").to_ascii_uppercase();
        assert!(
            collapse_whitespace(create_sql).to_ascii_uppercase().contains(&needle),
            "Ddl::index: create_sql for {qualified_name} must contain `{needle}`: {create_sql}"
        );
        self.steps.push(Step::Index {
            qualified_name,
            create_sql,
        });
        self
    }

    /// Runs `create_sql` only when `table` has no non-internal trigger `name`.
    ///
    /// # Panics
    /// When `create_sql` does not contain `TRIGGER <name> ` and ` ON <table> `
    /// (ASCII case-insensitive, whitespace collapsed): a mismatch would skip the
    /// creation forever.
    pub fn trigger(
        mut self,
        table: &'static str,
        name: &'static str,
        create_sql: &'static str,
    ) -> Self {
        let text = format!(" {} ", collapse_whitespace(create_sql)).to_ascii_uppercase();
        for needle in [format!(" TRIGGER {name} "), format!(" ON {table} ")] {
            let needle = needle.to_ascii_uppercase();
            assert!(
                text.contains(&needle),
                "Ddl::trigger: create_sql for {name} on {table} must contain `{}`: {create_sql}",
                needle.trim()
            );
        }
        self.steps.push(Step::Trigger {
            table,
            name,
            create_sql,
        });
        self
    }

    /// Runs the steps under [`MODULE_MIGRATE_LOCK_TIMEOUT`].
    pub async fn run(self) -> anyhow::Result<()> {
        self.run_with_lock_timeout(MODULE_MIGRATE_LOCK_TIMEOUT).await
    }

    /// [`Ddl::run`] with the `lock_timeout` GUC value as a parameter. `t` is a
    /// trusted crate-internal/test string spliced into `SET LOCAL`, which takes
    /// no bind parameter.
    pub(crate) async fn run_with_lock_timeout(self, t: &str) -> anyhow::Result<()> {
        assert!(
            !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()),
            "Ddl: lock_timeout {t:?} must be ASCII alphanumeric (digits + unit, e.g. \"60s\")"
        );

        // A failed step drops `tx`, whose Drop rolls back before the connection
        // returns to the pool.
        let mut tx = self.pool.begin().await.context("module DDL: begin")?;
        sqlx::query(&format!("SET LOCAL lock_timeout = '{t}'"))
            .execute(&mut *tx)
            .await
            .context("module DDL: set lock_timeout")?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(DDL_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(|e| step_error(e, ADVISORY_LOCK_STEP, t))?;

        for step in &self.steps {
            let create_sql = match step {
                Step::Sql(sql) => Some(*sql),
                Step::Index {
                    qualified_name,
                    create_sql,
                } => {
                    let missing: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NULL")
                        .bind(*qualified_name)
                        .fetch_one(&mut *tx)
                        .await
                        .map_err(|e| step_error(e, &step.label(), t))?;
                    missing.then_some(*create_sql)
                }
                Step::Trigger {
                    table,
                    name,
                    create_sql,
                } => {
                    let missing: bool = sqlx::query_scalar(
                        "SELECT NOT EXISTS (SELECT 1 FROM pg_trigger \
                         WHERE tgrelid = $1::regclass AND tgname = $2 AND NOT tgisinternal)",
                    )
                    .bind(*table)
                    .bind(*name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| step_error(e, &step.label(), t))?;
                    missing.then_some(*create_sql)
                }
            };
            if let Some(sql) = create_sql {
                // A bare `&str` with no arguments runs over the simple-query
                // protocol, so multi-statement text works as with `raw_sql`
                // (sqlx-postgres-0.8.6 `connection/executor.rs:280-282`), and
                // unlike `raw_sql(..).execute(&mut PgConnection)` its future is
                // `Send`.
                sqlx::Executor::execute(&mut *tx, sql)
                    .await
                    .map_err(|e| step_error(e, &step.label(), t))?;
            }
        }

        tx.commit().await.context("module DDL: commit")
    }
}

/// Module `migrate` futures cross `#[async_trait]`, which requires `Send`.
#[allow(dead_code)]
fn run_future_is_send(ddl: Ddl) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
    ddl.run()
}

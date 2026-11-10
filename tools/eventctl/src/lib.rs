//! `eventctl` connection settings. The operations themselves are
//! `asyncevents::admin`; `main.rs` is a thin arg dispatcher over them.

/// Dev-default DSN (mirrors CLAUDE.md); overridden by `DATABASE_URL`.
pub const DEFAULT_DSN: &str =
    "postgres://gamebackend:gamebackend@localhost:5432/gamebackend?sslmode=disable";

/// The connection string every command uses: `DATABASE_URL` or the dev default.
pub fn dsn() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string())
}

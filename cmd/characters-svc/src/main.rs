//! `characters-svc` — process A of the split (port of Go's `cmd/characters-svc`). It
//! hosts ONLY gateway + characters + messaging and stands up one shared QUIC edge
//! server; `characters` contributes its `characters.ownerOf` + player-op faces to
//! `edge::EDGE_SLOT` (topology-blind), and `app::run` installs them on this server
//! so a peer's inventory can resolve ownership over the mutually-authenticated edge.
//! The characters outbox relay runs in THIS process because it drains characters'
//! own outbox rows.

use std::sync::{Arc, Mutex};

use lifecycle::Module;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    // One shared QUIC edge server for the whole process. Modules contribute their
    // RPC faces to `edge::EDGE_SLOT` during `init`; `app::run` applies the
    // contributions onto this server after Build, then `listen`s it (a single UDP
    // port serves every edge method). Standing this up is the composition root's
    // legitimate topology knowledge — the modules never see it.
    let edge_server = Arc::new(Mutex::new(edge::Server::new()));

    // messaging LAST for Stop ordering (reverse) — the relay halts delivery before
    // characters tears down. gateway only contributes the HTTP front-handler and has
    // no Stop, so its position is immaterial.
    let mods: Vec<Box<dyn Module>> = vec![
        Box::new(gateway::Gateway::new()),
        Box::new(characters::Characters::new()),
        Box::new(messaging::Messaging::new()),
    ];

    // No player front: A serves peers over the internal mutual-TLS edge, not players.
    app::run(app::Config::from_env(), mods, Some(edge_server), None).await
}

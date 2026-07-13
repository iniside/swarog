//! `edge` — the QUIC + mutual-TLS RPC transport (port of Go's `edge` package). A
//! near-leaf: it depends only on `opsapi` (for the [`opsapi::Caller`] trait the
//! [`Client`] implements) and imports no module.
//!
//! - [`Client`] holds ONE persistent QUIC connection; each call opens a fresh
//!   bidirectional stream (persistent conn, stream-per-call).
//! - [`Server`] accepts connections/streams and dispatches one framed request per
//!   stream by method name, with precedence exact-`handle` > exact-`handle_identity`
//!   > longest-`handle_prefix`.
//! - The wire is JSON envelopes ([`codec`]) behind a 4-byte length prefix
//!   ([`frame`]); one envelope per stream (the stream is the correlation).
//! - [`DevCA`] is the shared dev trust anchor for the hop's MUTUAL TLS — the
//!   5-point spec is enforced in [`tls`].
//! - [`EdgeReg`]/[`EDGE_SLOT`] are the topology-blind registration seam: modules
//!   contribute registrations unconditionally; `app::run` applies them iff this
//!   process serves an internal edge.
//! - [`PlayerServer`]/[`PlayerClient`] are the separate PLAYER-facing plane:
//!   server-cert-only TLS (players hold no CA-signed leaf), its own ALPN
//!   ([`PLAYER_ALPN`]) and envelope ([`PlayerRequest`], bearer `token` instead of a
//!   trusted `identity`), and a 1 MiB frame cap ([`MAX_PLAYER_FRAME`]).

mod client;
mod codec;
mod frame;
mod player;
mod reg;
mod server;
mod tls;
mod wire;

pub use client::Client;
pub use reg::{EdgeReg, EDGE_SLOT};
pub use codec::{default_codec, Codec, JsonCodec};
pub use frame::{frame_bytes, read_frame, read_frame_max, write_frame, MAX_FRAME};
pub use player::{PlayerClient, PlayerHandler, PlayerRequest, PlayerRequestLimits, PlayerServer, MAX_PLAYER_FRAME};
pub use server::{ForwardHandler, Handler, HandlerResult, IdentityHandler, RunningServer, Server};
pub use tls::{dev_ca_from_env, shared_dev_ca, DevCA, TrustAnchor, ALPN, PLAYER_ALPN};
pub use wire::{Response, ResponseCode};

/// Errors from the edge transport.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("edge: io: {0}")]
    Io(#[source] std::io::Error),
    #[error("edge: json codec: {0}")]
    Codec(#[source] serde_json::Error),
    #[error("edge: frame too large: {size} > {max}")]
    FrameTooLarge { size: usize, max: usize },
    #[error("edge: certificate generation: {0}")]
    Rcgen(#[source] rcgen::Error),
    #[error("edge: rustls: {0}")]
    Rustls(#[source] rustls::Error),
    #[error("edge: tls config: {0}")]
    Tls(String),
    #[error("edge: dev CA: {0}")]
    Ca(String),
    #[error("edge: connect: {0}")]
    Connect(String),
    #[error("edge: connection: {0}")]
    Connection(String),
    #[error("edge: stream: {0}")]
    Stream(String),
    /// The peer returned an `ok:false` response envelope (a handler/dispatch error
    /// or a malformed request). Carries the peer's error string.
    #[error("edge: remote error: {0}")]
    Remote(String),
    /// The peer's dispatch table has no handler for the requested method: detected by
    /// [`Client`] via the response envelope's typed [`ResponseCode::UnknownMethod`]
    /// field (NOT the error text — see [`UNKNOWN_METHOD_PREFIX`]). Typed apart from
    /// [`Error::Remote`] so consumers (the admin fan-out's "peer has no admin surface"
    /// probe) never string-sniff, and so the opsapi mapping can classify it as
    /// non-retryable. Carries the peer's full error string (still sentinel-prefixed
    /// for readability), displayed verbatim. Internal plane only — the player plane
    /// has no method table and never produces this.
    #[error("{0}")]
    UnknownMethod(String),
}

/// The human-readable prefix the internal [`Server`]'s dispatch puts on an unmatched
/// method's `error` string (`server.rs`), for logs and debugging. It is NO LONGER
/// load-bearing for classification: the [`Client`] types an unknown method off the
/// envelope's [`ResponseCode::UnknownMethod`] field (`client.rs`), not this text.
/// Sniffing the text was replaced because a handler that propagated an INNER peer's
/// unknown-method message re-stamped this prefix and got misclassified as a typed
/// 404 (2026-07-13 remediation Step 16 — reverses the 2026-07-11 Step 7 rejection of
/// the envelope-code option). This constant is now a message formatter only.
pub(crate) const UNKNOWN_METHOD_PREFIX: &str = "edge: unknown method";

/// Maps an edge transport failure onto an [`opsapi::Error`] for the [`opsapi::Caller`]
/// boundary. [`Error::UnknownMethod`] — the peer answered but serves no such method —
/// maps to [`opsapi::Status::NotFound`] (non-retryable: the peer will keep not knowing
/// the method). Every OTHER edge-level failure is [`opsapi::Status::Unavailable`]
/// (a peer-answered failure is NEVER replayed — `remote`'s provenance gate replays
/// only connection-fatal failures, regardless of retry mode): the DOMAIN status of
/// a completed operation rides
/// INSIDE the response payload envelope (the `#[rpc]` layer, Step 5), not at this
/// transport level, so a non-OK edge response here means the call did not complete.
///
/// BLAST RADIUS (intentional, Step 7 of the 2026-07-11 remediation plan): this is
/// the single conversion used by EVERY generated rpc client and the gateway's
/// Remote dispatch. Mapping `UnknownMethod → NotFound` means a gateway→svc method
/// mismatch (version skew, misdeploy) surfaces to the front as a 404 that is
/// indistinguishable from a domain not-found — accepted, because unknown-method is
/// not retryable and a 503 would invite pointless retries.
impl From<Error> for opsapi::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::UnknownMethod(msg) => opsapi::Error::not_found(msg),
            other => opsapi::Error::unavailable(other.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::net::SocketAddr;
    use std::sync::Arc;

    fn loopback() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    fn handler<F>(f: F) -> Handler
    where
        F: Fn(Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync + 'static,
    {
        Arc::new(f)
    }

    fn id_handler<F>(f: F) -> IdentityHandler
    where
        F: Fn(Option<String>, Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync + 'static,
    {
        Arc::new(f)
    }

    // End-to-end over loopback QUIC with the dev CA: an exact `handle` echoes, an
    // `handle_identity` reflects the threaded identity, a prefix relay serves a
    // family, and an unknown method errors — all over ONE mutually-authenticated
    // connection.
    #[tokio::test]
    async fn end_to_end_dispatch_and_identity_thread_through() {
        let ca = DevCA::generate().unwrap();

        let mut srv = Server::new();
        srv.handle(
            "echo",
            handler(|payload| Box::pin(async move { Ok(payload) })),
        );
        srv.handle_identity(
            "whoami",
            id_handler(|identity, _payload| {
                Box::pin(async move {
                    // Reflect the identity the sender stamped into the envelope.
                    let id = identity.unwrap_or_else(|| "<none>".into());
                    Ok(format!(r#""{id}""#).into_bytes())
                })
            }),
        );
        srv.handle_prefix(
            "fwd.",
            Arc::new(|method: String, _payload: Vec<u8>| {
                Box::pin(async move { Ok(format!(r#""served:{method}""#).into_bytes()) })
            }),
        );

        let running = srv.listen(loopback(), &ca).unwrap();
        let addr = running.local_addr();

        let client = Client::dial(addr, &ca).await.unwrap();

        // Exact handle echoes the payload verbatim.
        let resp = client.call_raw("echo", br#"{"n":1}"#).await.unwrap();
        assert_eq!(resp, br#"{"n":1}"#);

        // Identity threads through: the server sees the exact player_id the client
        // stamped — the crux of the auth trust boundary.
        let resp = client
            .call_raw_id("whoami", Some("player-42"), b"null")
            .await
            .unwrap();
        assert_eq!(resp, br#""player-42""#);

        // No identity → the adapter sees none.
        let resp = client.call_raw_id("whoami", None, b"null").await.unwrap();
        assert_eq!(resp, br#""<none>""#);

        // Longest-prefix forward serves the family under the original method name.
        let resp = client.call_raw("fwd.anything", b"null").await.unwrap();
        assert_eq!(resp, br#""served:fwd.anything""#);

        // Unknown method → the TYPED variant (the client detects the shared
        // sentinel), which the opsapi mapping classifies as non-retryable NotFound.
        let err = client.call_raw("nope", b"null").await.unwrap_err();
        assert!(matches!(&err, Error::UnknownMethod(_)), "{err:?}");
        let ops: opsapi::Error = err.into();
        assert_eq!(ops.status, opsapi::Status::NotFound);

        // The Caller trait routes identically (bytes in/out), proving the generated
        // client (Step 5) can compose over this exact seam.
        let resp = opsapi::Caller::call(
            &client,
            "echo",
            None,
            br#"{"n":2}"#,
            opsapi::RetryMode::Never,
        )
            .await
            .unwrap();
        assert_eq!(resp, br#"{"n":2}"#);

        client.close();
        running.close();
    }

    // THE false-positive proof (Step 16, reverses 2026-07-11 Step 7): an OUTER edge
    // server whose handler CALLS an inner edge peer with a nonexistent method, gets a
    // genuine `Error::UnknownMethod` whose `Display` is the verbatim sentinel-prefixed
    // text, and propagates it via `?`. The inner unknown-method text thus lands in the
    // OUTER reply's `error` string — but the outer reply carries NO `code`, so the
    // outer client MUST classify it as `Error::Remote` (a handler failure), not the
    // typed `UnknownMethod`. On the old text-sniff code the outer client saw the
    // prefix in `error` and misclassified it as `UnknownMethod` → NotFound (red).
    #[tokio::test]
    async fn handler_propagating_inner_unknown_method_is_remote_not_unknown_method() {
        let ca = DevCA::generate().unwrap();

        // Inner peer: serves ONLY "known" — any other method is a real unknown-method.
        let mut inner = Server::new();
        inner.handle("known", handler(|p| Box::pin(async move { Ok(p) })));
        let inner_running = inner.listen(loopback(), &ca).unwrap();
        let inner_client = Arc::new(Client::dial(inner_running.local_addr(), &ca).await.unwrap());

        // Sanity: the inner peer genuinely produces a TYPED unknown-method for a
        // missing method (this is the text the outer handler will propagate).
        let inner_err = inner_client.call_raw("missing", b"null").await.unwrap_err();
        assert!(matches!(&inner_err, Error::UnknownMethod(_)), "{inner_err:?}");
        assert!(
            inner_err.to_string().starts_with(UNKNOWN_METHOD_PREFIX),
            "inner unknown-method Display must carry the sentinel prefix: {inner_err}"
        );

        // Outer peer: its "proxy" handler calls the inner peer's missing method and
        // propagates the inner error via `?` — so the inner sentinel text becomes the
        // outer handler's error string.
        let mut outer = Server::new();
        {
            let inner_client = inner_client.clone();
            outer.handle(
                "proxy",
                handler(move |_p| {
                    let inner_client = inner_client.clone();
                    Box::pin(async move {
                        let bytes = inner_client.call_raw("missing", b"null").await?;
                        Ok(bytes)
                    })
                }),
            );
        }
        let outer_running = outer.listen(loopback(), &ca).unwrap();
        let outer_client = Client::dial(outer_running.local_addr(), &ca).await.unwrap();

        let err = outer_client.call_raw("proxy", b"null").await.unwrap_err();
        // The outer reply's error text CONTAINS the sentinel (propagated from inner),
        // proving the text sniff WOULD have fired here...
        assert!(
            err.to_string().contains(UNKNOWN_METHOD_PREFIX),
            "the propagated inner text must carry the sentinel: {err}"
        );
        // ...yet the typed code makes it a Remote handler failure, never UnknownMethod.
        assert!(
            matches!(&err, Error::Remote(_)),
            "a handler that propagates an inner unknown-method must be Remote, got {err:?}"
        );
        // And at the opsapi boundary it stays Unavailable — a non-replayed peer
        // answer (FailureProvenance::PeerAnswer never replays) — not a lying 404.
        let ops: opsapi::Error = err.into();
        assert_eq!(ops.status, opsapi::Status::Unavailable);

        inner_client.close();
        outer_client.close();
        inner_running.close();
        outer_running.close();
    }

    // The SPLIT scenario: server and client each independently `DevCA::load` the
    // SAME minted PEM files (as two `*-svc` processes would) and interoperate. This
    // is the shared-anchor path Step 11 relies on — stronger than one in-memory CA
    // handed to both sides.
    #[tokio::test]
    async fn two_independently_loaded_cas_from_shared_files_interoperate() {
        let dir = std::env::temp_dir();
        let cert = dir.join(format!("edge-split-{}.crt", std::process::id()));
        let key = dir.join(format!("edge-split-{}.key", std::process::id()));
        DevCA::generate()
            .unwrap()
            .write_pem(cert.to_str().unwrap(), key.to_str().unwrap())
            .unwrap();

        let server_ca = DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();
        let client_ca = DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();

        let mut srv = Server::new();
        srv.handle("echo", handler(|p| Box::pin(async move { Ok(p) })));
        let running = srv.listen(loopback(), &server_ca).unwrap();

        let client = Client::dial(running.local_addr(), &client_ca).await.unwrap();
        let resp = client.call_raw("echo", br#"{"shared":true}"#).await.unwrap();
        assert_eq!(resp, br#"{"shared":true}"#);

        client.close();
        running.close();
        let _ = std::fs::remove_file(cert);
        let _ = std::fs::remove_file(key);
    }

    // A handler panic is contained into an error response, not a torn-down stream.
    #[tokio::test]
    async fn handler_panic_becomes_error_response() {
        let ca = DevCA::generate().unwrap();
        let mut srv = Server::new();
        srv.handle("boom", handler(|_p| Box::pin(async move { panic!("kaboom") })));
        srv.handle("ok", handler(|p| Box::pin(async move { Ok(p) })));
        let running = srv.listen(loopback(), &ca).unwrap();
        let client = Client::dial(running.local_addr(), &ca).await.unwrap();

        let err = client.call_raw("boom", b"null").await.unwrap_err();
        assert!(matches!(&err, Error::Remote(msg) if msg.contains("panic")), "{err:?}");
        // A genuine remote error stays a retryable transport failure at the opsapi
        // boundary — only UnknownMethod maps to NotFound.
        let ops: opsapi::Error = err.into();
        assert_eq!(ops.status, opsapi::Status::Unavailable);
        // The connection survives — a subsequent call still works.
        let resp = client.call_raw("ok", br#"1"#).await.unwrap();
        assert_eq!(resp, b"1");

        client.close();
        running.close();
    }

    // THE mTLS proof (part 1): a client whose cert chains to a DIFFERENT CA cannot
    // establish the connection — the shared anchor is enforced on both sides.
    #[tokio::test]
    async fn client_from_a_different_ca_is_rejected() {
        let server_ca = DevCA::generate().unwrap();
        let rogue_ca = DevCA::generate().unwrap(); // an independent, untrusted anchor

        let mut srv = Server::new();
        srv.handle("echo", handler(|p| Box::pin(async move { Ok(p) })));
        let running = srv.listen(loopback(), &server_ca).unwrap();
        let addr = running.local_addr();

        // Dialing with the rogue CA: the client neither trusts the server's cert nor
        // presents a server-trusted client cert. The handshake must fail (at connect
        // or, defensively, on the first call).
        assert_rejected(Client::dial(addr, &rogue_ca).await).await;

        running.close();
    }

    // THE mTLS proof (part 2): a client that TRUSTS the server's CA but presents NO
    // client certificate is rejected — proving the server REQUIRES a client cert
    // (WebPkiClientVerifier), the load-bearing half of mutual TLS.
    #[tokio::test]
    async fn client_with_no_certificate_is_rejected() {
        let ca = DevCA::generate().unwrap();
        let mut srv = Server::new();
        srv.handle("echo", handler(|p| Box::pin(async move { Ok(p) })));
        let running = srv.listen(loopback(), &ca).unwrap();
        let addr = running.local_addr();

        let no_auth_cfg = ca.client_tls_without_client_auth().unwrap();
        assert_rejected(Client::dial_with_config(addr, no_auth_cfg).await).await;

        running.close();
    }

    /// A dial result that must represent a rejected handshake: either `dial` already
    /// errored, or the connection "opened" but the first call fails. Either way the
    /// unauthenticated peer gets no service.
    async fn assert_rejected(dial: Result<Client, Error>) {
        match dial {
            Err(_) => { /* rejected at handshake — the expected path */ }
            Ok(client) => {
                let r = client.call_raw("echo", b"null").await;
                assert!(r.is_err(), "an un/mis-certed client must not be served, got {r:?}");
            }
        }
    }
}

#[cfg(test)]
mod player_e2e_tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::net::SocketAddr;
    use std::sync::Arc;

    fn loopback() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    fn player_handler<F>(f: F) -> PlayerHandler
    where
        F: Fn(String, Option<String>, Option<String>, Vec<u8>) -> BoxFuture<'static, HandlerResult>
            + Send
            + Sync
            + 'static,
    {
        Arc::new(f)
    }

    fn internal_handler<F>(f: F) -> Handler
    where
        F: Fn(Vec<u8>) -> BoxFuture<'static, HandlerResult> + Send + Sync + 'static,
    {
        Arc::new(f)
    }

    /// A [`PlayerServer`] whose handler reflects (method, token, api_key, payload)
    /// back as a JSON object — lets one assertion see everything the transport
    /// threaded through.
    fn reflecting_server() -> PlayerServer {
        let srv = PlayerServer::new();
        srv.set_handler(player_handler(|method, token, api_key, payload| {
            Box::pin(async move {
                let token = token.map_or("<none>".into(), |t| t);
                let api_key = api_key.map_or("<none>".into(), |k| k);
                let payload = String::from_utf8_lossy(&payload).into_owned();
                Ok(format!(
                    r#"{{"method":"{method}","token":"{token}","api_key":"{api_key}","payload":{payload}}}"#
                )
                .into_bytes())
            })
        }));
        srv
    }

    // Player-plane roundtrip WITH and WITHOUT a token + api_key: both are threaded
    // through verbatim (unverified claims — verification is the front's job, not the
    // transport's) and omitted fields still dispatch (the serde(default) proof over
    // the live wire, not just the envelope unit test).
    #[tokio::test]
    async fn player_roundtrip_with_and_without_token_and_api_key() {
        let ca = DevCA::generate().unwrap();
        let running = reflecting_server().listen(loopback(), &ca).unwrap();

        let client = PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap();

        let resp = client
            .call("characters.create", Some("dev-alice"), Some("dev-key-client"), br#"{"name":"hero"}"#)
            .await
            .unwrap();
        assert_eq!(
            resp,
            br#"{"method":"characters.create","token":"dev-alice","api_key":"dev-key-client","payload":{"name":"hero"}}"#
        );

        // No token, no key: the pre-key AuthNone shape — must dispatch, handler sees none.
        let resp = client.call("leaderboard.top", None, None, br#"{"n":10}"#).await.unwrap();
        assert_eq!(
            resp,
            br#"{"method":"leaderboard.top","token":"<none>","api_key":"<none>","payload":{"n":10}}"#
        );

        client.close();
        running.close();
    }

    // The playercli trust path: the dialer holds ONLY the CA certificate (the key is
    // DELETED before loading — a player never has it) and still verifies + reaches a
    // live PlayerServer whose CA was independently loaded from the same files.
    #[tokio::test]
    async fn load_cert_only_trust_anchor_dials_a_live_player_server() {
        let dir = std::env::temp_dir();
        let cert = dir.join(format!("edge-player-{}.crt", std::process::id()));
        let key = dir.join(format!("edge-player-{}.key", std::process::id()));
        DevCA::generate()
            .unwrap()
            .write_pem(cert.to_str().unwrap(), key.to_str().unwrap())
            .unwrap();

        let server_ca = DevCA::load(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();
        let running = reflecting_server().listen(loopback(), &server_ca).unwrap();

        // The player side: cert only, key gone.
        std::fs::remove_file(&key).unwrap();
        let anchor = DevCA::load_cert_only(cert.to_str().unwrap()).unwrap();
        let client = PlayerClient::dial(running.local_addr(), &anchor).await.unwrap();
        let resp = client.call("echo", None, None, br#"{"anchor":true}"#).await.unwrap();
        assert_eq!(
            resp,
            br#"{"method":"echo","token":"<none>","api_key":"<none>","payload":{"anchor":true}}"#
        );

        client.close();
        running.close();
        let _ = std::fs::remove_file(cert);
    }

    // Planes don't cross (direction 1): a PlayerClient — no client cert, player
    // ALPN — succeeds against the PlayerServer but MUST be rejected by the internal
    // mTLS Server (ALPN mismatch + missing client cert), even though both chain to
    // the same CA.
    #[tokio::test]
    async fn player_client_is_rejected_by_the_internal_mtls_server() {
        let ca = DevCA::generate().unwrap();

        // Sanity: the same client shape IS served on the player plane.
        let player_running = reflecting_server().listen(loopback(), &ca).unwrap();
        let ok_client =
            PlayerClient::dial(player_running.local_addr(), &ca.trust_anchor()).await.unwrap();
        ok_client.call("echo", None, None, b"null").await.unwrap();
        ok_client.close();
        player_running.close();

        // The internal mTLS plane rejects it.
        let mut srv = Server::new();
        srv.handle("echo", internal_handler(|p| Box::pin(async move { Ok(p) })));
        let running = srv.listen(loopback(), &ca).unwrap();

        match PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await {
            Err(_) => { /* rejected at handshake — the expected path */ }
            Ok(client) => {
                let r = client.call("echo", None, None, b"null").await;
                assert!(r.is_err(), "a player client must not be served on the mTLS plane, got {r:?}");
            }
        }
        running.close();
    }

    // Planes don't cross (direction 2): an internal mTLS Client — full client cert
    // but internal ALPN — must fail against the PlayerServer.
    #[tokio::test]
    async fn internal_client_is_rejected_by_the_player_server() {
        let ca = DevCA::generate().unwrap();
        let running = reflecting_server().listen(loopback(), &ca).unwrap();
        let addr = running.local_addr();

        match Client::dial(addr, &ca).await {
            Err(_) => { /* rejected at handshake — the expected path */ }
            Ok(client) => {
                let r = client.call_raw("echo", b"null").await;
                assert!(r.is_err(), "an internal client must not be served on the player plane, got {r:?}");
            }
        }
        running.close();
    }

    // The player frame cap: a > 1 MiB payload is rejected SERVER-side (the length
    // prefix is checked before any body allocation, and the receive side is stopped
    // so the blocked sender errors out instead of deadlocking) — and the rejection
    // is per-stream: the same connection serves a normal call right after.
    #[tokio::test]
    async fn oversize_player_frame_is_rejected_without_killing_the_connection() {
        let ca = DevCA::generate().unwrap();
        let running = reflecting_server().listen(loopback(), &ca).unwrap();
        let client = PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap();

        // A 2 MiB JSON string payload — well over MAX_PLAYER_FRAME but under the
        // internal MAX_FRAME, so only the player-plane cap can be what rejects it.
        let mut big = Vec::with_capacity((2 << 20) + 2);
        big.push(b'"');
        big.resize((2 << 20) + 1, b'x');
        big.push(b'"');
        let err = client.call("echo", None, None, &big).await.unwrap_err();
        // Depending on timing the client sees the server's ok:false envelope or its
        // own blocked write failing on the stopped stream — either way, rejected.
        assert!(
            !matches!(err, Error::FrameTooLarge { .. }),
            "the CLIENT must not be what rejects (server-side cap is the boundary), got {err:?}"
        );

        // The connection survives: a normal call still works.
        let resp = client.call("echo", None, None, br#"{"ok":1}"#).await.unwrap();
        assert_eq!(
            resp,
            br#"{"method":"echo","token":"<none>","api_key":"<none>","payload":{"ok":1}}"#
        );

        client.close();
        running.close();
    }

    // A PlayerServer with no handler installed answers every call with a transport
    // ok:false (front not wired) instead of hanging or crashing.
    #[tokio::test]
    async fn unwired_player_server_reports_front_not_wired() {
        let ca = DevCA::generate().unwrap();
        let running = PlayerServer::new().listen(loopback(), &ca).unwrap();
        let client = PlayerClient::dial(running.local_addr(), &ca.trust_anchor()).await.unwrap();

        let err = client.call("anything", None, None, b"null").await.unwrap_err();
        assert!(matches!(&err, Error::Remote(msg) if msg.contains("not wired")), "{err:?}");

        client.close();
        running.close();
    }
}

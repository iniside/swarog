//! The 400-vs-503 discrimination matrix for the ill-typed-request-body class
//! (`InvalidRequestBody` -> `ResponseCode::InvalidRequest` -> `Error::InvalidRequest`
//! -> `opsapi::Status::Invalid`). Every test here runs over REAL loopback QUIC, so the
//! whole chain — dispatch downcast, wire code, client classification, opsapi mapping —
//! executes; a unit call on `Dispatch` alone would skip the two ends that used to be
//! wrong.
//!
//! The load-bearing case is [`bare_serde_error_from_a_handler_stays_remote_unavailable`]:
//! the marker and the response-ENCODE failure are the SAME Rust type underneath
//! (`serde_json::Error`), so an implementation that downcast to `serde_json::Error`
//! instead of to the marker would turn a SERVER bug into a client-facing 400 and stay
//! green on every other test in this file. It is asserted against the marker case on
//! the SAME server, in one test, so the two cannot drift apart.
//!
//! Timing: no clocks. Every assertion is on a completed request/response.

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

/// A genuine request-BODY decode failure: the exact `serde_json::Error` shape a
/// generated server adapter's `from_slice::<…Request>` produces for an ill-typed body.
fn decode_class_error() -> serde_json::Error {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct SampleRequest {
        name: String,
    }
    serde_json::from_slice::<SampleRequest>(br#"{"name":123}"#)
        .expect_err("a JSON number is not a String — the ill-typed-body class")
}

/// A genuine response-ENCODE failure: `serde_json` cannot serialize a map with a
/// non-string key. Deliberately the SAME error TYPE as [`decode_class_error`] — that is
/// precisely why the dispatch must discriminate on the marker and never on the type.
fn encode_class_error() -> serde_json::Error {
    let mut m = std::collections::HashMap::new();
    m.insert((1i32, 2i32), 3i32);
    serde_json::to_vec(&m).expect_err("a tuple map key is not serializable to JSON")
}

/// THE discrimination proof, both halves against ONE server so no configuration
/// difference can explain the split verdict:
///
/// * `bad_body` fails with the typed `InvalidRequestBody` marker — the branch
///   `Dispatch::dispatch`'s `downcast_ref::<InvalidRequestBody>()` arm exists for. It
///   must come back stamped `ResponseCode::InvalidRequest`, typed
///   `Error::InvalidRequest`, and mapped to `Status::Invalid` (400). Before the fix
///   this was `Remote`/503.
/// * `bad_encode` fails with a BARE `serde_json::Error` (no marker) — the
///   response-encode class, a SERVER bug. It must stay code-less, `Error::Remote`,
///   `Status::Unavailable` (503). An implementation that downcast to
///   `serde_json::Error` would report a server bug to the front door as a client 400,
///   and would pass every OTHER assertion in this file.
#[tokio::test]
async fn marker_is_invalid_request_while_a_bare_serde_error_stays_remote() {
    let ca = DevCA::generate().unwrap();
    let mut srv = Server::new();
    srv.handle(
        "bad_body",
        handler(|_p| {
            Box::pin(async move { Err(Box::new(InvalidRequestBody(decode_class_error())) as _) })
        }),
    );
    srv.handle(
        "bad_encode",
        handler(|_p| Box::pin(async move { Err(Box::new(encode_class_error()) as _) })),
    );
    let running = srv.listen(loopback(), &ca).unwrap();
    let client = Client::dial(running.local_addr(), &ca).await.unwrap();

    // (1) The marker: typed InvalidRequest -> opsapi Invalid (400).
    let err = client.call_raw("bad_body", br#"{"name":123}"#).await.unwrap_err();
    assert!(
        matches!(&err, Error::InvalidRequest(_)),
        "the marker must type the client error as InvalidRequest, got {err:?}"
    );
    let ops: opsapi::Error = err.into();
    assert_eq!(
        ops.status,
        opsapi::Status::Invalid,
        "an ill-typed body must answer 400 across the edge, as the monolith does"
    );
    assert_eq!(ops.status.http(), 400);

    // (2) The bare serde_json::Error (same underlying type, no marker): Remote/503.
    let err = client.call_raw("bad_encode", b"null").await.unwrap_err();
    assert!(
        matches!(&err, Error::Remote(_)),
        "a response-ENCODE failure is a SERVER fault and must stay Remote, got {err:?}"
    );
    let ops: opsapi::Error = err.into();
    assert_eq!(
        ops.status,
        opsapi::Status::Unavailable,
        "a server-side encode bug must never be reported to the caller as a 400"
    );
    assert_eq!(ops.status.http(), 503);

    client.close();
    running.close();
}

/// The relay boundary, mirroring `handler_propagating_inner_unknown_method_is_remote…`:
/// an OUTER handler calls an INNER peer, gets a genuine `Error::InvalidRequest`, and
/// `?`-propagates it. The propagated error is a *boxed `edge::Error`*, NOT an
/// `InvalidRequestBody`, so the outer dispatch's downcast must MISS and the outer reply
/// must stay code-less: the outer caller's own request was fine, and re-stamping the
/// inner verdict would tell it (falsely) that its body is malformed. Recognition covers
/// exactly one hop, by identity — the same no-re-stamping property `UnknownMethod`
/// relies on.
#[tokio::test]
async fn handler_propagating_inner_invalid_request_is_remote_not_invalid_request() {
    let ca = DevCA::generate().unwrap();

    // Inner peer: its handler fails with the real marker.
    let mut inner = Server::new();
    inner.handle(
        "inner_op",
        handler(|_p| {
            Box::pin(async move { Err(Box::new(InvalidRequestBody(decode_class_error())) as _) })
        }),
    );
    let inner_running = inner.listen(loopback(), &ca).unwrap();
    let inner_client = Arc::new(Client::dial(inner_running.local_addr(), &ca).await.unwrap());

    // Sanity: the inner peer genuinely produces the TYPED InvalidRequest (this is the
    // error the outer handler will propagate verbatim).
    let inner_err = inner_client.call_raw("inner_op", b"null").await.unwrap_err();
    assert!(matches!(&inner_err, Error::InvalidRequest(_)), "{inner_err:?}");

    // Outer peer: propagates the inner error through `?` into the boxed HandlerResult.
    let mut outer = Server::new();
    {
        let inner_client = inner_client.clone();
        outer.handle(
            "proxy",
            handler(move |_p| {
                let inner_client = inner_client.clone();
                Box::pin(async move {
                    let bytes = inner_client.call_raw("inner_op", b"null").await?;
                    Ok(bytes)
                })
            }),
        );
    }
    let outer_running = outer.listen(loopback(), &ca).unwrap();
    let outer_client = Client::dial(outer_running.local_addr(), &ca).await.unwrap();

    let err = outer_client.call_raw("proxy", b"null").await.unwrap_err();
    assert!(
        matches!(&err, Error::Remote(_)),
        "a handler relaying an inner InvalidRequest must be Remote, got {err:?}"
    );
    let ops: opsapi::Error = err.into();
    assert_eq!(
        ops.status,
        opsapi::Status::Unavailable,
        "the outer caller's own body was fine — its request must not be blamed"
    );

    inner_client.close();
    outer_client.close();
    inner_running.close();
    outer_running.close();
}

/// The OUTER envelope boundary the fix must NOT have crossed: a framed payload that is
/// not a `Request` at all (wire/framing corruption, not a typed-body mismatch) takes
/// `Dispatch::dispatch`'s early `err_response("edge: malformed request envelope")`
/// return — which is code-LESS, so the caller keeps classifying it `Remote`/503.
///
/// Asserted on the RAW wire reply (a hand-written frame, since `Client::call_raw`
/// cannot emit a corrupt envelope): `code` must be absent. `code: None` -> `Remote` ->
/// `Unavailable` is the client mapping pinned by the two tests above and by
/// `handler_panic_becomes_error_response`.
#[tokio::test]
async fn malformed_request_envelope_stays_code_less() {
    let ca = DevCA::generate().unwrap();
    let mut srv = Server::new();
    srv.handle("echo", handler(|p| Box::pin(async move { Ok(p) })));
    let running = srv.listen(loopback(), &ca).unwrap();
    let client = Client::dial(running.local_addr(), &ca).await.unwrap();

    // A well-formed JSON document that is NOT a `Request` (no `method`, no `payload`).
    let (mut send, mut recv) = client.connection().open_bi().await.unwrap();
    write_frame(&mut send, br#"{"nope":1}"#).await.unwrap();
    send.finish().unwrap();
    let raw = read_frame(&mut recv).await.unwrap();
    let resp: Response = serde_json::from_slice(&raw).unwrap();

    assert!(!resp.ok, "a corrupt envelope is an ok:false reply");
    assert!(
        resp.error.as_deref().unwrap_or_default().contains("malformed request envelope"),
        "expected the envelope-decode message, got {:?}",
        resp.error
    );
    assert_eq!(
        resp.code, None,
        "envelope corruption is a WIRE fault: it must never carry InvalidRequest \
         (that would blame the caller's body for a framing problem and answer 400)"
    );

    // The connection is unharmed — the corrupt frame was per-stream.
    let ok = client.call_raw("echo", br#"{"n":1}"#).await.unwrap();
    assert_eq!(ok, br#"{"n":1}"#);

    client.close();
    running.close();
}

use super::*;

#[test]
fn identity_maps_with_player_id_and_player_id() {
    assert_eq!(Identity::none().player_id(), None);
    assert_eq!(Identity::player("p1").player_id(), Some("p1"));
    // Empty string is treated as no identity (Go's `pid != ""` guard).
    assert_eq!(Identity::player("").player_id(), None);
}

#[test]
fn status_of_matches_go_semantics() {
    assert_eq!(status_of(None), Status::Ok);
    let e = Error::not_found("nope");
    assert_eq!(status_of(Some(&e)), Status::NotFound);
    assert_eq!(e.status, Status::NotFound);
}

#[test]
fn status_http_mapping() {
    assert_eq!(Status::Ok.http(), 200);
    assert_eq!(Status::NotFound.http(), 404);
    assert_eq!(Status::Forbidden.http(), 403);
    assert_eq!(Status::Invalid.http(), 400);
    assert_eq!(Status::Unavailable.http(), 503);
    assert_eq!(Status::Internal.http(), 500);
    assert_eq!(Status::Unauthorized.http(), 401);
    assert_eq!(Status::Conflict.http(), 409);
}

#[test]
fn error_display_is_the_message() {
    let e = Error::conflict("email taken");
    assert_eq!(e.to_string(), "email taken");
}

// A LocalInvoker / OpBinding round-trip proving the closure shapes are usable:
// a bytes-in/bytes-out invoker and a decode/encode pair compose the way the
// gateway (Step 10) will drive them.
#[tokio::test]
async fn opset_closures_compose() {
    let decode: DecodeFn = Arc::new(|body, path| {
        let id = path.get("id").cloned().unwrap_or_default();
        let body = body.unwrap_or(b"null");
        Ok(format!(r#"{{"id":"{id}","body":{}}}"#, std::str::from_utf8(body).unwrap()).into_bytes())
    });
    let invoke: LocalInvoker = Arc::new(|ident, req| {
        Box::pin(async move {
            // Requires an identity, mirroring an AuthPlayer op.
            let pid = ident
                .player_id()
                .ok_or_else(|| Error::invalid("no identity"))?;
            Ok(format!(r#"{{"status":"ok","pid":"{pid}","echo":{}}}"#, String::from_utf8(req).unwrap())
                .into_bytes())
        })
    });
    let encode: EncodeFn = Arc::new(|resp| Ok((Some(resp.to_vec()), Status::Ok)));

    let op = OpSet {
        operation: Operation {
            method: "demo.echo".into(),
            verb: "POST".into(),
            path: "/demo/{id}".into(),
            auth: AuthReq::Player,
            success: 200,
            retry_mode: RetryMode::Never,
        },
        binding: OpBinding {
            method: "demo.echo".into(),
            decode,
            encode,
        },
        local: LocalOp {
            method: "demo.echo".into(),
            invoke,
        },
    };

    let mut path = PathArgs::new();
    path.insert("id".into(), "42".into());
    let wire_req = (op.binding.decode)(Some(b"123"), &path).unwrap();
    let wire_resp = (op.local.invoke)(Identity::player("alice"), wire_req)
        .await
        .unwrap();
    let (body, status) = (op.binding.encode)(&wire_resp).unwrap();
    assert_eq!(status, Status::Ok);
    let body = String::from_utf8(body.unwrap()).unwrap();
    assert!(body.contains(r#""pid":"alice""#), "{body}");
    assert!(body.contains(r#""id":"42""#), "{body}");

    // No identity → the invoker rejects with Invalid (the AuthPlayer contract).
    let wire_req = (op.binding.decode)(Some(b"1"), &path).unwrap();
    let err = (op.local.invoke)(Identity::none(), wire_req).await.unwrap_err();
    assert_eq!(err.status, Status::Invalid);
}

//! End-to-end proofs for the `#[rpc]` codegen (this test crate plays the
//! `<name>rpc` GLUE role of the Step-2 split — see `src/lib.rs` for the api role):
//!   1. `client_server_roundtrip_over_edge` — the generated `Client` marshals a call
//!      through a real edge QUIC `Caller` to the generated `register_server`
//!      handlers, threading identity and folding an `Err` into a non-OK domain status
//!      carried INSIDE the response envelope.
//!   2. `operations_expose_only_http_methods` — `operations()`/`route_bindings()`
//!      yield the right `Operation` shape for `#[http]` methods and omit wire-only
//!      ones.
//!   3. `gateway_glue_decode_invoke_encode` — the gateway-facing decode → local
//!      invoke → encode path (no transport) for a path-arg op, on both the OK and the
//!      Forbidden branch.

use std::sync::Arc;

use async_trait::async_trait;
use opsapi::{AuthReq, Caller, Error, Identity, PathArgs, RetryMode, Status};

// The glue's signatures re-resolve here (the metadata travels as tokens), so the
// api crate's domain types must be in scope — exactly like a real `<name>rpc` crate.
use rpc_macro_tests::{Holding, Owner, Sample};

// Expand the api crate's metadata-callback macro into the edge-dependent glue:
// a local `sample_rpc` module with Client / register_server / provide_remote that
// re-exports the pure `rpc_macro_tests::sample_rpc` surface. This is the real
// two-crate handoff (an integration test IS a separate crate).
rpc_macro_tests::sample_sample_meta!(rpc_macro::generate_glue);

// --- A concrete impl --------------------------------------------------------

struct SampleImpl;

struct RecordingCaller {
    modes: std::sync::Mutex<Vec<RetryMode>>,
}

#[async_trait]
impl Caller for RecordingCaller {
    async fn call(
        &self,
        method: &str,
        _identity: Option<&str>,
        _payload: &[u8],
        retry_mode: RetryMode,
    ) -> Result<Vec<u8>, Error> {
        self.modes.lock().unwrap().push(retry_mode);
        match method {
            "sample.ownerOf" => Ok(
                br#"{"status":"Ok","value":{"player_id":"p","ok":true}}"#.to_vec(),
            ),
            "sample.grant" => Ok(br#"{"status":"Ok","value":[]}"#.to_vec()),
            _ => unreachable!("unexpected method {method}"),
        }
    }
}

#[async_trait]
impl Sample for SampleImpl {
    async fn grant(
        &self,
        caller: Identity,
        item_id: String,
        qty: i64,
    ) -> Result<Vec<Holding>, Error> {
        let pid = caller
            .player_id()
            .ok_or_else(|| Error::invalid("no identity"))?;
        Ok(vec![Holding {
            item_id,
            qty,
            owner: pid.to_string(),
        }])
    }

    async fn list_character(
        &self,
        caller: Identity,
        character_id: String,
    ) -> Result<Vec<Holding>, Error> {
        if character_id == "forbidden" {
            return Err(Error::forbidden("not your character"));
        }
        let pid = caller
            .player_id()
            .ok_or_else(|| Error::invalid("no identity"))?;
        Ok(vec![Holding {
            item_id: "starter".into(),
            qty: 1,
            owner: format!("{pid}:{character_id}"),
        }])
    }

    async fn owner_of(&self, character_id: String) -> Result<Owner, Error> {
        if character_id == "missing" {
            return Err(Error::not_found("no such character"));
        }
        Ok(Owner {
            player_id: format!("owner-of-{character_id}"),
            ok: true,
        })
    }

    async fn find_owner(&self, character_id: String) -> Result<Option<String>, Error> {
        if character_id == "absent" {
            return Ok(None);
        }
        Ok(Some(format!("owner-of-{character_id}")))
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_server_roundtrip_over_edge() {
    let ca = edge::DevCA::generate().unwrap();

    // Server: install the generated adapters onto a real edge QUIC server.
    let mut srv = edge::Server::new();
    sample_rpc::register_server(&mut srv, Arc::new(SampleImpl));
    let running = srv.listen("127.0.0.1:0".parse().unwrap(), &ca).unwrap();
    let addr = running.local_addr();

    // Client: the edge Client is the `opsapi::Caller`; the generated rpc Client
    // composes over it — exactly the split-topology path.
    let edge_client = edge::Client::dial(addr, &ca).await.unwrap();
    let caller: Arc<dyn Caller> = Arc::new(edge_client);
    let client = sample_rpc::Client::new(caller);

    // Identity threads through: grant sees the player_id the client stamped.
    let holdings = client
        .grant(Identity::player("alice"), "sword".into(), 3)
        .await
        .unwrap();
    assert_eq!(
        holdings,
        vec![Holding {
            item_id: "sword".into(),
            qty: 3,
            owner: "alice".into()
        }]
    );

    // Wire-only method, unauthenticated: all args marshalled, no identity.
    let owner = client.owner_of("c1".into()).await.unwrap();
    assert_eq!(
        owner,
        Owner {
            player_id: "owner-of-c1".into(),
            ok: true
        }
    );

    // Err path: the domain status rides INSIDE the payload envelope. The edge call
    // itself SUCCEEDS (ok:true); the client reconstructs the NotFound from the
    // envelope's status, not from a transport failure.
    let err = client.owner_of("missing".into()).await.unwrap_err();
    assert_eq!(err.status, Status::NotFound);
    assert_eq!(err.msg, "no such character");

    // A Forbidden from an HTTP-bound method round-trips identically.
    let err = client
        .list_character(Identity::player("bob"), "forbidden".into())
        .await
        .unwrap_err();
    assert_eq!(err.status, Status::Forbidden);

    // Option<T> return round-trip (the response-envelope collapse regression). Both a
    // present value AND a genuine `None` must survive the QUIC round-trip — the `None`
    // as `Ok(None)`, never as a transport/internal error.
    let some = client.find_owner("c1".into()).await.unwrap();
    assert_eq!(some, Some("owner-of-c1".to_string()));
    let none = client.find_owner("absent".into()).await.unwrap();
    assert_eq!(none, None, "Ok(None) must round-trip as None, not an error");

    client_close(&client);
    running.close();
}

// The generated Client holds the Arc<dyn Caller>; nothing to close on it directly,
// but keep a hook so the intent (drop order) is explicit.
fn client_close(_c: &sample_rpc::Client) {}

#[test]
fn operations_expose_only_http_methods() {
    let ops = sample_rpc::operations(Arc::new(SampleImpl));
    // Two #[http] methods (grant, list_character); owner_of is wire-only.
    assert_eq!(ops.len(), 2);

    let grant = ops
        .iter()
        .find(|o| o.operation.method == "sample.grant")
        .expect("grant operation present");
    assert_eq!(grant.operation.verb, "POST");
    assert_eq!(grant.operation.path, "/sample/grant");
    assert_eq!(grant.operation.auth, AuthReq::Player);
    assert_eq!(grant.operation.success, 200);
    assert_eq!(grant.operation.retry_mode, RetryMode::Never);

    let list = ops
        .iter()
        .find(|o| o.operation.method == "sample.listCharacter")
        .expect("list_character operation present");
    assert_eq!(list.operation.verb, "GET");
    assert_eq!(list.operation.path, "/sample/character/{id}");
    assert_eq!(list.operation.retry_mode, RetryMode::OnceAfterReconnect);

    // owner_of is NOT exposed as an operation.
    assert!(ops.iter().all(|o| o.operation.method != "sample.ownerOf"));

    // route_bindings mirrors operations minus the LocalOp.
    let rb = sample_rpc::route_bindings();
    assert_eq!(rb.len(), 2);
    assert!(rb.iter().any(|r| r.operation.method == "sample.grant"));

    // The consts carry the wire names.
    assert_eq!(sample_rpc::METHOD_GRANT, "sample.grant");
    assert_eq!(sample_rpc::METHOD_LIST_CHARACTER, "sample.listCharacter");
    assert_eq!(sample_rpc::METHOD_OWNER_OF, "sample.ownerOf");
}

#[tokio::test]
async fn retry_marker_reaches_generated_client_and_missing_marker_defaults_never() {
    let caller = Arc::new(RecordingCaller { modes: std::sync::Mutex::new(Vec::new()) });
    let client = sample_rpc::Client::new(caller.clone());

    client.owner_of("c1".into()).await.unwrap();
    client
        .grant(Identity::player("alice"), "sword".into(), 1)
        .await
        .unwrap();

    assert_eq!(
        *caller.modes.lock().unwrap(),
        vec![RetryMode::OnceAfterReconnect, RetryMode::Never]
    );
}

#[tokio::test]
async fn gateway_glue_decode_invoke_encode() {
    // Find the list_character OpSet (a path-arg op).
    let ops = sample_rpc::operations(Arc::new(SampleImpl));
    let op = ops
        .into_iter()
        .find(|o| o.operation.method == "sample.listCharacter")
        .unwrap();

    // OK branch: decode the path wildcard into the wire request, invoke with an
    // identity, encode the wire response to the external HTTP body.
    let mut path = PathArgs::new();
    path.insert("id".into(), "c7".into());
    let wire_req = (op.binding.decode)(None, &path).unwrap();
    let wire_resp = (op.local.invoke)(Identity::player("carol"), wire_req)
        .await
        .unwrap();
    let (body, status) = (op.binding.encode)(&wire_resp).unwrap();
    assert_eq!(status, Status::Ok);
    let holdings: Vec<Holding> = serde_json::from_slice(&body.unwrap()).unwrap();
    assert_eq!(holdings[0].owner, "carol:c7");

    // Forbidden branch: the encode surfaces the domain status as an Err carrying it.
    let mut bad = PathArgs::new();
    bad.insert("id".into(), "forbidden".into());
    let wire_req = (op.binding.decode)(None, &bad).unwrap();
    let wire_resp = (op.local.invoke)(Identity::player("carol"), wire_req)
        .await
        .unwrap();
    let err = (op.binding.encode)(&wire_resp).unwrap_err();
    assert_eq!(err.status, Status::Forbidden);
}

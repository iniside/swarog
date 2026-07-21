//! Routing-as-data: build an [`Operation`] + [`OpBinding`] (and so a [`RouteBinding`])
//! from a serializable [`OpManifest`] alone — the DATA half of D2's data-driven gateway.
//!
//! The `#[rpc]` macro emits a per-op [`OpBinding`] whose `decode`/`encode` close over the
//! op's concrete `<Method>Request`/`<Method>Response` types (see `tools/rpc-macro`'s
//! `gen_decode`/`gen_encode`). A gateway that imports the `<name>rpc` glue at COMPILE time
//! gets those typed closures for free. A data-driven gateway (`cmd/gateway-svc`, managed)
//! instead fetches an [`OpManifest`] over the wire at RUNTIME ([`crate::DESCRIBE_METHOD`])
//! and must reconstruct the SAME decode/encode with NO knowledge of the request/response
//! Rust types. This module is that reconstruction — it consumes only `opsapi`'s own types,
//! so it links into a front-door process that names no provider crate.
//!
//! ## Faithful, with three recorded caveats
//! The generated closures serialize/deserialize the exact typed structs; these rebuild the
//! wire request/response as untyped [`serde_json::Value`]. That is WIRE-JSON-EQUIVALENT,
//! not byte-identical, and the difference is deliberate:
//!
//!   (i)   **Typeless passthrough is wire-JSON-EQUIVALENT, not byte-identical.** The typed
//!         path emits `to_vec(&Request)`; here we emit `to_vec(&Value)` assembled from the
//!         raw body + path wildcards. The two deserialize to the SAME `Request` svc-side,
//!         but their bytes can differ in number formatting (`1.0` vs `1`) and whitespace.
//!         No consumer compares the wire bytes, only decodes them, so this is invisible.
//!   (ii)  **Malformed-JSON 400 stays at the gateway.** We keep the body PARSE (rather than
//!         relaying opaque bytes), so a malformed body is [`Error::invalid`] here — a 400 at
//!         the front door, exactly where the typed decode raised it. Relaying unparsed bytes
//!         would have pushed the failure to the peer and surfaced it as a 503 instead.
//!   (iii) **Empty-body default synthesis is resolved svc-side via serde defaults.** An
//!         absent/empty body decodes to `{}`; every `<Method>Request` derives `Default` +
//!         `#[serde(default)]`, so the svc zero-fills `{}` into `Request::default()` —
//!         identical to the typed path's `Request::default()` starting point. No deviation:
//!         `body_names` renames cancel symmetrically HTTP↔wire (the external HTTP key already
//!         equals the wire key), so a BODY arg needs no key-rename here; `wire_key` is
//!         load-bearing ONLY to inject a PATH arg into the wire request.

use std::sync::Arc;

use serde_json::Value;

use crate::{
    ArgSource, DecodeFn, EncodeFn, Error, OpBinding, OpManifest, Operation, RouteBinding, Status,
};

/// The [`Operation`] an [`OpManifest`] declares — route + auth + success + the faithful
/// `retry_mode` (carried through describe from the SAME `#[retry_safe]` authority the macro
/// reads, so a rebuilt op preserves its one-replay-after-reconnect instead of defaulting).
pub fn operation(m: &OpManifest) -> Operation {
    Operation {
        method: m.method.clone(),
        verb: m.verb.clone(),
        path: m.path.clone(),
        auth: m.auth,
        success: m.success,
        retry_mode: m.retry_mode,
    }
}

/// The data-driven [`DecodeFn`] for an [`OpManifest`]: parse the HTTP body as a JSON object
/// (absent/empty → `{}`; present-but-malformed or non-object → [`Error::invalid`], keeping
/// the 400 at the gateway, caveat (ii)), then inject each PATH arg's matched wildcard value
/// under its `wire_key` — exactly what the generated `decode` does. Only path args are
/// captured; body args ride the parsed object under their own external key, which already
/// equals the wire key (caveat (iii)).
fn decode(m: &OpManifest) -> DecodeFn {
    // Capture only the path-arg injections: (wire_key, wildcard-name). Body args need no
    // handling — they arrive in the parsed body object under the key a client already sends.
    let path_args: Vec<(String, String)> = m
        .args
        .iter()
        .filter_map(|a| match &a.source {
            ArgSource::Path { wildcard } => Some((a.wire_key.clone(), wildcard.clone())),
            ArgSource::Body => None,
        })
        .collect();
    Arc::new(move |body: Option<&[u8]>, path: &crate::PathArgs| {
        // Absent/empty body → an empty object; the svc zero-fills it via serde defaults
        // (caveat (iii)). A present-but-malformed or non-object body is a 400 at the front.
        let mut obj = match body {
            Some(b) if !b.is_empty() => match serde_json::from_slice::<Value>(b) {
                Ok(Value::Object(map)) => map,
                Ok(_) => return Err(Error::invalid("request body must be a json object")),
                Err(_) => return Err(Error::invalid("invalid json")),
            },
            _ => serde_json::Map::new(),
        };
        for (wire_key, wildcard) in &path_args {
            let val = path.get(wildcard).cloned().unwrap_or_default();
            obj.insert(wire_key.clone(), Value::String(val));
        }
        serde_json::to_vec(&Value::Object(obj)).map_err(|e| Error::internal(e.to_string()))
    })
}

/// The fully-generic [`EncodeFn`] — carries ZERO per-op data (no response type). Reduce the
/// `{status, err, value?}` wire envelope: a non-`Ok` status becomes an [`Error`] carrying it
/// (→ the right HTTP code); a present `value` key (even `null`) is the domain body; an absent
/// `value` is a no-content 204. Identical outcome to the generated `encode`, which reads the
/// typed `<Method>Response`'s `status`/`err`/`value` fields.
fn encode() -> EncodeFn {
    Arc::new(|resp: &[u8]| {
        let v: Value =
            serde_json::from_slice(resp).map_err(|e| Error::internal(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| Error::internal("response envelope must be a json object"))?;
        let status: Status = obj
            .get("status")
            .cloned()
            .ok_or_else(|| Error::internal("response envelope missing `status`"))
            .and_then(|s| serde_json::from_value(s).map_err(|e| Error::internal(e.to_string())))?;
        if status != Status::Ok {
            let err = obj.get("err").and_then(Value::as_str).unwrap_or_default();
            return Err(Error::new(status, err));
        }
        match obj.get("value") {
            // Key present (even `null`): the op returns a value → the domain body.
            Some(value) => {
                let body =
                    serde_json::to_vec(value).map_err(|e| Error::internal(e.to_string()))?;
                Ok((Some(body), Status::Ok))
            }
            // Absent: a no-return op → 204.
            None => Ok((None, Status::Ok)),
        }
    })
}

/// The [`OpBinding`] (decode + encode) an [`OpManifest`] declares, rebuilt from data alone.
pub fn binding(m: &OpManifest) -> OpBinding {
    OpBinding {
        method: m.method.clone(),
        decode: decode(m),
        encode: encode(),
    }
}

/// The complete [`RouteBinding`] — [`Operation`] + [`OpBinding`] — a data-driven gateway
/// contributes to `opsapi::SLOT`/`BINDING_SLOT` for one describe-fetched op. The IMPL-FREE
/// twin of the `#[rpc]` macro's `route_bindings()`, built with no compile-time import of the
/// provider's `<name>rpc` glue.
pub fn route_binding(m: &OpManifest) -> RouteBinding {
    RouteBinding {
        operation: operation(m),
        binding: binding(m),
    }
}

#[cfg(test)]
#[path = "databind_tests.rs"]
mod databind_tests;

//! `rpc-macro` — the `#[rpc]` codegen (the Rust port of Go's `tools/rpcgen`).
//!
//! `#[rpc(prefix = "...")]` is an attribute macro applied to a **capability trait**.
//! It emits, into a child module `<snake(trait)>_rpc`, the transport glue that would
//! otherwise be hand-written per method — the debt the project explicitly forbids:
//!
//!   - a wire **request** struct per method (one field per non-identity arg),
//!   - a wire **response** struct per method carrying `{ status, err, value? }` where
//!     `status: opsapi::Status` is the DOMAIN outcome riding INSIDE the payload
//!     envelope (an edge-level `ok:false` is a separate *transport* failure),
//!   - `METHOD_<NAME>: &str = "<prefix>.<lowerCamel(name)>"` consts,
//!   - a `Client` over `Arc<dyn opsapi::Caller>` implementing the source trait,
//!   - `register_server(&mut edge::Server, Arc<dyn Trait>)` installing one
//!     `handle_identity` adapter per method,
//!   - and, for `#[http(...)]`-annotated methods only, `operations(impl) ->
//!     Vec<opsapi::OpSet>` and `route_bindings() -> Vec<opsapi::RouteBinding>` (the
//!     decode/encode/local glue the gateway routes over).
//!
//! # THE IDENTITY CONVENTION (set here; Steps 8–10 depend on it — do not change)
//!
//! Rust has no ambient `context.Context`, so a method that needs the caller's
//! VERIFIED player identity declares it as an **explicit leading parameter of type
//! `opsapi::Identity`** (the first parameter after `&self`). The macro recognises it
//! by the parameter type's final path segment being `Identity` and:
//!
//!   - **strips it from the wire request struct** (it never travels in the body);
//!   - on the **client**, threads `identity.player_id()` into
//!     `Caller::call(method, identity, payload)`;
//!   - on the **server** adapter, reconstructs it from the edge envelope's identity
//!     string via `opsapi::Identity::player(envelope_identity)` and passes it as the
//!     leading argument to the impl;
//!   - in the **local invoker**, passes the `opsapi::Identity` the gateway supplies
//!     as the leading argument.
//!
//! A method WITHOUT a leading `Identity` parameter (e.g. `owner_of`) is an
//! unauthenticated capability: the client sends `identity = None` and the server
//! ignores the envelope identity. Every argument is marshalled normally. This is the
//! exact Rust twin of Go's rule "first param is `context.Context`, carrying the
//! gateway-verified `player_id`, and is not marshalled".
//!
//! # `#[http(...)]` attribute
//!
//! ```ignore
//! #[http(verb = "POST", path = "/inventory/character/{id}", auth = "player",
//!        success = 200, path_args(character_id = "id"), body_names(item_id = "sku"))]
//! ```
//!
//!   - `verb`/`path`/`success` — the HTTP route + success status.
//!   - `auth` — `"none"` → `AuthReq::None`, `"player"` → `AuthReq::Player`.
//!   - `path_args(param = "wildcard")` — that method param is sourced from the path
//!     wildcard `{wildcard}`, not the JSON body.
//!   - `body_names(param = "json_key")` — override a body arg's external JSON key.
//!
//! # Constraints (surfaced as ordinary compile errors, per the plan)
//!
//! The source trait must be `#[async_trait::async_trait]` (each method is `async fn`,
//! since the client awaits the transport); the last (only) return must be
//! `Result<T, opsapi::Error>` (folded into `status`/`err`); every other param/result
//! must be `serde::Serialize`/`serde::Deserialize` — an unsupported type surfaces as
//! an ordinary compile error at the generated serde site (a token-only macro cannot
//! and does not attempt Go rpcgen's whole-program type rejection). HTTP-bound method
//! request structs additionally derive `Default`, so their params must be `Default`.
//! Method output is deterministic: methods are sorted by name before emission.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use std::collections::BTreeMap;
use syn::{
    parse::{Parse, ParseStream},
    spanned::Spanned,
    FnArg, GenericArgument, Ident, ItemTrait, LitInt, LitStr, Pat, PathArguments, ReturnType,
    Signature, Token, TraitItem, Type,
};

/// The `#[rpc(prefix = "...")]` attribute.
#[proc_macro_attribute]
pub fn rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as RpcArgs);
    let item_trait = syn::parse_macro_input!(item as ItemTrait);
    match expand(args, item_trait) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------------
// Attribute parsing
// ---------------------------------------------------------------------------

struct RpcArgs {
    prefix: String,
}

impl Parse for RpcArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // prefix = "characters"
        let key: Ident = input.parse()?;
        if key != "prefix" {
            return Err(syn::Error::new(key.span(), "expected `prefix = \"...\"`"));
        }
        input.parse::<Token![=]>()?;
        let lit: LitStr = input.parse()?;
        Ok(RpcArgs {
            prefix: lit.value(),
        })
    }
}

/// One method's `#[http(...)]` binding.
#[derive(Default)]
struct HttpBind {
    verb: String,
    path: String,
    /// `AuthReq` variant ident: `None` or `Player`.
    auth: String,
    success: u16,
    /// param name -> path wildcard name.
    path_args: BTreeMap<String, String>,
    /// param name -> external body JSON key.
    body_names: BTreeMap<String, String>,
}

fn parse_http(attr: &syn::Attribute) -> syn::Result<HttpBind> {
    let mut b = HttpBind::default();
    let mut seen_auth = false;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("verb") {
            b.verb = meta.value()?.parse::<LitStr>()?.value();
        } else if meta.path.is_ident("path") {
            b.path = meta.value()?.parse::<LitStr>()?.value();
        } else if meta.path.is_ident("success") {
            b.success = meta.value()?.parse::<LitInt>()?.base10_parse()?;
        } else if meta.path.is_ident("auth") {
            let v = meta.value()?.parse::<LitStr>()?.value();
            b.auth = match v.as_str() {
                "none" => "None".to_string(),
                "player" => "Player".to_string(),
                other => {
                    return Err(meta.error(format!(
                        "auth must be \"none\" or \"player\", got {other:?}"
                    )))
                }
            };
            seen_auth = true;
        } else if meta.path.is_ident("path_args") {
            meta.parse_nested_meta(|inner| {
                let param = inner
                    .path
                    .get_ident()
                    .ok_or_else(|| inner.error("path_args key must be a bare param name"))?
                    .to_string();
                let wild = inner.value()?.parse::<LitStr>()?.value();
                b.path_args.insert(param, wild);
                Ok(())
            })?;
        } else if meta.path.is_ident("body_names") {
            meta.parse_nested_meta(|inner| {
                let param = inner
                    .path
                    .get_ident()
                    .ok_or_else(|| inner.error("body_names key must be a bare param name"))?
                    .to_string();
                let key = inner.value()?.parse::<LitStr>()?.value();
                b.body_names.insert(param, key);
                Ok(())
            })?;
        } else {
            return Err(meta.error("unknown #[http(...)] key"));
        }
        Ok(())
    })?;
    if b.verb.is_empty() || b.path.is_empty() || !seen_auth || b.success == 0 {
        return Err(syn::Error::new(
            attr.span(),
            "#[http(...)] requires verb, path, auth and success",
        ));
    }
    Ok(b)
}

// ---------------------------------------------------------------------------
// Method model
// ---------------------------------------------------------------------------

struct Arg {
    ident: Ident,
    ty: Type,
    /// Set when this arg is sourced from a path wildcard (`path_args`).
    wildcard: Option<String>,
    /// External JSON key when it differs from the param name (`body_names`).
    rename: Option<String>,
}

struct MethodModel {
    /// The Go-style Pascal name, e.g. `OwnerOf` (for type names).
    pascal: String,
    /// The wire method string, e.g. `characters.ownerOf`.
    wire: String,
    /// The full method signature (minus `#[http]`), reproduced for the client impl.
    sig: Signature,
    method_ident: Ident,
    has_identity: bool,
    id_ident: Option<Ident>,
    args: Vec<Arg>,
    /// `Some(T)` for a `Result<T, _>` where `T != ()`.
    value_ty: Option<Type>,
    http: Option<HttpBind>,
}

fn build_method(
    prefix: &str,
    f: &mut syn::TraitItemFn,
) -> syn::Result<MethodModel> {
    // Pull off (and remove) an `#[http(...)]` attribute so the re-emitted trait is
    // clean (`http` is not a registered attribute).
    let mut http = None;
    let mut kept = Vec::new();
    for attr in f.attrs.drain(..) {
        if attr.path().is_ident("http") {
            http = Some(parse_http(&attr)?);
        } else {
            kept.push(attr);
        }
    }
    f.attrs = kept;

    let sig = f.sig.clone();
    let method_ident = sig.ident.clone();
    let name = method_ident.to_string();

    let mut inputs = sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(_)) => {}
        _ => {
            return Err(syn::Error::new(
                sig.span(),
                "rpc method must take &self as its first parameter",
            ))
        }
    }

    let http_ref = http.as_ref();
    let mut has_identity = false;
    let mut id_ident = None;
    let mut args = Vec::new();
    for (i, input) in inputs.enumerate() {
        let pt = match input {
            FnArg::Typed(pt) => pt,
            FnArg::Receiver(_) => {
                return Err(syn::Error::new(input.span(), "unexpected self parameter"))
            }
        };
        let ident = match &*pt.pat {
            Pat::Ident(pi) => pi.ident.clone(),
            _ => {
                return Err(syn::Error::new(
                    pt.pat.span(),
                    "rpc method parameters must be simple identifiers",
                ))
            }
        };
        // The first parameter, if typed `Identity`, is the caller identity.
        if i == 0 && is_identity_type(&pt.ty) {
            has_identity = true;
            id_ident = Some(ident);
            continue;
        }
        let pname = ident.to_string();
        let wildcard = http_ref.and_then(|b| b.path_args.get(&pname).cloned());
        let rename = http_ref.and_then(|b| b.body_names.get(&pname).cloned());
        args.push(Arg {
            ident,
            ty: (*pt.ty).clone(),
            wildcard,
            rename,
        });
    }

    let value_ty = result_ok_type(&sig.output)?;

    Ok(MethodModel {
        pascal: to_pascal(&name),
        wire: format!("{prefix}.{}", to_lower_camel(&name)),
        sig,
        method_ident,
        has_identity,
        id_ident,
        args,
        value_ty,
        http,
    })
}

/// `true` when the type's final path segment is `Identity` (bare, or `opsapi::`-
/// qualified). This is the identity-convention recogniser.
fn is_identity_type(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident == "Identity";
        }
    }
    false
}

/// Extracts `T` from a `Result<T, _>` return type; `None` when `T` is `()`.
fn result_ok_type(output: &ReturnType) -> syn::Result<Option<Type>> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                output.span(),
                "rpc method must return Result<T, opsapi::Error>",
            ))
        }
    };
    let tp = match ty {
        Type::Path(tp) => tp,
        _ => return Err(syn::Error::new(ty.span(), "rpc method must return Result<..>")),
    };
    let seg = tp
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new(ty.span(), "rpc method must return Result<..>"))?;
    if seg.ident != "Result" {
        return Err(syn::Error::new(
            seg.ident.span(),
            "rpc method must return Result<T, opsapi::Error>",
        ));
    }
    let ok = match &seg.arguments {
        PathArguments::AngleBracketed(ab) => ab.args.first().and_then(|a| match a {
            GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        }),
        _ => None,
    }
    .ok_or_else(|| syn::Error::new(seg.span(), "Result must have a type argument"))?;

    // Unit `()` → no value field.
    if let Type::Tuple(tup) = &ok {
        if tup.elems.is_empty() {
            return Ok(None);
        }
    }
    Ok(Some(ok))
}

// ---------------------------------------------------------------------------
// Expansion
// ---------------------------------------------------------------------------

fn expand(args: RpcArgs, mut item_trait: ItemTrait) -> syn::Result<TokenStream2> {
    let trait_ident = item_trait.ident.clone();
    let vis = item_trait.vis.clone();
    let module_ident = format_ident!("{}_rpc", to_snake(&trait_ident.to_string()));

    // Build a model per method, stripping `#[http]` from the trait as we go.
    let mut methods = Vec::new();
    for it in item_trait.items.iter_mut() {
        if let TraitItem::Fn(f) = it {
            methods.push(build_method(&args.prefix, f)?);
        }
    }
    // Deterministic emission order (Go sorts by method name).
    methods.sort_by(|a, b| a.pascal.cmp(&b.pascal));

    let consts = methods.iter().map(gen_const);
    let req_structs = methods.iter().map(gen_request_struct);
    let resp_structs = methods.iter().map(gen_response_struct);
    let client_methods = methods.iter().map(gen_client_method).collect::<Vec<_>>();
    let adapters = methods.iter().map(gen_server_adapter).collect::<Vec<_>>();

    let bound: Vec<&MethodModel> = methods.iter().filter(|m| m.http.is_some()).collect();
    let opset_pushes = bound.iter().map(|m| gen_opset_push(m)).collect::<Vec<_>>();
    let route_pushes = bound.iter().map(|m| gen_route_push(m)).collect::<Vec<_>>();

    let expanded = quote! {
        #item_trait

        #[doc = "Generated RPC glue (see `rpc_macro`). One module per `#[rpc]` trait."]
        #vis mod #module_ident {
            #![allow(
                clippy::all,
                clippy::pedantic,
                clippy::nursery,
                dead_code,
                unused_imports,
                unused_variables,
                non_snake_case,
                non_upper_case_globals
            )]
            use super::*;

            // Method-name consts (wire identifiers).
            #(#consts)*

            // Per-method wire envelopes.
            #(#req_structs)*
            #(#resp_structs)*

            /// Implements the source trait over an `opsapi::Caller`, marshalling each
            /// call into its wire envelope. The split-topology client; in the
            /// monolith the real service is called directly.
            pub struct Client {
                caller: ::std::sync::Arc<dyn ::opsapi::Caller>,
            }

            impl Client {
                /// Returns a `Client` that calls through `caller`.
                pub fn new(caller: ::std::sync::Arc<dyn ::opsapi::Caller>) -> Self {
                    Client { caller }
                }
            }

            #[::async_trait::async_trait]
            impl #trait_ident for Client {
                #(#client_methods)*
            }

            /// Installs one edge identity-adapter per method of `impl_` onto `server`.
            /// Each adapter deserialises the request, reconstructs the caller identity
            /// from the (mutually authenticated) envelope — the trust boundary,
            /// identity is read ONLY from the envelope, never a client-supplied body
            /// field — calls the impl, and marshals the `{status, err, value}`
            /// response envelope (folding an `opsapi::Error` into `status`/`err`).
            pub fn register_server(
                server: &mut ::edge::Server,
                impl_: ::std::sync::Arc<dyn #trait_ident + ::core::marker::Send + ::core::marker::Sync>,
            ) {
                #(#adapters)*
            }

            /// The gateway contributions for each `#[http]`-bound method of `impl_`:
            /// each `OpSet` pairs the static `Operation` (route/auth/success) with its
            /// `OpBinding` (decode/encode) and `LocalOp` (in-process invoker). A
            /// module contributes these to the `opsapi` slots; LocalBackend and
            /// RemoteBackend then consume the SAME wire envelopes. Deterministic
            /// order (methods sorted by name).
            pub fn operations(
                impl_: ::std::sync::Arc<dyn #trait_ident + ::core::marker::Send + ::core::marker::Sync>,
            ) -> ::std::vec::Vec<::opsapi::OpSet> {
                let mut __ops: ::std::vec::Vec<::opsapi::OpSet> = ::std::vec::Vec::new();
                #(#opset_pushes)*
                __ops
            }

            /// The impl-free route table for each `#[http]`-bound method: the
            /// `Operation` + its `OpBinding`, with NO `LocalOp`. A remote-only front
            /// door (which hosts no module) builds its route table from this and
            /// dispatches each op over the edge.
            pub fn route_bindings() -> ::std::vec::Vec<::opsapi::RouteBinding> {
                let mut __rb: ::std::vec::Vec<::opsapi::RouteBinding> = ::std::vec::Vec::new();
                #(#route_pushes)*
                __rb
            }
        }
    };
    Ok(expanded)
}

fn gen_const(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let wire = &m.wire;
    quote! {
        pub const #const_ident: &str = #wire;
    }
}

fn gen_request_struct(m: &MethodModel) -> TokenStream2 {
    let name = format_ident!("{}Request", m.pascal);
    let fields = m.args.iter().map(|a| {
        let ident = &a.ident;
        let ty = &a.ty;
        // A body arg with an overridden external key gets a serde rename; a path arg
        // keeps its param-name key (wire-internal, populated by decode).
        let rename = a
            .rename
            .as_ref()
            .map(|k| quote! { #[serde(rename = #k)] });
        quote! { #rename pub #ident: #ty, }
    });
    // Bound methods derive Default so `decode` can build from partial input (path
    // wildcards absent from the body). `#[serde(default)]` makes a missing body
    // field zero-valued (matching Go's lenient json.Unmarshal).
    let extra = if m.http.is_some() {
        quote! { #[derive(::core::default::Default)] #[serde(default)] }
    } else {
        quote! {}
    };
    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize)]
        #extra
        pub struct #name {
            #(#fields)*
        }
    }
}

fn gen_response_struct(m: &MethodModel) -> TokenStream2 {
    let name = format_ident!("{}Response", m.pascal);
    // The value is carried as a raw `serde_json::Value` (defaulting to `null`), NOT a
    // typed `Option<T>`. A typed `Option<T>` collapses `Some(None)` → `null` → `None`
    // on the round-trip, so a method returning `Option<U>` (e.g. `owner_of` →
    // `Ok(None)`) could not be distinguished from a missing value — the client would
    // mistake a legitimate `None` for a transport/internal error. A raw `Value`
    // preserves `null` faithfully; the client deserializes it into the method's real
    // return type (where `null` → `None` for an `Option` return). Wire bytes are
    // identical to the old typed field for every non-`None` case.
    let value_field = m.value_ty.as_ref().map(|_| {
        quote! {
            #[serde(default)]
            pub value: ::serde_json::Value,
        }
    });
    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize)]
        pub struct #name {
            pub status: ::opsapi::Status,
            #[serde(default, skip_serializing_if = "::std::string::String::is_empty")]
            pub err: ::std::string::String,
            #value_field
        }
    }
}

fn gen_client_method(m: &MethodModel) -> TokenStream2 {
    let sig = &m.sig;
    let const_ident = method_const_ident(m);
    let req_name = format_ident!("{}Request", m.pascal);
    let resp_name = format_ident!("{}Response", m.pascal);

    let field_idents: Vec<&Ident> = m.args.iter().map(|a| &a.ident).collect();
    let build_req = quote! { #req_name { #(#field_idents: #field_idents),* } };

    let identity_expr = match &m.id_ident {
        Some(id) => quote! { #id.player_id() },
        None => quote! { ::core::option::Option::None },
    };

    let ret_expr = if let Some(vty) = &m.value_ty {
        // `status == Ok` was already checked above, so `resp.value` carries the real
        // return value (possibly `null` for an `Option` return). Deserialize the raw
        // `Value` into the method's declared type — `null` → `None` faithfully.
        quote! {
            let __ret: #vty = ::serde_json::from_value(resp.value)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            ::core::result::Result::Ok(__ret)
        }
    } else {
        quote! { ::core::result::Result::Ok(()) }
    };

    quote! {
        #sig {
            let __req = #build_req;
            let __payload = ::serde_json::to_vec(&__req)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            let __resp_bytes = self.caller.call(#const_ident, #identity_expr, &__payload).await?;
            let resp: #resp_name = ::serde_json::from_slice(&__resp_bytes)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            if resp.status != ::opsapi::Status::Ok {
                return ::core::result::Result::Err(::opsapi::Error::new(resp.status, resp.err));
            }
            #ret_expr
        }
    }
}

/// The `Ok`/`Err` match arms building the response envelope from the impl's
/// `Result`, shared by the server adapter and the local invoker.
fn response_arms(m: &MethodModel) -> TokenStream2 {
    let resp_name = format_ident!("{}Response", m.pascal);
    if m.value_ty.is_some() {
        quote! {
            ::core::result::Result::Ok(__v) => #resp_name {
                status: ::opsapi::Status::Ok,
                err: ::std::string::String::new(),
                // Serialize the return value into the envelope's raw `Value`. This
                // preserves `None` for an `Option` return as JSON `null` (rather than
                // collapsing it into a missing field). Serialization of a well-formed
                // `Serialize` domain type is infallible; a `null` fallback is a safe
                // floor that the client would surface as an internal error.
                value: ::serde_json::to_value(&__v).unwrap_or(::serde_json::Value::Null),
            },
            ::core::result::Result::Err(__e) => #resp_name {
                status: __e.status,
                err: __e.msg,
                value: ::serde_json::Value::Null,
            },
        }
    } else {
        quote! {
            ::core::result::Result::Ok(()) => #resp_name {
                status: ::opsapi::Status::Ok,
                err: ::std::string::String::new(),
            },
            ::core::result::Result::Err(__e) => #resp_name {
                status: __e.status,
                err: __e.msg,
            },
        }
    }
}

fn gen_server_adapter(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let req_name = format_ident!("{}Request", m.pascal);
    let method = &m.method_ident;

    let field_idents: Vec<&Ident> = m.args.iter().map(|a| &a.ident).collect();
    let call = if m.has_identity {
        quote! { __impl.#method(__id, #(__req.#field_idents),*).await }
    } else {
        quote! { __impl.#method(#(__req.#field_idents),*).await }
    };
    let arms = response_arms(m);

    quote! {
        {
            let __impl = impl_.clone();
            let __h: ::edge::IdentityHandler = ::std::sync::Arc::new(
                move |__identity: ::core::option::Option<::std::string::String>, __payload: ::std::vec::Vec<u8>| {
                    let __impl = __impl.clone();
                    ::std::boxed::Box::pin(async move {
                        let __req: #req_name = ::serde_json::from_slice(&__payload)?;
                        let __id = ::opsapi::Identity::player(__identity.unwrap_or_default());
                        let __result = #call;
                        let __resp = match __result { #arms };
                        ::core::result::Result::Ok(::serde_json::to_vec(&__resp)?)
                    })
                },
            );
            server.handle_identity(#const_ident, __h);
        }
    }
}

/// The `decode` closure (HTTP body + path wildcards -> wire request bytes).
fn gen_decode(m: &MethodModel) -> TokenStream2 {
    let req_name = format_ident!("{}Request", m.pascal);
    let has_body = m.args.iter().any(|a| a.wildcard.is_none());
    let body_read = if has_body {
        quote! {
            if let ::core::option::Option::Some(__b) = __body {
                if !__b.is_empty() {
                    __req = ::serde_json::from_slice(__b)
                        .map_err(|_| ::opsapi::Error::invalid("invalid json"))?;
                }
            }
        }
    } else {
        quote! {}
    };
    let path_sets = m.args.iter().filter_map(|a| {
        a.wildcard.as_ref().map(|w| {
            let ident = &a.ident;
            quote! { __req.#ident = __path.get(#w).cloned().unwrap_or_default(); }
        })
    });
    quote! {
        let __decode: ::opsapi::DecodeFn = ::std::sync::Arc::new(
            |__body: ::core::option::Option<&[u8]>, __path: &::opsapi::PathArgs| {
                let mut __req = #req_name::default();
                #body_read
                #(#path_sets)*
                ::serde_json::to_vec(&__req)
                    .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))
            },
        );
    }
}

/// The `encode` closure (wire response bytes -> external HTTP body + Status).
fn gen_encode(m: &MethodModel) -> TokenStream2 {
    let resp_name = format_ident!("{}Response", m.pascal);
    let ok_body = if m.value_ty.is_some() {
        quote! {
            let __body = ::serde_json::to_vec(&__r.value)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            ::core::result::Result::Ok((::core::option::Option::Some(__body), ::opsapi::Status::Ok))
        }
    } else {
        quote! {
            ::core::result::Result::Ok((::core::option::Option::None, ::opsapi::Status::Ok))
        }
    };
    quote! {
        let __encode: ::opsapi::EncodeFn = ::std::sync::Arc::new(|__resp: &[u8]| {
            let __r: #resp_name = ::serde_json::from_slice(__resp)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            if __r.status != ::opsapi::Status::Ok {
                return ::core::result::Result::Err(::opsapi::Error::new(__r.status, __r.err));
            }
            #ok_body
        });
    }
}

fn gen_operation_literal(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let b = m.http.as_ref().expect("bound method");
    let verb = &b.verb;
    let path = &b.path;
    let success = b.success;
    let auth = format_ident!("{}", b.auth);
    quote! {
        ::opsapi::Operation {
            method: #const_ident.to_string(),
            verb: #verb.to_string(),
            path: #path.to_string(),
            auth: ::opsapi::AuthReq::#auth,
            success: #success,
        }
    }
}

fn gen_opset_push(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let req_name = format_ident!("{}Request", m.pascal);
    let method = &m.method_ident;
    let decode = gen_decode(m);
    let encode = gen_encode(m);
    let operation = gen_operation_literal(m);

    let field_idents: Vec<&Ident> = m.args.iter().map(|a| &a.ident).collect();
    let call = if m.has_identity {
        quote! { __impl.#method(__identity, #(__req.#field_idents),*).await }
    } else {
        quote! { __impl.#method(#(__req.#field_idents),*).await }
    };
    let arms = response_arms(m);

    quote! {
        {
            #decode
            #encode
            let __invoke: ::opsapi::LocalInvoker = {
                let __impl = impl_.clone();
                ::std::sync::Arc::new(move |__identity: ::opsapi::Identity, __req_bytes: ::std::vec::Vec<u8>| {
                    let __impl = __impl.clone();
                    ::std::boxed::Box::pin(async move {
                        let __req: #req_name = ::serde_json::from_slice(&__req_bytes)
                            .map_err(|__e| ::opsapi::Error::invalid(__e.to_string()))?;
                        let __result = #call;
                        let __resp = match __result { #arms };
                        ::serde_json::to_vec(&__resp)
                            .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))
                    })
                })
            };
            __ops.push(::opsapi::OpSet {
                operation: #operation,
                binding: ::opsapi::OpBinding {
                    method: #const_ident.to_string(),
                    decode: __decode,
                    encode: __encode,
                },
                local: ::opsapi::LocalOp {
                    method: #const_ident.to_string(),
                    invoke: __invoke,
                },
            });
        }
    }
}

fn gen_route_push(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let decode = gen_decode(m);
    let encode = gen_encode(m);
    let operation = gen_operation_literal(m);
    quote! {
        {
            #decode
            #encode
            __rb.push(::opsapi::RouteBinding {
                operation: #operation,
                binding: ::opsapi::OpBinding {
                    method: #const_ident.to_string(),
                    decode: __decode,
                    encode: __encode,
                },
            });
        }
    }
}

fn method_const_ident(m: &MethodModel) -> Ident {
    format_ident!("METHOD_{}", to_upper_snake(&m.method_ident.to_string()))
}

// ---------------------------------------------------------------------------
// Name conversions
// ---------------------------------------------------------------------------

/// `owner_of` -> `OwnerOf`.
fn to_pascal(snake: &str) -> String {
    snake
        .split('_')
        .filter(|s| !s.is_empty())
        .map(capitalize)
        .collect()
}

/// `owner_of` -> `ownerOf`.
fn to_lower_camel(snake: &str) -> String {
    let pascal = to_pascal(snake);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `owner_of` -> `OWNER_OF`.
fn to_upper_snake(snake: &str) -> String {
    snake.to_uppercase()
}

/// `OwnerOf` (Pascal) -> `owner_of` (snake). Used for the module name.
fn to_snake(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

//! `rpc-macro` — the `#[rpc]` codegen (the Rust port of Go's `tools/rpcgen`).
//!
//! Since the Step-2 fortress refactor the codegen is SPLIT across two macros so a
//! domain's contract crate stays pure (no `edge` dependency) while its transport
//! glue lives in a sibling crate:
//!
//! ## `#[rpc(prefix = "...")]` — applied in the `<name>api` crate
//!
//! An attribute macro applied to a **capability trait**. It emits, into a child
//! module `<snake(trait)>_rpc`, the TRANSPORT-FREE half:
//!
//!   - a wire **request** struct per method (one field per non-identity arg),
//!   - a wire **response** struct per method carrying `{ status, err, value? }` where
//!     `status: opsapi::Status` is the DOMAIN outcome riding INSIDE the payload
//!     envelope (an edge-level `ok:false` is a separate *transport* failure),
//!   - `METHOD_<NAME>: &str = "<prefix>.<lowerCamel(name)>"` consts,
//!   - for `#[http(...)]`-annotated methods only, `operations(impl) ->
//!     Vec<opsapi::OpSet>` and `route_bindings() -> Vec<opsapi::RouteBinding>` (the
//!     decode/encode/local glue the gateway routes over),
//!   - for EVERY method (http-bound and wire-only), `wire_ops() ->
//!     Vec<opsapi::WireOp>` — each method's name + `RetryMode`, so a wire-only
//!     method's `#[retry_safe]` surfaces as a contract-golden value.
//!
//! It ALSO emits a `#[macro_export] macro_rules! <prefix>_<snake(trait)>_meta`
//! **metadata-callback macro**: a proc macro cannot re-parse another crate (and the
//! re-emitted trait has its `#[http]` attrs stripped), so the full pre-strip method
//! metadata is carried as a token tree that the callback macro hands to a
//! caller-supplied macro — the standard token-tree callback pattern.
//!
//! ## `generate_glue!` — invoked in the `<name>rpc` crate
//!
//! The glue crate contains one line per trait:
//! `charactersapi::characters_ownership_meta!(rpc_macro::generate_glue);`
//! which expands to the EDGE-DEPENDENT half, in a module of the same
//! `<snake(trait)>_rpc` name (which also re-exports the pure module's contents, so
//! `<name>rpc::<snake>_rpc::*` is a superset of the api crate's module):
//!
//!   - a `Client` over `Arc<dyn opsapi::Caller>` implementing the source trait,
//!   - `register_server(&mut edge::Server, Arc<dyn Trait>)` installing one
//!     `handle_identity` adapter per method,
//!   - `provide_remote(&registry::Registry, Arc<dyn opsapi::Caller>)` — provides the
//!     generated `Client` under the capability's canonical key
//!     (`registry::key(prefix, snake_trait)`); the building block for each glue
//!     crate's hand-written `remote_factories()` (the Step-4 generic-`remote` seam).
//!
//! The glue crate's `lib.rs` must `use <name>api::*;` (plus `opsapi::{Error,
//! Identity}`) so the signatures' domain types resolve at the invocation site.
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
use rpc_contract_model::{HttpBind, RpcArgs};
use syn::{
    parse::{Parse, ParseStream},
    Ident, ItemTrait, LitStr, Token,
};

/// The `#[rpc(prefix = "...")]` attribute (the pure, transport-free half — see the
/// crate docs).
#[proc_macro_attribute]
pub fn rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as RpcArgs);
    // Snapshot the ORIGINAL tokens (with `#[http]` still attached) before parsing:
    // this is the metadata the callback macro carries to `generate_glue!`.
    let original: TokenStream2 = TokenStream2::from(item.clone());
    let item_trait = syn::parse_macro_input!(item as ItemTrait);
    match expand(args, original, item_trait) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// `generate_glue!` — the edge-dependent half, expanded in the `<name>rpc` crate via
/// a `<name>api` metadata-callback macro (see the crate docs). Input shape:
///
/// ```ignore
/// prefix = "characters", api = charactersapi, #[...] pub trait Ownership { ... }
/// ```
#[proc_macro]
pub fn generate_glue(input: TokenStream) -> TokenStream {
    let glue = syn::parse_macro_input!(input as GlueInput);
    match expand_glue(glue) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------------
// Expansion
// ---------------------------------------------------------------------------

struct MethodModel {
    pascal: String,
    wire: String,
    sig: syn::Signature,
    method_ident: Ident,
    has_identity: bool,
    id_ident: Option<Ident>,
    args: Vec<rpc_contract_model::Arg>,
    value_ty: Option<syn::Type>,
    http: Option<HttpBind>,
    retry_safe: bool,
}

fn parse_methods(prefix: &str, item_trait: &mut ItemTrait) -> syn::Result<Vec<MethodModel>> {
    rpc_contract_model::build_methods(item_trait).map(|methods| {
        methods
            .into_iter()
            .map(|method| {
                let name = method.method_ident.to_string();
                MethodModel {
                    pascal: to_pascal(&name),
                    wire: format!("{prefix}.{}", to_lower_camel(&name)),
                    sig: method.sig,
                    method_ident: method.method_ident,
                    has_identity: method.has_identity,
                    id_ident: method.id_ident,
                    args: method.args,
                    value_ty: method.value_ty,
                    http: method.http,
                    retry_safe: method.retry_safe,
                }
            })
            .collect()
    })
}

/// The `#[rpc]` expansion: the cleaned trait, the pure `<snake>_rpc` module, and the
/// metadata-callback macro carrying the pre-strip tokens to `generate_glue!`.
fn expand(
    args: RpcArgs,
    original: TokenStream2,
    mut item_trait: ItemTrait,
) -> syn::Result<TokenStream2> {
    let trait_ident = item_trait.ident.clone();
    let vis = item_trait.vis.clone();
    let snake = to_snake(&trait_ident.to_string());
    let module_ident = format_ident!("{}_rpc", snake);

    // Build a model per method, stripping `#[http]` from the trait as we go.
    let mut methods = parse_methods(&args.prefix, &mut item_trait)?;
    // Deterministic emission order (Go sorts by method name).
    methods.sort_by(|a, b| a.pascal.cmp(&b.pascal));

    let consts = methods.iter().map(gen_const);
    let req_structs = methods.iter().map(gen_request_struct);
    let resp_structs = methods.iter().map(gen_response_struct);

    let bound: Vec<&MethodModel> = methods.iter().filter(|m| m.http.is_some()).collect();
    let opset_pushes = bound.iter().map(|m| gen_opset_push(m)).collect::<Vec<_>>();
    let route_pushes = bound.iter().map(|m| gen_route_push(m)).collect::<Vec<_>>();
    // wire_ops covers EVERY method (http-bound and wire-only), not just `bound`.
    let wire_op_literals = methods.iter().map(gen_wire_op_literal).collect::<Vec<_>>();

    // The metadata-callback macro. `$crate` cannot name this crate from a proc
    // macro, so the api crate's name comes from the build env (Cargo sets
    // CARGO_PKG_NAME for the crate currently compiling — i.e. the api crate).
    let meta_ident = format_ident!("{}_{}_meta", sanitize_ident(&args.prefix), snake);
    let api_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| syn::Error::new(trait_ident.span(), "CARGO_PKG_NAME not set"))?
        .replace('-', "_");
    let api_ident = format_ident!("{}", api_name);
    let prefix_lit = &args.prefix;
    // A literal `$` token for the macro_rules matcher/transcriber (quote! cannot
    // spell `$` directly).
    let d = proc_macro2::Punct::new('$', proc_macro2::Spacing::Alone);

    let expanded = quote! {
        #item_trait

        #[doc = "Generated transport-free RPC surface (see `rpc_macro`). One module per `#[rpc]` trait."]
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

            /// Every method's transport replay policy (`WireOp`), http-bound AND
            /// wire-only — unlike `operations`/`route_bindings`, which cover only
            /// `#[http]` methods. This surfaces a wire-only method's `#[retry_safe]`
            /// (otherwise compiled solely into the client's `RetryMode`) as a
            /// contract-golden value. Impl-free; deterministic order (methods sorted
            /// by name).
            pub fn wire_ops() -> ::std::vec::Vec<::opsapi::WireOp> {
                ::std::vec![ #(#wire_op_literals),* ]
            }
        }

        /// Metadata-callback macro (see `rpc_macro`): hands this trait's FULL
        /// pre-strip metadata token tree (names, signatures, `#[http]` attrs) to a
        /// caller-supplied macro. The `<name>rpc` glue crate invokes it as
        /// `<api>::<this>!(rpc_macro::generate_glue);` — a proc macro cannot re-parse
        /// another crate, so the tokens travel by macro expansion instead.
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #meta_ident {
            ( #d ( #d cb:tt )* ) => {
                #d ( #d cb )* ! {
                    prefix = #prefix_lit,
                    api = #api_ident,
                    #original
                }
            };
        }
    };
    Ok(expanded)
}

// ---------------------------------------------------------------------------
// The glue half (`generate_glue!`), expanded in the `<name>rpc` crate.
// ---------------------------------------------------------------------------

/// Parsed `generate_glue!` input: `prefix = "...", api = <crate ident>, <trait>`.
struct GlueInput {
    prefix: String,
    api: Ident,
    item_trait: ItemTrait,
}

impl Parse for GlueInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "prefix" {
            return Err(syn::Error::new(key.span(), "expected `prefix = \"...\"`"));
        }
        input.parse::<Token![=]>()?;
        let prefix: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let key: Ident = input.parse()?;
        if key != "api" {
            return Err(syn::Error::new(
                key.span(),
                "expected `api = <crate ident>`",
            ));
        }
        input.parse::<Token![=]>()?;
        let api: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let item_trait: ItemTrait = input.parse()?;
        Ok(GlueInput {
            prefix: prefix.value(),
            api,
            item_trait,
        })
    }
}

/// Emits the edge-dependent glue module: `Client`, `register_server`,
/// `provide_remote` — plus a `pub use` of the api crate's pure module so
/// `<name>rpc::<snake>_rpc::*` is a superset of `<name>api::<snake>_rpc::*`.
fn expand_glue(glue: GlueInput) -> syn::Result<TokenStream2> {
    let mut item_trait = glue.item_trait;
    let trait_ident = item_trait.ident.clone();
    let snake = to_snake(&trait_ident.to_string());
    let module_ident = format_ident!("{}_rpc", snake);
    let api = &glue.api;
    // Fully-qualified paths into the api crate: the trait itself and the pure module
    // holding the wire envelopes + consts (`#qual #name` concatenates into a path).
    let trait_path = quote! { ::#api::#trait_ident };
    let qual = quote! { ::#api::#module_ident:: };

    let mut methods = parse_methods(&glue.prefix, &mut item_trait)?;
    methods.sort_by(|a, b| a.pascal.cmp(&b.pascal));

    let client_methods = methods
        .iter()
        .map(|m| gen_client_method(m, &qual))
        .collect::<Vec<_>>();
    let adapters = methods
        .iter()
        .map(|m| gen_server_adapter(m, &qual))
        .collect::<Vec<_>>();

    let prefix_lit = &glue.prefix;
    let snake_lit = &snake;

    let expanded = quote! {
        #[doc = "Generated transport glue (see `rpc_macro`). One module per `#[rpc]` trait; re-exports the api crate's pure module."]
        pub mod #module_ident {
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

            // The transport-free surface (consts, wire envelopes, operations,
            // route_bindings) stays generated in the api crate; re-export it so this
            // glue module remains a drop-in superset of the old fused module.
            pub use #qual *;

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
            impl #trait_path for Client {
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
                impl_: ::std::sync::Arc<dyn #trait_path + ::core::marker::Send + ::core::marker::Sync>,
            ) {
                #(#adapters)*
            }

            /// Provides the generated [`Client`] under this capability's canonical
            /// registry key (`registry::key(prefix, snake_trait)`), so a co-hosted
            /// consumer's `require::<dyn Trait>` resolves to the edge-backed client.
            /// The building block for the glue crate's `remote_factories()`.
            pub fn provide_remote(
                reg: &::registry::Registry,
                caller: ::std::sync::Arc<dyn ::opsapi::Caller>,
            ) {
                let __client: ::std::sync::Arc<dyn #trait_path> =
                    ::std::sync::Arc::new(Client::new(caller));
                reg.provide::<dyn #trait_path>(::registry::key(#prefix_lit, #snake_lit), __client);
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
        let rename = a.rename.as_ref().map(|k| quote! { #[serde(rename = #k)] });
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

/// `qual` is the path prefix for the wire envelopes + consts: empty when they are
/// module-local (the pure emission), `::<api>::<snake>_rpc::` in the glue emission.
fn gen_client_method(m: &MethodModel, qual: &TokenStream2) -> TokenStream2 {
    let sig = &m.sig;
    let const_ident = method_const_ident(m);
    let req_name = format_ident!("{}Request", m.pascal);
    let resp_name = format_ident!("{}Response", m.pascal);

    let field_idents: Vec<&Ident> = m.args.iter().map(|a| &a.ident).collect();
    let build_req = quote! { #qual #req_name { #(#field_idents: #field_idents),* } };

    let identity_expr = match &m.id_ident {
        Some(id) => quote! { #id.player_id() },
        None => quote! { ::core::option::Option::None },
    };
    let retry_mode = if m.retry_safe {
        quote! { ::opsapi::RetryMode::OnceAfterReconnect }
    } else {
        quote! { ::opsapi::RetryMode::Never }
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
            let __resp_bytes = self.caller.call(#qual #const_ident, #identity_expr, &__payload, #retry_mode).await?;
            let resp: #qual #resp_name = ::serde_json::from_slice(&__resp_bytes)
                .map_err(|__e| ::opsapi::Error::internal(__e.to_string()))?;
            if resp.status != ::opsapi::Status::Ok {
                return ::core::result::Result::Err(::opsapi::Error::new(resp.status, resp.err));
            }
            #ret_expr
        }
    }
}

/// The `Ok`/`Err` match arms building the response envelope from the impl's
/// `Result`, shared by the server adapter and the local invoker. `qual` as in
/// [`gen_client_method`].
fn response_arms(m: &MethodModel, qual: &TokenStream2) -> TokenStream2 {
    let resp_name = format_ident!("{}Response", m.pascal);
    let resp_name = quote! { #qual #resp_name };
    if m.value_ty.is_some() {
        quote! {
            ::core::result::Result::Ok(__v) => match ::serde_json::to_value(&__v) {
                ::core::result::Result::Ok(__value) => #resp_name {
                    status: ::opsapi::Status::Ok,
                    err: ::std::string::String::new(),
                    // `None` deliberately becomes JSON `null` while retaining an OK
                    // status, so a legitimate `Ok(None)` survives the envelope.
                    value: __value,
                },
                ::core::result::Result::Err(__e) => #resp_name {
                    // Serialization is a server-side failure, not a successful null
                    // response and not an edge transport failure. Keep it inside the
                    // ordinary domain envelope so local and remote paths agree.
                    status: ::opsapi::Status::Internal,
                    err: ::std::format!("response serialization failed: {__e}"),
                    value: ::serde_json::Value::Null,
                },
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

/// `qual` as in [`gen_client_method`].
fn gen_server_adapter(m: &MethodModel, qual: &TokenStream2) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let req_name = format_ident!("{}Request", m.pascal);
    let method = &m.method_ident;

    let field_idents: Vec<&Ident> = m.args.iter().map(|a| &a.ident).collect();
    let call = if m.has_identity {
        quote! { __impl.#method(__id, #(__req.#field_idents),*).await }
    } else {
        quote! { __impl.#method(#(__req.#field_idents),*).await }
    };
    let arms = response_arms(m, qual);

    quote! {
        {
            let __impl = impl_.clone();
            let __h: ::edge::IdentityHandler = ::std::sync::Arc::new(
                move |__identity: ::core::option::Option<::std::string::String>, __payload: ::std::vec::Vec<u8>| {
                    let __impl = __impl.clone();
                    ::std::boxed::Box::pin(async move {
                        let __req: #qual #req_name = ::serde_json::from_slice(&__payload)?;
                        let __id = ::opsapi::Identity::player(__identity.unwrap_or_default());
                        let __result = #call;
                        let __resp = match __result { #arms };
                        ::core::result::Result::Ok(::serde_json::to_vec(&__resp)?)
                    })
                },
            );
            server.handle_identity(#qual #const_ident, __h);
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
    let retry_mode = if m.retry_safe {
        quote! { ::opsapi::RetryMode::OnceAfterReconnect }
    } else {
        quote! { ::opsapi::RetryMode::Never }
    };
    quote! {
        ::opsapi::Operation {
            method: #const_ident.to_string(),
            verb: #verb.to_string(),
            path: #path.to_string(),
            auth: ::opsapi::AuthReq::#auth,
            success: #success,
            retry_mode: #retry_mode,
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
    // Pure emission: the envelopes are module-local, no path qualifier.
    let arms = response_arms(m, &TokenStream2::new());

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

/// The `WireOp` literal for one method (http-bound OR wire-only): its method-name
/// const paired with its `RetryMode`. Mirrors the retry-mode selection in
/// [`gen_operation_literal`]/[`gen_client_method`], but is emitted for EVERY method so
/// a wire-only `#[retry_safe]` surfaces as a golden value.
fn gen_wire_op_literal(m: &MethodModel) -> TokenStream2 {
    let const_ident = method_const_ident(m);
    let retry_mode = if m.retry_safe {
        quote! { ::opsapi::RetryMode::OnceAfterReconnect }
    } else {
        quote! { ::opsapi::RetryMode::Never }
    };
    quote! {
        ::opsapi::WireOp {
            method: #const_ident,
            retry_mode: #retry_mode,
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

/// A wire prefix rendered as an identifier fragment for the metadata macro's name
/// (`characters` stays `characters`; any non-alphanumeric becomes `_`).
fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
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

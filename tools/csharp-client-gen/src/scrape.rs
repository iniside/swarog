//! The scrape phase: build the [`Manifest`] from two independent sources and
//! cross-check them.
//!
//! - **Phase A — runtime reachability.** Call `route_bindings()` on the five providers
//!   with `#[http]` methods. This is the AUTHORITATIVE player-reachable allow-list and
//!   the transport facts (verb/path/auth/success) — impl-free, no DB, no tokio.
//! - **Phase B — `syn` source parse.** `route_bindings()` carries NO argument/return
//!   types (its decode/encode are opaque closures), so the typed shape is recovered by
//!   parsing the same `api/*/api/src/lib.rs` sources: `#[http]` method signatures (with
//!   the leading `Identity` stripped, `body_names` renames applied) and every DTO
//!   `pub struct` reachable from an arg or return type.
//!
//! Two gates, each a hard failure (the caller turns an `Err` into a nonzero exit):
//! - **Drift gate** — Phase A ∩ Phase B on the `prefix.lowerCamel` wire string must be
//!   an exact set equality; a method on one side but not the other is a bug.
//! - **Provider-completeness gate** — every `#[rpc]` trait in ANY `api/*/api` crate that
//!   has at least one `#[http]` method must belong to a provider in [`PROVIDERS`]. This
//!   turns "a new player-facing module was added but not wired into this tool" into a
//!   build failure.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use syn::{
    Attribute, Fields, FnArg, GenericArgument, Item, ItemStruct, ItemTrait, Lit, LitStr,
    PathArguments, Pat, ReturnType, TraitItem, Type,
};

use crate::model::{ArgDef, DtoDef, FieldDef, Manifest, MethodDef, TypeRef};

/// THE conscious edit point (topiccheck-style): every player-facing provider — a
/// `#[rpc]` trait carrying `#[http]` methods — must be listed here AND wired into
/// [`phase_a`]. Adding an `#[http]` method to an EXISTING provider needs no edit; adding
/// a NEW provider module without editing this list is caught by the completeness gate.
const PROVIDERS: &[&str] = &["characters", "inventory", "accounts", "match", "leaderboard"];

/// Phase A: the authoritative reachable set + transport facts, straight from the
/// generated `route_bindings()`. Each entry pairs a provider prefix with its route
/// bindings. This mirrors [`PROVIDERS`] one-to-one (kept consistent by the drift gate,
/// which would flag any divergence between what we call here and what we parse).
fn phase_a() -> Vec<(&'static str, Vec<opsapi::RouteBinding>)> {
    vec![
        ("characters", charactersapi::player_rpc::route_bindings()),
        ("inventory", inventoryapi::holdings_rpc::route_bindings()),
        ("accounts", accountsapi::auth_rpc::route_bindings()),
        ("match", matchapi::match_rpc::route_bindings()),
        ("leaderboard", leaderboardapi::leaderboard_rpc::route_bindings()),
    ]
}

/// The workspace root, derived from this crate's compile-time manifest dir
/// (`tools/csharp-client-gen/` → up two). The tool is a dev/verify tool always run
/// in-repo, exactly like `topiccheck`, so the source tree is present at this path.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

// ---------------------------------------------------------------------------
// Parsed source model (Phase B intermediate)
// ---------------------------------------------------------------------------

/// One parsed `#[http]` method: its wire string, the typed args, and the return type.
struct ParsedHttpMethod {
    wire: String,
    args: Vec<ArgDef>,
    ret: TypeRef,
}

/// A parsed struct's fields: (field name, serde wire key, field type).
type StructFields = Vec<(String, String, Type)>;

/// One parsed `#[rpc(prefix = ...)]` trait.
struct ParsedTrait {
    prefix: String,
    /// The `#[http]` methods (wire-only methods without `#[http]` are excluded).
    http_methods: Vec<ParsedHttpMethod>,
}

/// The full Phase-B parse of every `api/*/api` crate: the `#[rpc]` traits and the global
/// DTO struct registry (name → fields), used to recurse reachable types.
pub(crate) struct Parsed {
    traits: Vec<ParsedTrait>,
    /// name → fields parsed from a `pub struct`.
    structs: BTreeMap<String, StructFields>,
}

// ---------------------------------------------------------------------------
// The public entry point
// ---------------------------------------------------------------------------

/// Scrapes the player-reachable surface into a [`Manifest`], running both gates. Any
/// gate failure (or a parse/type error) is an `Err` — the caller exits nonzero.
pub fn scrape() -> Result<Manifest> {
    let root = workspace_root();

    // Phase B: parse every api crate source (needed for BOTH the completeness gate and
    // the provider types).
    let parsed = parse_all_api_crates(&root)?;

    // --- Gate 2: provider-completeness (scan ALL api crates) ---
    let http_trait_prefixes: Vec<String> = parsed
        .traits
        .iter()
        .filter(|t| !t.http_methods.is_empty())
        .map(|t| t.prefix.clone())
        .collect();
    check_completeness(&http_trait_prefixes, PROVIDERS)
        .map_err(|e| anyhow!("provider-completeness gate FAILED: {e}"))?;

    // Phase A: runtime reachability + transport facts.
    let a = phase_a();
    let mut a_facts: BTreeMap<String, (String, String, String, String, u16)> = BTreeMap::new();
    for (provider, bindings) in &a {
        for rb in bindings {
            let op = &rb.operation;
            a_facts.insert(
                op.method.clone(),
                (
                    (*provider).to_string(),
                    op.verb.clone(),
                    op.path.clone(),
                    auth_str(op.auth),
                    op.success,
                ),
            );
        }
    }

    // Phase B methods restricted to known providers.
    let mut b_methods: BTreeMap<String, &ParsedHttpMethod> = BTreeMap::new();
    for t in &parsed.traits {
        if !PROVIDERS.contains(&t.prefix.as_str()) {
            continue;
        }
        for m in &t.http_methods {
            b_methods.insert(m.wire.clone(), m);
        }
    }

    // --- Gate 1: drift (Phase A ∩ Phase B set equality on the wire string) ---
    let a_keys: BTreeSet<String> = a_facts.keys().cloned().collect();
    let b_keys: BTreeSet<String> = b_methods.keys().cloned().collect();
    check_drift(&a_keys, &b_keys).map_err(|e| anyhow!("drift gate FAILED: {e}"))?;

    // Build the methods (sorted by wire string for a stable manifest).
    let mut methods: Vec<MethodDef> = Vec::new();
    for (wire, (provider, verb, path, auth, success)) in &a_facts {
        let m = b_methods
            .get(wire)
            .ok_or_else(|| anyhow!("internal: {wire} passed the drift gate but has no parsed sig"))?;
        methods.push(MethodDef {
            provider: provider.clone(),
            wire_method: wire.clone(),
            verb: verb.clone(),
            path: path.clone(),
            auth: auth.clone(),
            success: *success,
            args: m.args.clone(),
            ret: m.ret.clone(),
        });
    }
    methods.sort_by(|x, y| x.wire_method.cmp(&y.wire_method));

    // Collect reachable DTOs (BFS from every method arg + return type).
    let dtos = collect_dtos(&methods, &parsed.structs)?;

    // The `Status` variant names, in declaration order.
    let statuses = parse_status_variants(&root)?;

    Ok(Manifest { methods, dtos, statuses })
}

// ---------------------------------------------------------------------------
// The two gates (pure — unit-testable with hand-built inputs)
// ---------------------------------------------------------------------------

/// Provider-completeness: every parsed `#[http]`-bearing trait's provider prefix must be
/// in `providers`. FAILs (returns `Err`) listing any provider that is not — i.e. a new
/// player-facing module that was never wired into this tool.
pub fn check_completeness(http_trait_prefixes: &[String], providers: &[&str]) -> Result<(), String> {
    let mut missing: Vec<String> = http_trait_prefixes
        .iter()
        .filter(|p| !providers.contains(&p.as_str()))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "provider(s) expose #[http] methods but are not in the hardcoded PROVIDERS list: {} \
             (add them to PROVIDERS + phase_a)",
            missing.join(", ")
        ))
    }
}

/// Drift: the runtime `route_bindings()` set and the parsed `#[http]` set must be equal.
/// FAILs listing any method present on only one side.
pub fn check_drift(runtime: &BTreeSet<String>, parsed: &BTreeSet<String>) -> Result<(), String> {
    let mut errs: Vec<String> = Vec::new();
    for m in runtime {
        if !parsed.contains(m) {
            errs.push(format!("route_bindings() method {m:?} has no parsed #[http] signature"));
        }
    }
    for m in parsed {
        if !runtime.contains(m) {
            errs.push(format!("parsed #[http] method {m:?} has no route_binding"));
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

// ---------------------------------------------------------------------------
// Phase B — parsing
// ---------------------------------------------------------------------------

/// Discovers `api/<name>/api/src/lib.rs` for every domain under `api/`, sorted.
fn discover_api_lib_files(root: &Path) -> Result<Vec<PathBuf>> {
    let api_dir = root.join("api");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&api_dir)
        .with_context(|| format!("read api dir {}", api_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let lib = entry.path().join("api").join("src").join("lib.rs");
        if lib.is_file() {
            files.push(lib);
        }
    }
    files.sort();
    Ok(files)
}

/// Parses every api crate source into [`Parsed`]: `#[rpc]` traits (with their `#[http]`
/// method sigs) and a global `pub struct` registry. Reads each discovered file, then
/// delegates to [`parse_sources`] (kept separate so it's unit-testable on synthetic
/// `(path, source)` pairs without touching the real `api/` tree).
fn parse_all_api_crates(root: &Path) -> Result<Parsed> {
    let mut files = Vec::new();
    for file in discover_api_lib_files(root)? {
        let src = std::fs::read_to_string(&file)
            .with_context(|| format!("read {}", file.display()))?;
        files.push((file, src));
    }
    parse_sources(&files)
}

/// Parses already-read `(file, source)` pairs into [`Parsed`]. A `pub struct` name
/// colliding across two different files (a flat `name -> fields` map would silently let
/// the last-processed file overwrite the first DTO's fields — same failure class the
/// drift/completeness gates exist to catch) is a hard `bail!` naming BOTH source files.
pub(crate) fn parse_sources(files: &[(PathBuf, String)]) -> Result<Parsed> {
    let mut traits: Vec<ParsedTrait> = Vec::new();
    let mut structs: BTreeMap<String, StructFields> = BTreeMap::new();
    // name -> the file that first declared it, so a later collision can name both.
    let mut struct_provenance: BTreeMap<String, PathBuf> = BTreeMap::new();

    for (file, src) in files {
        let ast =
            syn::parse_file(src).with_context(|| format!("parse {}", file.display()))?;
        for item in ast.items {
            match item {
                Item::Trait(t) => {
                    if let Some(prefix) = rpc_prefix(&t) {
                        traits.push(parse_trait(&t, &prefix).with_context(|| {
                            format!("parse trait {} in {}", t.ident, file.display())
                        })?);
                    }
                }
                Item::Struct(s) => {
                    if let Some((name, fields)) = parse_struct(&s) {
                        if let Some(prev) = struct_provenance.get(&name) {
                            return Err(anyhow!(
                                "DTO struct {name:?} declared in both {} and {} — \
                                 cross-domain name collision (rename one; a flat name \
                                 registry would silently drop one DTO's fields)",
                                prev.display(),
                                file.display()
                            ));
                        }
                        struct_provenance.insert(name.clone(), file.clone());
                        structs.insert(name, fields);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(Parsed { traits, structs })
}

/// The `prefix` from a `#[rpc(prefix = "...")]` attribute, if the trait has one.
fn rpc_prefix(t: &ItemTrait) -> Option<String> {
    for a in &t.attrs {
        if a.path().is_ident("rpc") {
            let mut prefix = None;
            let _ = a.parse_nested_meta(|m| {
                if m.path.is_ident("prefix") {
                    let s: LitStr = m.value()?.parse()?;
                    prefix = Some(s.value());
                }
                Ok(())
            });
            return prefix;
        }
    }
    None
}

/// Parses a `#[rpc]` trait's `#[http]` methods (wire-only methods are skipped).
fn parse_trait(t: &ItemTrait, prefix: &str) -> Result<ParsedTrait> {
    let mut http_methods = Vec::new();
    for item in &t.items {
        let TraitItem::Fn(f) = item else { continue };
        let Some(http_attr) = f.attrs.iter().find(|a| a.path().is_ident("http")) else {
            continue; // wire-only method (no #[http]) — not player-reachable
        };
        let body_names = parse_http_body_names(http_attr)
            .with_context(|| format!("parse #[http] on {}", f.sig.ident))?;

        let name = f.sig.ident.to_string();
        let wire = format!("{prefix}.{}", to_lower_camel(&name));
        let args = parse_args(&f.sig, &body_names)
            .with_context(|| format!("parse args of {name}"))?;
        let ret = result_ok_type(&f.sig.output)
            .with_context(|| format!("parse return of {name}"))?;
        http_methods.push(ParsedHttpMethod { wire, args, ret });
    }
    Ok(ParsedTrait { prefix: prefix.to_string(), http_methods })
}

/// The method args after stripping a leading `Identity` param; `wire_name` applies the
/// `body_names` override (path-wildcard args have no override → keep the param name).
fn parse_args(sig: &syn::Signature, body_names: &BTreeMap<String, String>) -> Result<Vec<ArgDef>> {
    let mut args = Vec::new();
    let mut inputs = sig.inputs.iter();
    // Skip &self.
    match inputs.next() {
        Some(FnArg::Receiver(_)) => {}
        _ => return Err(anyhow!("method must take &self")),
    }
    for (i, input) in inputs.enumerate() {
        let FnArg::Typed(pt) = input else {
            return Err(anyhow!("unexpected receiver"));
        };
        let Pat::Ident(pi) = &*pt.pat else {
            return Err(anyhow!("param must be a simple identifier"));
        };
        // The leading param, when typed `Identity`, is the caller identity — stripped.
        if i == 0 && is_identity_type(&pt.ty) {
            continue;
        }
        let name = pi.ident.to_string();
        let wire_name = body_names.get(&name).cloned().unwrap_or_else(|| name.clone());
        let ty = map_type(&pt.ty)?;
        args.push(ArgDef { name, wire_name, ty });
    }
    Ok(args)
}

/// Reads the `body_names(param = "key")` overrides from an `#[http]` attr, consuming the
/// other keys (`verb`/`path`/`auth`/`success`/`path_args`) so the nested parse succeeds.
fn parse_http_body_names(attr: &Attribute) -> Result<BTreeMap<String, String>> {
    let mut body_names = BTreeMap::new();
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("body_names") {
            meta.parse_nested_meta(|inner| {
                let param = inner
                    .path
                    .get_ident()
                    .ok_or_else(|| inner.error("body_names key must be a bare param name"))?
                    .to_string();
                let key: LitStr = inner.value()?.parse()?;
                body_names.insert(param, key.value());
                Ok(())
            })
        } else if meta.path.is_ident("path_args") {
            meta.parse_nested_meta(|inner| {
                let _key: LitStr = inner.value()?.parse()?;
                Ok(())
            })
        } else {
            // verb/path/auth (LitStr) or success (LitInt) — consume the value.
            let _v: Lit = meta.value()?.parse()?;
            Ok(())
        }
    })
    .map_err(|e| anyhow!("{e}"))?;
    Ok(body_names)
}

/// Parses a `pub struct Name { ... }` into (name, [(field, wire key, type)]). Returns
/// `None` for non-pub structs, tuple/unit structs (our DTOs are all named-field pub).
fn parse_struct(s: &ItemStruct) -> Option<(String, StructFields)> {
    if !matches!(s.vis, syn::Visibility::Public(_)) {
        return None;
    }
    let Fields::Named(named) = &s.fields else {
        return None;
    };
    let mut fields = Vec::new();
    for f in &named.named {
        let ident = f.ident.as_ref()?.to_string();
        let wire = serde_rename(&f.attrs).unwrap_or_else(|| ident.clone());
        fields.push((ident, wire, f.ty.clone()));
    }
    Some((s.ident.to_string(), fields))
}

/// The `#[serde(rename = "...")]` override on a field, if present. Best-effort: unknown
/// serde options are ignored (these DTOs carry none, so this returns `None` for them).
fn serde_rename(attrs: &[Attribute]) -> Option<String> {
    for a in attrs {
        if !a.path().is_ident("serde") {
            continue;
        }
        let mut renamed = None;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("rename") {
                if let Ok(v) = m.value() {
                    if let Ok(s) = v.parse::<LitStr>() {
                        renamed = Some(s.value());
                    }
                }
            }
            Ok(())
        });
        if renamed.is_some() {
            return renamed;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Type mapping + DTO recursion
// ---------------------------------------------------------------------------

/// `true` when the type's final path segment is `Identity` — the macro's identity
/// recogniser, reimplemented (`tools/rpc-macro/src/lib.rs:343`).
fn is_identity_type(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident == "Identity";
        }
    }
    false
}

/// Maps a `syn::Type` into the minimal [`TypeRef`] lattice. `String`/`i64`/`Vec<T>`/`()`
/// are recognised; any other single-segment path is treated as a DTO `Struct(name)`.
fn map_type(ty: &Type) -> Result<TypeRef> {
    match ty {
        Type::Tuple(t) if t.elems.is_empty() => Ok(TypeRef::Unit),
        Type::Path(tp) => {
            let seg = tp
                .path
                .segments
                .last()
                .ok_or_else(|| anyhow!("empty type path"))?;
            let name = seg.ident.to_string();
            match name.as_str() {
                "String" => Ok(TypeRef::String),
                "i64" => Ok(TypeRef::I64),
                "Vec" => {
                    let inner = first_generic_type(&seg.arguments)
                        .ok_or_else(|| anyhow!("Vec without a type argument"))?;
                    Ok(TypeRef::Vec(Box::new(map_type(inner)?)))
                }
                other => Ok(TypeRef::Struct(other.to_string())),
            }
        }
        other => Err(anyhow!("unsupported type in player surface: {other:?}")),
    }
}

/// Extracts `T` from a `Result<T, _>` return type; `()` maps to [`TypeRef::Unit`]. Mirrors
/// the macro's `result_ok_type` (`tools/rpc-macro/src/lib.rs:353`).
fn result_ok_type(output: &ReturnType) -> Result<TypeRef> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => return Err(anyhow!("method must return Result<T, Error>")),
    };
    let Type::Path(tp) = ty else {
        return Err(anyhow!("method must return Result<..>"));
    };
    let seg = tp
        .path
        .segments
        .last()
        .ok_or_else(|| anyhow!("method must return Result<..>"))?;
    if seg.ident != "Result" {
        return Err(anyhow!("method must return Result<T, Error>"));
    }
    let ok = first_generic_type(&seg.arguments)
        .ok_or_else(|| anyhow!("Result without a type argument"))?;
    map_type(ok)
}

/// The first `<T>` type argument of an angle-bracketed path segment.
fn first_generic_type(args: &PathArguments) -> Option<&Type> {
    let PathArguments::AngleBracketed(ab) = args else {
        return None;
    };
    ab.args.iter().find_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// BFS every DTO reachable from a method arg or return type, recursing into struct
/// fields. Returns them sorted by name for a stable manifest. A referenced struct absent
/// from the registry is an error (a type we cannot describe to the emitter).
fn collect_dtos(
    methods: &[MethodDef],
    structs: &BTreeMap<String, StructFields>,
) -> Result<Vec<DtoDef>> {
    let mut queue: VecDeque<String> = VecDeque::new();
    for m in methods {
        for a in &m.args {
            collect_struct_names(&a.ty, &mut queue);
        }
        collect_struct_names(&m.ret, &mut queue);
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<DtoDef> = Vec::new();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let raw_fields = structs
            .get(&name)
            .ok_or_else(|| anyhow!("DTO {name:?} referenced but no pub struct found in api crates"))?;
        let mut fields = Vec::new();
        for (fname, wire, ty) in raw_fields {
            let ty_ref = map_type(ty)
                .with_context(|| format!("field {name}.{fname}"))?;
            let mut nested: VecDeque<String> = VecDeque::new();
            collect_struct_names(&ty_ref, &mut nested);
            queue.extend(nested);
            fields.push(FieldDef { name: fname.clone(), wire_name: wire.clone(), ty: ty_ref });
        }
        out.push(DtoDef { name, fields });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Pushes every `Struct(name)` mentioned by a [`TypeRef`] (recursing into `Vec`) onto
/// `into`.
fn collect_struct_names(ty: &TypeRef, into: &mut VecDeque<String>) {
    match ty {
        TypeRef::Struct(name) => into.push_back(name.clone()),
        TypeRef::Vec(inner) => collect_struct_names(inner, into),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Status enum + name conversions
// ---------------------------------------------------------------------------

/// Parses `core/opsapi/src/lib.rs` for the `Status` enum's variant names, in declaration
/// order (the taxonomy the C# client throws on).
fn parse_status_variants(root: &Path) -> Result<Vec<String>> {
    let path = root.join("core").join("opsapi").join("src").join("lib.rs");
    let src = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let ast = syn::parse_file(&src).with_context(|| format!("parse {}", path.display()))?;
    for item in ast.items {
        if let Item::Enum(e) = item {
            if e.ident == "Status" {
                return Ok(e.variants.iter().map(|v| v.ident.to_string()).collect());
            }
        }
    }
    Err(anyhow!("opsapi::Status enum not found in {}", path.display()))
}

/// The `AuthReq` variant as the wire string the manifest carries.
fn auth_str(a: opsapi::AuthReq) -> String {
    match a {
        opsapi::AuthReq::None => "none".to_string(),
        opsapi::AuthReq::Player => "player".to_string(),
    }
}

/// `owner_of` → `OwnerOf` (reimplements the macro's rule, `tools/rpc-macro:993`).
fn to_pascal(snake: &str) -> String {
    snake
        .split('_')
        .filter(|s| !s.is_empty())
        .map(capitalize)
        .collect()
}

/// `owner_of` → `ownerOf` (reimplements the macro's rule, `tools/rpc-macro:1002`).
fn to_lower_camel(snake: &str) -> String {
    let pascal = to_pascal(snake);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

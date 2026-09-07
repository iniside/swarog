//! Shared parser and syntax model for the repository's `#[rpc]` contracts.
//!
//! The proc macro owns code emission and external-client generators own their DTO
//! traversal and target-language models. This crate only gives those consumers one
//! interpretation of RPC prefixes, methods, identity, retry and HTTP argument mapping.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use syn::{
    parse::{Parse, ParseStream},
    spanned::Spanned,
    FnArg, GenericArgument, Ident, ItemTrait, LitInt, LitStr, Pat, PathArguments, ReturnType,
    Signature, Token, TraitItem, Type,
};

/// Every `.rs` file that carries a contract crate's declarations, recursively under
/// `src_dir` and sorted — the single answer the contract scanners share to "which
/// files make up `api/<domain>/<api|events>`". Scanning only `src/lib.rs` would make
/// a `mod topics;` split invisible to a scanner while the crate still compiles.
///
/// Test modules (`tests.rs`, `*_tests.rs` — the repo's only sanctioned test-file
/// shapes) are excluded: the consumers are text/`syn` scans that cannot tell a
/// fixture `#[rpc(` or `define(` inside a test from a real declaration.
pub fn contract_sources(src_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut pending = vec![src_dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if is_contract_source(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_contract_source(path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return false;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem != "tests" && !stem.ends_with("_tests"))
}

/// Every domain that has a module implementation under `<workspace_root>/modules/` —
/// the single answer the repository's gates share to "does this domain SERVE ops in some
/// process?".
///
/// The two questions are deliberately different. A domain's *contract* exists the moment
/// `api/<domain>/` exists, so contract-surface gates (topiccheck, contract-golden,
/// public-api) keep keying on `api/<domain>`. A domain's *served surface* — a gateway
/// stub, an ops-catalog row, a generated client provider, an input-policy entry — only
/// exists once something can answer the call, i.e. once `modules/<domain>` exists. Keying
/// a served-surface gate on `api/<domain>` forces the module to land in the same commit as
/// its contracts.
///
/// A directory counts when it holds a `Cargo.toml` (the filesystem's own answer to "which
/// crates live under `modules/`", independent of workspace-member registration). `gateway`
/// is included and simply never intersects a domain list, since it has no `api/gateway`.
///
/// An EMPTY result is an error, never an empty set: every workspace has modules, so
/// "nothing is served" can only mean the scan ran against the wrong root — and a silent
/// empty set would make every caller's gate vacuous instead of loud.
pub fn served_domains(workspace_root: &Path) -> std::io::Result<BTreeSet<String>> {
    module_dirs(&workspace_root.join("modules"))
}

/// [`served_domains`] against an explicit `modules/` root, for the scanners that already
/// hold that path (and for fixtures). Same contract, including the empty-scan `Err`: this
/// is the ONE place the "which module directories exist" answer is computed, so no caller
/// can hold a second, silently-degrading copy of the directory test.
pub fn module_dirs(modules_root: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut domains = BTreeSet::new();
    for entry in std::fs::read_dir(modules_root)? {
        let path = entry?.path();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            domains.insert(name.to_owned());
        }
    }
    if domains.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no module crate directory under {} — every gate keyed on this answer would \
                 be vacuous, so the empty scan is reported as a failure",
                modules_root.display()
            ),
        ));
    }
    Ok(domains)
}

/// The textual marker in a contract source that means "this domain exposes player-facing
/// HTTP ops" (an `#[http(…)]` attribute on an `#[rpc]` method). This is the ONLY
/// authoritative signal for HTTP surface: the generated `route_bindings()` exists even for
/// wire-only crates (e.g. `ratingrpc`), so the attribute — never the glue — is what the
/// served-surface gates key off. The domain name is the DIR name (`api/<name>`), not the
/// crate name: `modules/match`'s crate is `match_module` but its provider name is `match`.
pub const HTTP_OP_MARKER: &str = "#[http(";

/// The domains sanctioned to ship an `#[http(` contract surface WITHOUT `modules/<domain>`.
///
/// Every served-surface gate (archcheck rule 17, checkmodules' gateway-stub parity,
/// opscatalog-gen's hand-list, the C# provider list, the conformance input inventory) skips
/// a domain that is not in [`served_domains`]. That skip is a hole unless someone decided
/// it: a domain whose module lands under a DIVERGENT directory name (`api/social` served by
/// `modules/socialgraph`) would be skipped by all five and 404 through the gateway in the
/// split while working in the monolith. This list is that decision, written down;
/// [`contract_only_violations`] fails any un-listed skip AND any entry that has gone stale.
pub const CONTRACT_ONLY: &[&str] = &[];

/// Every `api/<domain>` whose contract sources declare at least one [`HTTP_OP_MARKER`] on a
/// NON-comment line, plus one error line per directory or file the scan could not read.
///
/// Scans EVERY source under `api/<domain>/api/src` via [`contract_sources`], not just
/// `lib.rs`: moving one `#[http(` method into `src/ops.rs` would otherwise make the
/// domain's HTTP surface invisible to every caller. An unreadable source is an ERROR line,
/// never a silently absent domain — a permission blip must not turn a caller vacuous.
pub fn http_op_domains(workspace_root: &Path) -> (BTreeSet<String>, Vec<String>) {
    let mut domains = BTreeSet::new();
    let mut errors = Vec::new();
    let api_root = workspace_root.join("api");
    let entries = match std::fs::read_dir(&api_root) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push(format!(
                "cannot read {}: {e} — the `{HTTP_OP_MARKER}` domain scan cannot run",
                api_root.display()
            ));
            return (domains, errors);
        }
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Some(domain) = dir.file_name().and_then(|name| name.to_str()).map(String::from) else {
            continue;
        };
        let src = dir.join("api").join("src");
        if !src.is_dir() {
            continue;
        }
        let sources = match contract_sources(&src) {
            Ok(sources) => sources,
            Err(e) => {
                errors.push(format!("cannot list contract sources under {}: {e}", src.display()));
                continue;
            }
        };
        let mut declares = false;
        for path in sources {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    errors.push(format!("cannot read contract source {}: {e}", path.display()));
                    continue;
                }
            };
            declares |= text.lines().any(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && contains_boundary_checked(line, HTTP_OP_MARKER)
            });
        }
        if declares {
            domains.insert(domain);
        }
    }
    (domains, errors)
}

/// True if `text` contains `pat` at a position whose PRECEDING byte is not an identifier
/// character (`[A-Za-z0-9_]`) — i.e. `pat` starts a fresh token rather than continuing a
/// longer one. A match at byte offset 0 always counts.
fn contains_boundary_checked(text: &str, pat: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(offset) = text[from..].find(pat) {
        let i = from + offset;
        if i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            return true;
        }
        from = i + 1;
    }
    false
}

/// One violation per `api/<domain>` that exposes HTTP ops with no `modules/<domain>` and no
/// [`CONTRACT_ONLY`] entry, and one per CONTRACT_ONLY entry that has gone stale (its
/// module landed, or its `#[http(` contract is gone). Empty = OK.
///
/// This is the self-check that keeps the served-surface gates' exemption set reviewed
/// rather than implicit: without it, a divergently-named module (`api/social` ⇄
/// `modules/socialgraph`) silently drops out of all five gates, and an exemption granted
/// once is never taken back.
pub fn contract_only_violations(workspace_root: &Path) -> Vec<String> {
    let (http_domains, mut violations) = http_op_domains(workspace_root);
    let served = match served_domains(workspace_root) {
        Ok(served) => served,
        Err(e) => {
            violations.push(format!(
                "cannot list served domains under {}/modules: {e} — the contract-only \
                 allow-list check cannot run",
                workspace_root.display()
            ));
            return violations;
        }
    };
    let allowed: BTreeSet<&str> = CONTRACT_ONLY.iter().copied().collect();
    for domain in &http_domains {
        if served.contains(domain) || allowed.contains(domain.as_str()) {
            continue;
        }
        violations.push(format!(
            "domain `{domain}` exposes HTTP ops (`{HTTP_OP_MARKER}` under \
             api/{domain}/api/src) but there is no modules/{domain}/Cargo.toml — every \
             served-surface gate (gateway stub, ops catalog, C# providers, input policy) \
             SKIPS it, so it would 404 through the gateway in the split while working in \
             the monolith. Name the module directory `modules/{domain}` (the provider name \
             is the api/ dir name), or, if the contracts are deliberately ahead of the \
             module, add \"{domain}\" to rpc_contract_model::CONTRACT_ONLY"
        ));
    }
    for entry in CONTRACT_ONLY {
        if served.contains(*entry) {
            violations.push(format!(
                "stale rpc_contract_model::CONTRACT_ONLY entry \"{entry}\": modules/{entry} \
                 now exists, so the served-surface gates already cover it — remove the entry \
                 so the exemption cannot outlive its reason"
            ));
        } else if !http_domains.contains(*entry) {
            violations.push(format!(
                "stale rpc_contract_model::CONTRACT_ONLY entry \"{entry}\": no \
                 `{HTTP_OP_MARKER}` contract under api/{entry}/api/src — the exemption \
                 names a domain that does not exist (or no longer has HTTP ops); remove it"
            ));
        }
    }
    violations
}

/// The parsed arguments of `#[rpc(prefix = "...")]`.
pub struct RpcArgs {
    pub prefix: String,
}

impl Parse for RpcArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "prefix" {
            return Err(syn::Error::new(key.span(), "expected `prefix = \"...\"`"));
        }
        input.parse::<Token![=]>()?;
        let lit: LitStr = input.parse()?;
        Ok(Self {
            prefix: lit.value(),
        })
    }
}

/// One method's parsed `#[http(...)]` binding.
#[derive(Default)]
pub struct HttpBind {
    pub verb: String,
    pub path: String,
    /// `opsapi::AuthReq` variant name: `None` or `Player`.
    pub auth: String,
    pub success: u16,
    pub path_args: BTreeMap<String, String>,
    pub body_names: BTreeMap<String, String>,
}

/// One marshalled RPC argument. A leading `Identity` is represented separately on
/// [`MethodModel`] and therefore never appears here.
pub struct Arg {
    pub ident: Ident,
    pub ty: Type,
    pub wildcard: Option<String>,
    pub rename: Option<String>,
}

/// Parsed syntax shared by Rust expansion and source-based tooling.
pub struct MethodModel {
    /// The signature after macro-only `#[http]`/`#[retry_safe]` attributes are removed.
    pub sig: Signature,
    pub method_ident: Ident,
    pub has_identity: bool,
    pub id_ident: Option<Ident>,
    pub args: Vec<Arg>,
    /// `Some(T)` for `Result<T, _>` where `T` is not unit.
    pub value_ty: Option<Type>,
    pub http: Option<HttpBind>,
    pub retry_safe: bool,
}

/// Returns the prefix declared on an `ItemTrait`'s `#[rpc]` attribute.
pub fn trait_prefix(item: &ItemTrait) -> syn::Result<Option<String>> {
    for attr in &item.attrs {
        if !attr.path().is_ident("rpc") {
            continue;
        }
        let mut prefix = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("prefix") {
                prefix = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else {
                Err(meta.error("unknown #[rpc(...)] key"))
            }
        })?;
        return prefix
            .map(Some)
            .ok_or_else(|| syn::Error::new(attr.span(), "#[rpc(...)] requires prefix"));
    }
    Ok(None)
}

/// Parses every RPC method in a trait, stripping macro-only attributes from the
/// supplied trait exactly as the proc macro requires.
pub fn build_methods(item: &mut ItemTrait) -> syn::Result<Vec<MethodModel>> {
    let mut methods = Vec::new();
    for trait_item in &mut item.items {
        if let TraitItem::Fn(method) = trait_item {
            methods.push(build_method(method)?);
        }
    }
    Ok(methods)
}

/// Parses one RPC method and validates its HTTP argument mappings.
pub fn build_method(method: &mut syn::TraitItemFn) -> syn::Result<MethodModel> {
    let mut http = None;
    let mut retry_safe = false;
    let mut kept = Vec::new();
    for attr in method.attrs.drain(..) {
        if attr.path().is_ident("http") {
            http = Some(parse_http(&attr)?);
        } else if attr.path().is_ident("retry_safe") {
            if retry_safe {
                return Err(syn::Error::new(attr.span(), "duplicate #[retry_safe]"));
            }
            retry_safe = true;
        } else {
            kept.push(attr);
        }
    }
    method.attrs = kept;

    let sig = method.sig.clone();
    let method_ident = sig.ident.clone();
    let name = method_ident.to_string();
    let mut inputs = sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(_)) => {}
        _ => {
            return Err(syn::Error::new(
                sig.span(),
                "rpc method must take &self as its first parameter",
            ));
        }
    }

    let mut has_identity = false;
    let mut id_ident = None;
    let mut args = Vec::new();
    for (index, input) in inputs.enumerate() {
        let typed = match input {
            FnArg::Typed(typed) => typed,
            FnArg::Receiver(_) => {
                return Err(syn::Error::new(input.span(), "unexpected self parameter"));
            }
        };
        let ident = match &*typed.pat {
            Pat::Ident(pattern) => pattern.ident.clone(),
            _ => {
                return Err(syn::Error::new(
                    typed.pat.span(),
                    "rpc method parameters must be simple identifiers",
                ));
            }
        };
        if index == 0 && is_identity_type(&typed.ty) {
            has_identity = true;
            id_ident = Some(ident);
            continue;
        }
        let parameter = ident.to_string();
        args.push(Arg {
            ident,
            ty: (*typed.ty).clone(),
            wildcard: http
                .as_ref()
                .and_then(|binding| binding.path_args.get(&parameter).cloned()),
            rename: http
                .as_ref()
                .and_then(|binding| binding.body_names.get(&parameter).cloned()),
        });
    }

    if let Some(binding) = &http {
        validate_http_mappings(&sig, &name, &args, binding)?;
        match (binding.auth.as_str(), has_identity) {
            ("Player", false) => {
                return Err(syn::Error::new(
                    sig.span(),
                    "HTTP method with auth = \"player\" must take a leading Identity parameter",
                ));
            }
            ("None", true) => {
                return Err(syn::Error::new(
                    sig.span(),
                    "HTTP method with a leading Identity parameter must declare auth = \"player\"",
                ));
            }
            _ => {}
        }
    }

    Ok(MethodModel {
        sig: sig.clone(),
        method_ident,
        has_identity,
        id_ident,
        args,
        value_ty: result_ok_type(&sig.output)?,
        http,
        retry_safe,
    })
}

fn parse_http(attr: &syn::Attribute) -> syn::Result<HttpBind> {
    let mut binding = HttpBind::default();
    let mut seen_auth = false;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("verb") {
            binding.verb = meta.value()?.parse::<LitStr>()?.value();
        } else if meta.path.is_ident("path") {
            binding.path = meta.value()?.parse::<LitStr>()?.value();
        } else if meta.path.is_ident("success") {
            let literal = meta.value()?.parse::<LitInt>()?;
            let success = literal.base10_parse()?;
            if !(200..=299).contains(&success) {
                return Err(syn::Error::new(
                    literal.span(),
                    "#[http(...)] success must be between 200 and 299",
                ));
            }
            binding.success = success;
        } else if meta.path.is_ident("auth") {
            let value = meta.value()?.parse::<LitStr>()?.value();
            binding.auth = match value.as_str() {
                "none" => "None".to_owned(),
                "player" => "Player".to_owned(),
                other => {
                    return Err(meta.error(format!(
                        "auth must be \"none\" or \"player\", got {other:?}"
                    )));
                }
            };
            seen_auth = true;
        } else if meta.path.is_ident("path_args") {
            meta.parse_nested_meta(|inner| {
                let parameter = inner
                    .path
                    .get_ident()
                    .ok_or_else(|| inner.error("path_args key must be a bare param name"))?
                    .to_string();
                let wildcard = inner.value()?.parse::<LitStr>()?.value();
                binding.path_args.insert(parameter, wildcard);
                Ok(())
            })?;
        } else if meta.path.is_ident("body_names") {
            meta.parse_nested_meta(|inner| {
                let parameter = inner
                    .path
                    .get_ident()
                    .ok_or_else(|| inner.error("body_names key must be a bare param name"))?
                    .to_string();
                let wire_name = inner.value()?.parse::<LitStr>()?.value();
                binding.body_names.insert(parameter, wire_name);
                Ok(())
            })?;
        } else {
            return Err(meta.error("unknown #[http(...)] key"));
        }
        Ok(())
    })?;
    if binding.verb.is_empty() || binding.path.is_empty() || !seen_auth || binding.success == 0 {
        return Err(syn::Error::new(
            attr.span(),
            "#[http(...)] requires verb, path, auth and success",
        ));
    }
    Ok(binding)
}

fn validate_http_mappings(
    sig: &Signature,
    name: &str,
    args: &[Arg],
    binding: &HttpBind,
) -> syn::Result<()> {
    let parameter_names: HashSet<String> = args.iter().map(|arg| arg.ident.to_string()).collect();
    for key in binding.path_args.keys().chain(binding.body_names.keys()) {
        if !parameter_names.contains(key) {
            return Err(syn::Error::new(
                sig.span(),
                format!(
                    "#[http(...)] path_args/body_names entry {key:?} names no parameter of method `{name}`"
                ),
            ));
        }
    }
    let placeholders = parse_path_placeholders(&binding.path);
    let values: HashSet<&String> = binding.path_args.values().collect();
    for placeholder in &placeholders {
        if !values.contains(placeholder) {
            return Err(syn::Error::new(
                sig.span(),
                format!(
                    "#[http(...)] path template of `{name}` has placeholder {{{placeholder}}} with no matching path_args value"
                ),
            ));
        }
    }
    for value in binding.path_args.values() {
        if !placeholders.contains(value) {
            return Err(syn::Error::new(
                sig.span(),
                format!(
                    "#[http(...)] path_args value {value:?} of `{name}` does not appear as a {{...}} placeholder in path {:?}",
                    binding.path
                ),
            ));
        }
    }
    Ok(())
}

fn parse_path_placeholders(path: &str) -> Vec<String> {
    let mut placeholders = Vec::new();
    let mut current = None;
    for character in path.chars() {
        match character {
            '{' => current = Some(String::new()),
            '}' => {
                if let Some(name) = current.take() {
                    placeholders.push(name);
                }
            }
            _ => {
                if let Some(name) = current.as_mut() {
                    name.push(character);
                }
            }
        }
    }
    placeholders
}

fn is_identity_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "Identity"))
}

fn result_ok_type(output: &ReturnType) -> syn::Result<Option<Type>> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                output.span(),
                "rpc method must return Result<T, opsapi::Error>",
            ));
        }
    };
    let Type::Path(path) = ty else {
        return Err(syn::Error::new(
            ty.span(),
            "rpc method must return Result<..>",
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new(ty.span(), "rpc method must return Result<..>"))?;
    if segment.ident != "Result" {
        return Err(syn::Error::new(
            segment.ident.span(),
            "rpc method must return Result<T, opsapi::Error>",
        ));
    }
    let ok = match &segment.arguments {
        PathArguments::AngleBracketed(arguments) => arguments.args.first().and_then(|argument| {
            if let GenericArgument::Type(ty) = argument {
                Some(ty.clone())
            } else {
                None
            }
        }),
        _ => None,
    }
    .ok_or_else(|| syn::Error::new(segment.span(), "Result must have a type argument"))?;
    if matches!(&ok, Type::Tuple(tuple) if tuple.elems.is_empty()) {
        Ok(None)
    } else {
        Ok(Some(ok))
    }
}

#[cfg(test)]
mod tests;

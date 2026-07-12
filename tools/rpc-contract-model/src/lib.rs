//! Shared parser and syntax model for the repository's `#[rpc]` contracts.
//!
//! The proc macro owns code emission and external-client generators own their DTO
//! traversal and target-language models. This crate only gives those consumers one
//! interpretation of RPC prefixes, methods, identity, retry and HTTP argument mapping.

use std::collections::{BTreeMap, HashSet};

use syn::{
    parse::{Parse, ParseStream},
    spanned::Spanned,
    FnArg, GenericArgument, Ident, ItemTrait, LitInt, LitStr, Pat, PathArguments, ReturnType,
    Signature, Token, TraitItem, Type,
};

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
            binding.success = meta.value()?.parse::<LitInt>()?.base10_parse()?;
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

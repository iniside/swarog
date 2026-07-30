//! Source-derived inventory of string-bearing RPC request fields.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use syn::{Attribute, Fields, GenericArgument, Item, ItemStruct, PathArguments, Type};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Exposure {
    External,
    Wire,
}

impl Exposure {
    pub fn label(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Wire => "wire",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InputKey {
    pub wire_method: String,
    pub wire_field_name: String,
    pub exposure: Exposure,
}

type StructFields = Vec<(String, Type)>;

/// What the traversal knows about a named type declared in the API crate under scan.
enum Shape {
    /// A struct with named fields — traversed field by field.
    Named(StructFields),
    /// A tuple or unit struct: no field names exist to build an [`InputKey`] from, so
    /// using one as a request argument is rejected rather than silently skipped.
    Unnamed,
    /// A `type X = ...;` alias, resolved to its target before any other decision.
    Alias(Type),
}

type Types = BTreeMap<String, Shape>;

/// Rust primitives, which by construction carry no caller-supplied text. This is a
/// closed category, not a hand-maintained decision list.
const SCALAR_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32",
    "u64", "u128", "usize",
];

/// Named NON-primitive request types the traversal deliberately stops at, each with the
/// reason it carries no caller-supplied string. Anything else is a hard failure, so a
/// new foreign type cannot enter a contract's request surface unexamined.
const OPAQUE_REQUEST_TYPES: &[(&str, &str)] = &[(
    "Identity",
    "opsapi::Identity is minted by the gateway from an already-verified session, never \
     parsed from a caller-supplied request field",
)];

/// String-keyed map types: a caller-controlled bag of text with no declared field names,
/// so both legs are recorded under the reserved `<key>`/`<value>` suffixes.
const STRING_MAP_TYPES: &[&str] = &["BTreeMap", "HashMap"];

pub fn discover(api_root: &Path) -> Result<BTreeSet<InputKey>> {
    let mut out = BTreeSet::new();
    for domain in sorted_dirs(api_root)? {
        let src = domain.join("api/src");
        if !src.is_dir() {
            continue;
        }
        let files = rpc_contract_model::contract_sources(&src)
            .with_context(|| format!("read contract sources {}", src.display()))?;
        let sources = files
            .iter()
            .map(|path| {
                std::fs::read_to_string(path)
                    .with_context(|| format!("read RPC contract {}", path.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        let discovered = discover_sources(&sources)
            .with_context(|| format!("discover request fields under {}", src.display()))?;
        for key in discovered {
            if !out.insert(key.clone()) {
                bail!("duplicate discovered input key: {}", render_key(&key));
            }
        }
    }
    Ok(out)
}

fn discover_sources(sources: &[String]) -> Result<BTreeSet<InputKey>> {
    let syntax = sources
        .iter()
        .map(|source| syn::parse_file(source).context("parse API source"))
        .collect::<Result<Vec<_>>>()?;
    let mut types = Types::new();
    for file in &syntax {
        for item in &file.items {
            let (name, shape) = match item {
                Item::Struct(item) => (item.ident.to_string(), parse_struct(item)?),
                Item::Type(item) => (item.ident.to_string(), Shape::Alias((*item.ty).clone())),
                _ => continue,
            };
            if types.insert(name.clone(), shape).is_some() {
                bail!("ambiguous request DTO {name:?}: declared more than once in one API crate");
            }
        }
    }

    let mut out = BTreeSet::new();
    for file in &syntax {
        for item in &file.items {
            let Item::Trait(item) = item else { continue };
            let Some(prefix) = rpc_contract_model::trait_prefix(item)? else {
                continue;
            };
            let mut item = item.clone();
            for method in rpc_contract_model::build_methods(&mut item)? {
                let wire_method =
                    format!("{prefix}.{}", lower_camel(&method.method_ident.to_string()));
                let exposure = if method.http.is_some() {
                    Exposure::External
                } else {
                    Exposure::Wire
                };
                for arg in method.args {
                    let arg_name = arg.ident.to_string();
                    let wire_name = arg.rename.unwrap_or(arg_name);
                    collect_type(
                        &arg.ty,
                        &wire_method,
                        &wire_name,
                        exposure,
                        &types,
                        &mut BTreeSet::new(),
                        &mut out,
                    )?;
                }
            }
        }
    }
    Ok(out)
}

/// Walks one request argument (or nested field) and records every string it can carry.
///
/// TOTAL BY CONSTRUCTION: every arm either records a key, recurses, or `bail!`s. A type
/// the traversal cannot resolve is a hard failure, never a silent skip — a skipped type
/// means an unbounded caller string reaches storage with every conformance gate green.
fn collect_type(
    ty: &Type,
    method: &str,
    field: &str,
    exposure: Exposure,
    types: &Types,
    visiting: &mut BTreeSet<String>,
    out: &mut BTreeSet<InputKey>,
) -> Result<()> {
    if let Type::Reference(reference) = ty {
        return collect_type(&reference.elem, method, field, exposure, types, visiting, out);
    }
    if is_string(ty) {
        let key = InputKey {
            wire_method: method.to_owned(),
            wire_field_name: field.to_owned(),
            exposure,
        };
        if !out.insert(key.clone()) {
            bail!("duplicate discovered input key: {}", render_key(&key));
        }
        return Ok(());
    }
    if let Some(inner) = container_inner(ty) {
        return collect_type(inner, method, field, exposure, types, visiting, out);
    }
    let Some(name) = type_name(ty) else {
        bail!(
            "{method} request field {field:?} has {}, which the input inventory cannot \
             traverse. Request arguments must be named types (see the accepted set in \
             tools/conformance/src/input_inventory.rs).",
            describe(ty)
        );
    };
    if let Some((key_ty, value_ty)) = string_map_args(ty) {
        collect_type(
            key_ty,
            method,
            &format!("{field}.<key>"),
            exposure,
            types,
            visiting,
            out,
        )?;
        return collect_type(
            value_ty,
            method,
            &format!("{field}.<value>"),
            exposure,
            types,
            visiting,
            out,
        );
    }
    if let Some(shape) = types.get(&name) {
        if !visiting.insert(name.clone()) {
            bail!("recursive request DTO {name:?} is unsupported");
        }
        match shape {
            Shape::Alias(target) => {
                collect_type(target, method, field, exposure, types, visiting, out)?;
            }
            Shape::Named(fields) => {
                for (child, child_ty) in fields {
                    collect_type(
                        child_ty,
                        method,
                        &format!("{field}.{child}"),
                        exposure,
                        types,
                        visiting,
                        out,
                    )?;
                }
            }
            Shape::Unnamed => bail!(
                "{method} request field {field:?} has type {name:?}, a tuple or unit struct: \
                 its members have no wire field names, so no input policy can be attached to \
                 them. Give it named fields."
            ),
        }
        visiting.remove(&name);
        return Ok(());
    }
    if SCALAR_TYPES.contains(&name.as_str()) {
        return Ok(());
    }
    if OPAQUE_REQUEST_TYPES.iter().any(|(ty, _)| *ty == name) {
        return Ok(());
    }
    bail!(
        "{method} request field {field:?} has type {name:?}, which the input inventory \
         cannot traverse: it is not a String, a container or string map of one, a DTO or \
         alias declared in this API crate, a primitive scalar, or a listed opaque type. \
         Either give it named String fields in its own api crate, or — if it genuinely \
         carries no caller-supplied text — add it to OPAQUE_REQUEST_TYPES in \
         tools/conformance/src/input_inventory.rs together with the reason."
    );
}

fn container_inner(ty: &Type) -> Option<&Type> {
    type_args(ty, &["Option", "Vec", "Box"])?.first().copied()
}

/// The `(K, V)` of a string-keyed map, for the map types the request surface may use.
fn string_map_args(ty: &Type) -> Option<(&Type, &Type)> {
    let args = type_args(ty, STRING_MAP_TYPES)?;
    match args[..] {
        [key, value] => Some((key, value)),
        _ => None,
    }
}

/// The angle-bracketed type arguments of `ty`, if its last path segment is one of `names`.
fn type_args<'a>(ty: &'a Type, names: &[&str]) -> Option<Vec<&'a Type>> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if !names.contains(&segment.ident.to_string().as_str()) {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    Some(
        args.args
            .iter()
            .filter_map(|arg| match arg {
                GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
    )
}

/// A human name for a type the traversal rejects, so the failure says WHAT it hit.
fn describe(ty: &Type) -> &'static str {
    match ty {
        Type::Array(_) => "an array type",
        Type::BareFn(_) => "a function-pointer type",
        Type::Group(_) | Type::Paren(_) => "a grouped type",
        Type::ImplTrait(_) => "an `impl Trait` type",
        Type::Infer(_) => "an inferred (`_`) type",
        Type::Macro(_) => "a macro-generated type",
        Type::Never(_) => "the never (`!`) type",
        Type::Ptr(_) => "a raw-pointer type",
        Type::Slice(_) => "a slice type",
        Type::TraitObject(_) => "a trait-object type",
        Type::Tuple(_) => "a tuple type",
        _ => "an unnamed type",
    }
}

fn is_string(ty: &Type) -> bool {
    matches!(type_name(ty).as_deref(), Some("String" | "str"))
}

fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else { return None };
    Some(path.path.segments.last()?.ident.to_string())
}

fn parse_struct(item: &ItemStruct) -> Result<Shape> {
    let Fields::Named(named) = &item.fields else {
        return Ok(Shape::Unnamed);
    };
    let mut fields = Vec::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        fields.push((
            serde_rename(&field.attrs)?.unwrap_or_else(|| ident.to_string()),
            field.ty.clone(),
        ));
    }
    Ok(Shape::Named(fields))
}

fn serde_rename(attrs: &[Attribute]) -> Result<Option<String>> {
    let mut rename = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            }
            Ok(())
        })?;
    }
    Ok(rename)
}

fn sorted_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = std::fs::read_dir(root)
        .with_context(|| format!("read API root {}", root.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    Ok(dirs)
}

fn lower_camel(snake: &str) -> String {
    let mut parts = snake.split('_').filter(|part| !part.is_empty());
    let mut out = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

pub fn render_key(key: &InputKey) -> String {
    format!(
        "{}\t{}\t{}",
        key.wire_method,
        key.wire_field_name,
        key.exposure.label()
    )
}

pub fn render_golden(keys: &BTreeSet<InputKey>) -> String {
    let mut out = String::from("wire_method\twire_field_name\texposure\n");
    for key in keys {
        out.push_str(&render_key(key));
        out.push('\n');
    }
    out
}

pub fn api_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api")
}

pub fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("input-fields.golden.tsv")
}

pub fn golden_findings(actual: &str, committed: &str) -> Vec<String> {
    if actual == committed {
        Vec::new()
    } else {
        vec!["input inventory differs from tools/conformance/input-fields.golden.tsv — update tools/conformance/src/policy.rs, then regenerate the snapshot with `cargo run -p verifyctl -- --bless-input-golden` and commit it".to_owned()]
    }
}

pub fn policy_key_findings(discovered: &BTreeSet<InputKey>, policy: &[InputKey]) -> Vec<String> {
    let mut findings = Vec::new();
    let policy_set = policy.iter().cloned().collect::<BTreeSet<_>>();
    if policy_set.len() != policy.len() {
        let mut seen = BTreeSet::new();
        for key in policy {
            if !seen.insert(key) {
                findings.push(format!("duplicate input policy for {}", render_key(key)));
            }
        }
    }
    for key in discovered.difference(&policy_set) {
        findings.push(format!("missing input policy for {}", render_key(key)));
    }
    for key in policy_set.difference(discovered) {
        findings.push(format!("orphan input policy for {}", render_key(key)));
    }
    findings
}

#[cfg(test)]
#[path = "input_inventory_tests.rs"]
mod tests;

//! Source-derived inventory of string-bearing RPC request fields.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use syn::{
    Attribute, Fields, GenericArgument, Item, ItemStruct, ItemTrait, PathArguments, TraitItem, Type,
};

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

/// One NON-primitive request type the traversal deliberately stops at.
pub struct OpaqueType {
    /// The type path EXACTLY as a contract writes it. Matched against the written path,
    /// not the last segment, so a foreign `somecrate::Identity` is not allowlisted by
    /// sharing a name with the listed type.
    pub name: &'static str,
    /// Workspace-relative file declaring the type — the source of truth the list's
    /// self-check (`input_inventory_tests.rs`) parses, so an entry naming a renamed or
    /// deleted type fails instead of quietly allowlisting nothing.
    pub declared_in: &'static str,
    /// Why a request argument of this type carries no caller-supplied text.
    pub why: &'static str,
}

/// The NON-primitive request types the traversal stops at. Anything else is a hard
/// failure, so a new foreign type cannot enter a contract's request surface unexamined.
pub const OPAQUE_REQUEST_TYPES: &[OpaqueType] = &[OpaqueType {
    name: "Identity",
    declared_in: "core/opsapi/src/lib.rs",
    why: "opsapi::Identity is set at exactly two trusted seams — the gateway front handler \
          after bearer verification, and the generated edge-server adapter from the \
          mTLS-authenticated request envelope's identity field — and never parsed from a \
          caller-supplied request field. Its player_id IS written to storage by domain \
          modules, so the property that matters is the provenance, not the destination; \
          the wrapped string is unreachable as a request DTO field because the type has no \
          public field at all",
}];

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
    let mut traits = Vec::new();
    for file in &syntax {
        walk_items(&file.items, &mut types, &mut traits)?;
    }

    let mut out = BTreeSet::new();
    for item in traits {
        let Some(prefix) = rpc_contract_model::trait_prefix(item)? else {
            continue;
        };
        for trait_item in &item.items {
            if let TraitItem::Macro(mac) = trait_item {
                bail!(
                    "#[rpc] trait {:?} has a macro invocation {}! in its body, so its real \
                     method set is not visible to the input inventory. Write the methods out.",
                    item.ident.to_string(),
                    path_string(&mac.mac.path)
                );
            }
        }
        let mut item = item.clone();
        for method in rpc_contract_model::build_methods(&mut item)? {
            let wire_method = format!("{prefix}.{}", lower_camel(&method.method_ident.to_string()));
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
    Ok(out)
}

/// Collects every declared request DTO and every trait, RECURSING into inline `mod`
/// blocks.
///
/// TOTAL BY CONSTRUCTION, like [`collect_type`]: an item either contributes, is
/// descended into, or is a shape that cannot declare a contract. A top-level-only walk
/// makes an `#[rpc]` trait or request DTO inside `pub mod admin { … }` invisible — no
/// key, no golden row, and every input gate green over an unexamined caller string.
fn walk_items<'a>(
    items: &'a [Item],
    types: &mut Types,
    traits: &mut Vec<&'a ItemTrait>,
) -> Result<()> {
    for item in items {
        let (name, shape) = match item {
            Item::Struct(item) => (item.ident.to_string(), parse_struct(item)?),
            Item::Type(item) => (item.ident.to_string(), Shape::Alias((*item.ty).clone())),
            Item::Trait(item) => {
                traits.push(item);
                continue;
            }
            Item::Mod(module) => {
                // `#[cfg(test)]` mirrors rpc_contract_model::contract_sources' exclusion of
                // tests.rs: a fixture contract inside a test module is not a real contract.
                if !is_cfg_test(&module.attrs) {
                    if let Some((_, inner)) = &module.content {
                        walk_items(inner, types, traits)?;
                    }
                }
                continue;
            }
            // An item-position macro INVOCATION can expand to a trait or a DTO the scan
            // would never see. A `macro_rules!` DEFINITION (ident set) declares nothing by
            // itself — every use of it is an invocation, caught here.
            Item::Macro(mac) if mac.ident.is_none() => bail!(
                "item-position macro invocation {}! in a contract source: it may expand to an \
                 #[rpc] trait or a request DTO the input inventory cannot see. Write the \
                 declaration out.",
                path_string(&mac.mac.path)
            ),
            _ => continue,
        };
        if types.insert(name.clone(), shape).is_some() {
            bail!("ambiguous request DTO {name:?}: declared more than once in one API crate");
        }
    }
    Ok(())
}

fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                found |= meta.path.is_ident("test");
                Ok(())
            });
            found
        }
    })
}

/// A path rendered the way it is WRITTEN (`opsapi::Identity`), generic arguments
/// dropped — the form [`OPAQUE_REQUEST_TYPES`] is matched against.
fn path_string(path: &syn::Path) -> String {
    let mut out = if path.leading_colon.is_some() {
        "::".to_owned()
    } else {
        String::new()
    };
    for (index, segment) in path.segments.iter().enumerate() {
        if index > 0 {
            out.push_str("::");
        }
        out.push_str(&segment.ident.to_string());
    }
    out
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
    let written = written_path(ty).unwrap_or_else(|| name.clone());
    if OPAQUE_REQUEST_TYPES
        .iter()
        .any(|opaque| opaque.name == written)
    {
        return Ok(());
    }
    bail!(
        "{method} request field {field:?} has type {written:?}, which the input inventory \
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

fn written_path(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else { return None };
    path.qself.is_none().then(|| path_string(&path.path))
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

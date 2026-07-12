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
type Structs = BTreeMap<String, StructFields>;

pub fn discover(api_root: &Path) -> Result<BTreeSet<InputKey>> {
    let mut out = BTreeSet::new();
    for domain in sorted_dirs(api_root)? {
        if domain.file_name().is_some_and(|name| name == "admin") {
            continue;
        }
        let src = domain.join("api/src");
        if !src.is_dir() {
            continue;
        }
        let files = rust_files(&src)?;
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
    let mut structs = Structs::new();
    for file in &syntax {
        for item in &file.items {
            if let Item::Struct(item) = item {
                if let Some((name, fields)) = parse_struct(item)? {
                    if structs.insert(name.clone(), fields).is_some() {
                        bail!("ambiguous request DTO {name:?}: declared more than once in one API crate");
                    }
                }
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
                        &structs,
                        &mut BTreeSet::new(),
                        &mut out,
                    )?;
                }
            }
        }
    }
    Ok(out)
}

fn collect_type(
    ty: &Type,
    method: &str,
    field: &str,
    exposure: Exposure,
    structs: &Structs,
    visiting: &mut BTreeSet<String>,
    out: &mut BTreeSet<InputKey>,
) -> Result<()> {
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
        return collect_type(inner, method, field, exposure, structs, visiting, out);
    }
    let Some(name) = type_name(ty) else {
        return Ok(());
    };
    let Some(fields) = structs.get(&name) else {
        return Ok(());
    };
    if !visiting.insert(name.clone()) {
        bail!("recursive request DTO {name:?} is unsupported");
    }
    for (child, child_ty) in fields {
        collect_type(
            child_ty,
            method,
            &format!("{field}.{child}"),
            exposure,
            structs,
            visiting,
            out,
        )?;
    }
    visiting.remove(&name);
    Ok(())
}

fn container_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if matches!(segment.ident.to_string().as_str(), "Option" | "Vec" | "Box") {
        let PathArguments::AngleBracketed(args) = &segment.arguments else {
            return None;
        };
        args.args.iter().find_map(|arg| match arg {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
    } else {
        None
    }
}

fn is_string(ty: &Type) -> bool {
    type_name(ty).as_deref() == Some("String")
}

fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else { return None };
    Some(path.path.segments.last()?.ident.to_string())
}

fn parse_struct(item: &ItemStruct) -> Result<Option<(String, StructFields)>> {
    let Fields::Named(named) = &item.fields else {
        return Ok(None);
    };
    let mut fields = Vec::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        fields.push((
            serde_rename(&field.attrs)?.unwrap_or_else(|| ident.to_string()),
            field.ty.clone(),
        ));
    }
    Ok(Some((item.ident.to_string(), fields)))
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

fn rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("read source directory {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
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
        vec!["input inventory differs from tools/conformance/input-fields.golden.tsv — update policy and commit the regenerated snapshot".to_owned()]
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
mod tests {
    use super::*;

    #[test]
    fn traverses_request_dtos_but_not_outputs() {
        let source = r#"
            #[derive(serde::Serialize)]
            pub struct Request { #[serde(rename = "displayName")] pub display_name: String, pub tags: Vec<Option<String>> }
            pub struct Output { pub secret: String }
            #[rpc(prefix = "demo")]
            pub trait Demo { #[http(verb="POST", path="/", auth="none", success=200)] async fn send(&self, request: Request) -> Result<Output, Error>; }
        "#;
        let keys = discover_sources(&[source.to_owned()]).unwrap();
        assert_eq!(
            keys.into_iter()
                .map(|key| render_key(&key))
                .collect::<Vec<_>>(),
            [
                "demo.send\trequest.displayName\texternal",
                "demo.send\trequest.tags\texternal"
            ]
        );
    }

    #[test]
    fn golden_omission_is_a_finding() {
        assert!(!golden_findings("header\na\n", "header\n").is_empty());
    }

    #[test]
    fn missing_or_orphan_or_duplicate_policy_is_a_finding() {
        let a = InputKey {
            wire_method: "demo.send".into(),
            wire_field_name: "a".into(),
            exposure: Exposure::External,
        };
        let b = InputKey {
            wire_field_name: "b".into(),
            ..a.clone()
        };
        let discovered = BTreeSet::from([a]);
        let findings = policy_key_findings(&discovered, &[b.clone(), b]);
        assert!(findings
            .iter()
            .any(|finding| finding.contains("missing input policy")));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("orphan input policy")));
        assert!(findings
            .iter()
            .any(|finding| finding.contains("duplicate input policy")));
    }
}

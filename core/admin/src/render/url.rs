use controlplane::{EntityRef, Link};
use super::*;

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// Percent-encodes everything outside RFC 3986's unreserved set.
pub(crate) fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn entity_url(entity: &EntityRef) -> String {
    format!("/admin/e/{}/{}", encode(&entity.kind), encode(&entity.id))
}

pub(crate) fn link_url(link: &Link) -> String {
    seen::record(link);
    match link {
        Link::Entity(entity) => entity_url(entity),
        Link::Page { module, page, params } => {
            let mut url = format!("/admin/p/{}/{}", encode(module), encode(page));
            let query: Vec<String> = params.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect();
            if !query.is_empty() {
                url.push('?');
                url.push_str(&query.join("&"));
            }
            url
        }
    }
}

pub(crate) fn page_url(module: &str, page: &str) -> String {
    format!("/admin/p/{}/{}", encode(module), encode(page))
}

pub(crate) fn entity_param(entity: &EntityRef) -> String {
    format!("{}:{}", entity.kind, entity.id)
}

/// Panel URL query keys of the typed `PanelQuery` fields.
pub(crate) const QUERY_SEARCH: &str = controlplane::PAGE_PARAM_SEARCH;
pub(crate) const QUERY_CURSOR: &str = "cursor";
pub(crate) const QUERY_BEFORE: &str = "before";
pub(crate) const QUERY_STATE: &str = controlplane::PAGE_PARAM_QUEUE_STATE;
/// A modal URL's bound element.
pub(crate) const QUERY_ITEM: &str = "item";

/// `path` with `query` appended, `key` replaced by `value` (dropped when `value` is `None`).
pub(crate) fn with_param(path: &str, query: &[(String, String)], key: &str, value: Option<&str>) -> String {
    let mut parts: Vec<String> = query
        .iter()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    if let Some(v) = value {
        parts.push(format!("{}={}", encode(key), encode(v)));
    }
    if parts.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

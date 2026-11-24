use std::collections::BTreeMap;
use controlplane::EntityRef;
use serde::Serialize;
use crate::engine::actions;
use super::*;

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Disabled actions carry their reason in `hint` and its kind in `refusal`.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionItem {
    pub(crate) module: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) danger: bool,
    /// `primary`, `default` or `ghost`.
    pub(crate) emphasis: &'static str,
    /// The icon follows the label.
    pub(crate) trailing_icon: bool,
    /// The icon alone, the label as its accessible name: only a `TableRow` button draws it.
    pub(crate) icon_only: bool,
    /// Shown on an empty panel.
    pub(crate) when_empty: bool,
    /// An `ActionPoint::QueueRow` action: drawn on the queue items listing it, never in the
    /// panel tools.
    pub(crate) queue_row: bool,
    /// An `ActionPoint::TableRow` action: drawn on the table rows listing it, never in the
    /// panel tools.
    pub(crate) table_row: bool,
    /// `GET` opens the modal, `POST` invokes.
    pub(crate) url: String,
    pub(crate) enabled: bool,
    pub(crate) hint: Option<String>,
    /// Why the action is drawn disabled: its admission refusal, or `ModuleDown` in a menu group
    /// whose module is down ([`menu_groups`]); `None` for an action drawn enabled.
    pub(crate) refusal: Option<actions::RefusalKind>,
    /// `invoke`, `form`, `confirm`, `link` or `copy`.
    pub(crate) then: &'static str,
    pub(crate) href: Option<String>,
    /// Fields an `invoke` posts without a modal; rendered as `hx-vals` JSON.
    #[serde(serialize_with = "as_json")]
    pub(crate) vals: BTreeMap<String, String>,
}

fn as_json<S: serde::Serializer>(vals: &BTreeMap<String, String>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&serde_json::to_string(vals).map_err(serde::ser::Error::custom)?)
}

pub(crate) fn then_name(then: &controlplane::Then) -> &'static str {
    seen::record(then);
    match then {
        controlplane::Then::Invoke => "invoke",
        controlplane::Then::Form(_) => "form",
        controlplane::Then::Confirm(_) => "confirm",
        controlplane::Then::Link(_) => "link",
        controlplane::Then::Copy => "copy",
    }
}

pub(crate) fn emphasis_name(emphasis: controlplane::Emphasis) -> &'static str {
    seen::record(&emphasis);
    match emphasis {
        controlplane::Emphasis::Primary => "primary",
        controlplane::Emphasis::Default => "default",
        controlplane::Emphasis::Ghost => "ghost",
    }
}

/// `action` bound to an entity and an element: the modal URL carries `?entity=` and
/// `?item=`, a token-less invoke posts `_entity` and `_item`.
pub(crate) fn bind(action: &ActionItem, entity: Option<&EntityRef>, element: Option<&str>) -> ActionItem {
    let mut bound = action.clone();
    let mut query = Vec::new();
    if let Some(e) = entity {
        query.push(("entity".to_string(), entity_param(e)));
        bound.vals.insert(actions::FIELD_ENTITY.to_string(), entity_param(e));
    }
    if let Some(element) = element {
        query.push((QUERY_ITEM.to_string(), element.to_string()));
        bound.vals.insert(actions::FIELD_ITEM.to_string(), element.to_string());
    }
    bound.url = with_param(&action.url, &query, "", None);
    bound
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MenuGroup {
    pub(crate) module: String,
    pub(crate) items: Vec<ActionItem>,
    /// The module's source missed the latest manifest refresh: the group is marked
    /// unavailable and its items are drawn disabled, each with the `ModuleDown` refusal.
    pub(crate) down: bool,
}

pub(crate) fn menu_groups(items: Vec<ActionItem>, down: impl Fn(&str) -> bool) -> Vec<MenuGroup> {
    let mut groups: Vec<MenuGroup> = Vec::new();
    for mut item in items {
        if down(&item.module) {
            item.refusal = Some(actions::RefusalKind::ModuleDown);
        }
        match groups.iter_mut().find(|g| g.module == item.module) {
            Some(group) => group.items.push(item),
            None => groups.push(MenuGroup { module: item.module.clone(), down: down(&item.module), items: vec![item] }),
        }
    }
    groups
}

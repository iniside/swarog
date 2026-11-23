use std::collections::BTreeMap;
use controlplane::{decode_field, EntityRef, Field, FieldKind, FieldValue, FormSpec, Primitive, ReasonReq};
use serde::Serialize;
use crate::engine::actions::{self, money_currency_key, Chrome};
use crate::engine::labels::Labels;
use crate::engine::panels::PanelState;
use super::*;

// ---- Form ----

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FieldView {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) type_name: String,
    /// `input`, `int`, `textarea`, `toggle`, `select`, `checks`, `ref`, `money` or `request_id`.
    pub(crate) control: &'static str,
    pub(crate) mono: bool,
    pub(crate) placeholder: Option<String>,
    pub(crate) required: bool,
    pub(crate) readonly: bool,
    pub(crate) help: Option<String>,
    pub(crate) value: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected: Vec<String>,
    pub(crate) currency: String,
    /// The body key of a Money field's currency input.
    pub(crate) currency_name: String,
    pub(crate) ref_icon: &'static str,
    pub(crate) ref_kind: String,
    pub(crate) error: Option<String>,
}

/// One segment of a form's `div.seg`; `on` is the picked mode.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ModeView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) submit_label: String,
    pub(crate) on: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FormView {
    pub(crate) fields: Vec<FieldView>,
    /// The picked mode's submit label for a form with modes.
    pub(crate) submit_label: String,
    pub(crate) note: Option<String>,
    pub(crate) modes: Vec<ModeView>,
    /// The picked mode's id, posted as `_mode`; empty for a form without modes.
    pub(crate) mode: String,
    /// The change note posted as `_reason`: `required`, `optional` or `none` (no input).
    pub(crate) reason: &'static str,
    pub(crate) reason_label: String,
    /// The change note input's placeholder.
    pub(crate) reason_hint: Option<String>,
    pub(crate) reason_value: String,
    pub(crate) reason_error: Option<String>,
}

/// A `Primitive::Form` in a panel; `form_id` and `post_url` are set when it has a form
/// instance to post as.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct PanelFormView {
    #[serde(flatten)]
    pub(crate) form: FormView,
    pub(crate) post_url: Option<String>,
    pub(crate) csrf: String,
    pub(crate) form_id: Option<String>,
    pub(crate) banner: Option<String>,
    /// Save stays disabled: the instance is consumed.
    pub(crate) locked: bool,
}

fn posted_values(posted: Option<&[(String, String)]>, key: &str) -> Vec<String> {
    posted.unwrap_or_default().iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
}

/// A field shows its stored default when the form is fresh or the field readonly, else what
/// was posted; Multi and Money show the decoded value where it decodes.
fn field(f: &Field, posted: Option<&[(String, String)]>, errors: &BTreeMap<String, String>) -> FieldView {
    let raw = match posted {
        Some(body) if !f.readonly => actions::posted_raw(f, body),
        _ => f.default.clone(),
    };
    let decoded = raw.as_deref().and_then(|r| decode_field(&f.kind, r).ok());
    if let Some(v) = &decoded {
        seen::record(v);
    }
    let currency_name = money_currency_key(&f.key);
    let reposted = posted.filter(|_| !f.readonly);
    let (value, selected, currency) = match (&f.kind, decoded) {
        (_, Some(FieldValue::Multi(picked))) => (String::new(), picked, String::new()),
        (_, Some(FieldValue::Money { amount, currency })) => (amount, Vec::new(), currency),
        (FieldKind::Multi(_), _) => (String::new(), posted_values(reposted, &f.key), String::new()),
        (FieldKind::Money(_), _) => {
            let amount = posted_values(reposted, &f.key).into_iter().next().unwrap_or_default();
            let currency = posted_values(reposted, &currency_name).into_iter().next().unwrap_or_default();
            (amount, Vec::new(), currency)
        }
        _ => (raw.unwrap_or_default(), Vec::new(), String::new()),
    };
    seen::record(&f.kind);
    let (control, type_name, options, mono) = match &f.kind {
        FieldKind::String => ("input", "string".to_string(), Vec::new(), false),
        FieldKind::Text => ("textarea", "text".to_string(), Vec::new(), false),
        FieldKind::Int => ("int", "int".to_string(), Vec::new(), false),
        FieldKind::Float => ("input", "float".to_string(), Vec::new(), false),
        FieldKind::Bool => ("toggle", "bool".to_string(), Vec::new(), false),
        FieldKind::Enum(o) => ("select", "enum".to_string(), o.clone(), false),
        FieldKind::Multi(o) => ("checks", "multi".to_string(), o.clone(), false),
        FieldKind::Json => ("textarea", "json".to_string(), Vec::new(), true),
        FieldKind::Ref(kind) => ("ref", format!("ref {kind}"), Vec::new(), false),
        FieldKind::Money(c) => ("money", "money".to_string(), c.clone(), false),
        FieldKind::Duration => ("input", "duration".to_string(), Vec::new(), false),
        FieldKind::RequestId => ("request_id", "idempotency key".to_string(), Vec::new(), true),
    };
    let ref_kind = match &f.kind {
        FieldKind::Ref(kind) => kind.clone(),
        _ => String::new(),
    };
    FieldView {
        key: f.key.clone(),
        label: f.label.clone(),
        type_name,
        control,
        mono: mono || f.mono,
        placeholder: f.placeholder.clone(),
        required: f.required,
        readonly: f.readonly,
        help: f.help.clone(),
        selected,
        currency,
        currency_name,
        value,
        options,
        ref_icon: if matches!(ref_kind.as_str(), "player" | "user") { "user" } else { "hash" },
        ref_kind,
        error: errors.get(&f.key).cloned(),
    }
}

/// `instance` is the id of the form instance the form posts as, `None` when it has none;
/// a request id field shows it, whatever the mode, and is left out without it. `posted` is
/// the body being shown again, `None` for a fresh form. The picked mode is the one `posted`
/// names, else the first.
pub(crate) fn form(
    spec: &FormSpec,
    instance: Option<&str>,
    posted: Option<&[(String, String)]>,
    errors: &BTreeMap<String, String>,
) -> FormView {
    let named = posted.and_then(|body| actions::first(body, actions::FIELD_MODE));
    let picked = spec.modes.iter().find(|m| Some(m.id.as_str()) == named).or(spec.modes.first());
    let modes = spec
        .modes
        .iter()
        .map(|m| ModeView {
            id: m.id.clone(),
            label: m.label.clone(),
            submit_label: m.submit_label.clone(),
            on: picked.is_some_and(|p| p.id == m.id),
        })
        .collect();
    seen::record(&spec.reason);
    let reason = match spec.reason {
        ReasonReq::Required => "required",
        ReasonReq::Optional => "optional",
        ReasonReq::None => "none",
    };
    FormView {
        fields: spec
            .fields
            .iter()
            .filter_map(|f| match (&f.kind, instance) {
                (FieldKind::RequestId, None) => None,
                (FieldKind::RequestId, Some(id)) => {
                    Some(FieldView { value: id.to_string(), readonly: true, ..field(f, None, errors) })
                }
                _ => Some(field(f, posted, errors)),
            })
            .collect(),
        submit_label: picked.map_or_else(|| spec.submit_label.clone(), |m| m.submit_label.clone()),
        note: spec.note.clone(),
        modes,
        mode: picked.map(|m| m.id.clone()).unwrap_or_default(),
        reason,
        reason_label: spec.reason_label.clone().unwrap_or_else(|| "Reason".to_string()),
        reason_hint: spec.reason_hint.clone(),
        reason_value: posted.and_then(|b| actions::first(b, actions::FIELD_REASON)).unwrap_or_default().to_string(),
        reason_error: errors.get(actions::FIELD_REASON).cloned(),
    }
}

pub(super) fn panel_form(spec: &FormSpec, input: &PanelInput<'_>) -> PanelFormView {
    let none = BTreeMap::new();
    let instance = input.form.as_ref();
    PanelFormView {
        form: form(
            spec,
            instance.map(|i| i.form_id.as_str()),
            instance.and_then(|i| i.posted),
            instance.map_or(&none, |i| i.errors),
        ),
        post_url: instance.map(|i| i.post_url.clone()),
        csrf: input.csrf.to_string(),
        form_id: instance.map(|i| i.form_id.clone()),
        banner: instance.and_then(|i| i.banner.clone()),
        locked: instance.is_some_and(|i| i.locked),
    }
}

/// A panel form's frame: the contribution its instance was minted for.
pub(crate) struct FormFrame<'a> {
    pub(crate) module: &'a str,
    pub(crate) decl: &'a controlplane::ContributionDecl,
    pub(crate) mode: &'static str,
    pub(crate) path: &'a str,
    pub(crate) query: &'a [(String, String)],
    pub(crate) entity: Option<&'a EntityRef>,
    pub(crate) actions: &'a [ActionItem],
    pub(crate) csrf: &'a str,
}

/// The panel fragment of a panel form shown again from its stored spec and chrome after a
/// 422 or 409.
pub(crate) fn panel_form_result(
    frame: &FormFrame<'_>,
    spec: &FormSpec,
    chrome: &Chrome,
    instance: FormInstance<'_>,
) -> PanelRender {
    let body = chrome.around(Primitive::Form(spec.clone()));
    let input = PanelInput {
        module: frame.module,
        contribution: &frame.decl.id,
        title: &frame.decl.title,
        icon: &frame.decl.icon,
        mode: frame.mode,
        state: PanelState::Ok,
        path: frame.path,
        query: frame.query,
        entity: frame.entity,
        search: None,
        error: None,
        age_s: None,
        last_value: None,
        min_role: crate::role_name(frame.decl.min_role),
        fallback_title: "",
        readonly: frame.decl.readonly.as_deref(),
        body: Some(&body),
        actions: frame.actions,
        row_menu_kinds: &[],
        header_kinds: &[],
        list_columns: &[],
        labels: &Labels::new(),
        csrf: frame.csrf,
        form: Some(instance),
    };
    panel(&input)
}

use std::collections::BTreeMap;
use controlplane::{ConfirmSpec, ConfirmTone, FormSpec, ReasonReq};
use serde::Serialize;
use crate::engine::actions;
use super::*;

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

/// What a modal renders: the form or the confirmation its instance stored.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ModalSpec<'a> {
    Form(&'a FormSpec),
    Confirm(&'a ConfirmSpec),
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ModalView {
    pub(crate) label: String,
    pub(crate) post_url: String,
    pub(crate) csrf: String,
    /// The form instance the modal posts as `_form`.
    pub(crate) form_id: String,
    /// The `modal-icon` class.
    pub(crate) tone: &'static str,
    pub(crate) icon: String,
    /// `danger` or `primary`.
    pub(crate) go: &'static str,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) impact: Vec<(String, CellView)>,
    /// `required`, `optional` or `none`.
    pub(crate) reason: &'static str,
    /// The reason input's placeholder.
    pub(crate) reason_hint: Option<String>,
    pub(crate) reason_value: String,
    pub(crate) reason_error: Option<String>,
    pub(crate) type_to_confirm: Option<String>,
    pub(crate) confirm_label: String,
    pub(crate) form: Option<FormView>,
    pub(crate) banner: Option<String>,
    /// Submit stays disabled: the instance is consumed.
    pub(crate) locked: bool,
}

/// The modal of `decl` for its form instance `form_id`; `posted` is the body being shown
/// again with `errors`, `None` for a fresh modal.
pub(crate) fn modal(
    decl: &controlplane::ActionDecl,
    spec: ModalSpec<'_>,
    post_url: String,
    csrf: String,
    form_id: String,
    posted: Option<&[(String, String)]>,
    errors: &BTreeMap<String, String>,
) -> ModalView {
    let reason_value = posted.and_then(|b| actions::first(b, actions::FIELD_REASON)).unwrap_or_default();
    let mut view = ModalView {
        label: decl.label.clone(),
        post_url,
        csrf,
        form_id,
        tone: "info",
        icon: if decl.icon.is_empty() { "info".to_string() } else { decl.icon.clone() },
        go: if decl.danger { "danger" } else { "primary" },
        title: decl.label.clone(),
        body: String::new(),
        impact: Vec::new(),
        reason: "none",
        reason_hint: None,
        reason_value: reason_value.to_string(),
        reason_error: errors.get(actions::FIELD_REASON).cloned(),
        type_to_confirm: None,
        confirm_label: decl.label.clone(),
        form: None,
        banner: None,
        locked: false,
    };
    match spec {
        ModalSpec::Form(spec) => {
            view.tone = "ok";
            view.go = "primary";
            view.form = Some(form(spec, posted, errors));
            if let Some(title) = &spec.title {
                view.title = title.clone();
            }
        }
        ModalSpec::Confirm(spec) => {
            seen::record(&spec.tone);
            view.tone = match spec.tone {
                ConfirmTone::Danger => "danger",
                ConfirmTone::Warn => "warn",
                ConfirmTone::Info => "info",
            };
            view.go = if spec.tone == ConfirmTone::Danger { "danger" } else { "primary" };
            if decl.icon.is_empty() {
                view.icon = if spec.tone == ConfirmTone::Danger { "alert" } else { "info" }.to_string();
            }
            view.title = spec.title.clone();
            view.body = spec.body.clone();
            view.impact = spec.impact.iter().map(|(k, v)| (k.clone(), cell(v))).collect();
            view.reason_hint = spec.reason_hint.clone();
            seen::record(&spec.reason);
            view.reason = match spec.reason {
                ReasonReq::Required => "required",
                ReasonReq::Optional => "optional",
                ReasonReq::None => "none",
            };
            view.type_to_confirm = spec.type_to_confirm.clone();
            view.confirm_label = spec.confirm_label.clone();
        }
    }
    let shown: Vec<&str> = view.form.iter().flat_map(|f| f.fields.iter().map(|x| x.key.as_str())).collect();
    let stray: Vec<String> = errors
        .iter()
        .filter(|(k, _)| {
            !shown.contains(&k.as_str()) && !(k.as_str() == actions::FIELD_REASON && view.reason != "none")
        })
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    if !stray.is_empty() {
        view.banner = Some(stray.join("; "));
    }
    view
}

/// A non-redirect POST result: the modal shown again, or a toast.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionResultView {
    pub(crate) modal: Option<ModalView>,
    pub(crate) toast: Option<String>,
    /// The toast's tone: `err` or `info`.
    pub(crate) tone: &'static str,
}

/// The modal of `decl` shown again after a POST for `instance`: its banner, when set,
/// replaces the stray-error banner.
pub(crate) fn modal_rerender(
    decl: &controlplane::ActionDecl,
    spec: ModalSpec<'_>,
    csrf: String,
    instance: FormInstance<'_>,
) -> ActionResultView {
    let mut view = modal(decl, spec, instance.post_url, csrf, instance.form_id, instance.posted, instance.errors);
    if instance.banner.is_some() {
        view.banner = instance.banner;
    }
    view.locked = instance.locked;
    ActionResultView { modal: Some(view), toast: None, tone: "err" }
}

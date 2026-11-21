//! No disabled action control without its action and refusal kind: every disabled `.btn` and
//! `.menu-item.disabled` a golden case draws for a refused `ActionItem` carries
//! `data-refused-action` and `data-refusal`, which `tools/admindiff`'s crawl reads. A control
//! is an action refusal by what production drew it with, not by the macro that drew it: its
//! title is a `Refusal::hint`, the hint of an action `routes::action_item` refused while the
//! case was built (a declared `ReadOnly` text), or a menu group's module-down title. Any other
//! titled disabled control is listed in [`NOT_ACTIONS`] with its reason.

use super::*;

/// The titled disabled controls that are no action, each with why it carries no action id.
const NOT_ACTIONS: &[(&str, &str)] = &[(
    "Read-only",
    "panel_form's Save of a form drawn with no form id: a form control, not an action, so it has no action id to carry",
)];

/// The title the row menu gives each item of a group whose module is down, after the module:
/// `menu` in `primitives.html.tmpl`.
const MODULE_DOWN: &str = " module unavailable";

/// One opening tag of a render: its name and its attributes, values unescaped.
struct Tag {
    name: String,
    attrs: Vec<(String, Option<String>)>,
    text: String,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_deref().unwrap_or(""))
    }

    fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_whitespace().any(|c| c == class))
    }

    /// A disabled action control's shape: a `button.btn[disabled]` or a `.menu-item.disabled`.
    fn disabled_control(&self) -> bool {
        (self.name == "button" && self.has_class("btn") && self.attr("disabled").is_some())
            || (self.has_class("menu-item") && self.has_class("disabled"))
    }
}

/// An attribute value as the template escaped it, unescaped.
fn unescape(v: &str) -> String {
    v.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2f;", "/")
        .replace("&amp;", "&")
}

/// Every element opening tag of `raw`, in order; comments, doctypes and close tags are skipped.
fn tags(raw: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
            continue;
        }
        let mut end = rest.len();
        let mut quote: Option<char> = None;
        for (i, c) in rest.char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '>') => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let inner = &rest[..end];
        let name_end = inner.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        out.push(Tag { name, attrs: attrs(&inner[name_end..]), text: format!("<{inner}>") });
        rest = &rest[end..];
    }
    out
}

/// The attributes of a tag after its name.
fn attrs(mut s: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if s.is_empty() {
            return out;
        }
        let name_end = s.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(s.len());
        let name = s[..name_end].to_ascii_lowercase();
        s = &s[name_end..];
        if let Some(after) = s.strip_prefix('=') {
            let (value, rest) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let close = after[1..].find(q).map_or(after.len(), |i| i + 1);
                    (&after[1..close], &after[(close + 1).min(after.len())..])
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            out.push((name, Some(unescape(value))));
            s = rest;
        } else {
            out.push((name, None));
        }
    }
}

/// The `data-refusal` value of each `RefusalKind`.
fn refusal_kinds() -> Vec<String> {
    use actions::RefusalKind::*;
    [Denied, NotImplemented, ReadOnly, ModuleDown]
        .iter()
        .map(|k| match serde_json::to_value(k).expect("a refusal kind serializes") {
            serde_json::Value::String(s) => s,
            other => panic!("a refusal kind serializes as a string, not {other}"),
        })
        .collect()
}

/// Every disabled control of `cases` drawn for a refused action that misses its action
/// (`<module>/<action>`) or its refusal kind, every titled disabled control that is no action
/// refusal and no [`NOT_ACTIONS`] entry, every control carrying a refusal that is not drawn
/// disabled, and every `NOT_ACTIONS` entry no case draws.
pub(super) fn check_refusals(cases: &[Case]) -> Result<(), Vec<String>> {
    let fixed = [actions::Refusal::Denied, actions::Refusal::NotImplemented, actions::Refusal::ReadOnly(None)];
    let hints: Vec<String> = fixed.iter().map(actions::Refusal::hint).collect();
    let kinds = refusal_kinds();
    let mut problems = Vec::new();
    let mut used = BTreeSet::new();
    for case in cases {
        for tag in tags(&case.raw) {
            let carries = tag.attr("data-refused-action").is_some() || tag.attr("data-refusal").is_some();
            if !tag.disabled_control() {
                if carries {
                    problems.push(format!("{}: {} carries a refusal but is drawn enabled", case.name, tag.text));
                }
                continue;
            }
            let Some(title) = tag.attr("title").filter(|t| !t.is_empty()) else { continue };
            let refusal = hints.iter().any(|h| h == title) || case.refused.contains(title) || title.ends_with(MODULE_DOWN);
            if refusal {
                let action = tag
                    .attr("data-refused-action")
                    .and_then(|a| a.split_once('/'))
                    .is_some_and(|(m, a)| !m.is_empty() && !a.is_empty());
                let kind = tag.attr("data-refusal").is_some_and(|k| kinds.iter().any(|known| known == k));
                if !action || !kind {
                    problems.push(format!(
                        "{}: {} draws an action refusal ({title:?}) without its action and refusal kind",
                        case.name, tag.text
                    ));
                }
            } else if NOT_ACTIONS.iter().any(|(t, _)| *t == title) {
                used.insert(title.to_string());
            } else {
                problems.push(format!(
                    "{}: {} is disabled with title {title:?}, which is no refusal hint, no refused action's hint and no module-down title; list it in NOT_ACTIONS with why if it is no action",
                    case.name, tag.text
                ));
            }
        }
    }
    for (title, _) in NOT_ACTIONS {
        if !used.contains(*title) {
            problems.push(format!("NOT_ACTIONS {title:?} is stale: no golden case draws a disabled control titled so"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[test]
fn every_disabled_action_control_carries_its_action_and_refusal_kind() {
    let env = render::env().expect("templates compile");
    if let Err(problems) = check_refusals(&cases(&env)) {
        panic!("disabled controls against their refusals:\n  {}", problems.join("\n  "));
    }
}

/// The check fails on each disabled branch of `primitives.html.tmpl` drawn without `refused()`,
/// one at a time, naming the control; and on a stale `NOT_ACTIONS` entry.
#[test]
fn the_refusal_check_fails_on_a_disabled_branch_drawn_without_refused() {
    let call = "{{ refused(a) }}";
    let templates = templates();
    let primitives = templates.iter().position(|(t, _)| t == "primitives").expect("primitives.html.tmpl");
    let at: Vec<usize> = templates[primitives].1.match_indices(call).map(|(i, _)| i).collect();
    assert!(at.len() >= 4, "action_button, row_button, menu_item and queue_action draw refused(): {at:?}");
    for i in at {
        let mut cut = templates.clone();
        cut[primitives].1.replace_range(i..i + call.len(), "");
        let line = templates[primitives].1[..i].lines().count();
        let problems = check_refusals(&cases(&env_of(&cut)))
            .err()
            .unwrap_or_else(|| panic!("primitives.html.tmpl:{line} drawn without refused() passes the check"));
        assert!(
            problems.iter().all(|p| p.contains("without its action and refusal kind")),
            "primitives.html.tmpl:{line}: only the refusal fails: {problems:#?}"
        );
    }
    let raw = r#"<button type="button" class="btn" disabled title="Not permitted for your role">Wipe</button>"#;
    let case = |raw: &str| Case {
        name: "t",
        golden: String::new(),
        raw: raw.to_string(),
        primitive: None,
        state: None,
        seen: render::seen::Record::new(),
        refs: render::asked::Asked::new(),
        asked: render::asked::Asked::new(),
        refused: render::refused::Refused::new(),
    };
    let problems = check_refusals(&[case(raw)]).expect_err("an untagged refusal fails");
    assert!(problems.iter().any(|p| p.contains("without its action and refusal kind")), "{problems:#?}");
    assert!(problems.iter().any(|p| p.contains("NOT_ACTIONS \"Read-only\" is stale")), "{problems:#?}");
    let enabled = r#"<button type="button" class="btn" data-refused-action="golden/wipe" data-refusal="denied">Wipe</button>"#;
    let problems = check_refusals(&[case(enabled)]).expect_err("an enabled control carrying a refusal fails");
    assert!(problems.iter().any(|p| p.contains("carries a refusal but is drawn enabled")), "{problems:#?}");
    let other = r#"<div class="menu-item disabled" title="Busy">x</div>"#;
    let problems = check_refusals(&[case(other)]).expect_err("an unlisted titled disabled control fails");
    assert!(problems.iter().any(|p| p.contains("list it in NOT_ACTIONS")), "{problems:#?}");
}

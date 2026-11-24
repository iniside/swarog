//! Admission order, the outcome → status mapping of a conflict, and a prepared modal's
//! spec and its failures.

use controlplane::{
    ConfirmTone, ContributionDecl, Emphasis, FieldKind, InvokeCall, PanelQuery, Point, Role, SearchHit, SearchQuery,
};

use super::*;

fn decl(id: &str, then: Then) -> ActionDecl {
    ActionDecl {
        id: id.to_string(),
        point: ActionPoint::PanelTool("load".to_string()),
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        danger: false,
        multi: false,
        min_role: Role::Viewer,
        then,
        implemented: true,
        emphasis: Emphasis::Default,
        trailing_icon: false,
        icon_only: false,
        when_empty: false,
        prepare: false,
    }
}

fn manifest(writes: bool, readonly: Option<&str>, actions: Vec<ActionDecl>) -> Manifest {
    let load = ContributionDecl {
        id: "load".to_string(),
        point: Point::Page("p".to_string()),
        order: 0,
        title: "Load".to_string(),
        icon: String::new(),
        min_role: Role::Viewer,
        cache_ttl_s: None,
        deadline_ms: 0,
        readonly: readonly.map(str::to_string),
    };
    Manifest { module: "m".to_string(), contributions: vec![load], actions, writes, ..Manifest::default() }
}

fn operator(role: Role) -> Operator {
    Operator { login: "ops".to_string(), role }
}

/// Role first, then `implemented`, then the module's writes and the contribution's
/// read-only text; a link never writes, so it is admitted on a read-only module.
#[test]
fn admit_checks_role_then_implemented_then_writes() {
    let refused = ActionDecl { min_role: Role::Admin, implemented: false, ..decl("ban", Then::Invoke) };
    let cases = [
        ("role below", Role::Operator, refused.clone(), false, None, Err(Refusal::Denied)),
        ("not implemented", Role::Admin, refused.clone(), false, None, Err(Refusal::NotImplemented)),
        ("no writes", Role::Admin, decl("ban", Then::Invoke), false, None, Err(Refusal::ReadOnly(None))),
        (
            "read-only contribution",
            Role::Admin,
            decl("ban", Then::Invoke),
            true,
            Some("frozen"),
            Err(Refusal::ReadOnly(Some("frozen".to_string()))),
        ),
        ("admitted", Role::Admin, decl("ban", Then::Invoke), true, None, Ok(())),
        (
            "a link on a read-only module",
            Role::Viewer,
            decl("open", Then::Link(Link::Entity(EntityRef { kind: "player".to_string(), id: "p-1".to_string() }))),
            false,
            None,
            Ok(()),
        ),
    ];
    for (what, role, action, writes, readonly, want) in cases {
        let m = manifest(writes, readonly, vec![action.clone()]);
        assert_eq!(admit(&operator(role), &m, &action), want, "{what}");
    }
}

/// A conflict, as an outcome or as a provider error, answers 409 and consumes the claim.
#[test]
fn a_conflict_answers_409_and_consumes_the_claim() {
    for invoked in [
        Invoked::Outcome(InvokeOutcome::Conflict { current_revision: 7 }),
        Invoked::Failed(opsapi::Error::conflict("stale revision")),
    ] {
        assert_eq!(http_status(&invoked), StatusCode::CONFLICT);
        assert!(consumes(&invoked));
    }
}

/// A timed-out invoke releases its claim.
#[test]
fn a_timed_out_invoke_releases_the_claim() {
    assert!(!consumes(&Invoked::TimedOut));
}

fn field(key: &str, kind: FieldKind, required: bool) -> Field {
    Field { kind, required, ..form(key).fields.remove(0) }
}

fn form_spec(fields: Vec<Field>) -> StoredSpec {
    StoredSpec::Form { form: Box::new(FormSpec { fields, ..form("unused") }), chrome: Chrome::default() }
}

fn posted(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A blank Int, Float, Json, Duration and Ref, a Money with an empty amount and a Multi with
/// no box ticked are left out of the input when optional and are "required" when required.
#[test]
fn blank_typed_fields_are_left_out_when_optional_and_required_otherwise() {
    let kinds = [
        ("count", FieldKind::Int),
        ("ratio", FieldKind::Float),
        ("blob", FieldKind::Json),
        ("wait", FieldKind::Duration),
        ("owner", FieldKind::Ref("player".to_string())),
        ("price", FieldKind::Money(vec!["gold".to_string()])),
        ("regions", FieldKind::Multi(vec!["a".to_string()])),
    ];
    let body = posted(&[
        ("count", ""),
        ("ratio", ""),
        ("blob", ""),
        ("wait", ""),
        ("owner", ""),
        ("price", ""),
        ("price__currency", "gold"),
    ]);
    let optional = form_spec(kinds.iter().map(|(k, kind)| field(k, kind.clone(), false)).collect());
    let Ok(submission) = build_submission(&optional, &body) else { panic!("blank optional fields are no error") };
    assert!(submission.input.is_empty(), "{:?}", submission.input);

    let required = form_spec(kinds.iter().map(|(k, kind)| field(k, kind.clone(), true)).collect());
    let Err(errors) = build_submission(&required, &body) else { panic!("blank required fields are errors") };
    let want: BTreeMap<String, String> = kinds.iter().map(|(k, _)| (k.to_string(), "required".to_string())).collect();
    assert_eq!(errors, want);
}

/// A required read-only field with no default is left out, not an error; a required String
/// posted as "" is "required"; an optional String posted as "" reaches the input as "".
#[test]
fn the_blank_rule_skips_readonly_then_keeps_an_optional_empty_string() {
    let readonly = Field { readonly: true, ..field("shown", FieldKind::String, true) };
    let name = field("name", FieldKind::String, true);
    let spec = form_spec(vec![readonly, name, field("note", FieldKind::String, false)]);
    let Err(errors) = build_submission(&spec, &posted(&[("shown", "x"), ("name", ""), ("note", "")])) else {
        panic!("a blank required String is an error")
    };
    assert_eq!(errors, BTreeMap::from([("name".to_string(), "required".to_string())]));

    let Ok(submission) = build_submission(&spec, &posted(&[("name", "Anna"), ("note", "")])) else {
        panic!("a filled required String and an empty optional one submit")
    };
    let want = BTreeMap::from([("name".to_string(), "Anna".to_string()), ("note".to_string(), String::new())]);
    assert_eq!(submission.input, want, "the read-only field without a default is left out");
}

/// A bound element id of 256 bytes is accepted; 257 bytes, an empty id and one with a
/// control character are refused.
#[test]
fn validate_item_bounds_length_and_refuses_empty_and_control_characters() {
    assert!(validate_item(&"x".repeat(ITEM_MAX)));
    assert!(!validate_item(&"x".repeat(ITEM_MAX + 1)));
    assert!(!validate_item(""));
    assert!(!validate_item("row\n1"));
}

fn confirm(type_to_confirm: Option<&str>) -> ConfirmSpec {
    ConfirmSpec {
        title: "Skip".to_string(),
        body: "Skip the event.".to_string(),
        tone: ConfirmTone::Danger,
        impact: Vec::new(),
        reason: ReasonReq::Required,
        reason_hint: None,
        type_to_confirm: type_to_confirm.map(str::to_string),
        confirm_label: "Skip".to_string(),
    }
}

fn form(key: &str) -> FormSpec {
    let field = Field {
        key: key.to_string(),
        label: key.to_string(),
        kind: FieldKind::String,
        required: false,
        help: None,
        default: None,
        readonly: false,
        mono: false,
        placeholder: None,
    };
    FormSpec {
        fields: vec![field],
        hidden: BTreeMap::new(),
        submit_label: "Save".to_string(),
        note: None,
        revision: None,
        title: None,
        modes: Vec::new(),
        reason: ReasonReq::None,
        reason_label: None,
        reason_hint: None,
    }
}

/// A prepared spec of the declared variant is stored as prepared (its `type_to_confirm`, not
/// the declared one); a timeout is a 504, a provider error keeps its status, and another
/// variant or a form failing `form_spec_errors` is a 503.
#[test]
fn prepared_spec_stores_the_prepared_variant_and_maps_each_failure() {
    let declared_confirm = Then::Confirm(confirm(None));
    let declared_form = Then::Form(form("name"));

    let stored = prepared_spec(&declared_confirm, Prepared::Then(Then::Confirm(confirm(Some("evt-12905")))));
    assert_eq!(stored, Ok(StoredSpec::Confirm(confirm(Some("evt-12905")))));
    let stored = prepared_spec(&declared_form, Prepared::Then(Then::Form(form("level"))));
    assert_eq!(stored, Ok(StoredSpec::Form { form: Box::new(form("level")), chrome: Chrome::default() }));

    let status = |r: Result<StoredSpec, (StatusCode, String)>| r.map(|_| ()).unwrap_err();
    assert_eq!(status(prepared_spec(&declared_confirm, Prepared::TimedOut)).0, StatusCode::GATEWAY_TIMEOUT);
    let (code, text) = status(prepared_spec(&declared_confirm, Prepared::Failed(opsapi::Error::not_found("no such event"))));
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(text.contains("no such event"), "{text}");
    let wrong = [
        prepared_spec(&declared_confirm, Prepared::Then(Then::Form(form("name")))),
        prepared_spec(&declared_form, Prepared::Then(Then::Confirm(confirm(None)))),
        prepared_spec(&declared_confirm, Prepared::Then(Then::Invoke)),
    ];
    for result in wrong {
        let (code, text) = status(result);
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(text.contains("different kind of modal"), "{text}");
    }
    let (code, text) = status(prepared_spec(&declared_form, Prepared::Then(Then::Form(form("_bad")))));
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert!(text.contains("\"_bad\""), "{text}");
}

/// A face whose `prepare` hangs or fails as told.
struct PrepareFace {
    hang: bool,
}

#[async_trait::async_trait]
impl ControlPlane for PrepareFace {
    async fn manifests(&self) -> Result<Vec<Manifest>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn panel(&self, _req: PanelQuery) -> Result<PanelBody, opsapi::Error> {
        Err(opsapi::Error::not_found("no panel"))
    }

    async fn search(&self, _req: SearchQuery) -> Result<Vec<SearchHit>, opsapi::Error> {
        Ok(Vec::new())
    }

    async fn invoke(&self, _req: InvokeCall) -> Result<InvokeOutcome, opsapi::Error> {
        Err(opsapi::Error::forbidden("no invoke"))
    }

    async fn prepare(&self, _req: PrepareCall) -> Result<Then, opsapi::Error> {
        if self.hang {
            std::future::pending().await
        } else {
            Err(opsapi::Error::unavailable("plane down"))
        }
    }
}

/// `prepare` gives up at [`INVOKE_DEADLINE`] and passes a provider error through.
#[tokio::test(start_paused = true)]
async fn prepare_times_out_at_the_invoke_deadline_and_passes_an_error_through() {
    let req = || PrepareCall {
        module: "m".to_string(),
        action: "skip".to_string(),
        entity: None,
        item: Some("sub-1".to_string()),
        operator: "ops".to_string(),
    };
    let started = tokio::time::Instant::now();
    assert!(matches!(prepare(&PrepareFace { hang: true }, req()).await, Prepared::TimedOut));
    assert_eq!(started.elapsed(), INVOKE_DEADLINE);
    match prepare(&PrepareFace { hang: false }, req()).await {
        Prepared::Failed(error) => assert_eq!(error.status, opsapi::Status::Unavailable),
        _ => panic!("a provider error is Failed"),
    }
}

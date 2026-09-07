use super::command::database_skip_refusal;

/// The opt-out is process-global.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvGuard(Option<String>);

impl EnvGuard {
    fn set(value: Option<&str>) -> Self {
        let previous = std::env::var(testdb::SKIP_ENV).ok();
        match value {
            Some(v) => std::env::set_var(testdb::SKIP_ENV, v),
            None => std::env::remove_var(testdb::SKIP_ENV),
        }
        Self(previous)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var(testdb::SKIP_ENV, value),
            None => std::env::remove_var(testdb::SKIP_ENV),
        }
    }
}

#[test]
fn the_test_stage_refuses_to_run_while_database_tests_are_opted_out() {
    let _serialized = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _guard = EnvGuard::set(Some("1"));
    let refusal = database_skip_refusal().expect("an opted-out run must be refused, not run");
    assert!(refusal.contains(testdb::SKIP_ENV), "{refusal}");
    assert!(refusal.contains("prove nothing"), "{refusal}");
}

#[test]
fn a_normal_run_is_not_refused() {
    let _serialized = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _unset = EnvGuard::set(None);
    assert!(database_skip_refusal().is_none());
    let _off = EnvGuard::set(Some("0"));
    assert!(database_skip_refusal().is_none());
}

/// The refusal is worth nothing unless `test` consults it BEFORE spawning cargo. A later
/// refactor that keeps the function but drops the call would leave the stage green and
/// silent again, so the wiring itself is pinned here.
#[test]
fn the_test_stage_consults_the_refusal_before_spawning_cargo() {
    let source = include_str!("command.rs");
    let body = source
        .split_once("pub fn test(ctx: &mut Context<'_>)")
        .expect("the test stage")
        .1;
    let call = body
        .find("database_skip_refusal()")
        .expect("the test stage must consult the refusal");
    let cargo = body.find("ctx.cargo").expect("the test stage runs cargo");
    assert!(
        call < cargo,
        "the refusal must be checked before the cargo invocation"
    );
}

use super::cli::{parse, Command, Topology};

#[test]
fn up_defaults_to_monolith_and_switches_topology() {
    assert_eq!(
        parse(["up".into()]).unwrap(),
        Command::Up {
            topology: Topology::Monolith,
            skip_build: false,
            overrides: vec![]
        }
    );
    assert_eq!(
        parse(["up".into(), "split".into(), "--skip-build".into()]).unwrap(),
        Command::Up {
            topology: Topology::Split,
            skip_build: true,
            overrides: vec![]
        }
    );
}

#[test]
fn override_is_structured_without_rendering_its_value() {
    let command = parse([
        "up".into(),
        "--env".into(),
        "DATABASE_URL=postgres://secret".into(),
    ])
    .unwrap();
    match command {
        Command::Up { overrides, .. } => {
            assert_eq!(overrides[0].0, "DATABASE_URL");
            assert_eq!(overrides[0].1, "postgres://secret");
        }
        _ => panic!("expected up"),
    }
    let error = parse(["up".into(), "--env".into(), "broken".into()])
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret"));
}

#[test]
fn microservices_alias_selects_split() {
    assert!(matches!(
        parse(["up".into(), "microservices".into()]).unwrap(),
        Command::Up {
            topology: Topology::Split,
            ..
        }
    ));
}

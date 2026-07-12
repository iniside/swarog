use super::*;
use std::sync::Arc;

/// `Convention::ALL` must contain every variant exactly once. The exhaustive
/// `match` below is the compile-time reminder: adding a variant to
/// [`Convention`] fails compilation HERE until it is handled, and the loop
/// then asserts the new variant was also added to `ALL`.
#[test]
fn all_contains_every_convention_variant() {
    // Compile-time exhaustiveness: a new variant breaks this match.
    let describe = |c: Convention| match c {
        Convention::EnvValidation => "EnvValidation",
        Convention::InputByteCaps => "InputByteCaps",
        Convention::InfraOutage503 => "InfraOutage503",
        Convention::ArgonParity => "ArgonParity",
    };

    let every_variant = [
        Convention::EnvValidation,
        Convention::InputByteCaps,
        Convention::InfraOutage503,
        Convention::ArgonParity,
    ];
    for v in every_variant {
        assert!(
            Convention::ALL.contains(&v),
            "Convention::ALL is missing {} — extend the ALL const",
            describe(v)
        );
    }
    assert_eq!(
        Convention::ALL.len(),
        every_variant.len(),
        "Convention::ALL length drifted from the variant count"
    );
    // No duplicates in ALL.
    for (i, a) in Convention::ALL.iter().enumerate() {
        for b in &Convention::ALL[i + 1..] {
            assert_ne!(a, b, "Convention::ALL contains {} twice", describe(*a));
        }
    }
}

#[test]
fn entry_stance_finds_declared_and_misses_undeclared() {
    let entry = Entry {
        module: "example",
        stances: vec![
            (
                Convention::EnvValidation,
                Stance::NotApplicable {
                    why: "no env parsed at init",
                },
            ),
            (
                Convention::ArgonParity,
                Stance::Applies(Fixture::ArgonParity(ArgonParams {
                    m_cost: 19456,
                    t_cost: 2,
                    p_cost: 1,
                    output_len: 32,
                })),
            ),
        ],
    };
    assert!(matches!(
        entry.stance(Convention::EnvValidation),
        Some(Stance::NotApplicable { why }) if !why.is_empty()
    ));
    assert!(matches!(
        entry.stance(Convention::ArgonParity),
        Some(Stance::Applies(Fixture::ArgonParity(_)))
    ));
    assert!(entry.stance(Convention::InputByteCaps).is_none());
    assert!(entry.stance(Convention::InfraOutage503).is_none());
}

/// Fixtures hold `Arc`ed probes — Clone must work and the clone must share
/// the same probe behavior.
#[test]
fn fixtures_are_cloneable_and_probes_survive_the_clone() {
    let cap_fixture = Fixture::InputByteCaps(vec![CapCase {
        name: "example",
        cap: 8,
        probe: Arc::new(|len| len > 8),
    }]);
    let cloned = cap_fixture.clone();
    if let Fixture::InputByteCaps(cases) = cloned {
        let case = &cases[0];
        assert!(!(case.probe)(case.cap));
        assert!((case.probe)(case.cap + 1));
    } else {
        panic!("clone changed the fixture variant");
    }

    let outage = Fixture::InfraOutage503(vec![OutageCase {
        name: "example",
        probe: Arc::new(|| Box::pin(async { OutageClass::Unavailable }) as BoxFuture<OutageClass>),
    }]);
    let _ = outage.clone();
}

#[test]
fn outage_class_equality_distinguishes_variants() {
    assert_eq!(OutageClass::Unavailable, OutageClass::Unavailable);
    assert_ne!(OutageClass::Unavailable, OutageClass::Rejected);
    assert_eq!(
        OutageClass::Other("boom".into()),
        OutageClass::Other("boom".into())
    );
    assert_ne!(
        OutageClass::Other("boom".into()),
        OutageClass::Other("bang".into())
    );
}

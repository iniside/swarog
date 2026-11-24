//! The sidebar section fallback: an entry naming no known section is placed in
//! [`FALLBACK_SECTION`].

use controlplane::{LocalFace, NavEntry, NavSection};

use super::*;

fn section(id: &str) -> NavSection {
    NavSection { id: id.to_string(), label: id.to_string(), order: 0 }
}

fn entry(id: &str, section: &str) -> NavEntry {
    NavEntry {
        section: section.to_string(),
        id: id.to_string(),
        label: id.to_string(),
        icon: String::new(),
        order: 0,
        page: id.to_string(),
        counter: None,
    }
}

#[test]
fn resolve_section_keeps_a_known_id_and_falls_back_to_platform() {
    let sections = [section("players"), section("live")];
    assert_eq!(resolve_section(&sections, "live"), "live");
    assert_eq!(resolve_section(&sections, "players"), "players");
    for unknown in ["nope", "", "Players"] {
        assert_eq!(resolve_section(&sections, unknown), FALLBACK_SECTION, "{unknown:?}");
    }
    assert_eq!(FALLBACK_SECTION, "platform");
}

/// The snapshot's nav places an entry naming no known section under the fixed `platform`
/// section, which is always among its sections, and keeps a known one.
#[test]
fn a_nav_entry_naming_no_known_section_is_placed_in_platform() {
    let manifest = Manifest {
        module: "m".to_string(),
        entries: vec![entry("lost", "nope"), entry("roster", "players")],
        ..Manifest::default()
    };
    let source = Source { peer: "local".to_string(), face: Arc::new(LocalFace::new(Vec::new())) };
    let snapshot = Snapshot::seeded(vec![source], Some(vec![manifest]));
    assert!(snapshot.sections.iter().any(|s| s.id == FALLBACK_SECTION));
    let placed: Vec<(&str, &str)> = snapshot.nav.iter().map(|n| (n.id.as_str(), n.section.as_str())).collect();
    assert_eq!(placed, [("roster", "players"), ("lost", FALLBACK_SECTION)], "players sorts before platform");
}

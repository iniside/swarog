use super::*;
use std::collections::BTreeSet;
use std::path::Path;

/// Step 15 (G3): pins `split_process_modules()`'s hand-written process-name list to
/// the filesystem set of `cmd/*-svc` directories. The compile-time `vec!` of
/// `<name>_svc::modules(...)` calls can't be derived (each is a distinct crate
/// import), so this is the drift tripwire: a 13th `cmd/<name>-svc` crate fails this
/// test loudly until it's added to `split_process_modules()` (and the fortress
/// build list in verify.sh/verify.ps1, which IS derived from the same directory
/// set — see `fortress_crates()` / `Get-FortressCrates`).
#[test]
fn split_fleet_matches_cmd_dirs() {
    let from_fleet: BTreeSet<String> = split_process_modules()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();

    let cmd_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cmd");
    let from_fs: BTreeSet<String> = std::fs::read_dir(&cmd_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", cmd_dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("readable dir entry");
            if !entry.file_type().expect("file type").is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with("-svc").then_some(name)
        })
        .collect();

    assert_eq!(
        from_fleet, from_fs,
        "split_process_modules() must list exactly the cmd/*-svc directories \
         (fleet has {from_fleet:?}, filesystem has {from_fs:?}) -- register the \
         new svc crate in tools/checkmodules::split_process_modules()"
    );
}

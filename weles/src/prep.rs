//! Prep pipeline: decides HOW the fleet gets spawnable — deployed binary
//! paths, artifact staging (`weles deploy`), pre-flight binary validation, and
//! the transient helper runs (`edgeca`, `adminctl create-user`) that must
//! succeed before any long-lived service is spawned. Consumes
//! [`crate::manifest`] (the WHAT — process names/ports/env) and
//! [`crate::platform::spawn`] (the ONLY spawn mechanism in this crate — see the
//! crate-wide invariant documented beside `SPAWN_LOCK` in `platform::mod`).
//! Never `std::process::Command` directly: the Windows spawn path uses blanket
//! handle inheritance with no `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` allow-list,
//! so a `std::process::Command` spawn racing a concurrent `platform::spawn`
//! could cross-inherit the other's transient inheritable stdio duplicates.
//!
//! weles is an orchestrator, not a build system: it never invokes `cargo` and
//! never reads `target/`. It executes ONLY artifacts staged in `<root>/deploy`
//! by `weles deploy`. (Because that removed the old `cargo build` child, this
//! crate no longer carries a `BUILD_ENV_ALLOWLIST`. The sibling
//! `tools/processctl/src/fleet.rs:8-14` allowlist — which devctl DOES use to
//! build — still omits `SYSTEMDRIVE`/`ProgramData`, a latent linker-env gap
//! recorded here as a known sibling; do NOT touch processctl for it.)
//!
//! # Deploy generations and the deploy↔up contract
//!
//! `weles deploy` no longer overwrites files in place. Each deploy stages a
//! fresh, immutable generation directory `<root>/deploy/gen-N/` (binaries +
//! a `manifest.json` recording each artifact's SHA-256 and byte length), and
//! ONLY once every copy+hash succeeds does it atomically flip the pointer file
//! `<root>/deploy/current` (write `current.tmp`, rename over `current`) to name
//! the new generation. A partial deploy leaves `current` untouched — it still
//! names the previous generation — and abandons `gen-N` as an observable stale
//! directory (no rollback needed: the live fleet's binary source never moved).
//!
//! CONTRACT CHANGE (recorded per Fix-the-Authority rule 4): `deploy` now mutates
//! the running fleet's binary *source of record* (`current`) while an `up` may be
//! live. This is made safe by PIN-AT-DISCOVER: [`Layout::discover`] reads
//! `current` exactly ONCE and pins `active_bin_dir = deploy/<gen>/` for the whole
//! life of that `up`. A running `up` never re-reads `current`, so a concurrent
//! `deploy` flipping it cannot affect the live fleet (and cannot mix generations
//! across a respawn — every service of one `up` runs one coherent generation).
//! Because each generation is a fresh directory the live fleet never holds open,
//! staging needs no rollout lock — `deploy` deliberately does NOT take the
//! exclusive up-lock (which `up` holds for its whole life; blocking on it would
//! defeat "deploy under a live fleet"). Retention protects the LIVE-PINNED
//! generation by NAME: an `up` records its pinned `gen-N` into `state.json`, and
//! deploy's prune reads it (via weles's own `state::load` +
//! `control::supervisor_alive`) and refuses to delete a live, non-terminal
//! supervisor's generation regardless of how far `current` has advanced. It also
//! protects the new current and the PRE-FLIP current (never a numeric
//! `current-1`, which an abandoned partial can poison). Everything else is
//! pruned tolerantly (a still-held directory is logged and skipped, never
//! fatal). This is deploy↔up coupling through `state.json` — the authority for
//! "in use" is "pinned by a live supervisor", leveraging the one-up-at-a-time
//! invariant (exactly one supervisor, one pinned generation). Keying it off the
//! live pin — not a generation number — is what stops a Unix `remove_dir_all`
//! from silently deleting the running fleet's binaries (closing "overwrite live
//! exe" must not open "delete live exe").

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fleet_toml::PrepareCmd;
use crate::manifest::lookup_env;
use crate::platform::{self, SpawnSpec};

const HELPER_SHUTDOWN_GRACE: Duration = Duration::from_secs(0);
const HELPER_SHUTDOWN_FORCE: Duration = Duration::from_secs(5);

/// The per-command deadline applied to a [`PrepareCmd`] that declares none
/// (`timeout_secs == 0`). Matches the old fixed `mint_ca`/`seed_admin` 30s.
const DEFAULT_PREPARE_TIMEOUT_SECS: u64 = 30;

/// THE single runtime authority for weles's fleet root — the directory under
/// which `run/` (state + `rollout.lock`), `deploy/`, and every spawned service's
/// cwd live. Every entry point (`up`/`deploy`/`status`/`down`) resolves the root
/// through here, replacing the two duplicated compile-time
/// `env!("CARGO_MANIFEST_DIR").parent()` derivations that used to live in
/// `main::state_path` and `supervisor::workspace_root` (identical only by
/// construction — nothing enforced their agreement).
///
/// Resolution chain, first match wins:
///   1. `flag` — the `--root <path>` value, parsed ONCE in `cli` and threaded in
///      (never re-read from argv here, which would be a second argv authority);
///   2. else `WELES_ROOT` (a non-empty value);
///   3. else walk the current directory UP to the repo marker — a directory
///      holding BOTH `Cargo.toml` and `tools/processctl/`. This matches
///      `tools/verifyctl/src/runner.rs::workspace_root` byte-for-byte so weles's
///      `<root>/run/rollout.lock` path stays identical to devctl/verifyctl and
///      the one-Postgres mutual exclusion (`crate::lock`) is preserved from any
///      subdirectory of the checkout;
///   4. else — a real off-checkout deploy with no marker above cwd — `bail!`
///      (fail-closed, never a silent flat-cwd that would mis-locate
///      state/lock/deploy), telling the operator to pass `--root` or set
///      `WELES_ROOT`.
///
/// `current_exe` is deliberately NOT consulted: a deployed weles binary installs
/// separately from the fleet's `deploy/`, so its own location is unrelated to the
/// root.
pub fn resolve_root(flag: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = flag {
        return Ok(root);
    }
    if let Some(env) = std::env::var_os("WELES_ROOT").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(env));
    }
    let mut directory = std::env::current_dir().context("resolve the current directory")?;
    loop {
        if directory.join("Cargo.toml").is_file() && directory.join("tools/processctl").is_dir() {
            return Ok(directory);
        }
        if !directory.pop() {
            bail!(
                "weles could not locate the fleet root: no --root <path> was given, WELES_ROOT is \
                 unset, and no ancestor of the current directory holds a Cargo.toml beside \
                 tools/processctl (the repo marker). Pass --root <path> or set WELES_ROOT to the \
                 root that holds run/ and deploy/."
            );
        }
    }
}

/// The workspace's on-disk layout as weles cares about it: the repo root, its
/// own `run/weles` scratch dir (created on discovery), `bin_dir` —
/// `<root>/deploy`, the FIXED directory `weles deploy` stages generations into
/// — and `active_bin_dir`, the ONE generation directory
/// (`<root>/deploy/gen-N/`) this layout is pinned to. weles never builds and
/// never reads `target/`.
///
/// PIN-AT-DISCOVER (authority): `active_bin_dir` is resolved from
/// `deploy/current` exactly once, in [`Layout::discover`], and never re-read.
/// Every binary path the fleet spawns from goes through [`Layout::binary`],
/// which resolves against this pinned directory — so the whole fleet of one
/// `up` (including any crash-respawn) runs one coherent generation even if a
/// concurrent `deploy` flips `current` underneath.
#[derive(Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    /// `<root>/deploy` — the generations root (`weles deploy` writes `gen-N/`
    /// and the `current` pointer here).
    pub bin_dir: PathBuf,
    /// `<root>/deploy/<gen>/` — the pinned active generation this layout
    /// resolves binaries from. On the deploy path (which stages a NEW
    /// generation and never spawns) this is an inert placeholder equal to
    /// `bin_dir`; see [`Layout::discover_for_deploy`].
    pub active_bin_dir: PathBuf,
    /// The deployed fleet, parsed AND validated from
    /// `active_bin_dir/fleet.toml` exactly ONCE at [`Layout::discover`]
    /// (PIN-AT-DISCOVER — never re-read), so the whole `up` boots one coherent
    /// fleet definition. `None` on the deploy path
    /// ([`Layout::discover_for_deploy`]), which stages a new generation and
    /// never reads a pinned fleet. Read through [`Layout::fleet`].
    fleet: Option<crate::fleet_toml::Fleet>,
}

impl Layout {
    /// Discovers the layout under `root` for an `up`, creating `root/run/weles`
    /// if absent and PINNING the active generation from `deploy/current`
    /// (config-as-code: no env override, no debug/release heuristic, no
    /// `CARGO_TARGET_DIR`). A missing/empty `deploy/current` (a fresh checkout,
    /// nothing ever deployed) is a clear error here — pointing the operator at
    /// `weles deploy` — rather than a raw missing-file symptom later in
    /// `validate_binaries`.
    pub fn discover(root: PathBuf) -> Result<Self> {
        let (run_dir, bin_dir) = Self::scaffold(&root)?;
        let active_bin_dir = pin_generation(&bin_dir)?;
        // Integrity gate (M1): before this `up` spawns anything, recompute every
        // staged artifact's SHA-256 from disk and compare to the pinned
        // generation's manifest. This is the read-back of the hash `deploy`
        // records (dead until now), so a torn/tampered binary is a legible
        // pre-boot failure — before the lock is even acquired — instead of an
        // opaque exec crash. `deploy` writes the manifest only after every
        // copy+hash succeeds and BEFORE it flips `current`, so a `current` that
        // names this generation always has a complete manifest to verify.
        verify_generation(&active_bin_dir).with_context(|| {
            format!("verify the pinned generation {}", active_bin_dir.display())
        })?;
        // PIN-AT-DISCOVER for the fleet too: read+parse+VALIDATE the deployed
        // `fleet.toml` from the pinned generation exactly ONCE here, so a
        // running `up` boots one coherent fleet definition and never re-reads
        // it (a concurrent `deploy` flipping `current` cannot change what this
        // `up` runs). A bad/absent fleet file fails the discover loudly, before
        // the lock is even acquired.
        let fleet_path = active_bin_dir.join("fleet.toml");
        let fleet = crate::fleet_toml::load(&fleet_path)?;
        crate::fleet_toml::validate(&fleet)
            .with_context(|| format!("validate deployed fleet {}", fleet_path.display()))?;
        Ok(Layout {
            root,
            run_dir,
            bin_dir,
            active_bin_dir,
            fleet: Some(fleet),
        })
    }

    /// Discovers the layout under `root` for a `deploy`. Unlike [`discover`],
    /// this does NOT require `deploy/current` to exist — a fresh checkout must
    /// be able to run its first `weles deploy`. `deploy` stages a brand-new
    /// generation and never resolves [`binary`], so `active_bin_dir` is set to
    /// an inert placeholder (`bin_dir`) that must never be spawned from.
    ///
    /// [`discover`]: Layout::discover
    /// [`binary`]: Layout::binary
    pub fn discover_for_deploy(root: PathBuf) -> Result<Self> {
        let (run_dir, bin_dir) = Self::scaffold(&root)?;
        let active_bin_dir = bin_dir.clone();
        Ok(Layout {
            root,
            run_dir,
            bin_dir,
            active_bin_dir,
            // The deploy path stages its OWN chosen `--fleet` file and never
            // reads a pinned fleet: there may not even be a `deploy/current`
            // yet (first deploy on a fresh checkout).
            fleet: None,
        })
    }

    /// Test-only bare constructor (no discovery, no pinned fleet) so unit tests
    /// in sibling modules can build a `Layout` pointing at scratch dirs. `fleet`
    /// is `None`: a test that never spawns/pins never reads it.
    #[cfg(test)]
    pub(crate) fn for_test(
        root: PathBuf,
        run_dir: PathBuf,
        bin_dir: PathBuf,
        active_bin_dir: PathBuf,
    ) -> Self {
        Layout { root, run_dir, bin_dir, active_bin_dir, fleet: None }
    }

    /// Shared discovery scaffolding: create `run/weles`, resolve `deploy`.
    fn scaffold(root: &Path) -> Result<(PathBuf, PathBuf)> {
        let run_dir = root.join("run").join("weles");
        std::fs::create_dir_all(&run_dir)
            .with_context(|| format!("create run dir {}", run_dir.display()))?;
        let bin_dir = root.join("deploy");
        Ok((run_dir, bin_dir))
    }

    /// Path to the pinned-generation binary for cargo package `pkg`
    /// (`deploy/<gen>/<pkg>[.exe]`). Infallible by design: the generation was
    /// pinned once at [`discover`] time, so respawn resolves the SAME path.
    ///
    /// [`discover`]: Layout::discover
    pub fn binary(&self, pkg: &str) -> PathBuf {
        self.active_bin_dir
            .join(format!("{pkg}{}", std::env::consts::EXE_SUFFIX))
    }

    /// The `gen-N` name this layout is pinned to, IFF `active_bin_dir` is a
    /// generation directly under the deploy root (the `up` path). Returns `None`
    /// for a deploy-path layout (where `active_bin_dir` is the inert `deploy/`
    /// placeholder). Recorded into `state.json` so a concurrent deploy's
    /// retention can protect the live generation by name.
    pub fn pinned_generation(&self) -> Option<String> {
        if self.active_bin_dir.parent() == Some(self.bin_dir.as_path()) {
            self.active_bin_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        } else {
            None
        }
    }

    /// The fleet pinned at [`discover`] (parsed + validated once). `None` on a
    /// deploy-path layout ([`discover_for_deploy`]). The `up` path always has
    /// `Some` — `discover` fails rather than return a layout without a fleet.
    ///
    /// [`discover`]: Layout::discover
    /// [`discover_for_deploy`]: Layout::discover_for_deploy
    pub fn fleet(&self) -> Option<&crate::fleet_toml::Fleet> {
        self.fleet.as_ref()
    }
}

/// Reads `deploy/current` (a small TEXT FILE naming the active generation dir,
/// e.g. `gen-3` — deliberately NOT a symlink: Windows symlink creation needs a
/// privilege dev machines usually lack) and resolves the pinned
/// `deploy/<gen>/`. A missing or empty pointer, or one naming a non-existent
/// directory, is a clear operator-facing error naming `weles deploy`.
fn pin_generation(bin_dir: &Path) -> Result<PathBuf> {
    let current = bin_dir.join("current");
    let gen = match std::fs::read_to_string(&current) {
        Ok(contents) => contents.trim().to_string(),
        Err(_) => bail!(
            "nothing deployed under {} — run `weles deploy <build-dir>` first",
            bin_dir.display()
        ),
    };
    if gen.is_empty() {
        bail!(
            "{} is empty — run `weles deploy <build-dir>` to stage a generation",
            current.display()
        );
    }
    let active = bin_dir.join(&gen);
    if !active.is_dir() {
        bail!(
            "{} names generation {gen}, but {} is not a directory — re-run `weles deploy`",
            current.display(),
            active.display()
        );
    }
    Ok(active)
}

/// One deployed generation's manifest (`deploy/gen-N/manifest.json`): a
/// greenfield record of exactly what was staged, for provenance and (M1)
/// rollback. There is nothing to migrate — a new deploy writes a new manifest.
#[derive(Debug, Serialize, Deserialize)]
pub struct GenerationManifest {
    pub gen: u64,
    pub artifacts: Vec<Artifact>,
    /// The `fleet.toml` stamped into this generation, tracked (SHA-256 + byte
    /// length) exactly like a staged binary — it is a first-class artifact of
    /// the deploy: `up` reads it back from the pinned generation to learn what
    /// to boot, so a torn or swapped fleet file is as fatal as a torn binary.
    /// Its copy+hash happens BEFORE the atomic `current` flip, so a missing or
    /// unreadable `--fleet` aborts the flip like a missing binary.
    pub fleet: Artifact,
}

/// One staged artifact within a generation: its package (or `"fleet.toml"` for
/// the stamped fleet file), on-disk file name, the SHA-256 of the bytes
/// actually written, and the byte length.
#[derive(Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub pkg: String,
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The full set of binaries a fleet stages and may execute: every
/// `[[service]]` `pkg` UNION every `[[prepare]]` `run`. Deterministic, deduped,
/// sorted — the authority for `weles deploy`'s copy set AND for `up`'s
/// pre-flight [`validate_binaries`] gate. Derived from the deployed
/// `fleet.toml`, never hardcoded: `edgeca`/`adminctl` stay staged because a
/// `[[prepare]]` hook references them, not because weles knows their names.
pub fn deploy_packages(fleet: &crate::fleet_toml::Fleet) -> Vec<String> {
    let mut pkgs: Vec<String> = fleet.services.iter().map(|svc| svc.pkg.clone()).collect();
    pkgs.extend(fleet.prepare.iter().map(|hook| hook.run.clone()));
    pkgs.sort_unstable();
    pkgs.dedup();
    pkgs
}

/// Pre-flight gate (didn't-forget style): every binary the chosen run needs
/// must already be staged in `layout.bin_dir` (`<root>/deploy`). Lists EVERY
/// missing binary, one per line, and points the operator at `weles deploy` —
/// weles executes only deployed artifacts and never builds. Called right after
/// the rollout lock, before any other validation, so a run with an incomplete
/// deploy dir dies pre-work instead of half-booting.
pub fn validate_binaries(layout: &Layout, packages: &[String]) -> Result<()> {
    let mut missing: Vec<PathBuf> = Vec::new();
    for pkg in packages {
        let path = layout.binary(pkg);
        if !path.is_file() {
            missing.push(path);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let mut message = String::from(
        "missing staged binaries — weles executes only what was deployed, it never builds:\n",
    );
    for path in &missing {
        message.push_str(&format!("  {}\n", path.display()));
    }
    message.push_str("build them and stage with: weles deploy <your-build-output-dir>");
    bail!("{message}")
}

/// `weles deploy <src_dir> --fleet <fleet_path>`: parses+validates
/// `fleet_path`, then stages the binaries it needs ([`deploy_packages`]) from
/// `src_dir` (resolved relative to the CURRENT directory, not the repo root)
/// AND the `fleet.toml` itself into a FRESH generation directory
/// `<root>/deploy/gen-N/` and, only once every copy+hash succeeds (the fleet
/// file included), atomically flips `<root>/deploy/current` to name it. `up`
/// later reads that stamped `fleet.toml` back to learn what to boot. Prints a
/// per-file report line (copied / missing / copy FAILED). See the module doc
/// for the deploy↔up contract and PIN-AT-DISCOVER.
///
/// Self-copy guard: `src_dir` and `bin_dir` are canonicalized up front and a
/// deploy FROM the deploy dir itself is rejected — passing `deploy/` as the
/// source would recursively stage generations. The guard makes both platforms
/// fail the same way, before any file is touched.
///
/// Failure semantics (atomic, rollback-free): the copy loop never aborts
/// mid-way — a missing source or a failed copy is recorded and the remaining
/// files are still processed. If ANY file was missing or failed, `current` is
/// left UNTOUCHED (still naming the previous generation) and the partial
/// `gen-N` is abandoned as an observable stale directory; the error enumerates
/// EVERY missing source and EVERY failed copy, one per line. A running `up`
/// pinned the old generation and is unaffected.
///
/// Retention: after a successful flip, retention protects the new current, the
/// PRE-FLIP current (captured before the flip — the generation a live `up` may
/// have pinned, robust to an abandoned partial that bumped the counter), and —
/// authoritatively — whatever generation a live, non-terminal supervisor
/// recorded pinning in `state.json`. Everything else is pruned tolerantly (an
/// undeletable directory is logged and skipped, never fatal). Keying "in use"
/// off the LIVE PIN BY NAME, not a numeric position, is what stops a Unix
/// `remove_dir_all` from silently deleting the running fleet's `gen-N/` (which
/// would leave crash-respawn — weles's differentiator — unable to find the
/// binary).
///
/// Concurrency: this is deploy↔up coupling (deploy reads `state.json`), but
/// deploy takes NO rollout lock. It DOES take the `deploy/`-scoped mutator lock
/// ([`crate::lock::acquire_deploy`]) — shared with `weles rollback` — so two
/// `current`-pointer mutators can never interleave a `next_generation` /
/// `current` flip and corrupt a generation (the M0 "run at most ONE `weles
/// deploy` at a time" operator discipline, now enforced; the M1 guard the module
/// doc tracked). A live `up` never contends on it (PIN-AT-DISCOVER: it neither
/// stages nor flips `current`), so a deploy under a live fleet stays
/// non-blocking.
pub fn deploy(layout: &Layout, src_dir: &Path, fleet_path: &Path) -> Result<()> {
    // Serialize against a concurrent `deploy`/`rollback` (the only other
    // `current`-mutators) for the whole stage-and-flip. DISTINCT from
    // run/rollout.lock; held for this function's lifetime by the RAII guard.
    let _mutator_lock = crate::lock::acquire_deploy(&layout.bin_dir)?;

    // Parse AND validate the chosen fleet FIRST — before staging anything: a
    // bad fleet must abort the deploy loudly (naming the offending rule), not
    // stamp an invalid fleet a later `up` would only reject at discover. This
    // also tells us which binaries to stage ([`deploy_packages`]).
    let fleet = crate::fleet_toml::load(fleet_path)?;
    crate::fleet_toml::validate(&fleet)
        .with_context(|| format!("validate fleet {}", fleet_path.display()))?;

    std::fs::create_dir_all(&layout.bin_dir)
        .with_context(|| format!("create deploy dir {}", layout.bin_dir.display()))?;

    // Canonicalize BOTH sides (handles relative-to-CWD paths and symlinks)
    // before touching any file: src == deploy root would recursively stage.
    let src_canonical = std::fs::canonicalize(src_dir)
        .with_context(|| format!("resolve source dir {}", src_dir.display()))?;
    let bin_canonical = std::fs::canonicalize(&layout.bin_dir)
        .with_context(|| format!("resolve deploy dir {}", layout.bin_dir.display()))?;
    if src_canonical == bin_canonical {
        bail!(
            "source dir {} IS the deploy dir {} — deploying deploy/ onto itself is \
             refused; pass your build output dir instead",
            src_dir.display(),
            layout.bin_dir.display()
        );
    }

    // Capture the generation `current` names BEFORE this deploy flips it — the
    // one a live `up` may have pinned. Retention keys "keep previous" off THIS
    // value, never `new_gen - 1` (an abandoned partial deploy permanently bumps
    // the counter, so `new_gen - 1` can name a manifest-less abandoned dir while
    // the real live generation is older).
    let pre_flip_current = read_current_generation(&layout.bin_dir);

    let gen = next_generation(&layout.bin_dir)?;
    let gen_name = format!("gen-{gen}");
    let gen_dir = layout.bin_dir.join(&gen_name);
    std::fs::create_dir_all(&gen_dir)
        .with_context(|| format!("create generation dir {}", gen_dir.display()))?;

    let mut missing: Vec<PathBuf> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut artifacts: Vec<Artifact> = Vec::new();
    for pkg in deploy_packages(&fleet) {
        let file = format!("{pkg}{}", std::env::consts::EXE_SUFFIX);
        let src = src_dir.join(&file);
        let dst = gen_dir.join(&file);
        if !src.is_file() {
            println!("weles: {pkg}: MISSING in {}", src_dir.display());
            missing.push(src);
            continue;
        }
        match copy_and_hash(&src, &dst) {
            Ok((sha256, bytes)) => {
                println!("weles: {pkg}: copied -> {} (sha256 {sha256})", dst.display());
                artifacts.push(Artifact {
                    pkg: pkg.clone(),
                    file,
                    sha256,
                    bytes,
                });
            }
            Err(error) => {
                println!("weles: {pkg}: copy FAILED -> {} ({error:#})", dst.display());
                failed.push(format!("{} ({error:#})", dst.display()));
            }
        }
    }

    // Stamp the chosen fleet into the generation as `fleet.toml`, tracked +
    // hashed like a binary. Accumulated into the same missing/failed model so a
    // copy failure here abandons `gen-N` and leaves `current` untouched, exactly
    // like a missing binary. (The parse+validate above already proved the source
    // is a readable, well-formed fleet, so this copy failing means an I/O fault.)
    let fleet_dst = gen_dir.join("fleet.toml");
    let fleet_artifact = match copy_and_hash(fleet_path, &fleet_dst) {
        Ok((sha256, bytes)) => {
            println!("weles: fleet.toml: copied -> {} (sha256 {sha256})", fleet_dst.display());
            Some(Artifact {
                pkg: "fleet.toml".to_string(),
                file: "fleet.toml".to_string(),
                sha256,
                bytes,
            })
        }
        Err(error) => {
            println!("weles: fleet.toml: copy FAILED -> {} ({error:#})", fleet_dst.display());
            failed.push(format!("{} ({error:#})", fleet_dst.display()));
            None
        }
    };

    if !missing.is_empty() || !failed.is_empty() {
        // Do NOT flip `current`: it still names the previous generation, so a
        // live `up` is untouched. `gen-N` is left as an observable stale dir.
        let mut message = format!(
            "weles deploy: incomplete — {gen_name} abandoned, `current` unchanged \
             (still the previous generation, no rollback needed):\n",
        );
        for path in &missing {
            message.push_str(&format!("  missing source: {}\n", path.display()));
        }
        for entry in &failed {
            message.push_str(&format!("  copy failed: {entry}\n"));
        }
        bail!("{message}");
    }

    // All copies+hashes succeeded — record the manifest, THEN atomically flip.
    // `fleet_artifact` is `Some` here: it is `None` only when its copy failed,
    // which would have pushed to `failed` and bailed above.
    let fleet_artifact = fleet_artifact
        .expect("fleet.toml artifact is Some when no copy failed (checked above)");
    let manifest = GenerationManifest { gen, artifacts, fleet: fleet_artifact };
    let manifest_path = gen_dir.join("manifest.json");
    let json = serde_json::to_vec_pretty(&manifest).context("serialize generation manifest")?;
    std::fs::write(&manifest_path, json)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    flip_current(&layout.bin_dir, &gen_name)?;
    println!("weles: deployed {gen_name}, current -> {gen_name}");

    // Retention protects, by NUMBER: the new current, the pre-flip current (the
    // generation a live `up` may have pinned across intervening deploys), and —
    // authoritatively — whatever generation a live, non-terminal supervisor
    // recorded pinning in `state.json`. The last one closes the case where the
    // live up pinned a generation now several deploys behind current.
    let mut protected: Vec<u64> = vec![gen];
    if let Some(previous) = pre_flip_current {
        protected.push(previous);
    }
    if let Some(pinned) = live_pinned_generation(&layout.run_dir) {
        protected.push(pinned);
    }
    prune_stale_generations(&layout.bin_dir, &layout.run_dir, &protected);
    Ok(())
}

/// `weles rollback [<target>]`: repoint `deploy/current` at an earlier staged
/// generation. `target` is either an explicit `gen-<N>` / bare `<N>`
/// ([`normalize_generation_name`]) or, when `None`, the highest good generation
/// strictly below the current one ([`predecessor_generation`]).
///
/// The target is VALIDATED before the flip: it must exist AND pass
/// [`verify_generation`] (a torn/tampered/manifest-less generation is refused so
/// rollback never repoints `current` at a generation the next `up` would only
/// reject at discover). Only then does [`flip_current`] atomically repoint
/// `current` — so a failed rollback leaves the live source of record untouched,
/// exactly like a partial deploy.
///
/// Concurrency: like `deploy`, rollback takes the `deploy/`-scoped mutator lock
/// (NOT the rollout lock) so it can never interleave with a concurrent
/// `deploy`/`rollback`. A live `up` is immune (PIN-AT-DISCOVER).
///
/// Retention: after the flip, rollback prunes with the SAME authority as
/// `deploy` — protecting the new current, the PRE-FLIP current (the roll-forward
/// target), and whatever generation a live, non-terminal supervisor recorded
/// pinning in `state.json` (via [`live_pinned_generation`]) — so rollback never
/// deletes the running fleet's binaries.
pub fn rollback(layout: &Layout, target: Option<&str>) -> Result<()> {
    // Serialize against a concurrent `deploy`/`rollback` for the whole
    // validate-and-flip. DISTINCT from run/rollout.lock; RAII-held.
    let _mutator_lock = crate::lock::acquire_deploy(&layout.bin_dir)?;

    let pre_flip_current = read_current_generation(&layout.bin_dir);

    let target_name = match target {
        Some(explicit) => normalize_generation_name(explicit)?,
        None => {
            let current = pre_flip_current.ok_or_else(|| {
                anyhow::anyhow!(
                    "nothing is deployed under {} — there is no `current` to roll back FROM; \
                     run `weles deploy` first",
                    layout.bin_dir.display()
                )
            })?;
            let previous = predecessor_generation(&layout.bin_dir, current).ok_or_else(|| {
                anyhow::anyhow!(
                    "no earlier good generation exists below gen-{current} under {} — nothing to \
                     roll back to",
                    layout.bin_dir.display()
                )
            })?;
            format!("gen-{previous}")
        }
    };

    let target_dir = layout.bin_dir.join(&target_name);
    if !target_dir.is_dir() {
        bail!(
            "rollback target {target_name} does not exist under {} — inspect deploy/ for the \
             available generations",
            layout.bin_dir.display()
        );
    }

    // Integrity gate BEFORE the flip: a torn/tampered/manifest-less generation
    // must never become `current`. This is the same read-back `Layout::discover`
    // runs at boot, run here so a bad rollback fails loudly instead of arming the
    // next `up` to fail.
    verify_generation(&target_dir)
        .with_context(|| format!("refusing to roll back to {target_name}"))?;

    let target_gen = parse_generation(OsStr::new(&target_name))
        .expect("normalize_generation_name/predecessor_generation produce a gen-<N> name");

    flip_current(&layout.bin_dir, &target_name)?;
    println!("weles: rolled back, current -> {target_name}");

    // Same retention authority as deploy: protect the new current, the pre-flip
    // current (roll-forward target), and whatever a live supervisor pinned.
    let mut protected: Vec<u64> = vec![target_gen];
    if let Some(previous) = pre_flip_current {
        protected.push(previous);
    }
    if let Some(pinned) = live_pinned_generation(&layout.run_dir) {
        protected.push(pinned);
    }
    prune_stale_generations(&layout.bin_dir, &layout.run_dir, &protected);
    Ok(())
}

/// Parses an operator-supplied rollback target into a `gen-<N>` directory name.
/// Accepts the full `gen-<N>` spelling OR a bare `<N>` (both unambiguous); any
/// other token is a fail-closed error rather than a silent mis-target
/// (anti-magic — never guess which generation was meant).
fn normalize_generation_name(target: &str) -> Result<String> {
    let target = target.trim();
    if let Some(n) = parse_generation(OsStr::new(target)) {
        return Ok(format!("gen-{n}"));
    }
    if let Ok(n) = target.parse::<u64>() {
        return Ok(format!("gen-{n}"));
    }
    bail!("rollback target {target:?} is not a generation — pass `gen-<N>` or a bare `<N>`");
}

/// The highest existing, MANIFEST-BEARING generation strictly below `current` —
/// the default `weles rollback` target. Skips the current generation, any
/// abandoned partial (a `gen-N` dir whose `manifest.json` is absent or
/// unparseable — never a rollback target), and any non-`gen-N` entry. `None`
/// when there is no earlier good generation to fall back to. The full
/// hash-level [`verify_generation`] still runs on the chosen target before the
/// flip; this is only the "is it a plausible generation" filter.
fn predecessor_generation(bin_dir: &Path, current: u64) -> Option<u64> {
    let mut best: Option<u64> = None;
    for entry in std::fs::read_dir(bin_dir).ok()?.flatten() {
        let Some(n) = parse_generation(&entry.file_name()) else {
            continue;
        };
        if n >= current {
            continue;
        }
        let manifest_path = bin_dir.join(format!("gen-{n}")).join("manifest.json");
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        if serde_json::from_slice::<GenerationManifest>(&bytes).is_err() {
            continue;
        }
        best = Some(best.map_or(n, |b| b.max(n)));
    }
    best
}

/// Streams `src` to `dst`, computing the SHA-256 of the bytes written as it
/// copies. Returns `(hex_sha256, byte_len)`.
fn copy_and_hash(src: &Path, dst: &Path) -> Result<(String, u64)> {
    let mut reader = File::open(src).with_context(|| format!("open {}", src.display()))?;
    let mut writer = File::create(dst).with_context(|| format!("create {}", dst.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", src.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        writer
            .write_all(&buf[..read])
            .with_context(|| format!("write {}", dst.display()))?;
        total += read as u64;
    }
    writer
        .flush()
        .with_context(|| format!("flush {}", dst.display()))?;

    // `File::create` gives `dst` the default mode (0644 on unix), dropping
    // any executable bit the source had. Every artifact staged here IS an
    // executable weles later execs directly (`platform::spawn`), so mirror
    // the source's mode onto the destination before it's ever eligible to be
    // pointed at by `current` (this runs inside the copy loop, before
    // `flip_current` — a half-deployed generation can never be `current`).
    // Windows has no executable-bit mode concept (executability is the file
    // extension), so this is a unix-only step; the Windows path is
    // unchanged.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let src_mode = std::fs::metadata(src)
            .with_context(|| format!("stat {}", src.display()))?
            .permissions()
            .mode();
        writer
            .set_permissions(std::fs::Permissions::from_mode(src_mode))
            .with_context(|| format!("set permissions on {}", dst.display()))?;
    }

    Ok((hex_digest(hasher.finalize()), total))
}

/// Streams `path` through SHA-256 WITHOUT copying it — the read-only twin of
/// [`copy_and_hash`]. Returns `(hex_sha256, byte_len)`. Used by
/// [`verify_generation`] to re-derive an artifact's digest from the bytes on
/// disk NOW, so the hash `deploy` recorded is finally read back.
fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut reader = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        total += read as u64;
    }
    Ok((hex_digest(hasher.finalize()), total))
}

/// Renders a finalized SHA-256 digest as lowercase hex. The single formatting
/// authority shared by [`copy_and_hash`] (write path) and [`hash_file`] (verify
/// path), so a staged and a re-derived digest are spelled identically.
fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Verifies a staged generation directory against its `manifest.json`: LOADS the
/// manifest, recomputes the SHA-256 and byte length of every recorded artifact
/// (binaries + the stamped `fleet.toml`) from the bytes on disk, and compares to
/// the recorded values. This is the READ-BACK of the hash `deploy` writes but
/// nothing consumed before M1 — a torn, truncated, or tampered artifact (a bit
/// flipped after staging, a half-written copy an OS crash left behind, a manual
/// edit) is caught HERE, before the generation is ever spawned or made
/// `current`, instead of surfacing as an opaque crash when weles execs a corrupt
/// binary.
///
/// Fails loudly, enumerating every offender, on: a missing / unreadable /
/// unparseable `manifest.json`, a manifest artifact whose file is absent or
/// unreadable, or any length/digest mismatch.
pub fn verify_generation(gen_dir: &Path) -> Result<()> {
    let manifest_path = gen_dir.join("manifest.json");
    let bytes = std::fs::read(&manifest_path).with_context(|| {
        format!(
            "read generation manifest {} — cannot verify the integrity of {}",
            manifest_path.display(),
            gen_dir.display()
        )
    })?;
    let manifest: GenerationManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse generation manifest {}", manifest_path.display()))?;

    // Every artifact the manifest records — the staged binaries plus the stamped
    // fleet.toml (a first-class artifact `up` reads back to learn what to boot).
    let artifacts = manifest.artifacts.iter().chain(std::iter::once(&manifest.fleet));

    let mut problems: Vec<String> = Vec::new();
    for artifact in artifacts {
        let path = gen_dir.join(&artifact.file);
        match hash_file(&path) {
            Ok((_, bytes)) if bytes != artifact.bytes => problems.push(format!(
                "{}: byte length {bytes} != manifest {} (truncated or overwritten)",
                artifact.file, artifact.bytes
            )),
            Ok((sha256, _)) if sha256 != artifact.sha256 => problems.push(format!(
                "{}: sha256 {sha256} != manifest {} (contents changed since deploy)",
                artifact.file, artifact.sha256
            )),
            Ok(_) => {}
            Err(error) => problems.push(format!("{}: {error:#}", artifact.file)),
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "generation {} failed integrity verification against its manifest:\n",
        gen_dir.display()
    );
    for problem in &problems {
        message.push_str(&format!("  {problem}\n"));
    }
    bail!("{message}")
}

/// Parses a `gen-<N>` directory name into `N`. Returns `None` for anything
/// else (`current`, `current.tmp`, stray files).
fn parse_generation(name: &OsStr) -> Option<u64> {
    name.to_str()?.strip_prefix("gen-")?.parse::<u64>().ok()
}

/// Scans `deploy/` for the highest existing `gen-<N>` and returns `N+1` (1 when
/// none exist). Never re-reads `current`: the next number is a max over dirs,
/// so an abandoned partial `gen-N` still advances the counter (its number is
/// never silently reused).
fn next_generation(bin_dir: &Path) -> Result<u64> {
    let mut max = 0u64;
    if let Ok(entries) = std::fs::read_dir(bin_dir) {
        for entry in entries.flatten() {
            if let Some(n) = parse_generation(&entry.file_name()) {
                max = max.max(n);
            }
        }
    }
    Ok(max + 1)
}

/// Atomically points `deploy/current` at `gen_name`: write `current.tmp`, then
/// rename over `current` (std's rename replaces on both platforms). Mirrors
/// `state::checkpoint`'s tmp→rename discipline — a torn `current` is never
/// observable.
fn flip_current(bin_dir: &Path, gen_name: &str) -> Result<()> {
    let current = bin_dir.join("current");
    let tmp = bin_dir.join("current.tmp");
    std::fs::write(&tmp, gen_name).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &current)
        .with_context(|| format!("rename {} over {}", tmp.display(), current.display()))?;
    Ok(())
}

/// Reads `deploy/current` and parses the generation number it names, if any.
/// `None` when nothing is deployed yet or `current` is unparseable.
fn read_current_generation(bin_dir: &Path) -> Option<u64> {
    let contents = std::fs::read_to_string(bin_dir.join("current")).ok()?;
    parse_generation(OsStr::new(contents.trim()))
}

/// The generation number a LIVE, non-terminal supervisor recorded pinning in
/// `run_dir/state.json`, if any. This is the authority for "in use": the
/// one-up-at-a-time invariant means at most one supervisor pins one generation,
/// so a concurrent deploy protects exactly that one by name. `None` when no
/// state file exists, its supervisor is dead or terminal, or it recorded no
/// pin (a legacy/monolith file). Reuses weles's OWN `state::load` +
/// `control::supervisor_alive` (zero-sharing — nothing new imported).
fn live_pinned_generation(run_dir: &Path) -> Option<u64> {
    let state = crate::state::load(&run_dir.join("state.json")).ok()??;
    if state.status.is_terminal() || !crate::control::supervisor_alive(&state.supervisor) {
        return None;
    }
    parse_generation(OsStr::new(state.pinned_generation.as_deref()?))
}

/// Which existing generations to prune: everything NOT in `protected`. Pure (no
/// I/O) so the retention policy is unit-testable in isolation. The authority for
/// "keep" is membership in `protected` (by number) — NOT a numeric position
/// relative to current, which cannot see a live up that pinned an older
/// generation or an abandoned partial that bumped the counter.
fn generations_to_prune(present: &[u64], protected: &[u64]) -> Vec<u64> {
    let mut stale: Vec<u64> = present
        .iter()
        .copied()
        .filter(|n| !protected.contains(n))
        .collect();
    stale.sort_unstable();
    stale
}

/// Deletes every generation not in `protected`. TOLERANT by design: a directory
/// that can't be removed (a live fleet on Windows may still hold a `.exe`, or
/// the entry is otherwise undeletable) is logged and skipped — NEVER an error.
/// Closing "overwrite live exe" must not open "delete live exe". Returns the
/// directories actually removed (for observability/tests).
///
/// The authority for "in use" is the LIVE PIN: immediately before each
/// `remove_dir_all` we re-read `live_pinned_generation(run_dir)` and skip a
/// generation that equals the fresh pin. This closes the TOCTOU window between
/// the caller's `protected` snapshot (built in `deploy`) and this delete loop —
/// a concurrent `up` could pin a generation AFTER the snapshot was taken but
/// BEFORE we reach its directory. We do NOT rename-first: on Windows a running
/// image is opened `FILE_SHARE_DELETE`, so renaming a LIVE generation's dir can
/// SUCCEED and invalidate the pinned `active_bin_dir` (`Layout::discover`), so
/// every crash-respawn would then fail to find its `.exe` — strictly worse than
/// today's partial delete, which at least leaves the locked live `.exe` in place.
///
/// A partial `remove_dir_all` of a genuinely-DEAD generation (not in
/// `protected`, not the live pin) is HARMLESS and self-healing: it is garbage no
/// fleet reads, `next_generation` ignores a partial dir when advancing the
/// counter, and the next prune finishes the `remove_dir_all`. The only case that
/// ever mattered is the LIVE generation, which the pin (+ the Windows liveness
/// check in `supervisor_alive`) protects.
fn prune_stale_generations(bin_dir: &Path, run_dir: &Path, protected: &[u64]) -> Vec<PathBuf> {
    let mut present = Vec::new();
    match std::fs::read_dir(bin_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Some(n) = parse_generation(&entry.file_name()) {
                    present.push(n);
                }
            }
        }
        Err(error) => {
            eprintln!("weles: could not scan {} for retention ({error}) — skipping prune", bin_dir.display());
            return Vec::new();
        }
    }

    let mut removed = Vec::new();
    for n in generations_to_prune(&present, protected) {
        // Fresh live-pin re-read right before destruction: a concurrent `up` may
        // have pinned this generation AFTER the caller's `protected` snapshot.
        if live_pinned_generation(run_dir) == Some(n) {
            eprintln!(
                "weles: generation {n} became the live pin since the retention snapshot — keeping it"
            );
            continue;
        }
        let path = bin_dir.join(format!("gen-{n}"));
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                println!("weles: pruned stale generation {}", path.display());
                removed.push(path);
            }
            Err(error) => {
                eprintln!(
                    "weles: could not prune {} ({error}) — skipping (a live fleet may hold it)",
                    path.display()
                );
            }
        }
    }
    removed
}

/// One provisioning command the fleet declares to run — in declared order —
/// BEFORE any long-lived service is spawned. weles knows the command NAME (a
/// staged package to exec), never its MEANING: `edgeca` mints the internal-edge
/// CA, `adminctl create-user` seeds the dev admin, and weles treats both the
/// same way — "spawn a process I was told to, with a deadline, and abort the
/// whole `up` if it fails." This is the same domain-blind philosophy as the
/// passthrough env list.
///
/// Runs each [`PrepareCmd`] in `commands`, in declared order, BEFORE the fleet
/// is spawned, aborting the whole `up` on the FIRST nonzero exit or timeout
/// (nothing is spawned past a failed hook).
///
/// Each command runs with `cwd = layout.root` (so a relative output path like
/// `run/weles/edge-ca.crt` resolves exactly as a spawned service's would),
/// stdout/stderr captured to `run_dir/<name>.{out,err}.log`, and an environment
/// built from the always-on [`crate::manifest::SERVICE_ENV_ALLOWLIST`] floor,
/// plus the `passthrough` keys forwarded from weles's own environment, plus the
/// literal `env` pairs applied last.
///
/// NO idempotency / file-existence short-circuit (the reversal recorded in the
/// plan's B1): a command that must tolerate re-running — `edgeca` regenerates
/// the CA each `up`, `adminctl create-user` upserts — is the fleet author's
/// responsibility. weles just runs what it was told, every `up`.
///
/// Defined in Step 1; the supervisor wires it into `run_up` (in the slot the old
/// `mint_ca`/`seed_admin` block held) in Step 3.
pub fn run_prepare(commands: &[PrepareCmd], layout: &Layout) -> Result<()> {
    for cmd in commands {
        run_one_prepare(cmd, layout)
            .with_context(|| format!("prepare command {:?}", cmd.name))?;
    }
    Ok(())
}

/// Runs a single [`PrepareCmd`] to completion (or its deadline). Factored out so
/// `run_prepare`'s per-command `with_context` names the offender uniformly.
fn run_one_prepare(cmd: &PrepareCmd, layout: &Layout) -> Result<()> {
    let program = layout.binary(&cmd.run);
    let args: Vec<OsString> = cmd.args.iter().map(OsString::from).collect();

    // Floor first, then forwarded passthrough keys, then the literal env last —
    // so an explicit `env` value always wins over a forwarded one.
    let mut env = filtered_env(crate::manifest::SERVICE_ENV_ALLOWLIST);
    for key in &cmd.passthrough {
        if let Some(value) = lookup_env(key) {
            env.insert(OsString::from(key), value);
        }
    }
    for (key, value) in &cmd.env {
        env.insert(OsString::from(key), OsString::from(value));
    }

    let out_path = layout.run_dir.join(format!("{}.out.log", cmd.name));
    let err_path = layout.run_dir.join(format!("{}.err.log", cmd.name));
    let stdout = File::create(&out_path)
        .with_context(|| format!("create {}", out_path.display()))?;
    let stderr = File::create(&err_path)
        .with_context(|| format!("create {}", err_path.display()))?;

    let mut proc = platform::spawn(SpawnSpec {
        program,
        args,
        env,
        cwd: Some(layout.root.clone()),
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
    .with_context(|| format!("spawn prepare command {:?} ({})", cmd.name, cmd.run))?;

    let timeout = Duration::from_secs(if cmd.timeout_secs == 0 {
        DEFAULT_PREPARE_TIMEOUT_SECS
    } else {
        cmd.timeout_secs
    });

    match wait_for_helper(&mut proc, timeout)? {
        Some(status) if status.success() => Ok(()),
        Some(status) => bail!(
            "prepare command {:?} exited with status {:?} — see {} / {}",
            cmd.name,
            status.code(),
            out_path.display(),
            err_path.display()
        ),
        None => Err(helper_timeout_failure(
            &mut proc,
            &cmd.name,
            timeout,
            &out_path,
            &err_path,
        )),
    }
}

/// Poll-with-deadline wait on a transient helper (never a blocking platform
/// wait, so it can never hang past `timeout`). Public so the integration
/// test in `tests/prep.rs` can drive the timeout branch with the
/// `__test-child` fixture.
pub fn wait_for_helper(
    proc: &mut platform::OwnedProc,
    timeout: Duration,
) -> Result<Option<platform::ExitInfo>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = proc.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The shared timeout branch for every transient helper (a [`PrepareCmd`] via
/// [`run_prepare`]): forcibly stops the still-running helper (0s grace / 5s
/// force — it already blew its deadline) and produces the operator-facing
/// error naming BOTH log paths. Public so the integration test in
/// `tests/prep.rs` can pin the branch: the error names the logs AND the
/// child is dead afterwards.
pub fn helper_timeout_failure(
    proc: &mut platform::OwnedProc,
    what: &str,
    timeout: Duration,
    out_path: &Path,
    err_path: &Path,
) -> anyhow::Error {
    if let Err(error) = proc.shutdown(HELPER_SHUTDOWN_GRACE, HELPER_SHUTDOWN_FORCE) {
        eprintln!("weles: stopping timed-out {what} failed: {error:#}");
    }
    anyhow::anyhow!(
        "{what} did not finish within {timeout:?} — see {} / {}",
        out_path.display(),
        err_path.display()
    )
}

/// Builds a child environment from the parent process's env, keeping only
/// `allowlist` keys (case-insensitive on Windows to match `%VAR%` lookup
/// semantics, exact-case on Unix).
fn filtered_env(allowlist: &[&str]) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    for key in allowlist {
        if let Some(value) = lookup_env(key) {
            // On Windows the lookup above is case-insensitive, so
            // case-variant allowlist entries (`HTTP_PROXY` / `http_proxy`)
            // resolve to the SAME parent variable — keep only the
            // first-inserted spelling instead of emitting a pair differing
            // only by case in the child's environment block. On Unix the
            // lookup is exact-case and the variants are genuinely distinct
            // variables, so no dedupe.
            if cfg!(windows)
                && env.keys().any(|existing: &OsString| {
                    existing
                        .to_str()
                        .is_some_and(|existing| existing.eq_ignore_ascii_case(key))
                })
            {
                continue;
            }
            env.insert(OsString::from(*key), value);
        }
    }
    env
}

#[cfg(test)]
#[path = "prep_tests.rs"]
mod prep_tests;

use std::collections::BTreeMap;
use std::env::consts::EXE_SUFFIX;
use std::path::PathBuf;

/// The single authority for locating built artifacts under a workspace, honoring
/// `CARGO_TARGET_DIR` the same way the build step does.
///
/// The build step spawns `cargo` with the frozen build environment
/// ([`EnvironmentSnapshot::build_environment`](crate::EnvironmentSnapshot::build_environment),
/// which carries `CARGO_TARGET_DIR` via `BUILD_ENV_ALLOWLIST`). Binary *lookup*
/// must resolve the target directory from that SAME authority, or a tool runs a
/// stale artifact (or fails to find one) whenever `CARGO_TARGET_DIR` is set.
/// [`WorkspaceLayout`] is that shared resolver: devctl, splitproof, and verifyctl
/// all build one from their own known workspace root and the frozen build env.
///
/// # Correctness coupling (do not break)
///
/// A *relative* `CARGO_TARGET_DIR` is resolved against `root`. This is correct
/// ONLY because these tools spawn `cargo` with `cwd = root`, and cargo resolves a
/// relative `CARGO_TARGET_DIR` against the invocation's current directory. If any
/// caller ever spawns the build with a different cwd, the build output directory
/// and this lookup will silently diverge — keep the spawn cwd equal to `root`.
///
/// # Known gap
///
/// `.cargo/config.toml`'s `build.target-dir` is NOT parsed here. Only the
/// `CARGO_TARGET_DIR` environment variable (and the `root/target` default) are
/// honored. A config-file override would desync build and lookup; none of these
/// dev tools set one, and resolving it would require `cargo metadata` in the hot
/// path, which this type deliberately avoids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceLayout {
    pub root: PathBuf,
    pub target_dir: PathBuf,
}

impl WorkspaceLayout {
    /// Builds a layout from a known workspace `root` and the frozen build
    /// environment (`EnvironmentSnapshot::build_environment()`).
    ///
    /// `target_dir` resolution:
    /// - `CARGO_TARGET_DIR` present and non-empty: verbatim if absolute,
    ///   otherwise resolved against `root`.
    /// - absent or empty: `root/target`.
    pub fn from_root(root: PathBuf, build_env: &BTreeMap<String, String>) -> Self {
        let target_dir = match build_env.get("CARGO_TARGET_DIR") {
            Some(value) if !value.is_empty() => {
                let candidate = PathBuf::from(value);
                if candidate.is_absolute() {
                    candidate
                } else {
                    root.join(candidate)
                }
            }
            _ => root.join("target"),
        };
        Self { root, target_dir }
    }

    /// Path to a built binary: `target_dir/<profile>/<package><EXE_SUFFIX>`.
    pub fn binary(&self, profile: &str, package: &str) -> PathBuf {
        self.target_dir
            .join(profile)
            .join(format!("{package}{EXE_SUFFIX}"))
    }
}

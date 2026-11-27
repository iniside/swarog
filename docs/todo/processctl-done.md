# Processctl

Last updated: 2026-11-27

- [x] **The MSVC linker PATH test fails here** (2026-11-27, verify-reds-fix step 3)
  `sanitized_build_path_contains_the_discovered_msvc_linker` failed under verifyctl because
  `BUILD_ENV_ALLOWLIST` lacked `ProgramFiles` while the VS lookup read the process environment. The
  allowlist now has it and `append_msvc_linker_path` reads the filtered map.

//! trybuild harness for the `#[rpc]` macro's compile-time `#[http(...)]` validation
//! (finding 9, plan Step 7). A valid mapping compiles; a bogus `path_args`/`body_names`
//! key or a placeholder/`path_args` mismatch is an ordinary compile error.
//!
//! Regenerate the `.stderr` expectations with `TRYBUILD=overwrite cargo test -p rpc-macro`.

#[test]
fn http_mapping_validation() {
    let t = trybuild::TestCases::new();
    t.pass("tests/compile_fail/pass_valid.rs");
    t.compile_fail("tests/compile_fail/bad_path_args_key.rs");
    t.compile_fail("tests/compile_fail/bad_body_names_key.rs");
    t.compile_fail("tests/compile_fail/placeholder_mismatch.rs");
}

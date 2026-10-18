//! Compile-time errors of `#[simjig::test]`, checked against `tests/harness_ui/*.stderr`
//! (regenerate with `TRYBUILD=overwrite`, then review the diff).

#[test]
fn signature_errors_point_at_the_offending_tokens() {
    trybuild::TestCases::new().compile_fail("tests/harness_ui/*.rs");
}

//! Compile-time properties of the public API, checked against `tests/ui/*.stderr`
//! (regenerate with `TRYBUILD=overwrite`, then review the diff).

#[test]
fn error_enums_are_non_exhaustive() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

//! Procedural macros for simjig.
//!
//! This crate is a thin shell: each macro converts its input to `proc_macro2` and calls a
//! module that holds all the logic, so the logic can be unit-tested and snapshot-tested
//! outside the compiler. Users reach the macros through `simjig` (`#[simjig::test]`); the
//! expansions refer only to `::simjig::...` paths.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;

mod test_attr;

/// Turns an `async fn` into a `#[test]` that runs it as the main task of a simulation.
///
/// ```ignore
/// #[simjig::test(timeout = "2ms")]
/// async fn backpressure(sim: &simjig::TestCtx, dut: fifo::Dut) -> simjig::Result {
///     let mut rng = sim.rng("stim");
///     // ...
///     Ok(())
/// }
/// ```
///
/// # Arguments
/// - `timeout = "<integer><unit>"` (unit `fs`, `ps`, `ns`, `us`, `ms` or `s`): the
///   simulation-time budget. Past it the test fails with `TimeLimit`. The value must be a
///   whole number of the design's ticks. Without it, the test has no time limit.
///
/// # Signature
/// `async fn name([sim: &simjig::TestCtx,] [dut: D]) [-> simjig::Result]`, where `D`
/// implements `simjig::DutModel` and the parameter names are free. A shared-reference
/// parameter is the context, a by-value parameter the DUT; the context comes first. The
/// function cannot be generic, `const`, `unsafe` or a method. Other attributes
/// (`#[ignore]`, `#[should_panic]`, docs) apply to the generated test.
///
/// # What the test does
/// It picks the root seed (`SIMJIG_SEED`, or a hash of the test name), builds the model
/// with `D::instantiate` and the DUT with `D::create`, runs the body as the task `main`,
/// and on any failure prints one report block (kind, time, task, location, message,
/// triage signature, a one-line repro command) and fails. Every run writes
/// `<target>/simjig/runs/<package>/<binary>/<test>/<seed>/{summary.json,events.jsonl}`.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_attr::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

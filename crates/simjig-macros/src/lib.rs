//! Procedural macros for simjig.
//!
//! This crate is a thin shell: each macro converts its input to `proc_macro2` and calls a
//! module that holds all the logic, so the logic can be unit-tested and snapshot-tested
//! outside the compiler. Users reach the macros through `simjig` (`#[simjig::test]`); the
//! expansions refer only to `::simjig::...` paths, or to the path given by `crate = ...`.

use proc_macro::TokenStream;

mod test_attr;

/// Turns an `async fn` into a `#[test]` that runs it as the main task of a simulation.
///
/// ```ignore
/// #[simjig::test(timeout = "2ms")]
/// async fn backpressure(ctx: &simjig::TestCtx, dut: fifo::Dut) -> simjig::Result {
///     let mut rng = ctx.rng("stim");
///     // ...
///     Ok(())
/// }
/// ```
///
/// # Arguments
/// - `timeout = "<integer><unit>"` (unit `fs`, `ps`, `ns`, `us`, `ms` or `s`): the
///   simulation-time budget. Past it the test fails with `TimeLimit`. The value must be a
///   whole number of the design's ticks. Without it, the budget is 10 ms of simulation
///   time (`simjig::__private::DEFAULT_TIMEOUT_MS`, rounded up to a whole tick), and the
///   `TimeLimit` report says it was the default; `timeout = "none"` removes the limit.
/// - `init = zero` or `init = random`: the model's initial values (see `simjig::InitValues`).
/// - `trace_hash = false`: the run keeps no trace hash, which costs two multiplications
///   per kernel event (about 2% of a busy clocked testbench's time). Everything else is
///   the same: the same events happen and are counted
///   and kept in the recent-events window, but the report, `summary.json` and the
///   outcome carry no hash, so two runs cannot be compared by it, and a waves replay
///   checks only that it ended the same way, not that it took the same path.
/// - `crate = <path>`: how the expansion names simjig, for a crate that renamed the
///   dependency (`sj = { package = "simjig", ... }` takes `crate = ::sj`). The default is
///   `::simjig`. The DUT bindings that simjig-build generates (`include_dut!`) still
///   name `::simjig`, so a crate with generated DUTs must keep the dependency's name.
///
/// # Signature
/// `async fn name([ctx: &simjig::TestCtx,] [dut: D, ...]) [-> simjig::Result]`, where each
/// `D` implements `simjig::DutModel` and the parameter names are free. A shared-reference
/// parameter is the context, a by-value parameter a DUT; the context comes first. Up to
/// four DUTs share one simulation (`async fn t(a: inv::Dut, b: flop::Dut)`), each with its
/// own model, evaluated in parameter order in every delta; see `simjig::Lockstep`. The
/// function cannot be generic (except with `duts`), `const`, `unsafe` or a method. Other attributes
/// (`#[ignore]`, `#[should_panic]`, docs) apply to the generated test.
///
/// # What the test does
/// It picks the root seed (`SIMJIG_SEED`, or a hash of the test name), builds the model
/// with `D::instantiate` and the DUT with `D::create` (several DUTs: their tuple's), runs the body as the task `main`,
/// and on any failure prints one report block (kind, time, task, location, message,
/// triage signature, a one-line repro command) and fails. Every run writes
/// `<target>/simjig/runs/<package>/<binary>/<test>/<seed>/{summary.json,events.jsonl}`.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_attr::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

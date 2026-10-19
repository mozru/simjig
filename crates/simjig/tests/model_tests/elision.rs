//! Eval elision (`Models::eval` skips an eval with nothing written since the last one)
//! against Verilator's scheduling: such an eval is not always a no-op. So it is an opt-in
//! (`ModelOptions::with_elide_evals`, `#[simjig::test(elide_evals)]`), and even then off
//! for a model whose build shows a variable C++ may write.

use simjig::{ModelOptions, Seed, SeedSource, Sim, ns, read_only, timer};

use super::duts::{sim, sim_with};
use crate::{input_time, rw_meta, rw_time};

fn elided() -> ModelOptions {
    ModelOptions::new(Seed::new(1, SeedSource::Env)).with_elide_evals(true)
}

/// `t_out` at 5 ns and at 10 ns, with nothing written at all.
fn t_out_at_5_and_10<D>(mut sim: Sim, t_out: fn(&D) -> u128, dut: D) -> (u128, u128)
where
    D: 'static,
{
    sim.run(async move {
        timer(ns(5)).await;
        read_only().await;
        let five = t_out(&dut);
        timer(ns(5)).await;
        read_only().await;
        (five, t_out(&dut))
    })
    .unwrap()
}

/// By default every eval runs, so logic fed by an input the design writes itself (which no
/// build output shows) re-reads `$time` on each, as a C++ loop calling `eval()` once per
/// timestep sees.
#[test]
fn by_default_an_eval_with_nothing_written_runs_logic_fed_by_a_self_written_input() {
    let (sim, dut) = sim::<input_time::Dut>();
    let seen = t_out_at_5_and_10(sim, |d: &input_time::Dut| d.t_out.get().to_u128(), dut);
    assert_eq!(seen, (5, 10));
}

/// The opt-in's documented limit: with elision on, that eval is skipped and `t_out` keeps
/// the time of the first eval. This is why elision is not the default.
#[test]
fn opted_in_elision_skips_the_eval_a_self_written_input_needs() {
    let (sim, dut) = sim_with::<input_time::Dut>(&elided());
    let seen = t_out_at_5_and_10(sim, |d: &input_time::Dut| d.t_out.get().to_u128(), dut);
    assert_eq!(seen, (0, 0));
}

/// Opting in changes nothing for a model whose build shows a writable public variable: it
/// evaluates every time.
#[test]
fn opted_in_elision_still_evaluates_a_model_with_a_public_rw_variable() {
    let (sim, dut) = sim_with::<rw_meta::Dut>(&elided());
    let seen = t_out_at_5_and_10(sim, |d: &rw_meta::Dut| d.t_out.get().to_u128(), dut);
    assert_eq!(seen, (5, 10));
}

/// Logic fed by a `public_flat_rw` variable runs on every eval (Verilator's 'ico' region,
/// 'first iteration' trigger), so an eval at a new time with nothing written re-reads
/// `$time`, as a C++ loop calling `eval()` once per timestep sees. Skipping it leaves
/// `t_out` at the time of the last write.
#[test]
fn an_eval_with_nothing_written_still_runs_logic_fed_by_a_public_rw_variable() {
    let (mut sim, dut) = sim::<rw_time::Dut>();
    let seen = sim
        .run(async move {
            timer(ns(5)).await;
            dut.knob.set(1);
            read_only().await;
            let at_write = dut.t_out.get().to_u128();
            timer(ns(5)).await;
            read_only().await;
            (at_write, dut.t_out.get().to_u128())
        })
        .unwrap();
    assert_eq!(seen, (5, 10));
}

/// The same for a variable that a metacomment makes `public_flat_rw`: nothing in the
/// signal table shows it, but the model's scope table does (`SIMJIG_FEATURE_PUBLIC_RW`),
/// and nothing need be written at all for the logic to run on each eval.
#[test]
fn an_unwritten_public_rw_variable_still_runs_its_logic_on_each_eval() {
    let (mut sim, dut) = sim::<rw_meta::Dut>();
    let seen = sim
        .run(async move {
            timer(ns(5)).await;
            read_only().await;
            let five = dut.t_out.get().to_u128();
            timer(ns(5)).await;
            read_only().await;
            (five, dut.t_out.get().to_u128())
        })
        .unwrap();
    assert_eq!(seen, (5, 10));
}

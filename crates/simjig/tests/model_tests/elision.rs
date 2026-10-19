//! Eval elision (`Models::eval` skips an eval with nothing written since the last one)
//! against Verilator's scheduling: such an eval is not always a no-op.

use simjig::{ReadOnly, Timer, ns};

use super::duts::sim;
use crate::{rw_meta, rw_time};

/// Logic fed by a `public_flat_rw` variable runs on every eval (Verilator's 'ico' region,
/// 'first iteration' trigger), so an eval at a new time with nothing written re-reads
/// `$time`, as a C++ loop calling `eval()` once per timestep sees. Skipping it leaves
/// `t_out` at the time of the last write.
#[test]
fn an_eval_with_nothing_written_still_runs_logic_fed_by_a_public_rw_variable() {
    let (mut sim, dut) = sim::<rw_time::Dut>();
    let seen = sim
        .run(async move {
            Timer::new(ns(5)).await;
            dut.knob.set(1);
            ReadOnly::new().await;
            let at_write = dut.t_out.get().to_u128();
            Timer::new(ns(5)).await;
            ReadOnly::new().await;
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
            Timer::new(ns(5)).await;
            ReadOnly::new().await;
            let five = dut.t_out.get().to_u128();
            Timer::new(ns(5)).await;
            ReadOnly::new().await;
            (five, dut.t_out.get().to_u128())
        })
        .unwrap();
    assert_eq!(seen, (5, 10));
}

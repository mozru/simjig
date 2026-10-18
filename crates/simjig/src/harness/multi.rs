//! Several DUTs in one test: tuple [`DutModel`]s.
//!
//! `#[simjig::test] async fn t(a: inv::Dut, b: flop::Dut)` runs `(inv::Dut, flop::Dut)` as
//! its DUT type. The tuple's [`DutModel`] loads each model on its own
//! ([`DutModel::load`]: its own `VerilatedContext`, its own `$urandom` state, its own
//! `--prefix`), puts them into one [`Sim`] in parameter order, and binds each DUT to its
//! index ([`DutModel::bind`]). The kernel then evaluates every model in every delta, in that
//! order, before applying the write log (`kernel::models`), so one delta loop and one time
//! axis serve all of them. Up to four DUTs are supported.
//!
//! Every model receives the same [`ModelOptions`], so two instances of one design get the
//! same Verilator seed: lockstep runs depend on it, since `$urandom` must draw the same
//! values on both sides.
//!
//! # Limits
//! - The models of one simulation must share a time precision
//!   ([`ModelSetError::PrecisionMismatch`](crate::ModelSetError::PrecisionMismatch)).
//! - DPI exports are not supported in a multi-model simulation. Verilator names an exported
//!   function's C symbol after the SystemVerilog function, without the `--prefix`, so two
//!   builds of one design that export it collide at link time; two instances of one build
//!   share the symbol, and which instance it runs in is the `svSetScope` scope, which
//!   simjig does not switch per model.

use super::dut::{DutModel, ModelOptions};
use crate::kernel::models::ModelSetError;
use crate::{Failure, FailureKind, Precision, Sim, SimTime};

/// The failure of building a simulation from models that cannot share one.
fn model_set_failure(error: &ModelSetError) -> Failure {
    let precision = match error {
        ModelSetError::PrecisionMismatch { expected, .. } => *expected,
        ModelSetError::Empty | ModelSetError::TooManySignals => Precision::PS,
    };
    let message = format!("the test's DUTs cannot share one simulation: {error}");
    Failure {
        kind: FailureKind::ModelFatal {
            message: message.clone(),
        },
        time: SimTime::ZERO,
        precision,
        task: None,
        location: None,
        message,
    }
}

/// Implements [`DutModel`] for a tuple of DUT types, model `i` being element `i`.
macro_rules! tuple_dut {
    ($($dut:ident $index:tt),+) => {
        /// Several DUTs in one simulation, numbered in tuple order; see `harness::multi`.
        impl<$($dut: DutModel),+> DutModel for ($($dut,)+) {
            fn instantiate(options: &ModelOptions) -> Result<Sim, Failure> {
                let models = vec![$($dut::load(options)?.into_backend()),+];
                Sim::from_backends(models).map_err(|e| model_set_failure(&e))
            }

            fn create(sim: &mut Sim) -> Result<Self, Failure> {
                Ok(($($dut::bind(sim, $index)?,)+))
            }
        }
    };
}

tuple_dut!(A 0, B 1);
tuple_dut!(A 0, B 1, C 2);
tuple_dut!(A 0, B 1, C 2, D 3);

//! [`DutModel`]: the seam between `#[simjig::test]` and the code that builds a model.
//!
//! The harness knows nothing about Verilator. For each test it asks the DUT type for a
//! [`Sim`] ([`DutModel::instantiate`], given the run's [`ModelOptions`]) and then for the
//! DUT value bound to that simulation ([`DutModel::create`]). simjig-build's generated
//! `Dut` types implement the trait on top of the model backend; tests of simjig itself
//! implement it on [`FakeModel`].
//!
//! A test with several DUTs (`async fn t(a: inv::Dut, b: flop::Dut)`) asks each DUT type
//! for its model alone ([`DutModel::load`]), puts the models into one [`Sim`] in parameter
//! order, and binds each DUT to its model's index ([`DutModel::bind`]); see
//! `harness::multi`.

use std::fmt;

use crate::kernel::backend::ModelBackend;
use crate::model::trace::TraceOptions;
use crate::{Failure, FailureKind, FakeModel, Precision, Sim, SimTime};

use super::seed::Seed;

/// What the harness passes to a model when it builds one for a test run.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelOptions {
    /// The run's root seed.
    pub seed: Seed,
    /// The seed for Verilator's own random number generator (`$random`, `$urandom`,
    /// randomized initial values), in `1..=i32::MAX`; see [`Seed::verilator_seed`].
    pub verilator_seed: u32,
    /// The FST waves a Verilator model records; `None` records none. Models that cannot
    /// trace (a [`FakeModel`]) ignore it.
    pub trace: Option<TraceOptions>,
}

impl ModelOptions {
    /// The options for a run with root seed `seed`.
    #[must_use]
    pub fn new(seed: Seed) -> Self {
        Self {
            seed,
            verilator_seed: seed.verilator_seed(),
            trace: None,
        }
    }

    /// These options, with the model recording the waves `trace` describes.
    #[must_use]
    pub fn with_trace(mut self, trace: TraceOptions) -> Self {
        self.trace = Some(trace);
        self
    }

    /// The Verilator plusargs these options stand for, in order; a Verilator backend
    /// passes them to `VerilatedContext::commandArgs`.
    #[must_use]
    pub fn verilator_args(&self) -> Vec<String> {
        vec![format!("+verilator+seed+{}", self.verilator_seed)]
    }
}

/// A design under test that `#[simjig::test]` can build: the type of a test's DUT
/// parameter.
///
/// This is the seam the model and codegen layers implement: simjig-build generates one
/// implementation per DUT (see [`include_dut!`](crate::include_dut)). The harness calls
/// `instantiate` and then `create` once per test, before the test body runs; an `Err`
/// from either fails the test with the usual report.
///
/// `()` implements it with an empty model, for tests that take no DUT.
///
/// ```
/// use simjig::{DutModel, FakeModel, Failure, ModelOptions, Precision, RawSig, Sim};
///
/// struct Counter {
///     clk: RawSig,
///     count: RawSig,
/// }
///
/// impl DutModel for Counter {
///     fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
///         let mut model = FakeModel::new(Precision::NS);
///         let clk = model.signal("clk", 1);
///         let count = model.signal("count", 8);
///         model.on_rising(clk, move |s| s.set(count, s.get(count) + 1));
///         Ok(Sim::from_fake(model))
///     }
///
///     fn create(sim: &mut Sim) -> Result<Self, Failure> {
///         // Signal names come from the design, so a missing one is a bug in this impl.
///         let sig = |name| sim.signal(name).ok_or(name);
///         match (sig("clk"), sig("count")) {
///             (Ok(clk), Ok(count)) => Ok(Self { clk, count }),
///             (Err(name), _) | (_, Err(name)) => panic!("no signal `{name}`"),
///         }
///     }
/// }
/// ```
#[expect(
    clippy::result_large_err,
    reason = "called once per test, and Failure stays plain data (as Sim::run)"
)]
pub trait DutModel: Sized + 'static {
    /// Builds the model and the simulation that will drive it.
    ///
    /// # Errors
    /// A [`Failure`] if the model cannot be built (for example
    /// [`FailureKind::ModelFatal`](crate::FailureKind::ModelFatal) when it fails to load).
    fn instantiate(options: &ModelOptions) -> Result<Sim, Failure>;

    /// Binds the DUT to `sim`: looks up its signals, adds its clocks.
    ///
    /// # Errors
    /// A [`Failure`] if the model does not match what the DUT type expects.
    fn create(sim: &mut Sim) -> Result<Self, Failure>;

    /// Builds the model alone, to share one [`Sim`] with the models of other DUTs.
    ///
    /// The default refuses: a DUT type that builds its `Sim` directly cannot share one.
    /// Generated `Dut` types implement it.
    ///
    /// # Errors
    /// A [`Failure`] if the model cannot be built, or if the type does not support it.
    fn load(_options: &ModelOptions) -> Result<LoadedModel, Failure> {
        Err(unsupported::<Self>("load"))
    }

    /// Binds the DUT to model number `model` of `sim` (its position among the test's DUT
    /// parameters, from 0).
    ///
    /// The default binds model 0 with [`create`](Self::create) and refuses any other.
    ///
    /// # Errors
    /// A [`Failure`] if model `model` is not the model this DUT type expects.
    fn bind(sim: &mut Sim, model: usize) -> Result<Self, Failure> {
        if model == 0 {
            Self::create(sim)
        } else {
            Err(unsupported::<Self>("bind"))
        }
    }
}

/// One model built by [`DutModel::load`], not yet part of a [`Sim`].
pub struct LoadedModel {
    backend: Box<dyn ModelBackend>,
}

impl LoadedModel {
    /// A [`FakeModel`], for a hand-written [`DutModel`] that takes part in a test with
    /// several DUTs.
    #[must_use]
    pub fn from_fake(model: FakeModel) -> Self {
        Self::from_backend(Box::new(model))
    }

    pub(crate) fn from_backend(backend: Box<dyn ModelBackend>) -> Self {
        Self { backend }
    }

    pub(crate) fn into_backend(self) -> Box<dyn ModelBackend> {
        self.backend
    }
}

impl fmt::Debug for LoadedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedModel")
            .field("precision", &self.backend.precision())
            .field("signals", &self.backend.slot_count())
            .finish()
    }
}

/// The failure of a default [`DutModel`] method that `D` does not provide.
fn unsupported<D>(method: &str) -> Failure {
    let message = format!(
        "`{}` does not implement `DutModel::{method}`, so it cannot share a simulation with \
         other DUTs; implement `load` and `bind` (generated `Dut` types do)",
        std::any::type_name::<D>()
    );
    Failure {
        kind: FailureKind::ModelFatal {
            message: message.clone(),
        },
        time: SimTime::ZERO,
        precision: Precision::PS,
        task: None,
        location: None,
        message,
    }
}

/// No DUT: an empty model with Verilator's default precision, 1 ps.
impl DutModel for () {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        Ok(Sim::from_fake(FakeModel::new(Precision::PS)))
    }

    fn create(_sim: &mut Sim) -> Result<Self, Failure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SeedSource;

    #[test]
    fn options_carry_the_verilator_seed_as_a_plusarg() {
        let seed = Seed::new(5, SeedSource::Env);
        let options = ModelOptions::new(seed);
        assert_eq!(options.verilator_seed, seed.verilator_seed());
        assert_eq!(
            options.verilator_args(),
            [format!("+verilator+seed+{}", seed.verilator_seed())]
        );
    }
}

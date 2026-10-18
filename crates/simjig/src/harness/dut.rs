//! [`DutModel`]: the seam between `#[simjig::test]` and the code that builds a model.
//!
//! The harness knows nothing about Verilator. For each test it asks the DUT type for a
//! [`Sim`] ([`DutModel::instantiate`], given the run's [`ModelOptions`]) and then for the
//! DUT value bound to that simulation ([`DutModel::create`]). simjig-build's generated
//! `Dut` types implement the trait on top of the model backend; tests of simjig itself
//! implement it on [`FakeModel`].

use crate::{Failure, FakeModel, Precision, Sim};

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
}

impl ModelOptions {
    /// The options for a run with root seed `seed`.
    #[must_use]
    pub fn new(seed: Seed) -> Self {
        Self {
            seed,
            verilator_seed: seed.verilator_seed(),
        }
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

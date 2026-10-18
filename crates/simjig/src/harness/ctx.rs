//! [`TestCtx`]: what a `#[simjig::test]` body knows about its own run.

use super::seed::{Rng, Seed};

/// The running test's identity and seeds, passed to a `#[simjig::test]` body that takes a
/// `&TestCtx` parameter.
///
/// ```ignore
/// #[simjig::test]
/// async fn random_writes(sim: &TestCtx, dut: fifo::Dut) {
///     let mut rng = sim.rng("stim");
///     dut.data.set(rng.below(256));
/// }
/// ```
///
/// It is not the [`Sim`](crate::Sim): the simulation is running while the body runs, so
/// its time and tasks are reached through the free functions ([`now`](crate::now),
/// [`spawn`](crate::spawn), ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCtx {
    test: String,
    seed: Seed,
    verilator_seed: u32,
}

impl TestCtx {
    pub(crate) fn new(test: String, seed: Seed, verilator_seed: u32) -> Self {
        Self {
            test,
            seed,
            verilator_seed,
        }
    }

    /// The test's name as the test runner reports it (`module::test`).
    #[must_use]
    pub fn test_name(&self) -> &str {
        &self.test
    }

    /// The run's root seed; re-run with `SIMJIG_SEED=<seed>` to reproduce the run.
    #[must_use]
    pub fn seed(&self) -> Seed {
        self.seed
    }

    /// The random stream named `path`, derived from the root seed.
    ///
    /// Each call starts the stream from its beginning, so give each independent source of
    /// randomness its own path (`"stim"`, `"ready"`): then adding draws to one stream never
    /// changes the values of another.
    #[must_use]
    pub fn rng(&self, path: &str) -> Rng {
        self.seed.rng(path)
    }

    /// The seed the model was given for Verilator's own random number generator
    /// (`+verilator+seed+<n>`).
    #[must_use]
    pub fn verilator_seed(&self) -> u32 {
        self.verilator_seed
    }
}

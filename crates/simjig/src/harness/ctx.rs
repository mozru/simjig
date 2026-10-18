//! [`TestCtx`]: what a `#[simjig::test]` body knows about its own run.
//!
//! Its named random streams are handed out once per path per test: a second
//! [`TestCtx::rng`] call with the same path panics, naming the first call. Two calls that
//! returned the same stream would draw identical values (two "independent" drivers in
//! lock-step), which no test means. Continuing the first stream instead would need a
//! shared, mutable `Rng` handle, and the values a task drew would then depend on how its
//! draws interleave with other tasks', which is exactly the coupling named streams exist
//! to avoid. A test that wants more values keeps the `Rng`; one that wants another stream
//! names it (`"stim.lane1"`).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::panic::Location;
use std::sync::{Arc, Mutex, PoisonError};

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
///
/// Clones share the record of which streams were taken.
#[derive(Clone)]
pub struct TestCtx {
    test: String,
    seed: Seed,
    verilator_seed: u32,
    /// Where each stream path was first taken.
    taken: Arc<Mutex<BTreeMap<String, &'static Location<'static>>>>,
}

impl fmt::Debug for TestCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestCtx")
            .field("test", &self.test)
            .field("seed", &self.seed)
            .field("verilator_seed", &self.verilator_seed)
            .finish_non_exhaustive()
    }
}

/// Two contexts are equal when they describe the same run (which streams were taken
/// does not count).
impl PartialEq for TestCtx {
    fn eq(&self, other: &Self) -> bool {
        (&self.test, self.seed, self.verilator_seed)
            == (&other.test, other.seed, other.verilator_seed)
    }
}

impl Eq for TestCtx {}

impl TestCtx {
    pub(crate) fn new(test: String, seed: Seed, verilator_seed: u32) -> Self {
        Self {
            test,
            seed,
            verilator_seed,
            taken: Arc::default(),
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
    /// Give each independent source of randomness its own path (`"stim"`, `"ready"`):
    /// then adding draws to one stream never changes the values of another. Each path
    /// can be taken once per test (see the module docs for why);
    /// [`Seed::rng`](crate::Seed::rng) on [`seed`](Self::seed) gives the same stream
    /// without the check, for a test that predicts values.
    ///
    /// # Panics
    /// If this test already took the stream `path`; the message names where.
    #[must_use]
    #[track_caller]
    pub fn rng(&self, path: &str) -> Rng {
        let here = Location::caller();
        let first = {
            let mut taken = self.taken.lock().unwrap_or_else(PoisonError::into_inner);
            match taken.entry(path.to_owned()) {
                Entry::Occupied(first) => Some(*first.get()),
                Entry::Vacant(slot) => {
                    slot.insert(here);
                    None
                }
            }
        };
        if let Some(first) = first {
            panic!(
                "the random stream {path:?} was already taken at {first}; a second \
                 `rng({path:?})` would repeat its values. Keep the first `Rng`, or name \
                 another stream (\"{path}.2\")"
            );
        }
        self.seed.rng(path)
    }

    /// The seed the model was given for Verilator's own random number generator
    /// (`+verilator+seed+<n>`).
    #[must_use]
    pub fn verilator_seed(&self) -> u32 {
        self.verilator_seed
    }
}

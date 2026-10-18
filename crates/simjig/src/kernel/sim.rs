//! [`Sim`]: one simulation (a model plus the kernel that drives it) and its entry point,
//! [`Sim::run`].

use std::fmt;
use std::future::Future;
use std::rc::Rc;

use super::OrInvariant;
use super::backend::ModelBackend;
use super::clock::{Clock, ClockEntry, ClockError, ClockHandle, ClockState};
use super::failure::Failure;
use super::fake::FakeModel;
use super::models::{ModelSetError, Models};
use super::panics;
use super::recorder::TraceRecord;
use super::sched;
use super::signal::RawSig;
use super::state::{self, Kernel, KernelStats, Phase};
use super::tasks;
use super::time::{Precision, SimTime};

/// A simulation: a model, its clocks and the kernel that runs a testbench against it.
///
/// ```
/// use simjig::{FakeModel, Precision, Sim, Clock, SimDuration, rising};
///
/// let mut model = FakeModel::new(Precision::NS);
/// let clk = model.signal("clk", 1);
/// let count = model.signal("count", 8);
/// model.on_rising(clk, move |s| s.set(count, s.get(count) + 1));
///
/// let mut sim = Sim::from_fake(model);
/// let clk = sim.signal("clk").unwrap();
/// let count = sim.signal("count").unwrap();
/// sim.add_clock(Clock::new(clk, SimDuration::from_ticks(10))).unwrap();
/// let seen = sim.run(async move {
///     for _ in 0..3 {
///         rising(clk).await;
///     }
///     count.read_u64()
/// });
/// // Right after an edge of a kernel clock, flops still show their pre-edge values.
/// assert_eq!(seen.unwrap(), 2);
/// ```
pub struct Sim {
    kernel: Rc<Kernel>,
    time_limit: Option<SimTime>,
    ran: bool,
}

impl fmt::Debug for Sim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sim")
            .field("sim", &self.kernel.sim_gen)
            .field("now", &self.kernel.now.get())
            .field("clocks", &self.kernel.clocks.borrow().len())
            .finish_non_exhaustive()
    }
}

impl Sim {
    /// A simulation of a [`FakeModel`].
    #[must_use]
    pub fn from_fake(model: FakeModel) -> Self {
        Self::from_backend(Box::new(model))
    }

    /// A simulation of several [`FakeModel`]s sharing time and one scheduler loop,
    /// numbered in the order given ([`signal_in`](Self::signal_in) takes the number).
    ///
    /// Every delta evaluates all of them, in that order, before the write log is applied,
    /// so a testbench that forwards one model's output to another's input sees it land one
    /// delta later whatever the task order. Watched-signal changes are compared across all
    /// models, and the next timestep is the earliest of every model's pending event (and
    /// the timers and clocks).
    ///
    /// # Errors
    /// See [`ModelSetError`].
    pub fn from_fakes(models: impl IntoIterator<Item = FakeModel>) -> Result<Self, ModelSetError> {
        let backends = models
            .into_iter()
            .map(|m| Box::new(m) as Box<dyn ModelBackend>)
            .collect();
        Self::from_backends(backends)
    }

    pub(crate) fn from_backend(model: Box<dyn ModelBackend>) -> Self {
        Self::from_backends(vec![model])
            .or_invariant("invariant: a single model always forms a valid set")
    }

    /// A simulation of `models`, numbered in the order given; see
    /// [`from_fakes`](Self::from_fakes) for how they run.
    pub(crate) fn from_backends(models: Vec<Box<dyn ModelBackend>>) -> Result<Self, ModelSetError> {
        Ok(Self {
            kernel: Rc::new(Kernel::new(Models::new(models)?)),
            time_limit: None,
            ran: false,
        })
    }

    /// The handle of the first model's signal named `name`, if it has one; with several
    /// models, [`signal_in`](Self::signal_in) picks the model.
    #[must_use]
    pub fn signal(&self, name: &str) -> Option<RawSig> {
        self.signal_in(0, name)
    }

    /// The handle of the signal named `name` of model `model` (0 for the first, in the
    /// order the models were given), if there is one.
    ///
    /// # Panics
    /// Never in practice: slot numbers fit `u32` by construction.
    #[must_use]
    pub fn signal_in(&self, model: usize, name: &str) -> Option<RawSig> {
        let starts = &self.kernel.starts;
        let (start, end) = (*starts.get(model)?, *starts.get(model + 1)?);
        let names = &self.kernel.names[start as usize..end as usize];
        let slot = names.iter().position(|n| &**n == name)?;
        let slot = u32::try_from(slot).or_invariant("invariant: slots are numbered by u32");
        let model = u32::try_from(model).or_invariant("invariant: model was found by index");
        Some(RawSig::in_model(self.kernel.sim_gen, model, slot))
    }

    /// How many models the simulation drives.
    #[must_use]
    pub fn model_count(&self) -> usize {
        self.kernel.starts.len() - 1
    }

    /// The coverage counters of model `model` (0 for a one-model simulation), copied out
    /// now: `None` unless it is a Verilator model built with coverage
    /// (`simjig_build::Dut::coverage`).
    ///
    /// The counters come in the order of the model's counter arrays (the ABI's `cov_runs`),
    /// one entry per counter; an index names the same coverage point for one build of the
    /// model, not across builds (docs/notes/tracecov.md). Read them between runs; the
    /// model's `coverage.dat` names every point.
    #[must_use]
    pub fn coverage(&self, model: usize) -> Option<Vec<u32>> {
        self.kernel.models.borrow().coverage(model)
    }

    /// The length of one tick.
    #[must_use]
    pub fn precision(&self) -> Precision {
        self.kernel.precision
    }

    /// Adds a kernel-driven clock, running from time 0. Coincident edges are applied in
    /// the order clocks are added. The handle stops and restarts it from inside the run.
    ///
    /// # Errors
    /// See [`ClockError`].
    ///
    /// # Panics
    /// If the signal belongs to another simulation.
    pub fn add_clock(&mut self, clock: Clock) -> Result<ClockHandle, ClockError> {
        let sig = clock.sig();
        assert_eq!(
            sig.sim_gen(),
            self.kernel.sim_gen,
            "add_clock: the signal belongs to another simulation"
        );
        let flat = state::flat_slot(&self.kernel.starts, sig.model(), sig.local_slot())
            .or_invariant("invariant: a handle of this simulation names one of its slots");
        let slot = flat as usize;
        let name = &self.kernel.names[slot];
        let mut clocks = self.kernel.clocks.borrow_mut();
        if clocks.iter().any(|c| c.slot == flat) {
            return Err(ClockError::AlreadyClocked {
                signal: name.to_string(),
            });
        }
        let state = ClockState::new(&clock, self.kernel.widths[slot], name, SimTime::ZERO)?;
        clocks.push(ClockEntry {
            config: clock,
            slot: flat,
            running: Some(state),
        });
        Ok(ClockHandle::new(self.kernel.sim_gen, clocks.len() - 1))
    }

    /// Fails the run with [`FailureKind::TimeLimit`](crate::FailureKind::TimeLimit) instead
    /// of advancing past `limit`; a guard against tests that would otherwise run forever.
    pub fn set_time_limit(&mut self, limit: SimTime) {
        self.time_limit = Some(limit);
    }

    /// Runs `main` as the task `main` until it returns, then cancels every task left.
    ///
    /// The run stops at the first failure; tasks are still cancelled (their `Drop` cleanup
    /// runs) before it returns.
    ///
    /// # Errors
    /// The [`Failure`] that stopped the run.
    ///
    /// # Panics
    /// If called twice on one `Sim`, or from inside another `Sim::run` on this thread.
    #[expect(
        clippy::result_large_err,
        reason = "a run returns once, and Failure stays plain data that tests match on"
    )]
    pub fn run<T: 'static>(
        &mut self,
        main: impl Future<Output = T> + 'static,
    ) -> Result<T, Failure> {
        assert!(!self.ran, "Sim::run can be called only once per Sim");
        self.ran = true;
        let k = Rc::clone(&self.kernel);
        let guard = state::install(Rc::clone(&k));
        let capture = panics::capture();
        let mut main = tasks::spawn_in(&k, None, "main", main);
        k.main.set(main.key());
        sched::run_loop(&k, self.time_limit);
        let result = main.take_result(&k);
        k.phase.set(Phase::Ended);
        drop(main);
        tasks::drop_all(&k);
        drop(capture);
        drop(guard);
        match (k.failure.borrow_mut().take(), result) {
            (Some(failure), _) => Err(failure),
            (None, Some(value)) => Ok(value),
            (None, None) => unreachable!("invariant: a run ends with a result or a failure"),
        }
    }

    /// The current simulation time (the end time, after [`run`](Self::run)).
    #[must_use]
    pub fn now(&self) -> SimTime {
        self.kernel.now.get()
    }

    /// Hash of every kernel event so far; equal for two runs of the same deterministic
    /// test.
    #[must_use]
    pub fn trace_hash(&self) -> u64 {
        self.kernel.recorder.borrow().hash()
    }

    /// The most recent kernel events, oldest first (a bounded window).
    #[must_use]
    pub fn recent_trace(&self) -> Vec<TraceRecord> {
        self.kernel.recorder.borrow().recent()
    }

    /// Kernel counters and table sizes; after a run, the tables must be empty.
    #[must_use]
    pub fn stats(&self) -> KernelStats {
        let mut stats = self.kernel.counters.get();
        stats.live_tasks = self.kernel.tasks.borrow().len();
        let waits = self.kernel.waits.borrow();
        stats.live_waiters = waits.live_waiters();
        stats.live_wait_lists = waits.live_lists();
        stats.records = self.kernel.recorder.borrow().count();
        stats
    }
}

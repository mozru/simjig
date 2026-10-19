//! Synchronous, cycle-based stepping: [`SyncClock`], the clock of a synchronous test
//! (`#[simjig::test(sync)]`, [`Sim::run_sync`](crate::Sim::run_sync)).
//!
//! A synchronous test is a plain function that drives the model the way a hand-written
//! Verilator loop does: set inputs, step the clock, read outputs. Each half cycle is one
//! eval: [`SyncClock::rise`] sets the clock to 1, advances time by half a period and evaluates
//! the model; [`SyncClock::fall`] does the same with 0; [`SyncClock::tick`] is both, so a cycle is
//! exactly two evals.
//!
//! Semantics (README, "Synchronous testbenches"):
//! - A write (`set`) goes into the model at once. The model never runs while the testbench
//!   does, so a value is never torn; the next eval takes it, together with that eval's
//!   clock edge, as a C++ loop's `top->in = x; top->clk = 1; top->eval();` does.
//! - A read (`get`) returns what the model holds: after `tick()`, the values settled by the
//!   falling edge's eval.
//! - There are no tasks, triggers, regions or delta cycles: `spawn`, a kernel clock and any
//!   awaited trigger panic, naming the mode.
//! - A model failure (`$fatal`, `$stop`, `$finish`, an `$error`) or the time limit fails
//!   the run and unwinds out of the test body with a private payload
//!   ([`Stopped`]) that [`Sim::run_sync`](crate::Sim::run_sync) turns back into the failure.
//! - No trace hash is kept, and no kernel events are recorded.

use std::fmt;
use std::panic;
use std::rc::Rc;

use super::sched::eval_failed;
use super::signal::RawSig;
use super::state::{self, Kernel, Phase};
use super::time::{SimDuration, SimTime};
use crate::FailureKind;

/// The payload a synchronous test body unwinds with once the run failed.
pub(crate) struct Stopped;

/// The clock of a synchronous test: each [`rise`](Self::rise) or [`fall`](Self::fall) is
/// one half period of simulation time and one eval. See the module docs.
pub struct SyncClock {
    kernel: Rc<Kernel>,
    clk: u32,
    half: SimDuration,
}

impl fmt::Debug for SyncClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncClock")
            .field("clk", &self.kernel.names[self.clk as usize])
            .field("half_period", &self.half)
            .finish_non_exhaustive()
    }
}

impl SyncClock {
    /// A clock on the 1-bit signal `clk` with period `period` (an even number of ticks,
    /// at least 2). It sets `clk` to 0, which the first eval takes.
    ///
    /// # Panics
    /// Outside a synchronous test, if `clk` is not 1 bit wide or belongs to another
    /// simulation, or if `period` is not an even number of ticks of at least 2.
    #[track_caller]
    pub fn new(clk: impl Into<RawSig>, period: SimDuration) -> Self {
        let clk = clk.into();
        let Some(kernel) = state::current_kernel() else {
            panic!("SyncClock needs a running synchronous test (`#[simjig::test(sync)]`)")
        };
        assert!(
            kernel.phase.get() == Phase::Sync,
            "SyncClock needs a synchronous test (`#[simjig::test(sync)]`); an async test steps \
             its clocks with `clock(..)` and triggers"
        );
        let ticks = period.ticks();
        assert!(
            ticks >= 2 && ticks.is_multiple_of(2),
            "the clock period is {ticks} ticks; it must be an even number of at least 2, so \
             each half period is a whole number of ticks"
        );
        let flat = clk.with(|k, flat| {
            let width = k.widths[flat as usize];
            assert!(
                width == 1,
                "`{}` is {width} bits wide; a clock is 1 bit",
                k.names[flat as usize]
            );
            flat
        });
        kernel.models.borrow_mut().write(flat, &[0]);
        Self {
            kernel,
            clk: flat,
            half: SimDuration::from_ticks(ticks / 2),
        }
    }

    /// `handle` bound to this run: a [`Bound`](crate::Bound) (or, for a writable handle, a
    /// [`BoundMut`](crate::BoundMut)) that reads and writes the model's storage directly,
    /// for a testbench's hot loop. The handle's own `get` and `set` do the same with a
    /// lookup of the current kernel and the handle's checks at each call.
    ///
    /// The direct way is taken only for a model held for the run (a lone model, without
    /// waves or DPI-C imports implemented in Rust, `ModelBackend::sync_begin`) and a slot
    /// of at most 64 bits; otherwise the bound handle uses the handle's path.
    ///
    /// # Panics
    /// If `handle` belongs to another simulation or its type's width is not the signal's.
    #[track_caller]
    pub fn bind<H: crate::signal::Bind>(&self, handle: H) -> H::Bound {
        let direct = self.kernel.models.borrow().held();
        handle.bind_to(crate::signal::sealed::BindTo {
            kernel: Rc::clone(&self.kernel),
            direct,
        })
    }

    /// One cycle: [`rise`](Self::rise), then [`fall`](Self::fall). Two evals.
    #[inline]
    pub fn tick(&mut self) {
        self.rise();
        self.fall();
    }

    /// `n` cycles.
    pub fn ticks(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// Half a period: sets the clock to 1 and evaluates the model.
    #[inline]
    pub fn rise(&mut self) {
        self.edge(1);
    }

    /// Half a period: sets the clock to 0 and evaluates the model.
    #[inline]
    pub fn fall(&mut self) {
        self.edge(0);
    }

    /// The current simulation time.
    pub fn now(&self) -> SimTime {
        self.kernel.now.get()
    }

    fn edge(&mut self, level: u32) {
        let k = &*self.kernel;
        assert!(
            k.phase.get() == Phase::Sync,
            "SyncClock used after its synchronous test ended"
        );
        if k.failed() {
            stop();
        }
        let t = k
            .now
            .get()
            .checked_add(self.half)
            .unwrap_or_else(|| panic!("simulation time overflowed 64 bits of ticks"));
        if let Some(limit) = k.time_limit.get()
            && t > limit
        {
            time_limit(k, t, limit);
        }
        k.now.set(t);
        if !k.direct.write(self.clk, &[level]) {
            k.models.borrow_mut().write(self.clk, &[level]);
        }
        let status = k.models.borrow_mut().eval_at(t);
        k.bump(|c| {
            c.steps += 1;
            c.evals += 1;
        });
        if let Err((model, status)) = status {
            eval_failed(k, model, status);
            stop();
        }
    }
}

/// Evaluates the model at the start of a synchronous run (t = 0), as an async run's first
/// timestep does; returns whether the run may go on.
pub(crate) fn first_eval(k: &Kernel) -> bool {
    let status = k.models.borrow_mut().eval_at(k.now.get());
    k.bump(|c| {
        c.steps += 1;
        c.evals += 1;
    });
    match status {
        Ok(()) => true,
        Err((model, status)) => {
            eval_failed(k, model, status);
            false
        }
    }
}

#[cold]
fn time_limit(k: &Kernel, next: SimTime, limit: SimTime) -> ! {
    let message = format!(
        "the next timestep, t={}, is past the time limit",
        next.display(k.precision)
    );
    k.fail(
        FailureKind::TimeLimit {
            limit,
            blocked: Vec::new(),
        },
        None,
        None,
        message,
    );
    stop();
}

/// Unwinds out of the test body; `resume_unwind` skips the panic hook, so nothing is
/// printed for it.
#[cold]
fn stop() -> ! {
    panic::resume_unwind(Box::new(Stopped));
}

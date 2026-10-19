//! The scheduler loop: one iteration per distinct timestamp.
//!
//! The order reproduces what cocotb 2.1 shows a testbench on Verilator 5.052 (its
//! `verilator.cpp` main loop with trusted inertial writes), pinned by the conformance
//! goldens in `tests/conformance/` and overriding DECISION §1 where they differ:
//!
//! 1. **Timed region.** Set the model's time. Wake `NextTimeStep` waiters and run tasks;
//!    then log the kernel clocks' edges due now (declaration order, as writes by the
//!    clock), and wake the `Timer`s due now one at a time, in registration order, running
//!    tasks after each. Timer tasks therefore see the state *before* this timestep's clock
//!    edges, and a trigger they arm still catches those edges.
//! 2. **Delta loop.** Repeat: `eval`; apply the write log (deposits); walk the primed
//!    value-change groups oldest first and fire each one whose edge happened, running tasks
//!    after each (see `waits`, "Value-change groups"); run tasks. Stop after an iteration
//!    that applied no write.
//!    Applying a clock edge and firing its waiters *before* the eval that clocks the flops
//!    is why, right after `rising(clk)` on a kernel clock, flops still show old values;
//!    design-driven signals change in an eval, so their waiters see post-eval values.
//! 3. **`ReadWrite` region.** Wake `ReadWrite` waiters, run tasks, apply the log, compare and
//!    wake; if anything was applied, go back to 2.
//!    Before leaving the deltas, a model whose next event is *now* (a `#0` still pending
//!    after its eval) is evaluated again, as another delta of the same timestep.
//! 4. **`ReadOnly` region.** Wake `ReadOnly` waiters and run tasks; any write fails the run.
//! 5. **Advance** to the earliest of: the timer wheel, the next clock edge, and every
//!    model's `next_time_slot()` among those with `events_pending()`. If there is none and
//!    the main task is still blocked, the run fails as `Starved` or `Deadlock`.
//!
//! Two limits make a timestep that never settles fail loudly:
//! - more than [`DELTA_MAX`] evals between two `ReadWrite` rounds (an oscillation, or a
//!   model `#0` loop) is a `DeltaLimit` failure naming the signals still being written;
//! - more than [`READ_WRITE_MAX`] `ReadWrite` rounds that woke a task is a `ReadWriteLimit`
//!   failure naming those tasks and where they await `ReadWrite`.
//!
//! # `--timing` safety (Verilator 5.052, docs/notes/spikes.md §4)
//! Verilator exits the process if `nextTimeSlot()` is called while `eventsPending()` is
//! false, or if the time is set past a pending slot. So `next_time_slot` is asked only of
//! models with events pending, the next timestep is never later than any model's next
//! slot (it is the minimum), and the kernel asserts that time strictly increases between
//! timesteps (Verilator does not check). A model reporting a slot before now is a
//! `ModelFatal` failure.

use std::panic::Location;

use super::backend::{EvalStatus, ModelError, ModelErrorKind};
use super::clock::ClockEntry;
use super::failure::{BlockedTask, FailureKind};
use super::recorder::TraceKind;
use super::signal::{self, Writer};
use super::slab::Key;
use super::state::{Kernel, Phase};
use super::tasks::run_ready;
use super::time::SimTime;
use super::waits::{WaitOn, WaitTable, fire_and_wake};

/// Evals allowed between two `ReadWrite` rounds of one timestep (or before the first)
/// before the run fails with `DeltaLimit`.
pub const DELTA_MAX: u32 = 64;

/// `ReadWrite` rounds that wake a task allowed in one timestep before the run fails with
/// `ReadWriteLimit`.
///
/// cocotb on Verilator has no such limit: its main loop returns to the deltas after every
/// `ReadWrite` callback that wrote, so a task that writes and awaits `ReadWrite` forever
/// hangs it. Each round is a deliberate step of the testbench (a handshake resolved
/// within one time, a register file filled at t=0), so a legitimate test may take many,
/// unlike delta oscillation; 10 000 rounds is far past any such test and still fails a
/// runaway loop within milliseconds.
pub const READ_WRITE_MAX: u32 = 10_000;

fn stopped(k: &Kernel) -> bool {
    k.failed() || k.main_done.get()
}

/// Fires the oldest Timer due at `t`; returns whether there was one.
fn fire_next_timer(k: &Kernel, t: SimTime) -> bool {
    // Most timesteps have no Timer due: no need to lend out the wake buffer.
    if k.waits.borrow().next_timer() != Some(t) {
        return false;
    }
    fire_and_wake(k, |w, out| w.fire_next_timer(t, out))
}

/// Runs timesteps until the main task completes, a failure is recorded, or time cannot
/// advance.
pub(crate) fn run_loop(k: &Kernel, limit: Option<SimTime>) {
    loop {
        step(k);
        if stopped(k) {
            return;
        }
        let Some(next) = next_time(k) else {
            if !k.failed() {
                report_blocked(k);
            }
            return;
        };
        if let Some(limit) = limit
            && next > limit
        {
            let message = format!(
                "the next timestep, t={}, is past the time limit",
                next.display(k.precision)
            );
            let blocked = blocked_tasks(k);
            k.fail(
                FailureKind::TimeLimit { limit, blocked },
                None,
                None,
                message,
            );
            return;
        }
        // Verilator does not check that time moves forward; the kernel must.
        assert!(
            next > k.now.get(),
            "invariant: time moves forward (from t={} to t={})",
            k.now.get().ticks(),
            next.ticks()
        );
        k.now.set(next);
    }
}

/// Whether a model has an event due at the current time (a pending `#0`).
fn model_event_now(k: &Kernel) -> bool {
    let next = k.models.borrow().next_event();
    next.is_some_and(|(_, t)| t == k.now.get())
}

fn step(k: &Kernel) {
    let t = k.now.get();
    k.models.borrow_mut().set_time(t);
    k.record_step();
    k.bump(|c| c.steps += 1);

    if k.waits.borrow().region_waited(WaitOn::NextTimeStep) {
        fire_and_wake(k, WaitTable::fire_next_step);
    }
    // Not only for NextTimeStep waiters: the main task's first poll is this one.
    run_ready(k);
    // The clock edges due at `t` are logged before any Timer task runs, so a Timer task
    // that stops a clock or writes its signal at `t` comes after that edge in the log.
    log_clock_edges(k, t);
    // One Timer at a time, each followed by every task it makes ready (cocotb runs its
    // event loop after each GPI callback). So a task woken by an older Timer, and any task
    // it wakes in turn, runs before the waiter of a younger Timer due at the same time:
    // `with_timeout` relies on it to break ties as cocotb does.
    while !stopped(k) && fire_next_timer(k, t) {
        run_ready(k);
    }
    run_ready(k);

    let (mut evals, mut rounds) = (0, 0);
    loop {
        loop {
            if stopped(k) || !eval(k, &mut evals) {
                return;
            }
            let applied = apply_and_compare(k);
            run_ready(k);
            if !applied {
                break;
            }
        }
        if stopped(k) {
            return;
        }
        let waited = k.waits.borrow().region_waited(WaitOn::ReadWrite);
        if waited {
            if rounds == READ_WRITE_MAX {
                let waiters = k.waits.borrow().read_write_waiters();
                read_write_limit(k, &waiters);
                return;
            }
            rounds += 1;
            evals = 0;
        }
        // A round with no ReadWrite waiter and nothing logged would wake nobody, run nothing
        // (every ready task has run), apply nothing and walk nothing (no model changed
        // since the walk after the last eval): skip straight to its end.
        let quiet = !waited && k.log.borrow().is_empty();
        let mut applied = false;
        if !quiet {
            fire_and_wake(k, WaitTable::fire_read_write);
            run_ready(k);
            if stopped(k) {
                return;
            }
            applied = apply_and_compare(k);
            run_ready(k);
        }
        if !applied && !model_event_now(k) {
            break;
        }
    }
    if stopped(k) {
        return;
    }
    k.phase.set(Phase::ReadOnly);
    k.record_read_only();
    // With no ReadOnly waiter there is nothing to run: the deltas ended with every ready
    // task run.
    if k.waits.borrow().region_waited(WaitOn::ReadOnly) {
        fire_and_wake(k, WaitTable::fire_read_only);
        run_ready(k);
    }
    if k.phase.get() == Phase::ReadOnly {
        k.phase.set(Phase::Active);
    }
}

/// Logs the edge of every clock due at `t`, in declaration order.
///
/// One clock at a time, with no borrow of the clocks held across the write; a write never
/// touches a clock, so this logs what collecting every edge first would.
fn log_clock_edges(k: &Kernel, t: SimTime) {
    let clocks = k.clocks.borrow().len();
    for i in 0..clocks {
        let edge = {
            let mut clocks = k.clocks.borrow_mut();
            let c = &mut clocks[i];
            c.running
                .as_mut()
                .filter(|r| r.next_time == t)
                .map(|r| (c.slot, r.take_edge()))
        };
        if let Some((slot, level)) = edge {
            signal::write(k, slot, &[level], Writer::Clock, None);
        }
    }
}

/// Evaluates the model once; returns `false` if the run must stop.
fn eval(k: &Kernel, evals: &mut u32) -> bool {
    if *evals == DELTA_MAX {
        let signals = k.log.borrow().pending_slots();
        let names: Vec<String> = signals
            .iter()
            .map(|&s| k.names[s as usize].to_string())
            .collect();
        let message = if names.is_empty() {
            format!(
                "the timestep did not settle in {DELTA_MAX} evals: the model keeps scheduling \
                 events at this time"
            )
        } else {
            format!(
                "the timestep did not settle in {DELTA_MAX} evals; still being written: {}",
                names.join(", ")
            )
        };
        k.fail(
            FailureKind::DeltaLimit {
                limit: DELTA_MAX,
                signals: names,
            },
            None,
            None,
            message,
        );
        return false;
    }
    let status = k.models.borrow_mut().eval(k.now.get());
    k.record_eval(*evals);
    k.bump(|c| c.evals += 1);
    *evals += 1;
    let Err((model, status)) = status else {
        return true;
    };
    eval_failed(k, model, status);
    false
}

/// Fails the run for model `model`'s eval `status` (anything but `Running`).
pub(crate) fn eval_failed(k: &Kernel, model: usize, status: Result<EvalStatus, ModelError>) {
    // With one model, messages stay as they were; with several, they name the model.
    let which = if k.models.borrow().len() > 1 {
        format!("model {model}: ")
    } else {
        String::new()
    };
    match status {
        Ok(EvalStatus::Running) => unreachable!("invariant: Models::eval stops on non-Running"),
        Ok(EvalStatus::Finished) => k.fail(
            FailureKind::DesignFinished,
            None,
            None,
            format!("{which}the design executed $finish before the main task returned"),
        ),
        Err(e) => {
            let message = format!("{which}{}", e.message);
            let kind = match e.kind {
                ModelErrorKind::Fatal => FailureKind::ModelFatal {
                    message: message.clone(),
                },
                ModelErrorKind::DpiPanic => FailureKind::DpiPanic {
                    message: message.clone(),
                },
                ModelErrorKind::Setup { help } => FailureKind::Setup {
                    message: message.clone(),
                    help,
                },
            };
            k.fail(kind, None, e.location, message);
        }
    }
}

/// Fails the run as `ReadWriteLimit`, naming the `ReadWrite` waiters (the first one is the
/// failure's task and location).
#[cold]
fn read_write_limit(k: &Kernel, waiters: &[(Option<Key>, Option<&'static Location<'static>>)]) {
    let blocked: Vec<BlockedTask> = waiters
        .iter()
        .map(|&(task, at)| BlockedTask {
            task: task.map_or_else(|| "<no task>".to_owned(), |t| k.task_name(t)),
            awaiting: vec![match at {
                Some(at) => format!("ReadWrite at {at}"),
                None => "ReadWrite".to_owned(),
            }],
        })
        .collect();
    let (task, at) = waiters[0];
    let message = format!(
        "{READ_WRITE_MAX} ReadWrite rounds in one timestep, and {} task(s) await ReadWrite again",
        blocked.len()
    );
    k.fail(
        FailureKind::ReadWriteLimit {
            limit: READ_WRITE_MAX,
            waiters: blocked,
        },
        task,
        at.map(Into::into),
        message,
    );
}

/// Applies the write log, then fires the value changes since the previous comparison.
/// Returns whether any write was applied.
fn apply_and_compare(k: &Kernel) -> bool {
    let applied = apply(k);
    fire_value_changes(k);
    applied
}

/// Applies the write log in write order, recording each write, and starts a new delta;
/// returns whether there was a write. No task runs and no write can be logged meanwhile.
fn apply(k: &Kernel) -> bool {
    let mut log = k.log.borrow_mut();
    // An empty log has nothing to apply, and no slot holds its delta's epoch, so not
    // starting a new delta changes nothing.
    if log.is_empty() {
        return false;
    }
    let mut model = k.models.borrow_mut();
    for (slot, value) in log.iter() {
        model.write(slot, value);
        let low = value
            .iter()
            .take(2)
            .rev()
            .fold(0u64, |a, &w| a << 32 | u64::from(w));
        k.record(TraceKind::Write, None, u64::from(slot), low);
    }
    log.clear();
    true
}

/// Fires the value-change groups whose edge happened, one group at a time in priming
/// order, each followed by every task it makes ready: cocotb on Verilator runs one VPI
/// value-change callback at a time (`callValueCbs`, in registration order) and its whole
/// event loop inside each. A task woken by one group that waits on a younger group not yet
/// reached in this walk joins it; groups primed during the walk wait for the next change.
///
/// A walk when no model changed since the previous walk began is skipped: every group
/// primed before that walk took the value then, every group primed since took it when
/// primed, and values change only in an eval or when the log is applied, never during a
/// walk; so no group could differ from its snapshot. So is a walk after evals alone when
/// every primed group watches a slot no eval changes (an input): only a write changes it.
fn fire_value_changes(k: &Kernel) {
    let now = {
        let models = k.models.borrow();
        (models.version(), models.written())
    };
    let last = k.walked.replace(Some(now));
    if last == Some(now)
        || last.is_some_and(|(_, written)| written == now.1)
            && !k.waits.borrow().watches_eval_changes()
    {
        return;
    }
    let until = {
        let mut waits = k.waits.borrow_mut();
        waits.begin_walk();
        waits.last_seq()
    };
    let mut at = 0;
    while !stopped(k) {
        let fired = fire_and_wake(k, |w, out| {
            let model = k.models.borrow();
            let mut ready = k.ready.borrow_mut();
            w.fire_next_change(&model, until, &mut at, &mut ready, out)
        });
        if !fired {
            break;
        }
        run_ready(k);
    }
    k.waits.borrow_mut().end_walk();
}

/// The next timestep, or `None` if nothing is scheduled. A model that reports a next
/// event at or before now is a failure (and also yields `None`).
fn next_time(k: &Kernel) -> Option<SimTime> {
    let now = k.now.get();
    let mut next = k.waits.borrow().next_timer();
    for edge in k.clocks.borrow().iter().filter_map(ClockEntry::next_edge) {
        next = Some(next.map_or(edge, |n| n.min(edge)));
    }
    let model_next = k.models.borrow().next_event();
    if let Some((model, m)) = model_next {
        if m < now {
            let which = if k.models.borrow().len() > 1 {
                format!("model {model}")
            } else {
                "the model".to_owned()
            };
            let message = format!(
                "{which} reported its next event at t={}, before now; it missed a time slot",
                m.display(k.precision)
            );
            k.fail(
                FailureKind::ModelFatal {
                    message: message.clone(),
                },
                None,
                None,
                message,
            );
            return None;
        }
        next = Some(next.map_or(m, |n| n.min(m)));
    }
    next
}

/// Fails the run as `Deadlock` (every blocked task waits on another task or a sync
/// primitive) or `Starved` (something waits on signals or time).
fn report_blocked(k: &Kernel) {
    let blocked = blocked_tasks(k);
    let deadlock = {
        let waits = k.waits.borrow();
        let unfinished = k.tasks.borrow().unfinished();
        unfinished.iter().all(|(key, _)| waits.only_lists(*key))
    };
    let n = blocked.len();
    if deadlock {
        let message = format!("{n} task(s) wait on each other and nothing else can wake them");
        k.fail(FailureKind::Deadlock { blocked }, None, None, message);
    } else {
        let message =
            format!("{n} task(s) are blocked and no timer, clock or model event is scheduled");
        k.fail(FailureKind::Starved { blocked }, None, None, message);
    }
}

/// Every unfinished task and what it waits on, in task-slot order.
fn blocked_tasks(k: &Kernel) -> Vec<BlockedTask> {
    let unfinished = k.tasks.borrow().unfinished();
    let waits = k.waits.borrow();
    unfinished
        .iter()
        .map(|(key, name)| BlockedTask {
            task: name.to_string(),
            awaiting: waits.describe(*key, &k.names, k.precision),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::OrInvariant;
    use super::super::backend::{ModelBackend, ModelError};
    use super::super::failure::FailureKind;
    use super::super::sim::Sim;
    use super::super::time::{Precision, SimDuration};
    use super::super::trigger::Timer;
    use super::*;

    /// A model with no signals whose first eval reports a panicking DPI-C import.
    struct DpiPanics;

    impl ModelBackend for DpiPanics {
        fn precision(&self) -> Precision {
            Precision::NS
        }
        fn slot_count(&self) -> u32 {
            0
        }
        fn slot_name(&self, _slot: u32) -> &str {
            unreachable!("no slots")
        }
        fn slot_width(&self, _slot: u32) -> u32 {
            unreachable!("no slots")
        }
        fn read(&self, _slot: u32, _out: &mut [u32]) {}
        fn write(&mut self, _slot: u32, _value: &[u32]) {}
        fn set_time(&mut self, _time: SimTime) {}
        fn eval(&mut self) -> Result<EvalStatus, ModelError> {
            Err(ModelError::dpi_panic(
                "DPI-C import `dpi_add` panicked: seven".into(),
                None,
            ))
        }
        fn events_pending(&self) -> bool {
            false
        }
        fn next_time_slot(&self) -> SimTime {
            SimTime::ZERO
        }
    }

    /// A one-bit model (`clk`) that settles, counting the evals it really runs.
    struct Settles {
        clk: u32,
        evals: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl ModelBackend for Settles {
        fn precision(&self) -> Precision {
            Precision::NS
        }
        fn slot_count(&self) -> u32 {
            1
        }
        fn slot_name(&self, _slot: u32) -> &'static str {
            "clk"
        }
        fn slot_width(&self, _slot: u32) -> u32 {
            1
        }
        fn read(&self, _slot: u32, out: &mut [u32]) {
            out[0] = self.clk;
        }
        fn write(&mut self, _slot: u32, value: &[u32]) {
            self.clk = value[0];
        }
        fn set_time(&mut self, _time: SimTime) {}
        fn eval(&mut self) -> Result<EvalStatus, ModelError> {
            self.evals.set(self.evals.get() + 1);
            Ok(EvalStatus::Running)
        }
        fn settles(&self) -> bool {
            true
        }
        fn events_pending(&self) -> bool {
            false
        }
        fn next_time_slot(&self) -> SimTime {
            SimTime::ZERO
        }
    }

    /// The eval that opens a timestep and the one that ends its delta loop find nothing
    /// written since the previous eval: a model that settles is not evaluated then, but
    /// the kernel still counts (and records) every eval.
    #[test]
    fn a_settled_model_is_evaluated_only_after_a_write() {
        let evals = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sim = Sim::from_backend(Box::new(Settles {
            clk: 0,
            evals: std::rc::Rc::clone(&evals),
        }));
        let clk = sim.signal("clk").or_invariant("the slot exists");
        sim.add_clock(super::super::clock::Clock::new(
            clk,
            SimDuration::from_ticks(10),
        ))
        .or_invariant("a clock");
        // Edges at 0, 5, .., 95, and the Timer's own timestep at 99.
        let edges = 20;
        sim.run(Timer::new(SimDuration::from_ticks(99)))
            .or_invariant("the run passes");
        let stats = sim.stats();
        assert_eq!(stats.steps, edges + 1);
        // Counted: per edge, the eval that opens the timestep and the one after the edge
        // is applied; none at t=99, where the Timer ends the run before the deltas.
        assert_eq!(stats.evals, 2 * edges);
        // Run: the one after each edge, and the run's very first eval (t=0).
        assert_eq!(u64::from(evals.get()), edges + 1);
    }

    #[test]
    fn a_dpi_panic_is_its_own_failure_kind() {
        let mut sim = Sim::from_backend(Box::new(DpiPanics));
        let f = sim.run(Timer::new(SimDuration::from_ticks(1))).unwrap_err();
        assert_eq!(
            f.kind,
            FailureKind::DpiPanic {
                message: "DPI-C import `dpi_add` panicked: seven".into()
            }
        );
        assert!(f.to_string().starts_with("simjig DpiPanic at t=0"), "{f}");
    }
}

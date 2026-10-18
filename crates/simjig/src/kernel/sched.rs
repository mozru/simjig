//! The scheduler loop: one iteration per distinct timestamp.
//!
//! The order reproduces what cocotb 2.1 shows a testbench on Verilator 5.052 (its
//! `verilator.cpp` main loop with trusted inertial writes), pinned by the conformance
//! goldens in `tests/conformance/` and overriding DECISION §1 where they differ:
//!
//! 1. **Timed region.** Set the model's time. Wake `NextTimeStep` waiters and run tasks;
//!    then wake `Timer`s due now, log the kernel clocks' edges due now (declaration order,
//!    as writes by the clock), and run tasks. Timer tasks therefore see the state *before*
//!    this timestep's clock edges, and a trigger they arm still catches those edges.
//! 2. **Delta loop.** Repeat: `eval`; apply the write log (deposits); compare watched
//!    slots against their values at the previous comparison and wake their waiters in
//!    registration order; run tasks. Stop after an iteration that applied no write.
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

use super::backend::{EvalStatus, ModelErrorKind};
use super::clock::ClockEntry;
use super::failure::{BlockedTask, FailureKind};
use super::recorder::TraceKind;
use super::signal::{self, Writer};
use super::slab::Key;
use super::state::{Kernel, Phase};
use super::tasks::run_ready;
use super::time::SimTime;
use super::waits::{WaitTable, Wake, wake_sorted};

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

fn wake_from(k: &Kernel, fire: impl FnOnce(&mut WaitTable, &mut Vec<Wake>)) {
    let mut wakes = Vec::new();
    fire(&mut k.waits.borrow_mut(), &mut wakes);
    wake_sorted(wakes);
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
    k.record(TraceKind::Step, None, 0, 0);
    k.bump(|c| c.steps += 1);

    wake_from(k, WaitTable::fire_next_step);
    run_ready(k);
    wake_from(k, |w, out| w.fire_time(t, out));
    let edges: Vec<(u32, u32)> = k
        .clocks
        .borrow_mut()
        .iter_mut()
        .filter_map(|c| {
            let schedule = c.running.as_mut().filter(|r| r.next_time == t)?;
            Some((c.slot, schedule.take_edge()))
        })
        .collect();
    for (slot, level) in edges {
        signal::write(k, slot, &[level], Writer::Clock, None);
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
        let waiters = k.waits.borrow().read_write_waiters();
        if !waiters.is_empty() {
            if rounds == READ_WRITE_MAX {
                read_write_limit(k, &waiters);
                return;
            }
            rounds += 1;
            evals = 0;
        }
        wake_from(k, WaitTable::fire_read_write);
        run_ready(k);
        if stopped(k) {
            return;
        }
        let applied = apply_and_compare(k);
        run_ready(k);
        if !applied && !model_event_now(k) {
            break;
        }
    }
    if stopped(k) {
        return;
    }
    k.phase.set(Phase::ReadOnly);
    k.record(TraceKind::ReadOnly, None, 0, 0);
    wake_from(k, WaitTable::fire_read_only);
    run_ready(k);
    if k.phase.get() == Phase::ReadOnly {
        k.phase.set(Phase::Active);
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
    let status = k.models.borrow_mut().eval();
    k.record(TraceKind::Eval, None, u64::from(*evals), 0);
    k.bump(|c| c.evals += 1);
    *evals += 1;
    let Err((model, status)) = status else {
        return true;
    };
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
    false
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

/// Applies the write log, then wakes waiters of watched slots that changed since the
/// previous comparison. Returns whether any write was applied.
fn apply_and_compare(k: &Kernel) -> bool {
    let writes = k.log.borrow_mut().take();
    {
        let mut model = k.models.borrow_mut();
        for (slot, value) in writes.iter() {
            model.write(slot, value);
        }
    }
    for (slot, value) in writes.iter() {
        let low = value
            .iter()
            .take(2)
            .rev()
            .fold(0u64, |a, &w| a << 32 | u64::from(w));
        k.record(TraceKind::Write, None, u64::from(slot), low);
    }
    let applied = !writes.is_empty();
    k.log.borrow_mut().recycle(writes);
    let mut wakes = Vec::new();
    {
        let model = k.models.borrow();
        k.waits.borrow_mut().compare(&model, &mut wakes);
    }
    wake_sorted(wakes);
    applied
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

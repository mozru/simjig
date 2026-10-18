//! FST waves from a Verilator model: [`TraceOptions`] (what to record) and `Tracer` (when
//! the backend calls the model's `trace_*` entries).
//!
//! # When a sample is written
//! One sample per timestep, at the end of that timestep, stamped with its simulation time
//! in ticks of the design's `timeprecision` (the FST header's timescale is that precision,
//! because the model's context has it). The kernel tells a backend nothing about timestep
//! ends, but it does call `set_time` exactly once at the start of each timestep, before any
//! write of that timestep is applied (`kernel::sched`). So when `set_time(t')` arrives, the
//! model still holds the final state of the previous timestep `t`, which is dumped at `t`.
//! The last timestep is dumped when the trace closes. A timestep without an eval (none
//! exists today) dumps nothing.
//!
//! # Failures
//! - An eval that fails dumps the state it reached and flushes, so the waves up to the
//!   failure are on disk even if the process dies before the model is dropped.
//! - After `$fatal` the model is poisoned and the C side refuses every trace call; the trace
//!   then ends at the last completed timestep, and `destroy` closes the file (it closes an
//!   open trace before it frees the model).
//! - A trace call that fails stops the trace (with one printed line), never the simulation.
//!
//! # Fork
//! An open trace registers a [`ForkHook`]: in each `fork::run_from_checkpoint` child it
//! detaches the inherited file and continues the waves in `<path stem>.branch-<i>.fst`
//! next to it (`trace_reopen`; docs/notes/tracecov.md), so the parent's file is never
//! written by a child, and it ends that file (last sample, header) before the child exits.
//! The parent does not flush before forking (see `fork_hook`).
//!
//! # Window
//! Samples before [`TraceOptions::start`] are skipped by the C side; its first real sample
//! is a full dump, so the file is complete from there. After [`TraceOptions::end`] the trace
//! is closed at the first later timestep.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use super::abi::{FEATURE_TRACE_FST, Status};
use super::meta::LoadError;
use super::raw::{RawModel, TracePort};
use crate::fork::{self, ForkHook, HookGuard};
use crate::kernel::time::SimTime;

/// What FST waves a Verilator model records, given to it when it is built
/// ([`ModelOptions::with_trace`](crate::ModelOptions::with_trace)).
///
/// The model must be verilated with FST support (`.trace_fst(true)` on its
/// `simjig_build::Dut`); otherwise building it fails with [`LoadError::NoTrace`], unless the
/// options are [`if_supported`](Self::if_supported).
///
/// ```
/// use simjig::{ModelOptions, Seed, SeedSource, SimTime, TraceOptions};
///
/// let waves = TraceOptions::new("waves.fst").depth(2).start(SimTime(5_000));
/// let options = ModelOptions::new(Seed::new(1, SeedSource::Env)).with_trace(waves);
/// assert_eq!(options.trace.as_ref().map(TraceOptions::depth_limit), Some(2));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceOptions {
    path: PathBuf,
    depth: u32,
    start: SimTime,
    end: Option<SimTime>,
    required: bool,
}

impl TraceOptions {
    /// Records every signal of the design, for the whole run, into the FST file `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            depth: 0,
            start: SimTime::ZERO,
            end: None,
            required: true,
        }
    }

    /// Records only `levels` scope levels from the top (`$dumpvars` semantics: 1 is the top
    /// module's own signals); 0 records everything.
    #[must_use]
    pub fn depth(mut self, levels: u32) -> Self {
        self.depth = levels;
        self
    }

    /// Writes no sample before `time`; the first sample at or after it is complete.
    #[must_use]
    pub fn start(mut self, time: SimTime) -> Self {
        self.start = time;
        self
    }

    /// Writes no sample after `time`, and closes the file at the first timestep past it.
    #[must_use]
    pub fn end(mut self, time: SimTime) -> Self {
        self.end = Some(time);
        self
    }

    /// Records only if the model supports FST; a model without it runs untraced instead of
    /// failing to build (the harness's replay uses this, and checks whether a file appeared).
    #[must_use]
    pub fn if_supported(mut self) -> Self {
        self.required = false;
        self
    }

    /// The FST file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The scope depth limit (0: none).
    #[must_use]
    pub fn depth_limit(&self) -> u32 {
        self.depth
    }

    /// The first time sampled.
    #[must_use]
    pub fn start_time(&self) -> SimTime {
        self.start
    }

    /// The last time sampled, if limited.
    #[must_use]
    pub fn end_time(&self) -> Option<SimTime> {
        self.end
    }
}

/// An open trace on one model; owned by the Verilator backend. See the module docs.
///
/// The state is shared with the trace's fork hook, which flushes it before a fork and, in a
/// branch child, moves it to the branch's file and closes it before the child exits.
#[derive(Debug)]
pub(crate) struct Tracer {
    state: Rc<RefCell<TraceState>>,
    /// Keeps the fork hook registered while the trace lives.
    _fork: HookGuard,
}

#[derive(Debug)]
struct TraceState {
    port: TracePort,
    path: PathBuf,
    end: Option<SimTime>,
    /// The timestep evaluated since the last sample, whose state is still to be dumped.
    pending: Option<SimTime>,
    open: bool,
}

impl Tracer {
    /// Opens the trace `options` asks for on `raw` (model `prefix`); `None` if the model has
    /// no FST support and the options allow that.
    pub(crate) fn open(
        raw: &mut RawModel,
        prefix: &'static str,
        options: &TraceOptions,
    ) -> Result<Option<Self>, LoadError> {
        if raw.features() & FEATURE_TRACE_FST == 0 {
            return if options.required {
                Err(LoadError::NoTrace { prefix })
            } else {
                Ok(None)
            };
        }
        let failed = |reason: String| LoadError::TraceOpen {
            prefix,
            path: options.path.clone(),
            reason,
        };
        let path = CString::new(options.path.as_os_str().as_encoded_bytes())
            .map_err(|_nul| failed("the path contains a NUL byte".to_owned()))?;
        let status = raw.trace_open(&path, options.depth, options.start.ticks());
        match Status::try_from(status) {
            Ok(Status::Ok) => {
                let state = Rc::new(RefCell::new(TraceState {
                    port: raw.trace_port(),
                    path: options.path.clone(),
                    end: options.end,
                    pending: None,
                    open: true,
                }));
                let hook = fork_hook(Rc::downgrade(&state));
                Ok(Some(Self {
                    state,
                    _fork: fork::register_hook(hook),
                }))
            }
            Ok(Status::IoError) => Err(failed("the file cannot be created".to_owned())),
            other => Err(failed(format!("the model returned {other:?}"))),
        }
    }

    /// The model was evaluated at `now`; its state there is dumped when the timestep ends.
    pub(crate) fn evaluated(&self, now: SimTime) {
        self.state.borrow_mut().pending = Some(now);
    }

    /// A timestep ended (the next one starts, or the model is torn down): dumps its state.
    pub(crate) fn end_timestep(&self) {
        self.state.borrow_mut().end_timestep();
    }

    /// An eval failed: dumps what the model reached and flushes it to disk.
    pub(crate) fn on_failure(&self) {
        let mut state = self.state.borrow_mut();
        state.end_timestep();
        state.flush();
    }

    /// Closes the file (its header is written now); later calls do nothing.
    pub(crate) fn close(&self) {
        self.state.borrow_mut().close();
    }

    /// The model was poisoned: no trace call may reach it again (`destroy` closes the
    /// file).
    pub(crate) fn stop(&self) {
        self.state.borrow_mut().open = false;
    }
}

impl TraceState {
    fn end_timestep(&mut self) {
        let Some(t) = self.pending.take().filter(|_| self.open) else {
            return;
        };
        if self.end.is_some_and(|end| t > end) {
            self.close();
            return;
        }
        let status = self.port.dump(t.ticks());
        self.check(status, "dump");
    }

    fn flush(&mut self) {
        if self.open {
            let status = self.port.flush();
            self.check(status, "flush");
        }
    }

    fn close(&mut self) {
        if std::mem::replace(&mut self.open, false) {
            let status = self.port.close();
            self.check(status, "close");
        }
    }

    /// Stops the trace after a failed call; a poisoned model fails every call silently,
    /// because its failure is already being reported. A destroyed model (`None`) has
    /// already closed its trace.
    fn check(&mut self, status: Option<i32>, what: &str) {
        let Some(status) = status else {
            self.open = false;
            return;
        };
        match Status::try_from(status) {
            Ok(Status::Ok) => {}
            Ok(Status::Poisoned) => self.open = false,
            other => {
                self.open = false;
                print_line(&format!(
                    "simjig: the trace {what} returned {other:?}; the waves stop here"
                ));
            }
        }
    }

    /// In a fork child: continues the waves in the branch's own file.
    fn reopen(&mut self, index: usize) -> Result<(), String> {
        if !self.open {
            return Ok(());
        }
        let file = branch_path(&self.path, index);
        let c_path = CString::new(file.as_os_str().as_encoded_bytes())
            .map_err(|_nul| format!("{} contains a NUL byte", file.display()))?;
        match self.port.reopen(&c_path).map(Status::try_from) {
            None | Some(Ok(Status::Ok)) => {
                self.path = file;
                Ok(())
            }
            Some(other) => {
                self.open = false;
                Err(format!(
                    "continuing the trace in {} returned {other:?}",
                    file.display()
                ))
            }
        }
    }
}

/// The trace file of fork branch `index` for a trace written to `path`:
/// `<dir>/<stem>.branch-<index>.fst`.
pub(crate) fn branch_path(path: &Path, index: usize) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.branch-{index}.fst"))
}

/// The hook that keeps a fork child's waves out of the parent's file (see the module
/// docs). It holds the state weakly: a trace dropped with its model leaves nothing to do.
fn fork_hook(state: Weak<RefCell<TraceState>>) -> ForkHook {
    let child = Weak::clone(&state);
    let exiting = state;
    ForkHook::new(
        // Nothing to flush: the child's copy of the writer's buffer is discarded when
        // `trace_reopen` detaches the inherited file, and the parent keeps its own. A flush
        // would only split the value-change block, and fstcpp then repeats the current
        // timestamp at the boundary (observed: one timestep sampled twice).
        || Ok(()),
        move |branch| {
            child
                .upgrade()
                .map_or(Ok(()), |s| s.borrow_mut().reopen(branch.index()))
        },
    )
    // A child exits without dropping the model, so the trace must end here: the last
    // timestep is dumped and the header written.
    .at_child_exit(move || {
        if let Some(state) = exiting.upgrade() {
            let mut state = state.borrow_mut();
            state.end_timestep();
            state.close();
        }
    })
}

#[expect(
    clippy::print_stdout,
    reason = "model output belongs in the test's captured output, like the test's own prints"
)]
fn print_line(text: &str) {
    println!("{text}");
}

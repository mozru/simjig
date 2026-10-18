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
//! # Window
//! Samples before [`TraceOptions::start`] are skipped by the C side; its first real sample
//! is a full dump, so the file is complete from there. After [`TraceOptions::end`] the trace
//! is closed at the first later timestep.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use super::abi::{FEATURE_TRACE_FST, Status};
use super::meta::LoadError;
use super::raw::RawModel;
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
#[derive(Debug)]
pub(crate) struct Tracer {
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
            Ok(Status::Ok) => Ok(Some(Self {
                end: options.end,
                pending: None,
                open: true,
            })),
            Ok(Status::IoError) => Err(failed("the file cannot be created".to_owned())),
            other => Err(failed(format!("the model returned {other:?}"))),
        }
    }

    /// The model was evaluated at `now`; its state there is dumped when the timestep ends.
    pub(crate) fn evaluated(&mut self, now: SimTime) {
        self.pending = Some(now);
    }

    /// A timestep ended (the next one starts, or the model is torn down): dumps its state.
    pub(crate) fn end_timestep(&mut self, raw: &mut RawModel) {
        let Some(t) = self.pending.take().filter(|_| self.open) else {
            return;
        };
        if self.end.is_some_and(|end| t > end) {
            self.close(raw);
            return;
        }
        let status = raw.trace_dump(t.ticks());
        self.check(status, "dump");
    }

    /// An eval failed: dumps what the model reached and flushes it to disk.
    pub(crate) fn on_failure(&mut self, raw: &mut RawModel) {
        self.end_timestep(raw);
        if self.open {
            let status = raw.trace_flush();
            self.check(status, "flush");
        }
    }

    /// Closes the file (its header is written now); later calls do nothing.
    pub(crate) fn close(&mut self, raw: &mut RawModel) {
        if std::mem::replace(&mut self.open, false) {
            let status = raw.trace_close();
            self.check(status, "close");
        }
    }

    /// Stops the trace after a failed call; a poisoned model fails every call silently,
    /// because its failure is already being reported.
    fn check(&mut self, status: i32, what: &str) {
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
}

#[expect(
    clippy::print_stdout,
    reason = "model output belongs in the test's captured output, like the test's own prints"
)]
fn print_line(text: &str) {
    println!("{text}");
}

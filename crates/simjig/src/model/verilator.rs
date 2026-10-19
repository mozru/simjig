//! [`VerilatorModel`]: the kernel's [`ModelBackend`] over a generated Verilator model.
//!
//! What it adds on top of [`RawModel`]:
//! - **Load checks** ([`VerilatorModel::load`]): the vtable's ABI version and size, its prefix
//!   and IR hash against the generated Rust, and its signal table against the generated
//!   [`SignalMeta`] list (name, width, access), slot by slot. A mismatch means the model and
//!   the Rust were built from different sources; nothing is evaluated.
//! - **Messages.** After every eval (and create, and `final`) the message ring is drained in
//!   order. `$display`/`$write`/`$warning`/`$info` text and Verilator warnings are printed with
//!   `print!`, so they land in the test's captured output next to its own prints. `$fatal`,
//!   `$error`, `$stop` and C++ exceptions become a [`ModelError`] naming the HDL file, line and
//!   scope; the kernel turns it into a `ModelFatal` failure.
//! - **Poisoning.** After FATAL or EXCEPTION (the C++ side unwound out of model code), no
//!   further eval reaches C++: every later eval returns the original error.
//! - **Waves.** With [`TraceOptions`], the model's FST trace is opened at load, sampled at the
//!   end of every timestep and closed on drop (see `model::trace`).
//! - **Teardown.** `Drop` dumps the last timestep, runs `final` blocks (unless poisoned),
//!   prints their output, closes the trace, then destroys the model.
//! - **DPI.** For a model with DPI-C imports implemented in Rust
//!   ([`ModelMeta::dpi_imports`]), every eval and `final` runs inside [`ModelDpi::scope`],
//!   so those imports reach this instance's Rust implementation; models without any skip
//!   the scope's cost. A DPI failure (a panic, no
//!   implementation) poisons the model like a FATAL (see `crate::dpi`).
//!
//! `$finish` is reported as [`EvalStatus::Finished`]; `$error` stops the run at Verilator's
//! default error limit of 1, as in cocotb on Verilator.

use std::ffi::CString;
use std::path::Path;

use super::abi::{
    ACCESS_WRITE, Dir, FEATURE_COVERAGE, FEATURE_RANDOM_INIT, FEATURE_TIMING, FEATURE_TRACE_FST,
    MSGS_PENDING, MsgKind, OwnedMsg, Status,
};
use super::meta::{Access, LoadError, ModelMeta, Storage};
use super::raw::{CreateError, RawModel, check_vtable};
use super::trace::{TraceOptions, Tracer};
use crate::dpi::{DpiFailure, ModelDpi};
use crate::kernel::backend::{CoverageFault, EvalStatus, ModelBackend, ModelError};
use crate::kernel::cell::Direct;
use crate::kernel::time::{Precision, SimTime};

/// A generated Verilator model behind the kernel's backend interface.
#[derive(Debug)]
pub(crate) struct VerilatorModel {
    raw: RawModel,
    precision: Precision,
    /// The error that poisoned the model; set after FATAL or EXCEPTION.
    poisoned: Option<String>,
    /// Scratch buffer for drained messages, reused across evals.
    msgs: Vec<OwnedMsg>,
    /// The instance's DPI-C implementation context; `None` for a model without DPI-C
    /// imports implemented in Rust.
    dpi: Option<ModelDpi>,
    /// The open FST trace, if any.
    tracer: Option<Tracer>,
    /// The time of the current timestep (the last `set_time`).
    now: SimTime,
    /// Held for a synchronous run (`sync_begin`): evals go through `eval_held`.
    held: bool,
}

impl VerilatorModel {
    /// Creates the model described by `meta`, passing `args` (plusargs such as
    /// `+verilator+seed+N`) to its context and, with `random_init`, drawing its initial
    /// values from that seed; checks it against the generated Rust, and opens the trace
    /// `trace` asks for.
    pub(crate) fn load(
        meta: &ModelMeta,
        args: &[String],
        random_init: Option<u32>,
        trace: Option<&TraceOptions>,
    ) -> Result<Self, LoadError> {
        // The version and size are checked before anything else is read from the table.
        let vt = check_vtable((meta.vtable)(), meta.ir_hash)
            .map_err(|e| LoadError::from_abi(meta.prefix, &e))?;
        let prefix = vt.prefix();
        if prefix != meta.prefix {
            return Err(LoadError::Prefix {
                expected: meta.prefix,
                found: prefix,
            });
        }
        let precision =
            Precision::from_exponent(meta.precision_exponent).map_err(|_unsupported| {
                LoadError::Precision {
                    prefix: meta.prefix,
                    exponent: meta.precision_exponent,
                }
            })?;
        if random_init.is_some() && vt.features & FEATURE_RANDOM_INIT == 0 {
            return Err(LoadError::NoRandomInit {
                prefix: meta.prefix,
            });
        }
        let (raw, status) = RawModel::create(vt, args, random_init).map_err(|e| match e {
            CreateError::NoHandle(status) => LoadError::Create {
                prefix: meta.prefix,
                reason: format!("no model handle (status {status})"),
            },
            CreateError::BadArg(arg) => LoadError::Create {
                prefix: meta.prefix,
                reason: format!("argument {arg:?} contains a NUL byte"),
            },
            CreateError::BadRow { slot, problem } => LoadError::SignalTable {
                prefix: meta.prefix,
                slot,
                problem,
            },
        })?;
        let mut model = Self {
            raw,
            precision,
            poisoned: None,
            msgs: Vec::new(),
            dpi: (!meta.dpi_imports.is_empty())
                .then(|| ModelDpi::new(meta.prefix, meta.dut_name())),
            tracer: None,
            now: SimTime::ZERO,
            held: false,
        };
        if let Err(e) = model.outcome(status) {
            // A model that failed to construct runs no final blocks when dropped.
            model.poisoned = Some(e.message.clone());
            return Err(LoadError::Create {
                prefix: meta.prefix,
                reason: e.message,
            });
        }
        model.check_table(meta)?;
        model.raw.take_coverage_runs();
        if let Some(options) = trace {
            model.tracer = Tracer::open(&mut model.raw, meta.prefix, options)?;
        }
        Ok(model)
    }

    /// Compares the C++ signal table with the generated metadata, slot by slot.
    fn check_table(&self, meta: &ModelMeta) -> Result<(), LoadError> {
        let rows = self.raw.signals();
        let names = self.raw.names();
        let mismatch = |slot, problem| LoadError::SignalTable {
            prefix: meta.prefix,
            slot,
            problem,
        };
        if rows.len() != meta.signals.len() {
            let problem = format!(
                "the model has {} signals and the generated Rust {}",
                rows.len(),
                meta.signals.len()
            );
            return Err(mismatch(rows.len().min(meta.signals.len()), problem));
        }
        for (slot, (row, want)) in rows.iter().zip(meta.signals).enumerate() {
            let name = &names[slot];
            if want.slot as usize != slot {
                let problem = format!("the Rust lists `{}` with slot {}", want.name, want.slot);
                return Err(mismatch(slot, problem));
            }
            if name != want.name {
                let problem = format!("the model has `{name}`, the Rust `{}`", want.name);
                return Err(mismatch(slot, problem));
            }
            if row.width != want.width {
                let problem = format!(
                    "`{name}` is {} bits in the model and {} in the Rust",
                    row.width, want.width
                );
                return Err(mismatch(slot, problem));
            }
            if want.storage != Storage::packed(row.width) {
                let problem = format!(
                    "`{name}` has storage {:?} in the Rust, but simjig's C ABI describes only \
                     packed signals ({:?} for {} bits)",
                    want.storage,
                    Storage::packed(row.width),
                    row.width
                );
                return Err(mismatch(slot, problem));
            }
            if !access_matches(row.dir, row.access, want.access) {
                let problem = format!(
                    "`{name}` has direction {} and access bits {:#x} in the model, but is {:?} \
                     in the Rust",
                    row.dir as u8, row.access, want.access
                );
                return Err(mismatch(slot, problem));
            }
        }
        Ok(())
    }

    /// Drains the message ring and maps a raw status to the backend's result, poisoning the
    /// model on FATAL and EXCEPTION.
    fn outcome(&mut self, raw_status: i32) -> Result<EvalStatus, ModelError> {
        self.msgs.clear();
        self.raw.drain(&mut self.msgs);
        let mut failure = None;
        for m in &self.msgs {
            match m.kind {
                Ok(MsgKind::Display) => print_display(&m.msg),
                Ok(MsgKind::Warning) => print_line(&format!("%Warning: {}", located(m))),
                Ok(MsgKind::Dropped) => print_line(&format!(
                    "simjig: the model's message ring overflowed; {} messages were lost",
                    m.line
                )),
                Ok(MsgKind::Finish) => {}
                Ok(MsgKind::Error | MsgKind::Stop | MsgKind::Fatal | MsgKind::Exception)
                | Err(_) => {
                    failure.get_or_insert_with(|| describe(m));
                }
            }
        }
        let status = Status::try_from(raw_status);
        let poison = matches!(
            status,
            Ok(Status::Fatal | Status::Exception | Status::Poisoned) | Err(_)
        );
        let result = match (status, failure) {
            (Ok(Status::Ok), None) => Ok(EvalStatus::Running),
            (Ok(Status::Finish), None) => Ok(EvalStatus::Finished),
            (_, Some(message)) => Err(message),
            (Ok(Status::Stop), None) => Err("the design executed $stop".to_owned()),
            (Ok(Status::Fatal | Status::Exception | Status::Poisoned), None) => Err(format!(
                "the model failed (status {raw_status}) without a message"
            )),
            (Ok(Status::Unsupported), None) => Err(
                "the model reported UNSUPPORTED from an entry point it must implement".to_owned(),
            ),
            (Ok(Status::IoError), None) => {
                Err("the model reported IO_ERROR from an entry point that opens no file".to_owned())
            }
            (Err(unknown), None) => Err(format!("the model returned {unknown:?}")),
        };
        result.map_err(|message| {
            if poison {
                self.poisoned = Some(message.clone());
                // The C side now refuses every trace call; `destroy` closes the file.
                if let Some(tracer) = &self.tracer {
                    tracer.poisoned();
                }
            }
            ModelError::fatal(message)
        })
    }
}

impl ModelBackend for VerilatorModel {
    fn precision(&self) -> Precision {
        self.precision
    }

    fn slot_count(&self) -> u32 {
        u32::try_from(self.raw.signals().len()).unwrap_or(u32::MAX)
    }

    fn slot_name(&self, slot: u32) -> &str {
        &self.raw.names()[slot as usize]
    }

    fn slot_width(&self, slot: u32) -> u32 {
        self.raw.signals()[slot as usize].width
    }

    fn read(&self, slot: u32, out: &mut [u32]) {
        self.raw.read(slot as usize, out);
    }

    fn write(&mut self, slot: u32, value: &[u32]) {
        self.raw.write(slot as usize, value);
    }

    // Held only with nothing to do around an eval: no waves to sample, no DPI-C context to
    // install, a healthy model. Otherwise every eval keeps the full path.
    fn sync_begin(&mut self) {
        if self.tracer.is_none() && self.dpi.is_none() && self.poisoned.is_none() {
            self.held = self.raw.hold() == Status::Ok as i32;
        }
    }

    fn sync_end(&mut self) {
        if std::mem::take(&mut self.held) {
            self.raw.release();
        }
    }

    #[inline]
    fn eval_at(&mut self, time: SimTime) -> Result<EvalStatus, ModelError> {
        if !self.held {
            self.set_time(time);
            return self.eval();
        }
        self.now = time;
        let status = self.raw.eval_held(time.ticks());
        if status == Status::Ok as i32 {
            return Ok(EvalStatus::Running);
        }
        // Messages to print, or a failure: the same handling as `eval`'s.
        self.outcome(status & !MSGS_PENDING)
    }

    fn eval_changes(&self, slot: u32) -> bool {
        !self.raw.is_input(slot as usize)
    }

    fn direct(&self, slot: u32) -> Option<Direct> {
        self.raw.direct(slot as usize)
    }

    fn set_time(&mut self, time: SimTime) {
        // The kernel sets the time once per timestep, before applying any of its writes, so
        // the model still holds the previous timestep's final state (`model::trace`).
        if time != self.now
            && self.poisoned.is_none()
            && let Some(tracer) = &self.tracer
        {
            tracer.end_timestep();
        }
        self.now = time;
        self.raw.set_time(time.ticks());
    }

    fn eval(&mut self) -> Result<EvalStatus, ModelError> {
        if let Some(reason) = &self.poisoned {
            return Err(ModelError::fatal(format!(
                "the model can no longer be evaluated: {reason}"
            )));
        }
        let (status, dpi_failure) = dpi_scope(self.dpi.as_mut(), || self.raw.eval());
        let result = self.outcome(status);
        // Before a DPI failure poisons the model on the Rust side: the C++ model is still
        // healthy, so the waves keep the timestep that failed.
        if self.poisoned.is_none()
            && let Some(tracer) = &self.tracer
        {
            tracer.evaluated(self.now);
            if result.is_err() || dpi_failure.is_some() {
                tracer.on_failure();
            }
        }
        match dpi_failure {
            // The DPI failure happened first: the design went on with a wrong value.
            Some(failure) => {
                self.poisoned = Some(failure.message.clone());
                // Never sampled again; the healthy C++ model still closes the file.
                if let Some(tracer) = &self.tracer {
                    tracer.stop();
                }
                Err(failure.into_model_error())
            }
            None => result,
        }
    }

    // Without `--timing`, an eval runs only what a deposit since the last one triggers,
    // and Verilator's eval loop returns settled; with waves on, every eval is sampled.
    fn settles(&self) -> bool {
        self.raw.features() & FEATURE_TIMING == 0
            && self.tracer.is_none()
            && self.poisoned.is_none()
    }

    fn events_pending(&self) -> bool {
        // Only a `--timing` model has events of its own; skip the call into C++.
        self.raw.features() & FEATURE_TIMING != 0
            && self.poisoned.is_none()
            && self.raw.events_pending()
    }

    fn next_time_slot(&self) -> SimTime {
        SimTime(self.raw.next_time_slot())
    }

    // Also after poisoning: the counters stay valid until destroy, and reading them calls
    // no model code.
    fn coverage(&self) -> Option<Vec<u32>> {
        self.raw.coverage()
    }

    fn can_trace(&self) -> bool {
        self.raw.features() & FEATURE_TRACE_FST != 0
    }

    fn zero_coverage(&mut self) -> Result<(), CoverageFault> {
        self.coverage_call(RawModel::cov_zero)
    }

    fn write_coverage(&mut self, path: &Path) -> Result<(), CoverageFault> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_nul| CoverageFault::Write("the path contains a NUL byte".to_owned()))?;
        self.coverage_call(|raw| raw.cov_write(&path))
    }
}

impl VerilatorModel {
    /// Runs one `cov_*` entry point, unless the model has no coverage or is poisoned (no
    /// call reaches a poisoned model), and maps its status.
    fn coverage_call(
        &mut self,
        call: impl FnOnce(&mut RawModel) -> i32,
    ) -> Result<(), CoverageFault> {
        if self.raw.features() & FEATURE_COVERAGE == 0 {
            return Err(CoverageFault::NotBuilt);
        }
        if let Some(reason) = &self.poisoned {
            return Err(CoverageFault::Failed(format!(
                "it was poisoned by an earlier failure ({reason}); read its counters with \
                 `coverage` instead"
            )));
        }
        let status = call(&mut self.raw);
        // A coverage call runs no design code, but drains like any entry point.
        self.msgs.clear();
        self.raw.drain(&mut self.msgs);
        match Status::try_from(status) {
            Ok(Status::Ok) => Ok(()),
            Ok(Status::IoError) => Err(CoverageFault::Write(
                "the file cannot be created (does its directory exist?)".to_owned(),
            )),
            other => Err(CoverageFault::Failed(format!(
                "the model returned {other:?}"
            ))),
        }
    }
}

/// Runs `f` inside `dpi`'s scope, or plainly for a model without Rust DPI imports.
fn dpi_scope<R>(dpi: Option<&mut ModelDpi>, f: impl FnOnce() -> R) -> (R, Option<DpiFailure>) {
    match dpi {
        Some(dpi) => dpi.scope(f),
        None => (f(), None),
    }
}

impl Drop for VerilatorModel {
    fn drop(&mut self) {
        self.sync_end();
        if self.poisoned.is_none() {
            if let Some(tracer) = &self.tracer {
                tracer.end_timestep();
            }
            let (status, dpi_failure) = dpi_scope(self.dpi.as_mut(), || self.raw.final_blocks());
            // A failure in a final block has no run left to fail; its message is printed.
            if let Err(e) = self.outcome(status) {
                print_line(&format!("simjig: in final blocks: {}", e.message));
            }
            if let Some(failure) = dpi_failure {
                print_line(&format!("simjig: in final blocks: {}", failure.message));
            }
        }
        // After a DPI poisoning the C++ model is healthy and closes the trace here; after a
        // FATAL the tracer makes no call and `destroy` closes it.
        if let Some(tracer) = &self.tracer {
            tracer.close();
        }
    }
}

/// Whether a C++ row's direction and access bits implement the generated access kind.
fn access_matches(dir: Dir, access: u8, want: Access) -> bool {
    let writable = access & ACCESS_WRITE != 0;
    match want {
        Access::In => dir == Dir::In && writable,
        Access::Out => dir == Dir::Out,
        Access::InOut => dir == Dir::Inout && writable,
        Access::PublicRd => dir == Dir::Internal,
        Access::PublicRw => dir == Dir::Internal && writable,
    }
}

/// `file:line: msg`, or just `msg` when the location is unknown.
fn located(m: &OwnedMsg) -> String {
    if m.file.is_empty() {
        m.msg.clone()
    } else {
        format!("{}:{}: {}", m.file, m.line, m.msg)
    }
}

/// The failure text for an error record: what, where (file, line, scope), and the message.
fn describe(m: &OwnedMsg) -> String {
    let what = match m.kind {
        Ok(MsgKind::Fatal) => "$fatal",
        Ok(MsgKind::Error) => "$error",
        Ok(MsgKind::Stop) => "$stop",
        Ok(MsgKind::Exception) => "a C++ exception",
        _ => "an unknown model message",
    };
    let (scope, text) = match split_assertion(&m.msg) {
        Some((scope, text)) => (scope, text),
        None => (m.hier.as_str(), m.msg.as_str()),
    };
    let scope = if scope.is_empty() {
        String::new()
    } else {
        format!(" in {scope}")
    };
    let place = if m.file.is_empty() {
        String::new()
    } else {
        format!(" at {}:{}", m.file, m.line)
    };
    format!("{what}{scope}{place}: {text}")
}

/// Splits the text Verilator 5.052 prints for `$fatal` and `$error`,
/// `[<time>] %Fatal: <file>:<line>: Assertion failed in <scope>: <message>`, into the scope
/// and the user's message. The hook that records the error receives no scope (it is only in
/// this text), and the time, file and line are already known. `None` for any other shape,
/// which is then reported verbatim.
fn split_assertion(msg: &str) -> Option<(&str, &str)> {
    let rest = msg.strip_prefix('[')?;
    let (_time, rest) = rest.split_once("] %")?;
    let (_severity, rest) = rest.split_once(": ")?;
    let (_location, rest) = rest.split_once(": ")?;
    let rest = rest.strip_prefix("Assertion failed in ")?;
    rest.split_once(": ")
}

/// Prints model output where the test harness captures it.
#[expect(
    clippy::print_stdout,
    reason = "model output belongs in the test's captured output, like the test's own prints"
)]
fn print_display(text: &str) {
    print!("{text}");
}

#[expect(
    clippy::print_stdout,
    reason = "model output belongs in the test's captured output, like the test's own prints"
)]
fn print_line(text: &str) {
    println!("{text}");
}

#[cfg(test)]
mod tests {
    use super::super::abi::{INIT_RANDOM, INIT_ZERO, ModelVtable, Status};
    use super::super::meta::{SignalMeta, Vtable};
    use super::super::raw::fake::{self, STATE};
    use super::*;

    fn fake_vtable() -> Vtable {
        thread_local! {
            static VT: Vtable = Vtable::leak(fake::vtable());
        }
        VT.with(|vt| *vt)
    }

    static FAKE: ModelMeta = ModelMeta {
        vtable: fake_vtable,
        prefix: "Vfake",
        module: "tests::fake",
        ir_hash: 7,
        precision_exponent: -9,
        signals: &[
            SignalMeta {
                name: "fake.a",
                slot: 0,
                width: 4,
                storage: Storage::CData,
                access: Access::In,
            },
            SignalMeta {
                name: "fake.y",
                slot: 1,
                width: 8,
                storage: Storage::CData,
                access: Access::Out,
            },
        ],
        dpi_imports: &[],
    };

    fn counts() -> (u32, u32, u32) {
        STATE.with_borrow(|s| (s.evals, s.finals, s.destroys))
    }

    #[test]
    fn a_fatal_poisons_the_model() {
        fake::reset();
        let mut model = VerilatorModel::load(&FAKE, &[], None, None).unwrap();
        STATE.with_borrow_mut(|s| {
            s.statuses.push_back(Status::Fatal);
            s.pending.push_back((MsgKind::Fatal as u32, c"boom"));
        });
        let first = model.eval().unwrap_err().message;
        assert_eq!(first, "$fatal in TOP.fake at fake.sv:7: boom");
        let second = model.eval().unwrap_err().message;
        assert_eq!(
            second,
            format!("the model can no longer be evaluated: {first}")
        );
        assert!(!model.events_pending());
        assert_eq!(counts(), (1, 0, 0), "the second eval must not reach C++");
        drop(model);
        assert_eq!(
            counts(),
            (1, 0, 1),
            "no final blocks after a fatal; destroyed once"
        );
    }

    #[test]
    fn a_clean_model_runs_final_blocks_then_is_destroyed() {
        fake::reset();
        let mut model = VerilatorModel::load(&FAKE, &[], None, None).unwrap();
        assert_eq!(model.eval(), Ok(EvalStatus::Running));
        drop(model);
        assert_eq!(counts(), (1, 1, 1));
    }

    #[test]
    fn only_a_model_with_rust_dpi_imports_gets_a_dpi_scope() {
        static WITH_DPI: ModelMeta = ModelMeta {
            dpi_imports: &["f"],
            ..FAKE
        };
        fake::reset();
        let plain = VerilatorModel::load(&FAKE, &[], None, None).unwrap();
        assert!(plain.dpi.is_none());
        drop(plain);
        fake::reset();
        let with_dpi = VerilatorModel::load(&WITH_DPI, &[], None, None).unwrap();
        assert!(with_dpi.dpi.is_some());
    }

    #[test]
    fn an_error_record_fails_the_eval_without_poisoning() {
        fake::reset();
        let mut model = VerilatorModel::load(&FAKE, &[], None, None).unwrap();
        STATE.with_borrow_mut(|s| s.pending.push_back((MsgKind::Error as u32, c"bad")));
        assert!(model.eval().is_err());
        assert_eq!(model.eval(), Ok(EvalStatus::Running));
    }

    /// A table from an older header is rejected on its size before any other field (here
    /// the prefix, which names another model) is read.
    #[test]
    fn a_shorter_vtable_is_rejected_before_its_prefix_is_read() {
        fn short() -> Vtable {
            thread_local! {
                static VT: Vtable = Vtable::leak(ModelVtable {
                    size: 136,
                    prefix: c"Vother".as_ptr(),
                    ..fake::vtable()
                });
            }
            VT.with(|vt| *vt)
        }
        static SHORT: ModelMeta = ModelMeta {
            vtable: short,
            ..FAKE
        };
        fake::reset();
        let err = VerilatorModel::load(&SHORT, &[], None, None).unwrap_err();
        assert_eq!(
            err,
            LoadError::VtableSize {
                prefix: "Vfake",
                model: 136,
                runtime: 216
            }
        );
        assert_eq!(counts(), (0, 0, 0), "nothing was created");
    }

    #[test]
    fn a_failure_while_creating_fails_the_load() {
        fake::reset();
        STATE.with_borrow_mut(|s| s.pending.push_back((MsgKind::Fatal as u32, c"init")));
        let err = VerilatorModel::load(&FAKE, &[], None, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed while being created: $fatal"),
            "{err}"
        );
        assert_eq!(
            counts(),
            (0, 0, 1),
            "a failed load still destroys the handle"
        );
    }

    #[test]
    fn writes_are_masked_and_outputs_are_read_only() {
        fake::reset();
        let mut model = VerilatorModel::load(&FAKE, &[], None, None).unwrap();
        model.write(0, &[0xff]);
        assert_eq!(STATE.with_borrow(|s| s.members[0]), 0x0f);
        let mut out = [0];
        model.read(0, &mut out);
        assert_eq!(out, [0x0f]);
        let write = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            model.write(1, &[1]);
        }));
        assert!(write.is_err(), "writing an output must panic");
        assert_eq!(STATE.with_borrow(|s| s.members[1]), 0);
    }

    fn traced_vtable() -> Vtable {
        thread_local! {
            static VT: Vtable = Vtable::leak(fake::traced_vtable());
        }
        VT.with(|vt| *vt)
    }

    static TRACED: ModelMeta = ModelMeta {
        vtable: traced_vtable,
        ..FAKE
    };

    fn calls() -> Vec<String> {
        STATE.with_borrow_mut(|s| std::mem::take(&mut s.calls))
    }

    fn traced(options: &TraceOptions) -> VerilatorModel {
        fake::reset();
        let model = VerilatorModel::load(&TRACED, &[], None, Some(options)).unwrap();
        let open = calls();
        assert_eq!(open.len(), 1, "{open:?}");
        assert!(open[0].starts_with("open w.fst "), "{open:?}");
        model
    }

    /// Runs timesteps at `times`, two evals each.
    fn steps(model: &mut VerilatorModel, times: &[u64]) {
        for &t in times {
            model.set_time(SimTime(t));
            assert_eq!(model.eval(), Ok(EvalStatus::Running));
            assert_eq!(model.eval(), Ok(EvalStatus::Running));
        }
    }

    #[test]
    fn each_timestep_is_dumped_once_at_its_end_with_its_time() {
        let mut model = traced(&TraceOptions::new("w.fst").depth(3).start(SimTime(7)));
        steps(&mut model, &[0, 10, 25]);
        drop(model);
        assert_eq!(
            calls(),
            [
                "eval", "eval", "dump 0", "eval", "eval", "dump 10", "eval", "eval", "dump 25",
                "final", "close", "destroy"
            ]
        );
    }

    #[test]
    fn the_trace_is_opened_with_its_depth_and_start() {
        fake::reset();
        let options = TraceOptions::new("w.fst").depth(3).start(SimTime(7));
        let _model = VerilatorModel::load(&TRACED, &[], None, Some(&options)).unwrap();
        assert_eq!(calls(), ["open w.fst depth=3 start=7"]);
    }

    #[test]
    fn a_failing_eval_dumps_and_flushes_at_once() {
        let mut model = traced(&TraceOptions::new("w.fst"));
        steps(&mut model, &[0]);
        model.set_time(SimTime(5));
        STATE.with_borrow_mut(|s| s.pending.push_back((MsgKind::Error as u32, c"bad")));
        assert!(model.eval().is_err());
        assert_eq!(
            calls(),
            ["eval", "eval", "dump 0", "eval", "dump 5", "flush"]
        );
        drop(model);
        assert_eq!(calls(), ["final", "close", "destroy"]);
    }

    #[test]
    fn a_poisoned_model_gets_no_trace_calls() {
        let mut model = traced(&TraceOptions::new("w.fst"));
        steps(&mut model, &[0]);
        model.set_time(SimTime(5));
        STATE.with_borrow_mut(|s| s.statuses.push_back(Status::Fatal));
        assert!(model.eval().is_err());
        model.set_time(SimTime(6));
        drop(model);
        assert_eq!(calls(), ["eval", "eval", "dump 0", "eval", "destroy"]);
    }

    /// In a fork child, a healthy trace moves to the branch's file and is closed there at
    /// exit, its last timestep dumped first.
    #[test]
    fn a_fork_child_reopens_the_trace_and_closes_it_at_exit() {
        let mut model = traced(&TraceOptions::new("w.fst"));
        steps(&mut model, &[0, 10]);
        assert_eq!(crate::fork::run_child_hooks_in_place(1), Ok(()));
        let calls_ = calls();
        assert_eq!(
            calls_[calls_.len() - 3..],
            ["reopen w.branch-1.fst", "dump 10", "close"]
        );
    }

    /// A trace that stopped before the fork (a DPI failure stops it; a FATAL leaves its
    /// closing to `destroy`) is not reopened in a child, and so still holds the inherited
    /// writer, which shares the parent's descriptor: the child must neither close it nor
    /// destroy the model, either of which writes the parent's file.
    #[test]
    fn a_fork_child_never_finishes_a_trace_it_did_not_reopen() {
        for poison in [Tracer::stop, Tracer::poisoned] {
            let mut model = traced(&TraceOptions::new("w.fst"));
            steps(&mut model, &[0]);
            poison(model.tracer.as_ref().expect("traced"));
            drop(calls());
            assert_eq!(crate::fork::run_child_hooks_in_place(0), Ok(()));
            assert_eq!(calls(), Vec::<String>::new());
            // A child never drops its models; this is the parent's own teardown.
            drop(model);
        }
    }

    /// A reopen that fails fails the branch, and leaves Rust nothing to close: whatever the
    /// C side still holds is the inherited writer.
    #[test]
    fn a_failed_reopen_leaves_no_file_to_close() {
        let mut model = traced(&TraceOptions::new("w.fst"));
        steps(&mut model, &[0]);
        drop(calls());
        STATE.with_borrow_mut(|s| s.trace_statuses.push_back(Status::IoError));
        let err = crate::fork::run_child_hooks_in_place(0).unwrap_err();
        assert!(err.contains("w.branch-0.fst"), "{err}");
        assert_eq!(calls(), ["reopen w.branch-0.fst"]);
        model.tracer.as_ref().expect("traced").close();
        assert_eq!(
            calls(),
            Vec::<String>::new(),
            "no close after a failed reopen"
        );
    }

    #[test]
    fn the_trace_closes_after_its_end_time() {
        let mut model = traced(&TraceOptions::new("w.fst").end(SimTime(10)));
        steps(&mut model, &[0, 10, 20, 30]);
        drop(model);
        let calls: Vec<String> = calls().into_iter().filter(|c| c != "eval").collect();
        assert_eq!(calls, ["dump 0", "dump 10", "close", "final", "destroy"]);
    }

    #[test]
    fn a_failed_dump_stops_the_trace_but_not_the_run() {
        let mut model = traced(&TraceOptions::new("w.fst"));
        STATE.with_borrow_mut(|s| s.trace_statuses.push_back(Status::Exception));
        steps(&mut model, &[0, 10, 20]);
        drop(model);
        let calls: Vec<String> = calls().into_iter().filter(|c| c != "eval").collect();
        assert_eq!(calls, ["dump 0", "final", "destroy"]);
    }

    #[test]
    fn a_model_without_fst_rejects_waves_unless_they_are_optional() {
        fake::reset();
        let err =
            VerilatorModel::load(&FAKE, &[], None, Some(&TraceOptions::new("w.fst"))).unwrap_err();
        assert_eq!(err, LoadError::NoTrace { prefix: "Vfake" });
        let help = err.help().expect("a setup error");
        assert!(help.contains(".trace_fst(true)"), "{help}");
        let optional = TraceOptions::new("w.fst").if_supported();
        let model = VerilatorModel::load(&FAKE, &[], None, Some(&optional)).unwrap();
        assert!(model.tracer.is_none());
    }

    #[test]
    fn random_init_reaches_create_only_on_a_model_that_supports_it() {
        fn random_vtable() -> Vtable {
            thread_local! {
                static VT: Vtable = Vtable::leak(fake::random_init_vtable());
            }
            VT.with(|vt| *vt)
        }
        static RANDOM: ModelMeta = ModelMeta {
            vtable: random_vtable,
            ..FAKE
        };
        fake::reset();
        let _zero = VerilatorModel::load(&RANDOM, &[], None, None).unwrap();
        assert_eq!(STATE.with_borrow(|s| s.init), Some((INIT_ZERO, 0)));
        let _random = VerilatorModel::load(&RANDOM, &[], Some(77), None).unwrap();
        assert_eq!(STATE.with_borrow(|s| s.init), Some((INIT_RANDOM, 77)));
        fake::reset();
        let err = VerilatorModel::load(&FAKE, &[], Some(77), None).unwrap_err();
        assert_eq!(err, LoadError::NoRandomInit { prefix: "Vfake" });
        assert!(err.help().is_some_and(|h| h.contains("XInitial::Unique")));
        assert_eq!(STATE.with_borrow(|s| s.init), None, "nothing was created");
    }

    fn record(kind: MsgKind, hier: &str, msg: &str) -> OwnedMsg {
        OwnedMsg {
            kind: Ok(kind),
            line: 31,
            time: 15_000,
            file: "fatal.sv".to_owned(),
            hier: hier.to_owned(),
            msg: msg.to_owned(),
        }
    }

    #[test]
    fn describe_names_the_scope_and_location() {
        let cases = [
            (
                record(
                    MsgKind::Fatal,
                    "",
                    "[15000] %Fatal: fatal.sv:31: Assertion failed in TOP.fatal: boom: x",
                ),
                "$fatal in TOP.fatal at fatal.sv:31: boom: x",
            ),
            (
                record(MsgKind::Stop, "", "Verilog $stop"),
                "$stop at fatal.sv:31: Verilog $stop",
            ),
            (
                record(MsgKind::Fatal, "TOP.u", "$readmem file syntax error"),
                "$fatal in TOP.u at fatal.sv:31: $readmem file syntax error",
            ),
        ];
        for (m, want) in cases {
            assert_eq!(describe(&m), want);
        }
    }
}

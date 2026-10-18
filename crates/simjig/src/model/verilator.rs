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
//! - **Teardown.** `Drop` runs `final` blocks (unless poisoned), prints their output, then
//!   destroys the model.
//!
//! `$finish` is reported as [`EvalStatus::Finished`]; `$error` stops the run at Verilator's
//! default error limit of 1, as in cocotb on Verilator.

use super::abi::{ACCESS_WRITE, Dir, MsgKind, OwnedMsg, Status};
use super::meta::{LoadError, ModelMeta, SignalAccess};
use super::raw::{CreateError, RawModel};
use crate::kernel::backend::{EvalStatus, ModelBackend, ModelError};
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
}

impl VerilatorModel {
    /// Creates the model described by `meta` and checks it against the generated Rust.
    pub(crate) fn load(meta: &ModelMeta) -> Result<Self, LoadError> {
        let vt = (meta.vtable)().as_abi();
        let prefix = vt.prefix();
        if prefix != meta.prefix {
            return Err(LoadError::Prefix {
                expected: meta.prefix,
                found: prefix,
            });
        }
        vt.check(meta.ir_hash)
            .map_err(|e| LoadError::from_abi(meta.prefix, &e))?;
        let precision =
            Precision::from_exponent(meta.precision_exponent).map_err(|_unsupported| {
                LoadError::Precision {
                    prefix: meta.prefix,
                    exponent: meta.precision_exponent,
                }
            })?;
        let (raw, status) = RawModel::create(vt).map_err(|e| match e {
            CreateError::NoHandle(status) => LoadError::Create {
                prefix: meta.prefix,
                reason: format!("no model handle (status {status})"),
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
        };
        if let Err(e) = model.outcome(status) {
            return Err(LoadError::Create {
                prefix: meta.prefix,
                reason: e.message,
            });
        }
        model.check_table(meta)?;
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
            if !access_matches(row.dir, row.access, want.access) {
                let problem = format!(
                    "`{name}` has direction {} and access bits {:#x} in the model, but is {:?} \
                     in the Rust",
                    row.dir, row.access, want.access
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
            (Err(unknown), None) => Err(format!("the model returned {unknown:?}")),
        };
        result.map_err(|message| {
            if poison {
                self.poisoned = Some(message.clone());
            }
            ModelError { message }
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

    fn set_time(&mut self, time: SimTime) {
        self.raw.set_time(time.ticks());
    }

    fn eval(&mut self) -> Result<EvalStatus, ModelError> {
        if let Some(reason) = &self.poisoned {
            return Err(ModelError {
                message: format!("the model can no longer be evaluated: {reason}"),
            });
        }
        let status = self.raw.eval();
        self.outcome(status)
    }

    fn events_pending(&self) -> bool {
        self.poisoned.is_none() && self.raw.events_pending()
    }

    fn next_time_slot(&self) -> SimTime {
        SimTime(self.raw.next_time_slot())
    }
}

impl Drop for VerilatorModel {
    fn drop(&mut self) {
        if self.poisoned.is_none() {
            let status = self.raw.final_blocks();
            // A failure in a final block has no run left to fail; its message is printed.
            if let Err(e) = self.outcome(status) {
                print_line(&format!("simjig: in final blocks: {}", e.message));
            }
        }
    }
}

/// Whether a C++ row's direction and access bits implement the generated access kind.
fn access_matches(dir: u8, access: u8, want: SignalAccess) -> bool {
    let writable = access & ACCESS_WRITE != 0;
    let Ok(dir) = Dir::try_from(dir) else {
        return false;
    };
    match want {
        SignalAccess::In => dir == Dir::In && writable,
        SignalAccess::Out => dir == Dir::Out,
        SignalAccess::InOut => dir == Dir::Inout && writable,
        SignalAccess::PublicRd => dir == Dir::Internal,
        SignalAccess::PublicRw => dir == Dir::Internal && writable,
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
    use super::*;

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

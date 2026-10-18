//! The conformance log of `tests/conformance/README.md` over typed handles, so a run against
//! a real model can be diffed byte for byte with cocotb's golden. Mirrors
//! `tests/kernel_tests/conflog.rs`; the signal label is the name without the top module
//! (`flop.q` logs as `q`), as cocotb names `dut.q`.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::future::Future;
use std::rc::Rc;

use simjig::{In, Out, PublicRd, now};
use simjig_bits::{Bit, Bits};

/// A handle whose value the log can record.
pub(crate) trait Sample {
    fn label(&self) -> String;
    fn sample(&self) -> u128;
}

/// A handle the log can write.
pub(crate) trait Drive: Sample {
    fn drive(&self, value: u128);
}

fn short(name: &str) -> String {
    name.split_once('.')
        .map_or(name, |(_, rest)| rest)
        .to_owned()
}

macro_rules! sample {
    ($($handle:ident<$ty:ty> => |$v:ident| $to:expr;)*) => {$(
        impl Sample for $handle<$ty> {
            fn label(&self) -> String {
                short(&self.name())
            }
            fn sample(&self) -> u128 {
                let $v = self.get();
                $to
            }
        }
    )*};
}

sample! {
    In<Bit> => |v| u128::from(v.to_bool());
    Out<Bit> => |v| u128::from(v.to_bool());
    In<Bits<8>> => |v| v.to_u128();
    Out<Bits<8>> => |v| v.to_u128();
    PublicRd<Bits<8>> => |v| v.to_u128();
}

impl Drive for In<Bit> {
    fn drive(&self, value: u128) {
        self.set(value);
    }
}

impl Drive for In<Bits<8>> {
    fn drive(&self, value: u128) {
        self.set(value);
    }
}

/// Lines in execution order, shared by every probe of a scenario.
#[derive(Clone, Default)]
pub(crate) struct Log {
    lines: Rc<RefCell<String>>,
}

impl Log {
    pub(crate) fn probe(&self, task: &'static str) -> Probe {
        Probe {
            log: self.clone(),
            task,
            trigger: "start".to_owned(),
            phase: "start",
        }
    }

    pub(crate) fn text(&self) -> String {
        self.lines.borrow().clone()
    }
}

/// One task's view: the trigger that last resumed it and the phase that implies.
pub(crate) struct Probe {
    log: Log,
    task: &'static str,
    trigger: String,
    phase: &'static str,
}

impl Probe {
    /// Awaits `trigger` and records `label` as what resumed the task.
    pub(crate) async fn wait<T>(&mut self, trigger: impl Future<Output = T>, label: &str) -> T {
        let out = trigger.await;
        self.woke(label);
        out
    }

    /// Records that the task resumed from the trigger labelled `label`.
    pub(crate) fn woke(&mut self, label: &str) {
        label.clone_into(&mut self.trigger);
        self.phase = match label {
            "ReadOnly" => "readonly",
            "ReadWrite" => "readwrite",
            _ => "active",
        };
    }

    fn line(&self, op: &str, signal: &str, value: &str) {
        // Time is logged in picoseconds; every scenario model has 1 ps precision.
        let mut lines = self.log.lines.borrow_mut();
        writeln!(
            lines,
            r#"{{"time":{},"phase":"{}","trigger":"{}","task":"{}","op":"{op}","signal":"{signal}","value":{value}}}"#,
            now().ticks(),
            self.phase,
            self.trigger,
            self.task,
        )
        .unwrap();
    }

    pub(crate) fn read(&self, sigs: &[&dyn Sample]) {
        for sig in sigs {
            self.line("read", &sig.label(), &sig.sample().to_string());
        }
    }

    pub(crate) fn write(&self, sig: &dyn Drive, value: u128) {
        sig.drive(value);
        self.line("write", &sig.label(), &value.to_string());
    }

    pub(crate) fn note(&self, what: &str, value: &str) {
        self.line("note", what, &format!("\"{value}\""));
    }
}

//! The conformance log: emits the golden JSONL format of `tests/conformance/README.md`
//! (`{"time":..,"phase":..,"trigger":..,"task":..,"op":..,"signal":..,"value":..}`), so a
//! simjig run of a scenario can be diffed byte for byte against cocotb's golden.
//!
//! A [`Probe`] is one task's view, like `conflog.py`'s: it remembers the label of the
//! trigger that last resumed the task and the phase that implies.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::future::Future;
use std::rc::Rc;

use simjig::{RawSig, now};

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

    pub(crate) fn read(&self, sigs: &[RawSig]) {
        for &sig in sigs {
            self.line("read", &sig.name(), &sig.read_u64().to_string());
        }
    }

    pub(crate) fn write(&self, sig: RawSig, value: u64) {
        sig.write_u64(value);
        self.line("write", &sig.name(), &value.to_string());
    }

    pub(crate) fn note(&self, what: &str, value: &str) {
        self.line("note", what, &format!("\"{value}\""));
    }
}

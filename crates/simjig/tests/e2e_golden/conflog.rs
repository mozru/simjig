//! The conformance log of `tests/conformance/README.md` over typed handles, so a run against
//! a real model diffs byte for byte with cocotb's golden. The signal label is the name
//! without the top module (`flop.q` logs as `q`), as cocotb names `dut.q`.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::future::Future;
use std::rc::Rc;

use simjig::{In, Out, PublicRd, RawWords, now};
use simjig_bits::Packed;

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

/// `value` as an unsigned integer (every logged signal is at most 128 bits).
fn to_u128<T: Packed<Raw: RawWords>>(value: T) -> u128 {
    let mut words = [0u32; 4];
    value.to_raw().to_words(&mut words);
    words
        .iter()
        .rev()
        .fold(0, |acc, &w| (acc << 32) | u128::from(w))
}

macro_rules! sample {
    ($($handle:ident)*) => {$(
        impl<T: Packed<Raw: RawWords>> Sample for $handle<T> {
            fn label(&self) -> String {
                short(&self.name())
            }
            fn sample(&self) -> u128 {
                to_u128(self.get())
            }
        }
    )*};
}
sample!(In Out PublicRd);

impl<T: Packed<Raw: RawWords> + From<u128>> Drive for In<T> {
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

    /// Compares the log with the golden `name`; panics at the first differing line.
    #[track_caller]
    pub(crate) fn check(&self, name: &str) {
        let path = format!(
            "{}/../../tests/conformance/golden/{name}.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        let golden = std::fs::read_to_string(&path).unwrap();
        let ours = self.lines.borrow();
        if *ours != golden {
            let first = ours
                .lines()
                .zip(golden.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(ours.lines().count().min(golden.lines().count()));
            panic!(
                "{name} differs from the golden at line {}:\n  ours:   {}\n  golden: {}\n--- ours ---\n{ours}",
                first + 1,
                ours.lines().nth(first).unwrap_or("<end>"),
                golden.lines().nth(first).unwrap_or("<end>"),
            );
        }
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

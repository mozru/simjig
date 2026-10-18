//! [`FakeModel`]: a pure-Rust model built from closures, for testing the kernel (and
//! testbench logic) without Verilator.
//!
//! The fake follows the order of a Verilator `eval()` (V3Sched): input-combinational logic
//! settles first, then every flop whose clock rose since that flop last looked samples the
//! settled values (non-blocking semantics: all flops read before any writes land, IEEE 1800
//! §10.4.2), then combinational logic settles again; this repeats while flops keep firing,
//! so flops on a derived clock (`div_clk`, itself a flop or comb output) fire in the same
//! eval as their source. Writing `d` and raising `clk` in the same eval captures the new
//! `d`, as in Verilator.
//!
//! Timed closures ([`FakeModel::at`]) stand in for `--timing` delays: they make
//! `events_pending`/`next_time_slot` report their time, and run in the first eval at or
//! after it.

use std::collections::BTreeMap;
use std::fmt;

use super::OrInvariant;
use super::backend::{EvalStatus, ModelBackend, ModelError, words};
use super::time::{Precision, SimTime};

/// Combinational passes allowed before an eval reports that logic did not settle.
const SETTLE_LIMIT: u32 = 100;

/// A signal of a [`FakeModel`], valid only for the model that created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakeSig(u32);

type Logic = Box<dyn FnMut(&mut FakeState)>;

struct SlotDesc {
    name: String,
    width: u32,
    offset: usize,
}

/// A model made of Rust closures. Build it, then pass it to
/// [`Sim::from_fake`](crate::Sim::from_fake).
pub struct FakeModel {
    precision: Precision,
    slots: Vec<SlotDesc>,
    state: FakeState,
    comb: Vec<Logic>,
    /// Each flop: its clock, the clock level it last saw, its logic.
    flops: Vec<(FakeSig, u32, Logic)>,
    timed: BTreeMap<SimTime, Vec<Logic>>,
}

impl fmt::Debug for FakeModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeModel")
            .field("precision", &self.precision)
            .field(
                "slots",
                &self.slots.iter().map(|s| &s.name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl FakeModel {
    /// An empty model whose ticks are `precision` long.
    #[must_use]
    pub fn new(precision: Precision) -> Self {
        Self {
            precision,
            slots: Vec::new(),
            state: FakeState::default(),
            comb: Vec::new(),
            flops: Vec::new(),
            timed: BTreeMap::new(),
        }
    }

    /// Adds a signal of `width` bits, initially 0.
    ///
    /// # Panics
    /// If `width` is 0 or the name is already used.
    pub fn signal(&mut self, name: &str, width: u32) -> FakeSig {
        assert!(
            width > 0,
            "fake signal `{name}` must be at least 1 bit wide"
        );
        assert!(
            self.slots.iter().all(|s| s.name != name),
            "fake signal `{name}` is declared twice"
        );
        let offset = self.state.values.len();
        self.state.values.resize(offset + words(width), 0);
        self.state.layout.push((offset, width));
        let slot = u32::try_from(self.slots.len()).or_invariant("invariant: fewer than 2^32 slots");
        self.slots.push(SlotDesc {
            name: name.to_owned(),
            width,
            offset,
        });
        FakeSig(slot)
    }

    /// Adds combinational logic, re-run until all values settle on every eval.
    pub fn comb(&mut self, logic: impl FnMut(&mut FakeState) + 'static) {
        self.comb.push(Box::new(logic));
    }

    /// Adds sequential logic that runs when bit 0 of `clock` rises.
    pub fn on_rising(&mut self, clock: FakeSig, logic: impl FnMut(&mut FakeState) + 'static) {
        self.flops.push((clock, 0, Box::new(logic)));
    }

    /// Adds a timed event (a stand-in for a `--timing` delay) that runs at `time`.
    pub fn at(&mut self, time: SimTime, logic: impl FnMut(&mut FakeState) + 'static) {
        self.timed.entry(time).or_default().push(Box::new(logic));
    }

    fn settle(&mut self) -> Result<(), ModelError> {
        for _ in 0..SETTLE_LIMIT {
            let before = self.state.values.clone();
            for logic in &mut self.comb {
                logic(&mut self.state);
            }
            if before == self.state.values {
                return Ok(());
            }
        }
        Err(ModelError {
            message: format!(
                "fake model: combinational logic did not settle in {SETTLE_LIMIT} passes"
            ),
        })
    }

    fn run_due_timed(&mut self) {
        let now = self.state.now;
        while let Some(entry) = self.timed.first_entry() {
            if *entry.key() > now {
                break;
            }
            for mut logic in entry.remove() {
                logic(&mut self.state);
            }
        }
    }

    /// Runs the flops whose clock rose since they last looked; returns whether any did.
    fn run_flops(&mut self) -> bool {
        let mut fired = Vec::new();
        for (i, (clock, seen, _)) in self.flops.iter_mut().enumerate() {
            let level = u32::from(self.state.get(*clock) & 1 == 1);
            if *seen == 0 && level == 1 {
                fired.push(i);
            }
            *seen = level;
        }
        if fired.is_empty() {
            return false;
        }
        self.state.staged = Some(self.state.values.clone());
        for i in fired {
            (self.flops[i].2)(&mut self.state);
        }
        if let Some(staged) = self.state.staged.take() {
            self.state.values = staged;
        }
        true
    }
}

impl ModelBackend for FakeModel {
    fn precision(&self) -> Precision {
        self.precision
    }

    fn slot_count(&self) -> u32 {
        u32::try_from(self.slots.len()).or_invariant("invariant: fewer than 2^32 slots")
    }

    fn slot_name(&self, slot: u32) -> &str {
        &self.slots[slot as usize].name
    }

    fn slot_width(&self, slot: u32) -> u32 {
        self.slots[slot as usize].width
    }

    fn read(&self, slot: u32, out: &mut [u32]) {
        let desc = &self.slots[slot as usize];
        out.copy_from_slice(&self.state.values[desc.offset..desc.offset + words(desc.width)]);
    }

    fn write(&mut self, slot: u32, value: &[u32]) {
        let desc = &self.slots[slot as usize];
        self.state.values[desc.offset..desc.offset + words(desc.width)].copy_from_slice(value);
    }

    fn set_time(&mut self, time: SimTime) {
        self.state.now = time;
    }

    fn eval(&mut self) -> Result<EvalStatus, ModelError> {
        self.run_due_timed();
        self.settle()?;
        let mut passes = 0;
        while self.run_flops() {
            self.settle()?;
            passes += 1;
            if passes == SETTLE_LIMIT {
                return Err(ModelError {
                    message: format!("fake model: flops kept firing for {SETTLE_LIMIT} passes"),
                });
            }
        }
        if let Some(message) = self.state.fatal.take() {
            return Err(ModelError { message });
        }
        Ok(if self.state.finished {
            EvalStatus::Finished
        } else {
            EvalStatus::Running
        })
    }

    fn events_pending(&self) -> bool {
        !self.timed.is_empty()
    }

    fn next_time_slot(&self) -> SimTime {
        *self
            .timed
            .keys()
            .next()
            .or_invariant("invariant: next_time_slot is only called when events are pending")
    }
}

/// The values of a [`FakeModel`], as seen by its closures.
///
/// Inside an [`on_rising`](FakeModel::on_rising) closure, reads see the values before the
/// edge and writes land after every triggered flop has run.
#[derive(Debug, Default)]
pub struct FakeState {
    values: Vec<u32>,
    /// `(offset, width)` per slot.
    layout: Vec<(usize, u32)>,
    staged: Option<Vec<u32>>,
    now: SimTime,
    fatal: Option<String>,
    finished: bool,
}

impl FakeState {
    fn span(&self, sig: FakeSig) -> (usize, u32) {
        self.layout[sig.0 as usize]
    }

    /// The value of a signal of at most 64 bits.
    ///
    /// # Panics
    /// If the signal is wider than 64 bits.
    #[must_use]
    pub fn get(&self, sig: FakeSig) -> u64 {
        let (offset, width) = self.span(sig);
        assert!(
            width <= 64,
            "fake signal is {width} bits wide; use get_words"
        );
        let low = u64::from(self.values[offset]);
        let high = if width > 32 {
            u64::from(self.values[offset + 1])
        } else {
            0
        };
        low | high << 32
    }

    /// Sets a signal of at most 64 bits; bits above its width are dropped, as in hardware.
    ///
    /// # Panics
    /// If the signal is wider than 64 bits.
    pub fn set(&mut self, sig: FakeSig, value: u64) {
        let (_, width) = self.span(sig);
        assert!(
            width <= 64,
            "fake signal is {width} bits wide; use set_words"
        );
        let masked = if width == 64 {
            value
        } else {
            value & ((1 << width) - 1)
        };
        let low = u32::try_from(masked & 0xffff_ffff).or_invariant("invariant: masked to 32 bits");
        let high = u32::try_from(masked >> 32).or_invariant("invariant: shifted to 32 bits");
        self.set_words(sig, &[low, high][..words(width)]);
    }

    /// The value of any signal as little-endian `u32` words.
    #[must_use]
    pub fn get_words(&self, sig: FakeSig) -> &[u32] {
        let (offset, width) = self.span(sig);
        &self.values[offset..offset + words(width)]
    }

    /// Sets any signal from little-endian `u32` words; bits above its width are dropped.
    ///
    /// # Panics
    /// If `value` does not hold exactly as many words as the signal.
    pub fn set_words(&mut self, sig: FakeSig, value: &[u32]) {
        let (offset, width) = self.span(sig);
        let n = words(width);
        assert_eq!(value.len(), n, "fake signal needs {n} words");
        let target = self.staged.as_mut().unwrap_or(&mut self.values);
        target[offset..offset + n].copy_from_slice(value);
        if width % 32 != 0 {
            target[offset + n - 1] &= (1 << (width % 32)) - 1;
        }
    }

    /// The model's current time.
    #[must_use]
    pub fn now(&self) -> SimTime {
        self.now
    }

    /// Makes the current eval fail with `message`, like `$fatal`.
    pub fn fatal(&mut self, message: &str) {
        self.fatal = Some(message.to_owned());
    }

    /// Makes the current eval report `$finish`.
    pub fn finish(&mut self) {
        self.finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flops_sample_settled_inputs_with_nonblocking_semantics() {
        let mut m = FakeModel::new(Precision::PS);
        let clk = m.signal("clk", 1);
        let d = m.signal("d", 8);
        let q1 = m.signal("q1", 8);
        let q2 = m.signal("q2", 8);
        m.on_rising(clk, move |s| s.set(q1, s.get(d)));
        // A shift register: q2 must see q1's value from before the edge.
        m.on_rising(clk, move |s| s.set(q2, s.get(q1)));
        m.write(d.0, &[5]);
        m.write(clk.0, &[1]);
        m.eval().unwrap();
        m.write(clk.0, &[0]);
        m.eval().unwrap();
        m.write(d.0, &[6]);
        m.write(clk.0, &[1]);
        m.eval().unwrap();
        let mut out = [0];
        m.read(q1.0, &mut out);
        assert_eq!(out, [6]);
        m.read(q2.0, &mut out);
        assert_eq!(out, [5]);
    }

    #[test]
    fn flops_on_a_derived_clock_fire_in_the_same_eval() {
        let mut m = FakeModel::new(Precision::PS);
        let clk = m.signal("clk", 1);
        let div = m.signal("div_clk", 1);
        let slow = m.signal("slow", 8);
        m.on_rising(clk, move |s| s.set(div, s.get(div) ^ 1));
        m.on_rising(div, move |s| s.set(slow, s.get(slow) + 1));
        m.write(clk.0, &[1]);
        m.eval().unwrap();
        let mut out = [0];
        m.read(slow.0, &mut out);
        assert_eq!(out, [1]);
    }

    #[test]
    fn unsettled_comb_logic_is_an_error() {
        let mut m = FakeModel::new(Precision::PS);
        let a = m.signal("a", 1);
        m.comb(move |s| s.set(a, s.get(a) ^ 1));
        assert!(m.eval().is_err());
    }

    #[test]
    fn set_masks_to_width() {
        let mut m = FakeModel::new(Precision::PS);
        let wide = m.signal("w", 40);
        m.comb(move |s| s.set(wide, u64::MAX));
        m.eval().unwrap();
        let mut out = [0; 2];
        m.read(wide.0, &mut out);
        assert_eq!(out, [u32::MAX, 0xff]);
    }
}

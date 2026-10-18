//! Several DUTs in one test: tuple [`DutModel`]s, and lockstep comparison of two models.
//!
//! `#[simjig::test] async fn t(a: inv::Dut, b: flop::Dut)` runs `(inv::Dut, flop::Dut)` as
//! its DUT type. The tuple's [`DutModel`] loads each model on its own
//! ([`DutModel::load`]: its own `VerilatedContext`, its own `$urandom` state, its own
//! `--prefix`), puts them into one [`Sim`] in parameter order, and binds each DUT to its
//! index ([`DutModel::bind`]). The kernel then evaluates every model in every delta, in that
//! order, before applying the write log (`kernel::models`), so one delta loop and one time
//! axis serve all of them. Up to four DUTs are supported.
//!
//! Every model receives the same [`ModelOptions`], so two instances of one design get the
//! same Verilator seed: lockstep runs depend on it, since `$urandom` must draw the same
//! values on both sides.
//!
//! # Lockstep
//! Two builds of one design (the old RTL and the new one, under two `--prefix`es, or two
//! instances of the same build) driven by the same stimulus: [`Mirror`] writes one value
//! to an input of both, and [`Lockstep`] compares typed outputs once per cycle and reports
//! the first cycle they differ as a [`Divergence`] listing every differing signal.
//!
//! A design with DPI-C imports implemented in Rust cannot run as two instances of *one
//! build*: `Dut::set_dpi` finds its model by (simulation, prefix), so it cannot address
//! the second instance, whose first DPI call fails the run (`FailureKind::Setup`). Build
//! the design a second time under another `simjig_build::Dut::new` name (another prefix)
//! and run one instance of each.
//!
//! # Limits
//! - The models of one simulation must share a time precision
//!   ([`ModelSetError::PrecisionMismatch`](crate::ModelSetError::PrecisionMismatch)).
//! - Two instances of one build with DPI-C imports implemented in Rust (above).
//! - DPI exports are not supported in a multi-model simulation. Verilator names an exported
//!   function's C symbol after the SystemVerilog function, without the `--prefix`, so two
//!   builds of one design that export it collide at link time; two instances of one build
//!   share the symbol, and which instance it runs in is the `svSetScope` scope, which
//!   simjig does not switch per model.

use std::fmt;

use simjig_bits::Packed;

use super::dut::{DutModel, ModelOptions};
use crate::kernel::models::ModelSetError;
use crate::kernel::state;
use crate::model::values::FieldDiff;
use crate::signal::{In, Out, RawWords};
use crate::{Failure, FailureKind, Precision, Sim, SimTime};

/// The failure of building a simulation from models that cannot share one.
fn model_set_failure(error: &ModelSetError) -> Failure {
    let precision = match error {
        ModelSetError::PrecisionMismatch { expected, .. } => *expected,
        ModelSetError::Empty | ModelSetError::TooManySignals => Precision::PS,
    };
    let message = format!("the test's DUTs cannot share one simulation: {error}");
    let help = match error {
        ModelSetError::PrecisionMismatch { .. } => {
            "build every DUT of the test with the same timeprecision (`--timescale` in \
             `verilator_args`, or `timeprecision` in the design)"
        }
        ModelSetError::Empty | ModelSetError::TooManySignals => "split the DUTs over several tests",
    };
    Failure {
        kind: FailureKind::Setup {
            message: message.clone(),
            help,
        },
        time: SimTime::ZERO,
        precision,
        task: None,
        location: None,
        message,
    }
}

/// Implements [`DutModel`] for a tuple of DUT types, model `i` being element `i`.
macro_rules! tuple_dut {
    ($($dut:ident $index:tt),+) => {
        /// Several DUTs in one simulation, numbered in tuple order; see `harness::multi`.
        impl<$($dut: DutModel),+> DutModel for ($($dut,)+) {
            fn instantiate(options: &ModelOptions) -> Result<Sim, Failure> {
                let models = vec![$($dut::load(&options.for_model($index))?.into_backend()),+];
                Sim::from_backends(models).map_err(|e| model_set_failure(&e))
            }

            fn create(sim: &mut Sim) -> Result<Self, Failure> {
                Ok(($($dut::bind(sim, $index)?,)+))
            }
        }
    };
}

tuple_dut!(A 0, B 1);
tuple_dut!(A 0, B 1, C 2);
tuple_dut!(A 0, B 1, C 2, D 3);

/// One input of two models, written together: the same stimulus for both sides of a
/// lockstep run.
pub struct Mirror<T> {
    reference: In<T>,
    candidate: In<T>,
}

impl<T> Clone for Mirror<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Mirror<T> {}

impl<T> fmt::Debug for Mirror<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mirror")
            .field("reference", &self.reference)
            .field("candidate", &self.candidate)
            .finish()
    }
}

impl<T: Packed<Raw: RawWords>> Mirror<T> {
    /// The input `reference` of one model paired with the same input `candidate` of the
    /// other.
    #[must_use]
    pub fn new(reference: In<T>, candidate: In<T>) -> Self {
        Self {
            reference,
            candidate,
        }
    }

    /// Deposits `value` into both inputs, in the same delta.
    ///
    /// # Panics
    /// As [`In::set`]: outside the running simulation, in `ReadOnly`, or if a literal does
    /// not fit the signal.
    #[track_caller]
    pub fn set(self, value: impl Into<T> + Copy + 'static) {
        self.reference.set(value);
        self.candidate.set(value);
    }
}

/// One comparison of a [`Lockstep`]: `None` when both sides agree.
type Check = Box<dyn Fn() -> Option<FieldDiff>>;

/// Compares two models' outputs once per cycle and reports the first cycle they differ.
///
/// Each comparison reads a typed value from the reference model and one from the
/// candidate; [`check`](Self::check) runs all of them, so a divergence lists every signal
/// that differs in that cycle, not only the first.
///
/// For a design with DPI-C imports implemented in Rust, the two sides must be two builds
/// (two `simjig_build::Dut::new` names), not two instances of one build: `set_dpi` cannot
/// tell two instances of one build apart (see the module docs).
///
/// ```ignore
/// #[simjig::test]
/// async fn rev2_matches_rev1(old: counter::Dut, new: counter_rev2::Dut) {
///     let en = Mirror::new(old.en, new.en);
///     let mut lockstep = Lockstep::new().output(old.count, new.count);
///     clock(old.clk, ns(10)).start();
///     clock(new.clk, ns(10)).start();
///     en.set(1);
///     for _ in 0..100 {
///         old.clk.rising().await;
///         ReadOnly::new().await;
///         lockstep.step(); // panics at the first divergent cycle
///     }
/// }
/// ```
pub struct Lockstep {
    checks: Vec<Check>,
    cycle: u64,
}

impl fmt::Debug for Lockstep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lockstep")
            .field("checks", &self.checks.len())
            .field("cycle", &self.cycle)
            .finish()
    }
}

impl Default for Lockstep {
    fn default() -> Self {
        Self::new()
    }
}

impl Lockstep {
    /// A comparison with nothing to compare yet, at cycle 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            cycle: 0,
        }
    }

    /// Also compares the output `reference` with `candidate`, named by both signals:
    /// `old.count vs new.count`, or, when the two have one hierarchical name (two builds of
    /// one top module), `counter.count (model 0) vs counter.count (model 1)`, by their
    /// models' indices in the test's DUT order.
    #[must_use]
    pub fn output<T>(self, reference: Out<T>, candidate: Out<T>) -> Self
    where
        T: Packed<Raw: RawWords> + PartialEq + fmt::Debug,
    {
        let (r, c) = (reference.name(), candidate.name());
        let name = if r == c {
            let (rm, cm) = (reference.raw().model(), candidate.raw().model());
            format!("{r} (model {rm}) vs {c} (model {cm})")
        } else {
            format!("{r} vs {c}")
        };
        self.compare(&name, move || reference.get(), move || candidate.get())
    }

    /// Also compares any typed sample of the two models (an internal `PublicRd`, a
    /// decoded struct, a tuple of outputs), under `name`.
    #[must_use]
    pub fn compare<S: PartialEq + fmt::Debug + 'static>(
        mut self,
        name: &str,
        reference: impl Fn() -> S + 'static,
        candidate: impl Fn() -> S + 'static,
    ) -> Self {
        let name = name.to_owned();
        self.checks.push(Box::new(move || {
            let (r, c) = (reference(), candidate());
            (r != c).then(|| FieldDiff::new(&name, &r, &c))
        }));
        self
    }

    /// The number of cycles checked so far (the index the next check gets).
    #[must_use]
    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    /// Compares every sample now, as cycle [`cycle`](Self::cycle), and moves to the next
    /// cycle.
    ///
    /// # Errors
    /// The [`Divergence`] if any sample differs; the cycle still counts.
    ///
    /// # Panics
    /// Outside a running simulation.
    pub fn check(&mut self) -> Result<(), Divergence> {
        let cycle = self.cycle;
        self.cycle += 1;
        let diffs: Vec<FieldDiff> = self.checks.iter().filter_map(|check| check()).collect();
        if diffs.is_empty() {
            return Ok(());
        }
        let (time, precision) =
            state::with_current("Lockstep::check", |k| (k.now.get(), k.precision));
        Err(Divergence {
            cycle,
            time,
            precision,
            diffs,
        })
    }

    /// [`check`](Self::check), failing the task at the first divergent cycle.
    ///
    /// # Panics
    /// With the [`Divergence`]'s text if any sample differs, which fails the test as a
    /// `TaskPanic` at the caller's line.
    #[track_caller]
    pub fn step(&mut self) {
        if let Err(divergence) = self.check() {
            panic!("{divergence}");
        }
    }
}

/// The first cycle in which two lockstep models disagreed: when, and every sample that
/// differed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// The cycle index, counting [`Lockstep::check`] calls from 0.
    pub cycle: u64,
    /// The simulation time of the check.
    pub time: SimTime,
    /// The simulation's precision, to print `time`.
    pub precision: Precision,
    /// Each differing sample: its name, the reference value, the candidate value.
    pub diffs: Vec<FieldDiff>,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lockstep divergence at cycle {} (t={}): {} sample(s) differ (reference != candidate)",
            self.cycle,
            self.time.display(self.precision),
            self.diffs.len()
        )?;
        for diff in &self.diffs {
            write!(f, "\n  {diff}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Divergence {}

//! [`DutModel`]: the seam between `#[simjig::test]` and the code that builds a model.
//!
//! The harness knows nothing about Verilator. For each test it asks the DUT type for a
//! [`Sim`] ([`DutModel::instantiate`], given the run's [`ModelOptions`]) and then for the
//! DUT value bound to that simulation ([`DutModel::create`]). simjig-build's generated
//! `Dut` types implement the trait on top of the model backend; tests of simjig itself
//! implement it on [`FakeModel`].
//!
//! A test with several DUTs (`async fn t(a: inv::Dut, b: flop::Dut)`) asks each DUT type
//! for its model alone ([`DutModel::load`]), puts the models into one [`Sim`] in parameter
//! order, and binds each DUT to its model's index ([`DutModel::bind`]); see
//! `harness::multi`.

use std::fmt;

use crate::kernel::backend::ModelBackend;
use crate::model::trace::TraceOptions;
use crate::{Failure, FailureKind, FakeModel, Precision, Sim, SimTime};

use super::seed::Seed;

/// The values a model's variables start from, before reset logic runs.
///
/// Verilator is 2-state: where a 4-state simulator starts every variable without an
/// initializer at `X` (IEEE 1800-2017 §6.8), a Verilator model starts it at a value
/// chosen when the model is constructed. [`Zero`](Self::Zero) is Verilator's default and
/// hides a register that reset never clears; [`Random`](Self::Random) gives every such
/// variable (inputs not yet driven included) a value drawn from the run's seed, so a
/// missing reset shows up as a seed-dependent failure that the report's `repro:` line
/// reproduces exactly.
///
/// Chosen per test with `#[simjig::test(init = random)]`, else by `SIMJIG_INIT=random`,
/// else zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InitValues {
    /// Every variable without an initializer starts at 0 (Verilator's default).
    #[default]
    Zero,
    /// Every variable without an initializer starts at a value drawn from
    /// [`Seed::init_seed`] of its model, its scope and its name. Only the start differs:
    /// `$urandom` and everything after construction draw as with [`Zero`](Self::Zero).
    /// A model built with `--x-initial 0` or `fast` cannot do it and fails to load
    /// (`LoadError::NoRandomInit`).
    Random,
}

impl InitValues {
    /// `"zero"` or `"random"`, as `SIMJIG_INIT` takes it and `summary.json` writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::Random => "random",
        }
    }

    /// Parses a `SIMJIG_INIT` value.
    ///
    /// # Errors
    /// A message naming the rejected text and the accepted values.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "zero" => Ok(Self::Zero),
            "random" => Ok(Self::Random),
            _ => Err(format!(
                "SIMJIG_INIT={text:?} is not an initial-value mode; use `zero` (Verilator's \
                 default) or `random`"
            )),
        }
    }
}

/// What the harness passes to a model when it builds one for a test run.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelOptions {
    /// The run's root seed.
    pub seed: Seed,
    /// The seed for Verilator's own random number generator (`$random`, `$urandom`), in
    /// `1..=i32::MAX`; see [`Seed::verilator_seed`]. Every model of a simulation gets the
    /// same one (`harness::multi`).
    pub verilator_seed: u32,
    /// The values the model's variables start from.
    pub init: InitValues,
    /// The seed of this model's initial values with [`InitValues::Random`],
    /// [`Seed::init_seed`] of its index, or of the first model of its design
    /// ([`DutModel::design`]) in a simulation with several (`harness::multi`).
    pub init_seed: u32,
    /// The FST waves a Verilator model records; `None` records none. Models that cannot
    /// trace (a [`FakeModel`]) ignore it.
    pub trace: Option<TraceOptions>,
    /// Eval elision, an opt-in: whether a Verilator model skips an eval with nothing
    /// written since its previous one. `false` (the default) evaluates every time, as a
    /// C++ loop does. See [`with_elide_evals`](Self::with_elide_evals) for when it is safe.
    pub elide_evals: bool,
}

impl ModelOptions {
    /// The options of model `index` of a simulation with several: the same, except that
    /// its initial values come from its own stream ([`Seed::init_seed`] of `index`;
    /// `harness::multi` then gives models of one design the first one's), and a
    /// trace of model `index > 0` goes to `<stem>.model-<index>.fst` beside the given file
    /// (model 0 keeps it), since each model writes its own FST.
    #[must_use]
    pub fn for_model(&self, index: usize) -> Self {
        let mut options = self.clone();
        options.init_seed = self.seed.init_seed(index);
        if index > 0
            && let Some(trace) = &mut options.trace
        {
            let stem = trace
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let path = trace
                .path()
                .with_file_name(format!("{stem}.model-{index}.fst"));
            *trace = trace.clone().with_path(path);
        }
        options
    }

    /// The options for a run with root seed `seed`.
    #[must_use]
    pub fn new(seed: Seed) -> Self {
        Self {
            seed,
            verilator_seed: seed.verilator_seed(),
            init: InitValues::Zero,
            init_seed: seed.init_seed(0),
            trace: None,
            elide_evals: false,
        }
    }

    /// These options, with the model's variables starting from `init`.
    #[must_use]
    pub fn with_init(mut self, init: InitValues) -> Self {
        self.init = init;
        self
    }

    /// The seed a model draws its initial values from, or `None` for zeros.
    #[must_use]
    pub fn random_init(&self) -> Option<u32> {
        (self.init == InitValues::Random).then_some(self.init_seed)
    }

    /// These options, with the model recording the waves `trace` describes.
    #[must_use]
    pub fn with_trace(mut self, trace: TraceOptions) -> Self {
        self.trace = Some(trace);
        self
    }

    /// These options, with eval elision on or off (`#[simjig::test(elide_evals)]`): a
    /// Verilator model then skips an eval with nothing written since its previous one,
    /// which in a clocked testbench is the eval that opens each timestep and the last one
    /// of each delta loop. The kernel still counts and records a skipped eval, so the
    /// trace hash is the same either way.
    ///
    /// It is safe exactly when such an eval would do nothing. Verilator 5.052 runs some
    /// logic on every `eval()` whatever was written (the 'ico' region's 'first iteration'
    /// trigger, `V3Sched.cpp`): logic fed by a variable C++ may write, by a top-level input
    /// the design writes itself (its `ASSIGNIN` lint waived), or by a `$sampled` value.
    /// Running pure combinational logic again changes nothing; running logic that reads
    /// `$time` or `$realtime`, prints (`$display`, `$write`), draws `$random` or
    /// `$urandom`, or calls a DPI-C import does, and a skipped eval then differs from a C++
    /// loop. So enable it only for a design where no such impure logic is fed by an input
    /// the design writes or by a `$sampled` value. simjig cannot check this from the build
    /// (nothing outside the design shows a write to its own input), so the choice is the
    /// user's. It never applies where simjig can tell an eval is not a no-op: a model with
    /// a writable public variable (`public_flat_rw`, `--public-flat-rw`, a read-write
    /// probe), one built with `--timing`, and one recording waves evaluate every time.
    #[must_use]
    pub fn with_elide_evals(mut self, on: bool) -> Self {
        self.elide_evals = on;
        self
    }

    /// The Verilator plusargs these options stand for, in order; a Verilator backend
    /// passes them to `VerilatedContext::commandArgs`.
    #[must_use]
    pub fn verilator_args(&self) -> Vec<String> {
        vec![format!("+verilator+seed+{}", self.verilator_seed)]
    }
}

/// A design under test that `#[simjig::test]` can build: the type of a test's DUT
/// parameter.
///
/// This is the seam the model and codegen layers implement: simjig-build generates one
/// implementation per DUT (see [`include_dut!`](crate::include_dut)). The harness calls
/// `instantiate` and then `create` once per test, before the test body runs; an `Err`
/// from either fails the test with the usual report.
///
/// `()` implements it with an empty model, for tests that take no DUT.
///
/// ```
/// use simjig::{DutModel, FakeModel, Failure, ModelOptions, Precision, RawSig, Sim};
///
/// struct Counter {
///     clk: RawSig,
///     count: RawSig,
/// }
///
/// impl DutModel for Counter {
///     fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
///         let mut model = FakeModel::new(Precision::NS);
///         let clk = model.signal("clk", 1);
///         let count = model.signal("count", 8);
///         model.on_rising(clk, move |s| s.set(count, s.get(count) + 1));
///         Ok(Sim::from_fake(model))
///     }
///
///     fn create(sim: &mut Sim) -> Result<Self, Failure> {
///         // Signal names come from the design, so a missing one is a bug in this impl.
///         let sig = |name| sim.signal(name).ok_or(name);
///         match (sig("clk"), sig("count")) {
///             (Ok(clk), Ok(count)) => Ok(Self { clk, count }),
///             (Err(name), _) | (_, Err(name)) => panic!("no signal `{name}`"),
///         }
///     }
/// }
/// ```
#[expect(
    clippy::result_large_err,
    reason = "called once per test, and Failure stays plain data (as Sim::run)"
)]
pub trait DutModel: Sized + 'static {
    /// Builds the model and the simulation that will drive it.
    ///
    /// # Errors
    /// A [`Failure`] if the model cannot be built (for example
    /// [`FailureKind::ModelFatal`](crate::FailureKind::ModelFatal) when it fails to load).
    fn instantiate(options: &ModelOptions) -> Result<Sim, Failure>;

    /// Binds the DUT to `sim`: looks up its signals, adds its clocks.
    ///
    /// # Errors
    /// A [`Failure`] if the model does not match what the DUT type expects.
    fn create(sim: &mut Sim) -> Result<Self, Failure>;

    /// Builds the model alone, to share one [`Sim`] with the models of other DUTs.
    ///
    /// The default refuses: a DUT type that builds its `Sim` directly cannot share one.
    /// Generated `Dut` types implement it.
    ///
    /// # Errors
    /// A [`Failure`] if the model cannot be built, or if the type does not support it.
    fn load(_options: &ModelOptions) -> Result<LoadedModel, Failure> {
        Err(unsupported::<Self>("load"))
    }

    /// Binds the DUT to model number `model` of `sim` (its position among the test's DUT
    /// parameters, from 0).
    ///
    /// The default binds model 0 with [`create`](Self::create) and refuses any other.
    ///
    /// # Errors
    /// A [`Failure`] if model `model` is not the model this DUT type expects.
    fn bind(sim: &mut Sim, model: usize) -> Result<Self, Failure> {
        if model == 0 {
            Self::create(sim)
        } else {
            Err(unsupported::<Self>("bind"))
        }
    }

    /// The design this DUT type's model is built from: the name of its top module, as
    /// its signals' hierarchical names start (`counter` for `counter.clk`).
    ///
    /// In a simulation with several DUTs, models of one design draw their random initial
    /// values from one seed, so two instances of a design, or two builds of one top module
    /// compared by a [`Lockstep`](crate::Lockstep), start alike (`harness::multi`). The
    /// default, `None`, keeps the model's own seed. Generated `Dut` types implement it.
    #[must_use]
    fn design() -> Option<&'static str> {
        None
    }
}

/// One model built by [`DutModel::load`], not yet part of a [`Sim`].
pub struct LoadedModel {
    backend: Box<dyn ModelBackend>,
}

impl LoadedModel {
    /// A [`FakeModel`], for a hand-written [`DutModel`] that takes part in a test with
    /// several DUTs.
    #[must_use]
    pub fn from_fake(model: FakeModel) -> Self {
        Self::from_backend(Box::new(model))
    }

    pub(crate) fn from_backend(backend: Box<dyn ModelBackend>) -> Self {
        Self { backend }
    }

    pub(crate) fn into_backend(self) -> Box<dyn ModelBackend> {
        self.backend
    }
}

impl fmt::Debug for LoadedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedModel")
            .field("precision", &self.backend.precision())
            .field("signals", &self.backend.slot_count())
            .finish()
    }
}

/// The failure of a default [`DutModel`] method that `D` does not provide.
fn unsupported<D>(method: &str) -> Failure {
    let message = format!(
        "`{}` does not implement `DutModel::{method}`, so it cannot share a simulation with \
         other DUTs",
        std::any::type_name::<D>()
    );
    Failure {
        kind: FailureKind::Setup {
            message: message.clone(),
            help: "implement `DutModel::load` and `DutModel::bind` for this DUT type \
                   (generated `Dut` types do), or use it as the test's only DUT",
        },
        time: SimTime::ZERO,
        precision: Precision::PS,
        task: None,
        location: None,
        message,
    }
}

/// No DUT: an empty model with Verilator's default precision, 1 ps.
impl DutModel for () {
    fn instantiate(_options: &ModelOptions) -> Result<Sim, Failure> {
        Ok(Sim::from_fake(FakeModel::new(Precision::PS)))
    }

    fn create(_sim: &mut Sim) -> Result<Self, Failure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SeedSource;

    #[test]
    fn each_model_draws_its_initial_values_from_its_own_stream() {
        let seed = Seed::new(5, SeedSource::Env);
        let zero = ModelOptions::new(seed);
        assert_eq!(zero.random_init(), None);
        let random = zero.clone().with_init(InitValues::Random);
        assert_eq!(random.random_init(), Some(seed.init_seed(0)));
        assert_eq!(random.for_model(0), random);
        let second = random.for_model(1);
        assert_eq!(second.random_init(), Some(seed.init_seed(1)));
        assert_ne!(seed.init_seed(0), seed.init_seed(1));
        // `$urandom` stays shared, so lockstep runs still draw alike.
        assert_eq!(second.verilator_args(), random.verilator_args());
    }

    #[test]
    fn init_values_parse_as_they_print() {
        for init in [InitValues::Zero, InitValues::Random] {
            assert_eq!(InitValues::parse(init.as_str()), Ok(init));
        }
        let err = InitValues::parse("x").unwrap_err();
        assert!(
            err.contains("SIMJIG_INIT=\"x\"") && err.contains("`random`"),
            "{err}"
        );
    }

    #[test]
    fn options_carry_the_verilator_seed_as_a_plusarg() {
        let seed = Seed::new(5, SeedSource::Env);
        let options = ModelOptions::new(seed);
        assert_eq!(options.verilator_seed, seed.verilator_seed());
        assert_eq!(
            options.verilator_args(),
            [format!("+verilator+seed+{}", seed.verilator_seed())]
        );
    }
}

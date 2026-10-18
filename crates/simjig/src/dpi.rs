//! DPI-C imports implemented in Rust: the one place where the simulator calls the
//! testbench.
//!
//! A design that declares `import "DPI-C" function int f(input int x)` calls the C symbol
//! `f` from inside `eval`. For every import, simjig-build's codegen emits a method of the
//! model's `Dpi` trait, a `Dut::set_dpi(impl Dpi)` that registers an implementation, and an
//! `extern "C"` *trampoline* named `f` (through [`dpi_import!`](crate::__dpi_import), so the
//! `#[no_mangle]` and the pointer reads stay inside simjig and a user crate can
//! `forbid(unsafe_code)`). The trampoline converts the C arguments and calls [`call`].
//!
//! # Which implementation runs
//! A DPI-C import is one C symbol per binary, but each model *instance* has its own
//! implementation. The model backend wraps every `eval` (and `final`) in
//! [`ModelDpi::scope`], which makes that instance's [`Slot`] the thread's *current* one;
//! [`call`] dispatches to it. Two instances of one model, even in one simulation, therefore
//! never see each other's implementation.
//!
//! `set_dpi` and the backend meet in a thread-local registry keyed by (simulation id,
//! model prefix): `set_dpi` finds the simulation through one of the `Dut`'s handles, the
//! backend through the kernel running its eval. Either may come first, so an
//! implementation can be registered before `Sim::run` or at the start of the test body
//! (the main task runs before the first eval at time 0).
//!
//! # Failures
//! [`call`] never unwinds into C++: the implementation runs under `catch_unwind`. A
//! panic, a call with no implementation registered, or a call that reached the trampoline
//! of another model (two models importing one name, see below) is recorded in the slot,
//! and the trampoline returns zero (the type's default) to the design. Later calls in the
//! same eval return zero without running the implementation. After the eval the backend
//! takes the failure, poisons the model (the design already consumed a wrong value, so
//! continuing would report garbage) and fails the run: a panic as `DpiPanic` with the
//! panicking line (from `kernel::panics`), anything else as `ModelFatal`.
//!
//! While an implementation runs, the kernel refuses every entry point (signal access,
//! triggers, `spawn`, `now()`, timers) with "DPI functions run inside eval; they can't
//! access signals or await" (`kernel::state::enter_dpi_call`): the model is in the middle
//! of an eval, so there is no consistent value to read and no time at which a write could
//! apply.
//!
//! # Duplicate names
//! Two models in one binary that import the same C name share one symbol: the linker keeps
//! one trampoline, and the other model's calls land in it. Each trampoline knows the prefix
//! it was generated for, so such a call is a failure naming both models. `set_dpi` also
//! claims its import names process-wide and panics if another model claimed one first.
//! (The build script detects the clash earlier when it builds both models.)

#[expect(
    unsafe_code,
    reason = "reads and writes the argument pointers C passes"
)]
mod abi;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};

pub use abi::{
    Abi, Bit, BitVec, Handle, I8, I16, I32, I64, InArg, InOutArg, Logic, LogicVec, OutArg, Param,
    Ret, SvLogicVecVal, U8, U16, U32, U64,
};

use crate::kernel::backend::ModelError;
use crate::kernel::failure::SourceLocation;
use crate::kernel::signal::RawSig;
use crate::kernel::{panics, state};

/// An SV `chandle` (C `void*`): an opaque pointer the design stores and hands back.
///
/// simjig never dereferences it. [`Chandle::NULL`] is the default, and what a failed call
/// returns.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chandle(*mut c_void);

impl Chandle {
    /// The null handle; a `chandle` variable's initial value (IEEE 1800-2023 §6.14).
    pub const NULL: Self = Self(std::ptr::null_mut());

    /// Wraps a pointer.
    #[must_use]
    pub const fn from_ptr(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    /// The pointer.
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Whether this is the null handle.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl Default for Chandle {
    fn default() -> Self {
        Self::NULL
    }
}

impl fmt::Debug for Chandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Chandle({:p})", self.0)
    }
}

/// The implementation of one model instance and the first failure of the current eval.
struct Slot {
    /// The model's prefix (`Vdpi`).
    prefix: &'static str,
    /// The registered implementation: a `Box<Box<dyn Trait>>` for the model's generated
    /// trait, so [`call`] can downcast without knowing the trait.
    imp: RefCell<Option<Box<dyn Any>>>,
    /// The first failure since the backend last asked.
    failure: RefCell<Option<DpiFailure>>,
    /// Whether `failure` is set; checked on every call, so kept apart from it.
    failed: Cell<bool>,
    /// Whether a model backend uses this slot (at most one may).
    claimed: Cell<bool>,
    /// For a model whose (simulation, prefix) slot another model already uses: calls fail,
    /// since `set_dpi` cannot address this instance.
    ambiguous: bool,
}

impl Slot {
    fn new(prefix: &'static str, ambiguous: bool) -> Rc<Self> {
        Rc::new(Self {
            prefix,
            imp: RefCell::new(None),
            failure: RefCell::new(None),
            failed: Cell::new(false),
            claimed: Cell::new(false),
            ambiguous,
        })
    }

    /// Records a failure that is not a panic, unless an earlier failure is pending.
    fn fail(&self, message: String) {
        self.record(DpiFailure {
            message,
            panicked: false,
            location: None,
        });
    }

    /// Records `failure` unless an earlier failure is pending.
    fn record(&self, failure: DpiFailure) {
        self.failure.borrow_mut().get_or_insert(failure);
        self.failed.set(true);
    }

    /// The pending failure, clearing it.
    fn take_failure(&self) -> Option<DpiFailure> {
        self.failed.set(false);
        self.failure.take()
    }
}

/// What went wrong in the DPI calls of one eval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DpiFailure {
    /// The whole message, naming the import and the model.
    pub(crate) message: String,
    /// Whether an implementation panicked (otherwise none was registered, or names clash).
    pub(crate) panicked: bool,
    /// The panicking line, if the kernel's panic hook saw it.
    pub(crate) location: Option<&'static SourceLocation>,
}

impl DpiFailure {
    /// The eval's error: `DpiPanic` for a panic, a model fatal for the rest.
    pub(crate) fn into_model_error(self) -> ModelError {
        if self.panicked {
            ModelError::dpi_panic(self.message, self.location)
        } else {
            ModelError::fatal(self.message)
        }
    }
}

/// The DPI state of one thread, in one thread-local so that a call costs one access.
struct Thread {
    /// The slot of the model being evaluated; taken out while a call runs.
    current: Cell<Option<Rc<Slot>>>,
}

thread_local! {
    static THREAD: Thread = const {
        Thread {
            current: Cell::new(None),
        }
    };
    /// Slots by (simulation id, model prefix).
    static SLOTS: RefCell<BTreeMap<(u32, &'static str), Rc<Slot>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Which model claimed each DPI-C import name, process-wide.
static CLAIMS: Mutex<BTreeMap<&'static str, &'static str>> = Mutex::new(BTreeMap::new());

/// The registered slot for (`sim_gen`, `prefix`), created if absent.
fn registered(sim_gen: u32, prefix: &'static str) -> Rc<Slot> {
    SLOTS.with_borrow_mut(|s| {
        let slot = s
            .entry((sim_gen, prefix))
            .or_insert_with(|| Slot::new(prefix, false));
        Rc::clone(slot)
    })
}

/// Registers `imp` as the implementation of model `prefix`'s DPI-C imports `imports`, for
/// the simulation `sig` belongs to. Generated `Dut::set_dpi` calls it; a later call
/// replaces the implementation from the next DPI call on.
///
/// # Panics
/// If another model already claimed one of `imports` in this process (see the module
/// docs), or if called from inside a DPI call.
#[track_caller]
pub fn install<T: ?Sized + 'static>(
    prefix: &'static str,
    imports: &'static [&'static str],
    sig: RawSig,
    imp: Box<T>,
) {
    if let Some(running) = state::in_dpi_call() {
        panic!(
            "DPI-C import `{running}` registered a DPI implementation: DPI functions run \
             inside eval; they can't access signals or await"
        );
    }
    claim(prefix, imports);
    let slot = registered(sig.sim_gen(), prefix);
    *slot.imp.borrow_mut() = Some(Box::new(imp));
}

/// Claims `imports` for `prefix`, panicking on a clash with another model.
#[track_caller]
fn claim(prefix: &'static str, imports: &'static [&'static str]) {
    let mut claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
    for &name in imports {
        let owner = *claims.entry(name).or_insert(prefix);
        assert!(
            owner == prefix,
            "DPI-C import `{name}` is declared by models `{owner}` and `{prefix}`, which are \
             loaded in one process; a DPI-C import is one C symbol per binary, so both would \
             call the same function. Put the two models in separate test binaries, or give \
             one import another C name (`import \"DPI-C\" other_name = function ...`)"
        );
    }
}

/// Runs the implementation of DPI-C import `name` for the current model; what generated
/// trampolines call. `prefix` is the model the trampoline was generated for.
///
/// Never panics: on any failure (see the module docs) it records it for the model and
/// returns `default`.
pub fn call<T: ?Sized + 'static, R>(
    prefix: &'static str,
    name: &'static str,
    default: R,
    f: impl FnOnce(&mut T) -> R,
) -> R {
    let mut default = Some(default);
    let mut f = Some(f);
    let result = THREAD.try_with(|t| {
        let slot = t.current.take()?;
        let result = dispatch(&slot, prefix, name, default.take()?, f.take()?);
        t.current.set(Some(slot));
        Some(result)
    });
    match (result, default) {
        (Ok(Some(result)), _) => result,
        // No current model (or the thread is exiting).
        (_, Some(default)) => {
            orphan(name);
            default
        }
        (_, None) => unreachable!("invariant: `default` is taken only when a slot is current"),
    }
}

fn dispatch<T: ?Sized + 'static, R>(
    slot: &Slot,
    prefix: &'static str,
    name: &'static str,
    default: R,
    f: impl FnOnce(&mut T) -> R,
) -> R {
    if slot.failed.get() {
        return default;
    }
    if slot.ambiguous {
        slot.fail(format!(
            "the design called DPI-C import `{name}`, but two models with prefix `{prefix}` \
             run in one simulation, so `set_dpi` cannot tell their implementations apart"
        ));
        return default;
    }
    // Equal literals are usually merged, so the pointer test settles almost every call.
    if !std::ptr::eq(slot.prefix, prefix) && slot.prefix != prefix {
        slot.fail(format!(
            "model `{}` called DPI-C import `{name}`, but the `{name}` linked into this binary \
             is the one generated for model `{prefix}`; a DPI-C import is one C symbol per \
             binary, so models that import the same name need separate test binaries",
            slot.prefix
        ));
        return default;
    }
    let mut imp = slot.imp.borrow_mut();
    let Some(imp) = imp.as_mut().and_then(|b| b.downcast_mut::<Box<T>>()) else {
        slot.fail(format!(
            "the design called DPI-C import `{name}`, but model `{prefix}` has no DPI \
             implementation; call `dut.set_dpi(..)` before the first eval (before `Sim::run`, \
             or first thing in the test, before any await)"
        ));
        return default;
    };
    let call = state::enter_dpi_call(name);
    panics::forget();
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| f(&mut **imp)));
    let location = panics::take();
    drop(call);
    match outcome {
        Ok(value) => value,
        Err(payload) => {
            let at = location.map(|l| format!(" at {l}")).unwrap_or_default();
            slot.record(DpiFailure {
                message: format!(
                    "DPI-C import `{name}` of model `{prefix}` panicked{at}: {}. The design \
                     received zero as its result, so the model is poisoned and is not \
                     evaluated again",
                    panic_message(&*payload)
                ),
                panicked: true,
                location,
            });
            drop_payload(payload);
            default
        }
    }
}

/// A call with no current model: from C++ outside an eval, from Rust calling the
/// trampoline directly, or from inside another DPI call. Unwinding is not an option (the
/// caller may be C++), and there is no run to fail, so it is reported on stderr.
fn orphan(name: &str) {
    let mut err = std::io::stderr();
    // A failed write to stderr leaves nothing else to report to.
    writeln!(
        err,
        "simjig: DPI-C import `{name}` was called outside a model eval (or from inside \
         another DPI call); it returned zero"
    )
    .ok();
}

/// The text of a panic payload.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s
    } else {
        "a panic with a non-string payload"
    }
}

/// Drops a panic payload without letting a panicking `Drop` unwind into C++.
fn drop_payload(payload: Box<dyn Any + Send>) {
    if let Err(nested) = panic::catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(nested);
    }
}

/// The DPI state of one model instance, owned by its backend.
#[derive(Debug)]
pub(crate) struct ModelDpi {
    prefix: &'static str,
    /// The (simulation id, slot) once the first eval inside a run found it.
    bound: Option<(u32, Rc<Slot>)>,
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("prefix", &self.prefix)
            .field("has_imp", &self.imp.borrow().is_some())
            .field("failure", &self.failure.borrow())
            .finish_non_exhaustive()
    }
}

impl ModelDpi {
    /// The DPI state of a model with prefix `prefix`.
    pub(crate) fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            bound: None,
        }
    }

    /// Runs `f` (an eval or the final blocks) with this model as the current DPI context.
    /// Returns its result and the DPI failure it raised, if any.
    pub(crate) fn scope<R>(&mut self, f: impl FnOnce() -> R) -> (R, Option<DpiFailure>) {
        let slot = self.slot();
        let previous = THREAD.with(|t| t.current.replace(Some(Rc::clone(&slot))));
        let restore = Restore(previous);
        let result = f();
        drop(restore);
        (result, slot.take_failure())
    }

    /// This model's slot: bound to the running simulation on the first eval inside a run,
    /// a private one (no implementation) before that.
    fn slot(&mut self) -> Rc<Slot> {
        if let Some((_, slot)) = &self.bound {
            return Rc::clone(slot);
        }
        let Some(sim_gen) = state::try_current("a model eval", |k| k.sim_gen) else {
            return Slot::new(self.prefix, false);
        };
        let shared = registered(sim_gen, self.prefix);
        let slot = if shared.claimed.replace(true) {
            Slot::new(self.prefix, true)
        } else {
            shared
        };
        self.bound = Some((sim_gen, Rc::clone(&slot)));
        slot
    }
}

impl Drop for ModelDpi {
    fn drop(&mut self) {
        let Some((sim_gen, slot)) = self.bound.take() else {
            return;
        };
        // `try_with`: a model dropped during thread teardown has no registry left.
        SLOTS
            .try_with(|s| {
                let mut s = s.borrow_mut();
                if s.get(&(sim_gen, self.prefix))
                    .is_some_and(|x| Rc::ptr_eq(x, &slot))
                {
                    s.remove(&(sim_gen, self.prefix));
                }
            })
            .ok();
    }
}

/// Restores the previous current slot, also if the scope unwinds.
struct Restore(Option<Rc<Slot>>);

impl Drop for Restore {
    fn drop(&mut self) {
        THREAD.try_with(|t| t.current.set(self.0.take())).ok();
    }
}

#[cfg(test)]
mod tests;

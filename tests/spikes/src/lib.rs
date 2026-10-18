//! Plumbing shared by the Stage 3 spikes (findings: `docs/notes/spikes.md`).
//!
//! Reference material, not production code. It shows the minimum a Rust testbench needs
//! to own a Verilator model:
//! - [`Context`] owns a `VerilatedContext` (time, seed, finish flag, routed output);
//! - [`Model`] owns one model instance, created through its shim's [`VTable`] (one per
//!   `--prefix`, see `shim/model.inc`), and borrows its context so it is dropped first;
//! - [`PrivateRng`] is the workaround for Verilator's per-thread `$urandom` stream;
//! - [`fork`] holds the `fork()` checkpoint helper.
//!
//! Each test file declares the `spk_vtable_<prefix>` functions it uses itself. Declaring
//! them here would make every test binary pull every model, and the DPI model needs
//! symbols only `tests/dpi.rs` defines.

pub mod fork;

use std::ffi::{CString, c_char, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::path::Path;
use std::ptr::NonNull;

/// Opaque `VerilatedContext`.
#[repr(C)]
pub struct VContext {
    _priv: [u8; 0],
    _pin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Opaque `VlRNG`.
#[repr(C)]
pub struct VRng {
    _priv: [u8; 0],
    _pin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Mirror of `SpkVtable` in `shim/spk_runtime.h`. Field order must match.
#[derive(Debug)]
#[repr(C)]
pub struct VTable {
    pub name: *const c_char,
    pub create: unsafe extern "C" fn(*mut VContext) -> *mut c_void,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub eval: unsafe extern "C" fn(*mut c_void),
    pub poke: unsafe extern "C" fn(*mut c_void, u32, u64),
    pub peek: unsafe extern "C" fn(*mut c_void, u32) -> u64,
    pub events_pending: unsafe extern "C" fn(*mut c_void) -> bool,
    pub next_time_slot: unsafe extern "C" fn(*mut c_void) -> u64,
    pub trace_open: unsafe extern "C" fn(*mut c_void, *const c_char),
    pub trace_dump: unsafe extern "C" fn(*mut c_void, u64),
    pub trace_flush: unsafe extern "C" fn(*mut c_void),
    pub trace_close: unsafe extern "C" fn(*mut c_void),
    pub internal_op: unsafe extern "C" fn(*mut c_void, u32, u64) -> u64,
}

unsafe extern "C" {
    fn spk_ctx_new(seed: u32, threads: u32) -> *mut VContext;
    fn spk_ctx_delete(c: *mut VContext);
    fn spk_ctx_time(c: *const VContext) -> u64;
    fn spk_ctx_set_time(c: *mut VContext, t: u64);
    fn spk_ctx_got_finish(c: *const VContext) -> bool;
    fn spk_ctx_make_current(c: *mut VContext);
    safe fn spk_ctx_current() -> *const VContext;
    fn spk_ctx_take_output(c: *const VContext, buf: *mut u8, cap: usize) -> usize;
    fn spk_rng_new(seed: u64) -> *mut VRng;
    fn spk_rng_delete(r: *mut VRng);
    fn spk_rng_swap(r: *mut VRng);
    /// Number of OS threads in this process.
    pub safe fn spk_thread_count() -> usize;
    fn spk_detach_fds_on(path: *const c_char) -> i32;
}

/// Owns a `VerilatedContext`. Construction does not make it current; see [`Self::make_current`].
#[derive(Debug)]
pub struct Context {
    raw: NonNull<VContext>,
}

impl Context {
    /// A new context with `randSeed(seed)`, `fatalOnError(false)` and `threads(1)`.
    ///
    /// `threads(1)` matters: by default a `VerilatedContext` starts a thread pool of
    /// (hardware threads - 1) workers when its first model is added, even for a model
    /// verilated with `--threads 1` (experiment 1).
    pub fn new(seed: u32) -> Self {
        Self::with_threads(seed, 1)
    }

    /// Like [`Self::new`] but leaves Verilator's default thread count (and pool).
    pub fn with_default_threads(seed: u32) -> Self {
        Self::with_threads(seed, 0)
    }

    fn with_threads(seed: u32, threads: u32) -> Self {
        // SAFETY: no preconditions; returns a fresh heap object.
        let raw = unsafe { spk_ctx_new(seed, threads) };
        Self {
            raw: NonNull::new(raw).expect("spk_ctx_new returned null"),
        }
    }

    pub fn raw(&self) -> *mut VContext {
        self.raw.as_ptr()
    }

    pub fn time(&self) -> u64 {
        // SAFETY: `raw` is live for `self`'s lifetime.
        unsafe { spk_ctx_time(self.raw()) }
    }

    pub fn set_time(&self, t: u64) {
        // SAFETY: `raw` is live; single-threaded use.
        unsafe { spk_ctx_set_time(self.raw(), t) }
    }

    pub fn got_finish(&self) -> bool {
        // SAFETY: `raw` is live.
        unsafe { spk_ctx_got_finish(self.raw()) }
    }

    /// `Verilated::threadContextp(this)`: what `$time`, `$urandom` seeding, `$finish`
    /// and `$display` routing use, whatever model is being evaluated.
    pub fn make_current(&self) {
        // SAFETY: `raw` is live.
        unsafe { spk_ctx_make_current(self.raw()) }
    }

    pub fn is_current(&self) -> bool {
        spk_ctx_current() == self.raw().cast_const()
    }

    /// Takes the text `spk_printf` routed to this context since the last call.
    pub fn take_output(&self) -> String {
        let mut buf = vec![0u8; 1 << 16];
        // SAFETY: `raw` is live; `buf` is writable for `buf.len()` bytes.
        let n = unsafe { spk_ctx_take_output(self.raw(), buf.as_mut_ptr(), buf.len()) };
        buf.truncate(n.min(buf.len()));
        String::from_utf8_lossy(&buf).into_owned()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: every `Model` borrowing this context has been dropped (lifetime 'c).
        unsafe { spk_ctx_delete(self.raw()) }
    }
}

/// Owns one model instance. Borrows its [`Context`], so it is destroyed first.
#[derive(Debug)]
pub struct Model<'c> {
    vt: &'static VTable,
    inst: NonNull<c_void>,
    _ctx: PhantomData<&'c Context>,
}

impl<'c> Model<'c> {
    /// # Safety
    /// `vt` must come from a `spk_vtable_<prefix>()` function of a linked shim.
    pub unsafe fn new(vt: *const VTable, ctx: &'c Context) -> Self {
        // SAFETY: the caller guarantees `vt` is a static table from a shim.
        let vt: &'static VTable = unsafe { &*vt };
        // SAFETY: `ctx` is live for 'c, which outlives the model.
        let inst = unsafe { (vt.create)(ctx.raw()) };
        Self {
            vt,
            inst: NonNull::new(inst).expect("create returned null"),
            _ctx: PhantomData,
        }
    }

    fn p(&self) -> *mut c_void {
        self.inst.as_ptr()
    }

    pub fn eval(&mut self) {
        // SAFETY: `inst` is live; no Rust reference into model memory exists.
        unsafe { (self.vt.eval)(self.p()) }
    }

    /// Writes a port by its shim id (`SPK_PORTS` order). No masking: spikes stay in range.
    pub fn poke(&mut self, port: u32, value: u64) {
        // SAFETY: `inst` is live; the shim aborts on an unknown id.
        unsafe { (self.vt.poke)(self.p(), port, value) }
    }

    pub fn peek(&self, port: u32) -> u64 {
        // SAFETY: `inst` is live; the shim aborts on an unknown id.
        unsafe { (self.vt.peek)(self.p(), port) }
    }

    pub fn events_pending(&mut self) -> bool {
        // SAFETY: `inst` is live.
        unsafe { (self.vt.events_pending)(self.p()) }
    }

    /// Calls `nextTimeSlot()` with no check: experiment 4 shows what happens when
    /// `events_pending()` is false (the process exits).
    pub fn next_time_slot_unchecked(&mut self) -> u64 {
        // SAFETY: `inst` is live. Verilator may call exit(1) here; that is the experiment.
        unsafe { (self.vt.next_time_slot)(self.p()) }
    }

    pub fn trace_open(&mut self, path: &Path) {
        let c = CString::new(path.to_str().expect("utf-8 path")).expect("no NUL");
        // SAFETY: `inst` is live; `c` outlives the call.
        unsafe { (self.vt.trace_open)(self.p(), c.as_ptr()) }
    }

    pub fn trace_dump(&mut self, time: u64) {
        // SAFETY: `inst` is live.
        unsafe { (self.vt.trace_dump)(self.p(), time) }
    }

    pub fn trace_flush(&mut self) {
        // SAFETY: `inst` is live.
        unsafe { (self.vt.trace_flush)(self.p()) }
    }

    pub fn trace_close(&mut self) {
        // SAFETY: `inst` is live.
        unsafe { (self.vt.trace_close)(self.p()) }
    }

    /// Model-specific operation on internals; op codes are documented in each shim.
    pub fn internal_op(&mut self, op: u32, arg: u64) -> u64 {
        // SAFETY: `inst` is live; the shim aborts on an unknown op.
        unsafe { (self.vt.internal_op)(self.p(), op, arg) }
    }
}

impl Drop for Model<'_> {
    fn drop(&mut self) {
        // SAFETY: `inst` came from `create` and is destroyed exactly once.
        unsafe { (self.vt.destroy)(self.p()) }
    }
}

/// A private `$urandom` stream for one model.
///
/// Verilator 5.052 draws `$urandom` from a `thread_local` `VlRNG`, so every model
/// evaluated on one thread shares one stream. Wrapping each eval in
/// [`PrivateRng::swap`] (before and after) restores per-model streams.
#[derive(Debug)]
pub struct PrivateRng {
    raw: NonNull<VRng>,
}

impl PrivateRng {
    pub fn new(seed: u64) -> Self {
        // SAFETY: no preconditions.
        let raw = unsafe { spk_rng_new(seed) };
        Self {
            raw: NonNull::new(raw).expect("spk_rng_new returned null"),
        }
    }

    /// Exchanges this state with the thread's `$urandom` state.
    pub fn swap(&mut self) {
        // SAFETY: `raw` is live; single-threaded.
        unsafe { spk_rng_swap(self.raw.as_ptr()) }
    }
}

impl Drop for PrivateRng {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `spk_rng_new`.
        unsafe { spk_rng_delete(self.raw.as_ptr()) }
    }
}

/// Points every descriptor open on `path` at /dev/null; returns how many it moved.
pub fn detach_fds_on(path: &Path) -> i32 {
    let c = CString::new(path.to_str().expect("utf-8 path")).expect("no NUL");
    // SAFETY: `c` is a valid C string for the call.
    unsafe { spk_detach_fds_on(c.as_ptr()) }
}

/// One full clock cycle on port `clk`: rise, eval, fall, eval.
pub fn cycle(m: &mut Model<'_>, clk: u32) {
    m.poke(clk, 1);
    m.eval();
    m.poke(clk, 0);
    m.eval();
}

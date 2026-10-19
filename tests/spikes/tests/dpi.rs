//! Experiment 3: DPI-C imports implemented in Rust, called from inside `eval()`.
//!
//! tests/duts/dpi imports `dpi_add` (pure) and `dpi_tick` (context). Here they are
//! `#[unsafe(no_mangle)] extern "C"` functions in this test binary; dpi_impl.c is not
//! linked. The rule they demonstrate: a Rust panic must never cross the `extern "C"`
//! boundary. The function catches it, parks the payload, returns a dummy value to the
//! model, and the testbench re-raises it after `eval()` returns.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use simjig_spikes::fork::{ChildEnd, in_child};
use simjig_spikes::{Context, Model, VTable, cycle};

unsafe extern "C" {
    fn spk_vtable_Vdpi() -> *const VTable;
}

// dpi ports (shim/dpi.cpp).
const CLK: u32 = 0;
const A: u32 = 1;
const B: u32 = 2;
const SUM: u32 = 3;
const CALLS: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PanicMode {
    Off,
    /// Panic inside the guard: caught, parked, re-raised after eval.
    Caught,
    /// Panic outside any guard: the `extern "C"` boundary aborts the process.
    Uncaught,
}

thread_local! {
    static MODE: Cell<PanicMode> = const { Cell::new(PanicMode::Off) };
    static ADD_CALLS: Cell<u64> = const { Cell::new(0) };
    static TICKS: Cell<i32> = const { Cell::new(0) };
    static PARKED: RefCell<Option<Box<dyn Any + Send>>> = const { RefCell::new(None) };
}

/// Runs `f`, parking a panic instead of letting it unwind into C++.
fn guard<R>(f: impl FnOnce() -> R) -> Option<R> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(payload) => {
            PARKED.with(|p| *p.borrow_mut() = Some(payload));
            None
        }
    }
}

/// Call after every eval: resumes a panic parked by a DPI function.
fn reraise_parked_panic() {
    if let Some(payload) = PARKED.with(|p| p.borrow_mut().take()) {
        panic::resume_unwind(payload);
    }
}

/// `import "DPI-C" pure function int dpi_add(input int x, input int y)`.
#[unsafe(no_mangle)]
pub extern "C" fn dpi_add(x: i32, y: i32) -> i32 {
    ADD_CALLS.set(ADD_CALLS.get() + 1);
    let mode = MODE.get();
    assert!(mode != PanicMode::Uncaught, "uncaught panic in dpi_add");
    guard(|| {
        assert!(mode != PanicMode::Caught, "caught panic in dpi_add");
        x.wrapping_add(y)
    })
    .unwrap_or(0)
}

/// `import "DPI-C" context function int dpi_tick()`.
#[unsafe(no_mangle)]
pub extern "C" fn dpi_tick() -> i32 {
    TICKS.set(TICKS.get() + 1);
    TICKS.get()
}

/// Same body as `dpi_add` without any guard: the baseline for the overhead measurement.
#[unsafe(no_mangle)]
pub extern "C" fn spk_rust_add_plain(x: i32, y: i32) -> i32 {
    x.wrapping_add(y)
}

fn dpi(ctx: &Context) -> Model<'_> {
    // SAFETY: table from the linked Vdpi shim.
    let mut m = unsafe { Model::new(spk_vtable_Vdpi(), ctx) };
    m.eval();
    m
}

#[test]
fn rust_dpi_functions_run_inside_eval() {
    let ctx = Context::new(1);
    let mut m = dpi(&ctx);
    m.poke(A, 2);
    m.poke(B, 3);
    m.eval();
    assert_eq!(m.peek(SUM), 5);
    for _ in 0..5 {
        cycle(&mut m, CLK);
    }
    assert_eq!((m.peek(CALLS), TICKS.get()), (5, 5));
}

#[test]
fn panic_in_dpi_is_caught_on_the_rust_side() {
    let ctx = Context::new(1);
    let mut m = dpi(&ctx);
    m.poke(B, 3);
    MODE.set(PanicMode::Caught);
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        m.poke(A, 10);
        m.eval();
        reraise_parked_panic();
    }));
    let msg = r.expect_err("panic re-raised after eval");
    let msg = msg
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| msg.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(msg.contains("caught panic in dpi_add"), "{msg}");
    // eval() completed with the dummy return value: the model's state is now wrong,
    // which is why simjig poisons the model after a DPI panic.
    assert_eq!(m.peek(SUM), 0);

    MODE.set(PanicMode::Off);
    m.poke(A, 1);
    m.eval();
    reraise_parked_panic();
    assert_eq!(m.peek(SUM), 4);
}

#[test]
fn uncaught_panic_in_an_extern_c_dpi_function_aborts() {
    let ctx = Context::new(1);
    let mut m = dpi(&ctx);
    let child = in_child(|| {
        MODE.set(PanicMode::Uncaught);
        m.poke(A, 7);
        m.eval();
        "survived".to_owned()
    });
    eprintln!("SPIKE uncaught DPI panic: child ended {:?}", child.end);
    assert_eq!(child.end, ChildEnd::Signaled(6), "SIGABRT");
}

#[test]
fn dpi_call_overhead() {
    let ctx = Context::new(1);
    let mut m = dpi(&ctx);
    let n: u64 = 20_000_000;
    // Best of 5 runs of n calls through a volatile function pointer.
    let mut per_call = |op: u32| {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                assert_eq!(m.internal_op(op, n), n);
                t.elapsed().as_secs_f64() * 1e9 / n as f64
            })
            .fold(f64::INFINITY, f64::min)
    };
    let (cxx, rust_plain, rust_guarded) = (per_call(1), per_call(2), per_call(0));

    // Whole evals: one dpi_add call each (a changes every time).
    let evals = 2_000_000u64;
    let before = ADD_CALLS.get();
    let t = Instant::now();
    for i in 1..=evals {
        m.poke(A, i);
        m.eval();
    }
    let eval_ns = t.elapsed().as_secs_f64() * 1e9 / evals as f64;
    let calls_per_eval = (ADD_CALLS.get() - before) as f64 / evals as f64;
    eprintln!(
        "SPIKE dpi ns/call: C++->C++ {cxx:.2}, C++->Rust plain {rust_plain:.2}, \
         C++->Rust guarded {rust_guarded:.2}; eval with one dpi_add call {eval_ns:.1} ns \
         ({calls_per_eval} calls/eval)"
    );
    assert_eq!(calls_per_eval, 1.0);
}

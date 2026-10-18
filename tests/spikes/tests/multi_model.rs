//! Experiment 2: several models in one process, each with its own `VerilatedContext`,
//! evaluated alternately on one thread.
//!
//! - `Vrng_a`/`Vrng_b`: the same design (rtl/rng_disp.sv) under two `--prefix`es;
//! - `Vcounter`, `Vfatal`: two different designs from tests/duts.
//!
//! Finding in one line: a model's *own* context is used only for what the generated code
//! reaches through `vlSymsp->_vm_contextp__`; `$time`, `$finish`, `$display` and the
//! `$urandom` seed go through `Verilated::threadContextp()`, and the `$urandom` *state* is
//! per thread. The tests below pin each of these down.

use std::path::{Path, PathBuf};
use std::process::Command;

use simjig_spikes::{Context, Model, PrivateRng, VTable, cycle};

unsafe extern "C" {
    fn spk_vtable_Vrng_a() -> *const VTable;
    fn spk_vtable_Vrng_b() -> *const VTable;
    fn spk_vtable_Vcounter() -> *const VTable;
    fn spk_vtable_Vfatal() -> *const VTable;
}

// rng_disp ports.
const R_CLK: u32 = 0;
const R_RND: u32 = 1;

/// The first eval only initialises the trigger state (see
/// `first_eval_never_sees_an_edge`), so every model is settled once with clk low.
fn settled(mut m: Model<'_>) -> Model<'_> {
    m.eval();
    m
}

fn rng_a(ctx: &Context) -> Model<'_> {
    // SAFETY: table from the linked Vrng_a shim.
    settled(unsafe { Model::new(spk_vtable_Vrng_a(), ctx) })
}

fn rng_b(ctx: &Context) -> Model<'_> {
    // SAFETY: table from the linked Vrng_b shim.
    settled(unsafe { Model::new(spk_vtable_Vrng_b(), ctx) })
}

#[test]
fn first_eval_never_sees_an_edge() {
    let ctx = Context::new(1);
    // SAFETY: table from the linked Vrng_a shim.
    let mut m = unsafe { Model::new(spk_vtable_Vrng_a(), &ctx) };
    m.poke(R_CLK, 1);
    m.eval(); // clk was 0 at construction, but this eval records 1 as the previous value
    assert_eq!((m.peek(R_RND), ctx.take_output()), (0, String::new()));
    m.poke(R_CLK, 0);
    m.eval();
    m.poke(R_CLK, 1);
    m.eval();
    assert_ne!(m.peek(R_RND), 0);
    assert!(ctx.take_output().starts_with("rng_disp"));
}

/// One posedge (one `$urandom` draw and one `$display`), then the negedge.
fn draw(ctx: &Context, m: &mut Model<'_>, switch_context: bool) -> u64 {
    if switch_context {
        ctx.make_current();
    }
    cycle(m, R_CLK);
    m.peek(R_RND)
}

#[test]
fn same_design_twice_and_two_designs_alternate_on_one_thread() {
    let (ca, cb, cc) = (Context::new(1), Context::new(2), Context::new(3));
    let (mut a, mut b) = (rng_a(&ca), rng_b(&cb));
    // SAFETY: table from the linked Vcounter shim.
    let mut c = unsafe { Model::new(spk_vtable_Vcounter(), &cc) };
    c.poke(1, 1);
    cycle(&mut c, 0);
    c.poke(1, 0);
    c.poke(2, 1);
    for i in 1..=50u64 {
        draw(&ca, &mut a, true);
        draw(&cb, &mut b, true);
        cc.make_current();
        cycle(&mut c, 0);
        assert_eq!(c.peek(3), i);
    }
}

#[test]
fn display_and_time_follow_thread_context_not_the_models_context() {
    let ca = Context::new(1);
    let mut a = rng_a(&ca);
    let cb = Context::new(2);
    let mut b = rng_b(&cb); // constructing b made cb the thread context
    ca.set_time(1000);
    cb.set_time(2000);
    assert!(cb.is_current());

    // No switch: A's $display lands in B's buffer and prints B's time.
    draw(&ca, &mut a, false);
    let (out_a, out_b) = (ca.take_output(), cb.take_output());
    eprintln!("SPIKE no switch: ctx A got {out_a:?}, ctx B got {out_b:?}");
    assert_eq!(out_a, "");
    assert!(out_b.starts_with("rng_disp t=2000 "), "{out_b:?}");

    // Switch before every eval: each model sees its own time and output channel.
    draw(&ca, &mut a, true);
    draw(&cb, &mut b, true);
    let (out_a, out_b) = (ca.take_output(), cb.take_output());
    eprintln!("SPIKE switch: ctx A got {out_a:?}, ctx B got {out_b:?}");
    assert!(out_a.starts_with("rng_disp t=1000 "), "{out_a:?}");
    assert!(out_b.starts_with("rng_disp t=2000 "), "{out_b:?}");
}

#[test]
fn finish_sets_the_thread_contexts_flag() {
    let cf = Context::new(1);
    // SAFETY: table from the linked Vfatal shim.
    let mut f = unsafe { Model::new(spk_vtable_Vfatal(), &cf) };
    f.eval();
    let cc = Context::new(2);
    // SAFETY: table from the linked Vcounter shim.
    let _c = unsafe { Model::new(spk_vtable_Vcounter(), &cc) };
    // fatal ports: clk 0, rst 1, fire 2, kind 3 (3 = $finish).
    f.poke(2, 1);
    f.poke(3, 3);
    cycle(&mut f, 0); // thread context is cc (constructed last)
    eprintln!(
        "SPIKE $finish without switch: fatal ctx={} counter ctx={}",
        cf.got_finish(),
        cc.got_finish()
    );
    assert!(!cf.got_finish());
    assert!(cc.got_finish(), "the wrong context received $finish");

    let cf2 = Context::new(3);
    // SAFETY: as above.
    let mut f2 = unsafe { Model::new(spk_vtable_Vfatal(), &cf2) };
    f2.eval();
    f2.poke(2, 1);
    f2.poke(3, 3);
    cf2.make_current();
    cycle(&mut f2, 0);
    assert!(cf2.got_finish());
}

/// Draws `n` values from A, optionally interleaving a draw from B after each one.
fn stream_a(n: usize, interleave_b: bool, private: bool) -> Vec<u64> {
    let (ca, cb) = (Context::new(7), Context::new(9));
    let (mut a, mut b) = (rng_a(&ca), rng_b(&cb));
    let (mut ra, mut rb) = (PrivateRng::new(7), PrivateRng::new(9));
    let eval = |ctx: &Context, m: &mut Model<'_>, r: &mut PrivateRng| {
        if private {
            r.swap();
        }
        let v = draw(ctx, m, true);
        if private {
            r.swap();
        }
        v
    };
    (0..n)
        .map(|_| {
            let v = eval(&ca, &mut a, &mut ra);
            if interleave_b {
                eval(&cb, &mut b, &mut rb);
            }
            v
        })
        .collect()
}

#[test]
fn urandom_stream_is_per_thread_unless_swapped_per_model() {
    // Verilator's own stream: B's draws consume A's values.
    let alone = stream_a(20, false, false);
    let shared = stream_a(20, true, false);
    eprintln!(
        "SPIKE urandom A alone {:x?}...; A with B interleaved {:x?}...",
        &alone[..3],
        &shared[..3]
    );
    assert_ne!(alone, shared, "streams turned out independent");
    // Every value A saw while interleaved is in the alone stream, two steps apart: one
    // stream, dealt out alternately.
    assert_eq!(shared[1..5], [alone[2], alone[4], alone[6], alone[8]]);

    // With a private VlRNG swapped in around each eval, A is independent of B.
    assert_eq!(stream_a(20, false, true), stream_a(20, true, true));
}

// ---- link-time symbol collisions ---------------------------------------------------

fn lib(name: &str) -> PathBuf {
    Path::new(env!("SPK_LIB_DIR")).join(name)
}

/// Strong (non-weak) external symbols an archive defines, via `nm`. Weak definitions
/// (inline functions, templates, vtables) are merged by the linker and never collide, so
/// they are excluded. macOS (cctools) `nm -gUm` spells them "weak external"; GNU nm uses W/V.
fn strong_symbols(archive: &Path) -> std::collections::BTreeSet<String> {
    let darwin = cfg!(target_os = "macos");
    let args: &[&str] = if darwin {
        &["-g", "-U", "-m"]
    } else {
        &["-g", "--defined-only"]
    };
    let out = Command::new("nm")
        .args(args)
        .arg(archive)
        .output()
        .expect("nm");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            if darwin {
                l.contains(" external ") && !l.contains("weak")
            } else {
                let ty = l.split_whitespace().nth(1).unwrap_or("");
                ty.len() == 1
                    && ty.chars().all(|c| c.is_ascii_uppercase())
                    && !matches!(ty, "W" | "V" | "U")
            }
        })
        .filter_map(|l| l.split_whitespace().last().map(str::to_owned))
        .collect()
}

#[test]
fn prefixed_models_share_no_strong_symbols_with_each_other_or_libverilated() {
    let a = strong_symbols(&lib("libspk_Vrng_a.a"));
    let b = strong_symbols(&lib("libspk_Vrng_b.a"));
    let counter = strong_symbols(&lib("libspk_Vcounter.a"));
    let dup = strong_symbols(&lib("libdup_Vcounter.a"));
    let verilated = strong_symbols(&lib("libspk_verilated.a"));
    assert!(!a.is_empty() && !verilated.is_empty());
    let ab: Vec<_> = a.intersection(&b).collect();
    let av: Vec<_> = a.intersection(&verilated).collect();
    let cd = counter.intersection(&dup).count();
    eprintln!(
        "SPIKE symbols: Vrng_a={} Vrng_b={} common={ab:?}; Vrng_a&verilated={av:?}; \
         Vcounter&dup(same prefix)={cd} of {}",
        a.len(),
        b.len(),
        counter.len()
    );
    assert!(ab.is_empty());
    assert!(av.is_empty());
    assert_eq!(
        cd,
        counter.len(),
        "same prefix twice collides on every symbol"
    );
}

/// Links a trivial program with every member of each archive forced in, so duplicate
/// strong symbols must be reported rather than silently resolved from the first archive.
fn force_link(archives: &[&str]) -> (bool, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let main = dir.join("collide_main.c");
    std::fs::write(&main, "int main(void) { return 0; }\n").expect("write main");
    let mut cmd = Command::new(env!("SPK_CXX"));
    cmd.arg("-x")
        .arg("c")
        .arg(&main)
        .arg("-x")
        .arg("none")
        .arg("-o")
        .arg(dir.join("collide"));
    for a in archives {
        if cfg!(target_os = "macos") {
            cmd.arg(format!("-Wl,-force_load,{}", lib(a).display()));
        } else {
            cmd.arg("-Wl,--whole-archive")
                .arg(lib(a))
                .arg("-Wl,--no-whole-archive");
        }
    }
    let conda = std::env::var("CONDA_PREFIX").unwrap_or_default();
    cmd.arg(format!("-L{conda}/lib"))
        .args(["-llz4", "-lz", "-lpthread"]);
    if cfg!(target_os = "macos") {
        cmd.arg("-Wl,-U,__Z15vl_time_stamp64v,-U,__Z13sc_time_stampv,-U,_vlog_startup_routines");
    }
    let out = cmd.output().expect("link");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn whole_archive_link_detects_a_duplicate_prefix_but_accepts_distinct_ones() {
    let (ok, err) = force_link(&["libspk_Vrng_a.a", "libspk_Vrng_b.a", "libspk_verilated.a"]);
    assert!(ok, "distinct prefixes + one libverilated must link:\n{err}");
    let (ok, err) = force_link(&[
        "libspk_Vcounter.a",
        "libdup_Vcounter.a",
        "libspk_verilated.a",
    ]);
    let first = err.lines().find(|l| l.contains("duplicate")).unwrap_or("");
    eprintln!("SPIKE duplicate prefix link: ok={ok} first error: {first}");
    assert!(!ok);
}

#[test]
fn creating_a_context_mid_run_reseeds_the_thread_stream() {
    let ca = Context::new(7);
    let mut a = rng_a(&ca);
    let first: Vec<u64> = (0..3).map(|_| draw(&ca, &mut a, true)).collect();
    let late = Context::new(9); // e.g. a second model created later in the test
    let next: Vec<u64> = (0..3).map(|_| draw(&ca, &mut a, true)).collect();
    eprintln!("SPIKE reseed: before new context {first:x?}, after {next:x?}");
    // The epoch bump makes the next draw reseed from the thread context (A, seed 7),
    // so A's stream restarts from the beginning.
    assert_eq!(first, next);
    drop(late);

    // A private stream swapped in around each eval is immune to the epoch bump.
    let cp = Context::new(7);
    let mut p = rng_a(&cp);
    let mut rng = PrivateRng::new(7);
    let mut private_draw = |m: &mut Model<'_>| {
        rng.swap();
        let v = draw(&cp, m, true);
        rng.swap();
        v
    };
    let first: Vec<u64> = (0..3).map(|_| private_draw(&mut p)).collect();
    let _late = Context::new(9);
    let next: Vec<u64> = (0..3).map(|_| private_draw(&mut p)).collect();
    assert_ne!(first, next);
}

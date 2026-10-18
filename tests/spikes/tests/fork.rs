//! Experiment 1: `fork()` checkpoints of a `--threads 1` model, inside a nextest test.
//!
//! Lines starting with `SPIKE` are the measurements quoted in docs/notes/spikes.md; run
//! with `--no-capture` to see them.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use simjig_spikes::fork::{ChildEnd, in_child};
use simjig_spikes::{Context, Model, VTable, cycle, detach_fds_on, spk_thread_count};

unsafe extern "C" {
    fn spk_vtable_Vcounter() -> *const VTable;
    fn spk_vtable_Vbigfifo() -> *const VTable;
}

// Vcounter ports (shim/counter.cpp).
const CLK: u32 = 0;
const RST: u32 = 1;
const EN: u32 = 2;
const COUNT: u32 = 3;

fn counter(ctx: &Context) -> Model<'_> {
    // SAFETY: the table comes from the linked Vcounter shim.
    unsafe { Model::new(spk_vtable_Vcounter(), ctx) }
}

/// One clock cycle, 10 time units, with trace dumps after each eval.
fn tick(ctx: &Context, m: &mut Model<'_>) {
    for level in [1, 0] {
        ctx.set_time(ctx.time() + 5);
        m.poke(CLK, level);
        m.eval();
        m.trace_dump(ctx.time());
    }
}

fn run(ctx: &Context, m: &mut Model<'_>, cycles: u32, en: u64) {
    m.poke(EN, en);
    for _ in 0..cycles {
        tick(ctx, m);
    }
}

fn reset(ctx: &Context, m: &mut Model<'_>) {
    m.poke(RST, 1);
    run(ctx, m, 1, 0);
    m.poke(RST, 0);
}

#[test]
fn child_diverges_and_parent_keeps_its_state() {
    let ctx = Context::new(1);
    let mut m = counter(&ctx);
    reset(&ctx, &mut m);
    run(&ctx, &mut m, 10, 1);
    let (count_at_fork, time_at_fork) = (m.peek(COUNT), ctx.time());
    assert_eq!(count_at_fork, 10);

    // Three branches from the same checkpoint, each with different stimulus.
    let branches: [(&str, u64); 3] = [
        ("reset+7", 7),
        ("hold5", 10),
        ("count300", (10 + 300) % 256),
    ];
    for (name, expected) in branches {
        let child = in_child(|| {
            let start = m.peek(COUNT);
            match name {
                "reset+7" => {
                    reset(&ctx, &mut m);
                    run(&ctx, &mut m, 7, 1);
                }
                "hold5" => run(&ctx, &mut m, 5, 0),
                _ => run(&ctx, &mut m, 300, 1),
            }
            format!("{start} {}", m.peek(COUNT))
        });
        assert_eq!(child.end, ChildEnd::Exited(0), "{name}");
        assert_eq!(child.report, format!("10 {expected}"), "{name}");
        // The parent is untouched by the child's divergence.
        assert_eq!(
            (m.peek(COUNT), ctx.time()),
            (count_at_fork, time_at_fork),
            "{name}"
        );
    }

    // The parent continues exactly like a run that never forked.
    run(&ctx, &mut m, 5, 1);
    let ref_ctx = Context::new(1);
    let mut reference = counter(&ref_ctx);
    reset(&ref_ctx, &mut reference);
    run(&ref_ctx, &mut reference, 15, 1);
    assert_eq!(
        (m.peek(COUNT), ctx.time()),
        (reference.peek(COUNT), ref_ctx.time())
    );
}

#[test]
fn only_the_forking_thread_survives() {
    let parent = spk_thread_count();
    let child = in_child(|| spk_thread_count().to_string());
    eprintln!(
        "SPIKE threads: parent={parent} child={} (runner: {})",
        child.report,
        runner()
    );
    assert_eq!(child.end, ChildEnd::Exited(0));
    assert_eq!(child.report, "1");
}

#[test]
fn default_context_starts_a_thread_pool_even_for_threads_1_models() {
    let base = spk_thread_count();
    let t = Instant::now();
    let default_ctx = Context::with_default_threads(1);
    let mut pooled = counter(&default_ctx);
    let pooled_setup = t.elapsed();
    let with_pool = spk_thread_count();
    let t = Instant::now();
    let single_ctx = Context::new(1);
    let _single = counter(&single_ctx);
    let single_setup = t.elapsed();
    let after_single = spk_thread_count();
    eprintln!(
        "SPIKE pool: base={base} +default ctx/model={with_pool} ({pooled_setup:?}) \
         +threads(1) ctx/model={after_single} ({single_setup:?})"
    );
    assert!(with_pool > base || std::thread::available_parallelism().unwrap().get() == 1);
    assert_eq!(after_single, with_pool, "threads(1) must not add threads");

    // A child forked with the pool alive has one thread, and a --threads 1 model still
    // evaluates there (it never dispatches to the pool), but the pool object is now a
    // set of dead threads: it must never be used or destroyed in the child.
    reset(&default_ctx, &mut pooled);
    let child = in_child(|| {
        run(&default_ctx, &mut pooled, 4, 1);
        format!("{} {}", spk_thread_count(), pooled.peek(COUNT))
    });
    assert_eq!(child.end, ChildEnd::Exited(0));
    assert_eq!(child.report, "1 4");
}

#[test]
fn panic_in_child_is_contained() {
    let ctx = Context::new(1);
    let mut m = counter(&ctx);
    reset(&ctx, &mut m);
    let child = in_child(|| panic!("branch failed"));
    assert_eq!(child.end, ChildEnd::Exited(101));
    run(&ctx, &mut m, 3, 1);
    assert_eq!(m.peek(COUNT), 3);
}

fn runner() -> &'static str {
    if std::env::var_os("NEXTEST").is_some() {
        "nextest"
    } else {
        "libtest"
    }
}

/// Times one fork of the current process (child exits at once), median of 5.
fn fork_latency() -> Duration {
    let mut v: Vec<Duration> = (0..5).map(|_| in_child(String::new).fork_latency).collect();
    v.sort();
    v[2]
}

#[test]
fn fork_and_copy_on_write_cost_with_a_64_mib_model() {
    // SAFETY: the table comes from the linked Vbigfifo shim.
    let ctx = Context::new(1);
    let mut m = unsafe { Model::new(spk_vtable_Vbigfifo(), &ctx) };
    let bytes = m.internal_op(0, 0);
    assert_eq!(bytes, 64 << 20);
    m.poke(1, 1);
    cycle(&mut m, 0);
    m.poke(1, 0);

    // Baseline: dirty every page of the model memory when it is already private.
    m.internal_op(1, 1);
    let t = Instant::now();
    let pages = m.internal_op(1, 2);
    let private_write = t.elapsed();
    let parent_sum = m.internal_op(2, 0);

    let child = in_child(|| {
        let t = Instant::now();
        m.internal_op(1, 3); // every write is a copy-on-write fault
        let cow = t.elapsed();
        let t = Instant::now();
        m.internal_op(1, 4); // pages are private again
        let again = t.elapsed();
        format!("{} {}", cow.as_nanos(), again.as_nanos())
    });
    assert_eq!(child.end, ChildEnd::Exited(0));
    // The child's 3s and 4s never reach the parent's memory.
    assert_eq!(m.internal_op(2, 0), parent_sum);
    let (cow, again) = child.report.split_once(' ').expect("two numbers");
    let cow_ns: u64 = cow.parse().expect("ns");
    eprintln!(
        "SPIKE cow: pages={pages} (4 KiB strides) private_write={private_write:?} \
         child_cow_write={cow_ns}ns ({:.0} ns/4KiB) child_rewrite={again}ns fork={:?}",
        cow_ns as f64 / pages as f64,
        child.fork_latency
    );

    // fork() latency against resident set size: model alone, then with ballast.
    eprintln!("SPIKE fork latency: model 64 MiB -> {:?}", fork_latency());
    for mib in [256usize, 1024] {
        let ballast: Vec<u8> = vec![1; mib << 20]; // vec![1; n] touches every page
        eprintln!(
            "SPIKE fork latency: model + {mib} MiB -> {:?}",
            fork_latency()
        );
        assert_eq!(ballast[mib << 19], 1);
    }
}

// ---- FST trace open across fork ------------------------------------------------------

/// Walks the FST block chain (u8 type, u64 big-endian length that includes itself) and
/// returns (block types, header end_time). A file passes only if the chain ends exactly
/// at EOF, every block type is one the FST format defines, and the header comes first.
/// (block types in file order, header end_time), or why the file is not valid FST.
type FstCheck = Result<(Vec<u8>, u64), String>;

fn fst_check(path: &PathBuf) -> FstCheck {
    let b = fs::read(path).map_err(|e| e.to_string())?;
    let mut types = Vec::new();
    let mut at = 0usize;
    while at < b.len() {
        let ty = b[at];
        let len_bytes: [u8; 8] = b
            .get(at + 1..at + 9)
            .ok_or("truncated length")?
            .try_into()
            .unwrap();
        let len = usize::try_from(u64::from_be_bytes(len_bytes)).map_err(|e| e.to_string())?;
        if !matches!(ty, 0..=8 | 254 | 255) || len < 8 || at + 1 + len > b.len() {
            return Err(format!(
                "bad block type {ty} len {len} at offset {at} of {}",
                b.len()
            ));
        }
        types.push(ty);
        at += 1 + len;
    }
    if types.first() != Some(&0) {
        return Err(format!(
            "first block is {:?}, not the header",
            types.first()
        ));
    }
    // Header payload: start_time u64, end_time u64 (big-endian), right after type+len.
    let end = u64::from_be_bytes(b[17..25].try_into().unwrap());
    Ok((types, end))
}

fn trace_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let p = dir.join(format!("fork-{name}.fst"));
    let _ = fs::remove_file(&p);
    p
}

/// What the forked child does with the trace it inherited.
#[derive(Clone, Copy, Debug)]
enum ChildTrace {
    /// Never touches it and `_exit`s.
    Ignore,
    /// Dumps its own cycles into it and closes it (what a normal destructor would do).
    DumpAndClose,
    /// Points the inherited descriptor at /dev/null, closes the inherited writer, and
    /// opens its own file.
    DetachAndReopen,
}

/// Parent: trace 20 cycles, flush, fork, wait for the child, trace 20 more, close.
/// Returns the parent's file check and the child's file check (if it wrote one).
fn fst_scenario(name: &str, mode: ChildTrace) -> (FstCheck, Option<FstCheck>) {
    let ctx = Context::new(1);
    let mut m = counter(&ctx);
    let path = trace_path(name);
    let child_path = trace_path(&format!("{name}-child"));
    m.trace_open(&path);
    reset(&ctx, &mut m);
    run(&ctx, &mut m, 20, 1);
    m.trace_flush();
    let threads = spk_thread_count();
    let child = in_child(|| {
        match mode {
            ChildTrace::Ignore => run(&ctx, &mut m, 50, 0),
            ChildTrace::DumpAndClose => {
                run(&ctx, &mut m, 50, 0);
                m.trace_close();
            }
            ChildTrace::DetachAndReopen => {
                detach_fds_on(&path);
                m.trace_close();
                m.trace_open(&child_path);
                run(&ctx, &mut m, 50, 0);
                m.trace_close();
            }
        }
        String::new()
    });
    assert_eq!(child.end, ChildEnd::Exited(0));
    run(&ctx, &mut m, 20, 1);
    m.trace_close();
    eprintln!("SPIKE fst {name}: threads with trace open = {threads}");
    let child_file = matches!(mode, ChildTrace::DetachAndReopen).then(|| fst_check(&child_path));
    (fst_check(&path), child_file)
}

/// 1 reset cycle + 20 + 20 cycles of 10 units: the last dump is at t = 410.
const PARENT_END: u64 = 410;

#[test]
fn fst_reference_without_fork_is_valid() {
    let ctx = Context::new(1);
    let mut m = counter(&ctx);
    let path = trace_path("reference");
    m.trace_open(&path);
    reset(&ctx, &mut m);
    run(&ctx, &mut m, 40, 1);
    m.trace_close();
    let (types, end) = fst_check(&path).expect("reference must be valid");
    eprintln!("SPIKE fst reference: blocks {types:?} end_time {end}");
    assert_eq!(end, PARENT_END);
}

#[test]
fn fst_child_that_ignores_the_trace_leaves_the_parent_file_valid() {
    let (parent, _) = fst_scenario("ignore", ChildTrace::Ignore);
    let (_, end) = parent.expect("parent file valid");
    assert_eq!(end, PARENT_END);
}

#[test]
fn fst_child_that_closes_the_inherited_trace_corrupts_the_parent_file() {
    let (parent, _) = fst_scenario("dump-close", ChildTrace::DumpAndClose);
    eprintln!("SPIKE fst dump-close: parent file -> {parent:?}");
    // Either the chain is broken or the header carries the child's end time.
    assert!(
        parent.map_or(true, |(_, end)| end != PARENT_END),
        "expected corruption"
    );
}

#[test]
fn fst_child_can_detach_and_trace_to_its_own_file() {
    let (parent, child) = fst_scenario("detach", ChildTrace::DetachAndReopen);
    let (_, end) = parent.expect("parent file valid");
    assert_eq!(end, PARENT_END);
    let (types, child_end) = child
        .expect("child wrote a file")
        .expect("child file valid");
    eprintln!("SPIKE fst detach: child blocks {types:?} end_time {child_end}");
    // The child's trace continues from the fork point (t = 210) for 50 cycles.
    assert_eq!(child_end, 210 + 500);
}

//! What the Marlin testbenches of the benchmark share: the stimulus streams, the run
//! length, the Marlin runtime and the result line. The other flows have the same (`bench/README.md`, "Testbench").

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use marlin::verilator::{VerilatorRuntime, VerilatorRuntimeOptions};

/// A driver redraws its rate every `PHASE` cycles, from [`RATES`] (percent of cycles).
pub const PHASE: u64 = 24;
pub const RATES: [u32; 3] = [10, 50, 90];
pub const PRODUCER_SEED: u32 = 0x1234_5678;
pub const CONSUMER_SEED: u32 = 0x9E37_79B9;

/// Marsaglia's xorshift32 (13, 17, 5), the same stream as the other flows'.
pub struct XorShift32(pub u32);

impl XorShift32 {
    pub fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// The rate for the next phase.
    pub fn rate(&mut self) -> u32 {
        RATES[(self.next() % 3) as usize]
    }
}

/// One driver's stream and the rate of its current phase.
pub struct Driver {
    pub rng: XorShift32,
    rate: u32,
}

impl Driver {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: XorShift32(seed),
            rate: 0,
        }
    }

    /// Whether to act in cycle `i` (the rate is redrawn at each phase's start).
    pub fn fire(&mut self, i: u64) -> bool {
        if i.is_multiple_of(PHASE) {
            self.rate = self.rng.rate();
        }
        self.rng.next() % 100 < self.rate
    }
}

/// Cycles to run: `BENCH_CYCLES`, else 10 000.
pub fn cycles() -> u64 {
    std::env::var("BENCH_CYCLES").map_or(10_000, |v| v.parse().expect("BENCH_CYCLES"))
}

pub fn manifest_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Marlin's runtime over `sources`, built into `BENCH_ARTIFACTS`.
pub fn runtime(sources: &[PathBuf]) -> VerilatorRuntime {
    let artifacts = std::env::var_os("BENCH_ARTIFACTS").map_or_else(
        || manifest_path("../../target/bench-marlin/artifacts"),
        PathBuf::from,
    );
    let no_dirs: &[PathBuf] = &[];
    VerilatorRuntime::new2(
        artifacts,
        sources,
        no_dirs,
        [],
        VerilatorRuntimeOptions::default(),
    )
    .expect("Marlin runtime")
}

/// An RTL file: `var` if set (`bench.py bug` points it at a copy with a bug), else
/// `default`, relative to this crate.
pub fn rtl(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| manifest_path(default), PathBuf::from)
}

/// Appends this run's result to `BENCH_OUT` (or prints it).
pub fn report(dut: &str, variant: &str, n: u64, start: Instant, result: &str) {
    let secs = start.elapsed().as_secs_f64();
    let line = format!(
        r#"{{"flow":"marlin","dut":"{dut}","variant":"{variant}","cycles":{n},"seconds":{secs:.6},{result}}}"#
    );
    match std::env::var_os("BENCH_OUT") {
        Some(path) => {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("BENCH_OUT opens");
            writeln!(f, "{line}").expect("BENCH_OUT written");
        }
        None => eprintln!("{line}"),
    }
}

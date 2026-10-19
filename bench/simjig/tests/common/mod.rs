//! What the simjig testbenches of the benchmark share: the stimulus stream, the run
//! length and the result line. The other flows have the same (`bench/README.md`,
//! "Testbench").

use std::io::Write as _;
use std::time::Instant;

use simjig::bits::Bit;

/// A driver redraws its rate every `PHASE` cycles, from [`RATES`] (percent of cycles), so
/// the FIFO fills and drains in bursts instead of hovering at half full.
pub const PHASE: u64 = 24;
pub const RATES: [u32; 3] = [10, 50, 90];
/// Seeds of the producer's and the consumer's streams (any nonzero value).
pub const PRODUCER_SEED: u32 = 0x1234_5678;
pub const CONSUMER_SEED: u32 = 0x9E37_79B9;

/// Marsaglia's xorshift32 (13, 17, 5): three shifts, so the Python copy is cheap and
/// draws the same sequence.
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

/// Cycles to run: `BENCH_CYCLES`, else 10 000.
pub fn cycles() -> u64 {
    std::env::var("BENCH_CYCLES").map_or(10_000, |v| v.parse().expect("BENCH_CYCLES"))
}

/// Appends this run's result to `BENCH_OUT` (or prints it).
pub fn report(dut: &str, variant: &str, n: u64, start: Instant, result: &str) {
    let secs = start.elapsed().as_secs_f64();
    let line = format!(
        r#"{{"flow":"simjig","dut":"{dut}","variant":"{variant}","cycles":{n},"seconds":{secs:.6},{result}}}"#
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

pub fn bit(b: Bit) -> u32 {
    u32::from(b == Bit::ONE)
}

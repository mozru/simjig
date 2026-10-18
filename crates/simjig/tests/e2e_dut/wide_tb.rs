//! `wide`: a 200-bit port through `WideBits`, combinational and registered.

use simjig::{ReadOnly, Timer, clock, ns};
use simjig_bits::WideBits;

use crate::wide;

/// A 200-bit value with every 32-bit word distinct and bit 199 set.
fn pattern(seed: u32) -> WideBits<200, 7> {
    let mut words = [0u32; 7];
    for (i, w) in (1u32..).zip(words.iter_mut()) {
        *w = seed.wrapping_mul(0x9e37_79b9).wrapping_add(i << 20) ^ i;
    }
    words[6] = (words[6] & 0xff) | 0x80;
    WideBits::from_words(words)
}

#[simjig::test]
async fn wide_values_round_trip_and_invert(dut: wide::Dut) {
    for seed in 1..4 {
        let v = pattern(seed);
        dut.i200.set(v);
        Timer::new(ns(1)).await;
        assert_eq!(dut.i200.get(), v);
        assert_eq!(dut.o200.get(), !v);
    }
}

#[simjig::test(timeout = "1us")]
async fn the_wide_register_captures_on_the_clock(dut: wide::Dut) {
    clock(dut.clk, ns(10)).start();
    for seed in 5..8 {
        let v = pattern(seed);
        dut.clk.falling().await;
        dut.i200.set(v);
        dut.clk.rising().await;
        ReadOnly::new().await;
        assert_eq!(dut.q200.get(), v);
    }
}

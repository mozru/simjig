//! Typed handles and the load checks against real models: every storage class round-trips,
//! wide (200-bit) ports included; literals that do not fit name the signal; writes are
//! masked; outputs cannot be written even through a `RawSig`; mismatched metadata is
//! rejected at load.

use simjig::__private::{Access, Binder, ModelMeta, SignalMeta, Storage, create, instantiate};
use simjig::{
    Clock, FailureKind, ModelOptions, ReadOnly, Seed, SeedSource, SimTime, TimeUnit, Timer, ns,
};
use simjig_bits::{Bit, Bits, WideBits};

use super::duts::{counter, flop, inv, sim, timing, wide};
use super::ticks;

/// A 200-bit value with every word distinct, and the top 8 bits (199..192) set.
fn pattern() -> WideBits<200, 7> {
    let mut words = [0u32; 7];
    for (i, w) in (0u32..).zip(words.iter_mut()) {
        *w = 0x9e37_79b9_u32.wrapping_mul(i + 1) ^ (i << 24);
    }
    words[6] |= 0xff;
    WideBits::from_words(words)
}

#[test]
fn output_follows_input_after_the_next_eval() {
    let (mut sim, dut) = sim::<inv::Dut>();
    let seen = sim.run(async move {
        let at_start = (dut.a.get(), dut.y.get());
        dut.a.set(1);
        // A deposit is not visible before it is applied.
        let before = dut.a.get();
        Timer::new(ticks(1)).await;
        (at_start, before, (dut.a.get(), dut.y.get()))
    });
    let (at_start, before, after) = seen.unwrap();
    // Before any eval, Verilator's members are zero (y = ~a has not been computed).
    assert_eq!(at_start, (Bit::ZERO, Bit::ZERO));
    assert_eq!(before, Bit::ZERO);
    assert_eq!(after, (Bit::ONE, Bit::ZERO));
}

#[test]
fn every_width_class_round_trips() {
    let (mut sim, dut) = sim::<wide::Dut>();
    let got = sim.run(async move {
        let wide = pattern();
        dut.i1.set(true);
        dut.i8.set(0x5a);
        dut.i33.set(0x1_dead_beef);
        dut.i64.set(0xfedc_ba98_7654_3210);
        dut.i65.set(0x1_0123_4567_89ab_cdef);
        dut.i200.set(wide);
        // Deposits are applied after the next eval: read back after the timestep.
        Timer::new(ticks(1)).await;
        (
            (dut.i1.get(), dut.o1.get()),
            (dut.i8.get(), dut.o8.get()),
            (dut.i33.get(), dut.o33.get()),
            (dut.i64.get(), dut.o64.get()),
            (dut.i65.get(), dut.o65.get()),
            (dut.i200.get(), dut.o200.get()),
        )
    });
    let (b1, b8, b33, b64, b65, b200) = got.unwrap();
    assert_eq!(b1, (Bit::ONE, Bit::ZERO));
    assert_eq!(b8, (Bits::new(0x5a), Bits::new(0xa5)));
    assert_eq!(b33, (Bits::new(0x1_dead_beef), Bits::new(0x0_2152_4110)));
    let v64: u64 = 0xfedc_ba98_7654_3210;
    assert_eq!(b64, (Bits::new(v64.into()), Bits::new((!v64).into())));
    let v65: u128 = 0x1_0123_4567_89ab_cdef;
    assert_eq!(b65, (Bits::new(v65), Bits::new(!v65 & ((1 << 65) - 1))));
    assert_eq!(b200, (pattern(), !pattern()));
}

#[test]
fn wide_flop_captures_on_the_clock() {
    let (mut sim, dut) = sim::<wide::Dut>();
    let q = sim.run(async move {
        dut.clk.set(0);
        dut.i200.set(pattern());
        Timer::new(ticks(1)).await;
        let before = dut.q200.get();
        dut.clk.set(1);
        ReadOnly::new().await;
        (before, dut.q200.get())
    });
    assert_eq!(q.unwrap(), (WideBits::from(0), pattern()));
}

#[test]
fn literal_that_does_not_fit_names_the_signal() {
    let (mut sim, dut) = sim::<wide::Dut>();
    let failure = sim
        .run(async move { dut.i8.set(0x15a) })
        .expect_err("an out-of-range literal must fail the run");
    let FailureKind::TaskPanic { message } = &failure.kind else {
        panic!("expected a TaskPanic, got {failure}");
    };
    assert_eq!(
        message,
        "signal `wide.i8` is 8 bits wide: value 0x15a does not fit (maximum 0xff)"
    );
}

#[test]
fn write_location_is_the_callers_line() {
    let (mut sim, dut) = sim::<inv::Dut>();
    let line = line!() + 3;
    let failure = sim.run(async move {
        ReadOnly::new().await;
        dut.a.set(1);
    });
    let failure = failure.expect_err("a write in ReadOnly fails the run");
    assert!(matches!(failure.kind, FailureKind::WriteInReadOnly { .. }));
    let location = failure.location.expect("the write's location");
    assert_eq!((location.file(), location.line()), (file!(), line));
}

#[test]
fn outputs_reject_raw_writes() {
    let (mut sim, _dut) = sim::<inv::Dut>();
    let y = sim.signal("inv.y").unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        // The panic unwinds out of the kernel's write-log application, so run never returns.
        drop(sim.run(async move {
            y.write_u64(1);
            Timer::new(ns(1)).await;
        }));
    }));
    let message = outcome.expect_err("writing an output must panic");
    let message = message.downcast_ref::<String>().unwrap();
    assert!(message.contains("signal `inv.y` is read-only"), "{message}");
}

#[test]
fn model_events_advance_time() {
    // timing_dut runs its own clock with `#5` delays: time advances with no kernel clock,
    // through events_pending/next_time_slot.
    let (mut sim, dut) = sim::<timing::Dut>();
    let seen = sim.run(async move {
        let mut seen = Vec::new();
        for _ in 0..3 {
            dut.iclk_out.rising().await;
            ReadOnly::new().await;
            seen.push((simjig::now().ticks(), dut.ticks.get().to_u128()));
        }
        Timer::new(ns(1)).await;
        dut.start.set(1);
        dut.done.falling().await;
        let fell = simjig::now().ticks();
        dut.done.rising().await;
        (seen, fell, simjig::now().ticks())
    });
    let (seen, fell, rose) = seen.unwrap();
    assert_eq!(seen, [(5_000, 1), (15_000, 2), (25_000, 3)]);
    assert_eq!(rose - fell, 23_000, "done rises 23 ns after it falls");
}

simjig::__private::link_model!(fn inv_vtable = "simjig_vtable_Vdut_inv");

const INV: ModelMeta = ModelMeta {
    vtable: inv_vtable,
    prefix: "Vdut_inv",
    ir_hash: inv::Dut::IR_HASH,
    precision_exponent: -12,
    signals: inv::Dut::SIGNALS,
};

static GOOD: ModelMeta = INV;

static BAD_HASH: ModelMeta = ModelMeta {
    ir_hash: 0xbad,
    ..INV
};

static BAD_PREFIX: ModelMeta = ModelMeta {
    prefix: "Vother",
    ..INV
};

const fn meta(name: &'static str, slot: u32, access: Access) -> SignalMeta {
    SignalMeta {
        name,
        slot,
        width: 1,
        storage: Storage::CData,
        access,
    }
}

static BAD_NAME: ModelMeta = ModelMeta {
    signals: &[meta("inv.a", 0, Access::In), meta("inv.z", 1, Access::Out)],
    ..INV
};

static BAD_ACCESS: ModelMeta = ModelMeta {
    signals: &[meta("inv.a", 0, Access::In), meta("inv.y", 1, Access::In)],
    ..INV
};

static BAD_STORAGE: ModelMeta = ModelMeta {
    signals: &[
        meta("inv.a", 0, Access::In),
        SignalMeta {
            storage: Storage::VlUnpacked {
                elem: &Storage::CData,
                len: 4,
            },
            ..meta("inv.y", 1, Access::Out)
        },
    ],
    ..INV
};

fn load_error(meta: &'static ModelMeta) -> String {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    let failure = instantiate(meta, &options).map(|_| ()).unwrap_err();
    assert_eq!(failure.time, SimTime::ZERO);
    let FailureKind::ModelFatal { message } = failure.kind else {
        panic!("expected ModelFatal, got {failure}");
    };
    message
}

#[test]
fn load_rejects_mismatched_metadata() {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    assert!(instantiate(&GOOD, &options).is_ok());
    let text = load_error(&BAD_HASH);
    assert!(
        text.contains("`Vdut_inv` was built from IR 0x5eed000000000001, the generated Rust from 0x0000000000000bad"),
        "{text}"
    );
    let text = load_error(&BAD_PREFIX);
    assert!(text.contains("belongs to model `Vdut_inv`"), "{text}");
    let text = load_error(&BAD_NAME);
    assert!(
        text.contains("slot 1: the model has `inv.y`, the Rust `inv.z`"),
        "{text}"
    );
    let text = load_error(&BAD_ACCESS);
    assert!(text.contains("`inv.y` has direction 2"), "{text}");
    let text = load_error(&BAD_STORAGE);
    assert!(text.contains("only packed signals"), "{text}");
}

#[test]
fn create_rejects_a_simulation_of_another_model() {
    let (sim, _flop) = sim::<flop::Dut>();
    let failure = create(&sim, &GOOD, inv::Dut::__bind)
        .map(|_| ())
        .unwrap_err();
    assert!(failure.message.contains("no `inv.a`"), "{failure}");
}

#[test]
fn binder_rejects_the_wrong_access() {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    let sim = instantiate(&GOOD, &options).unwrap();
    let bind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        create(&sim, &GOOD, |b| b.output::<Bit>(&inv::Dut::SIGNALS[0])).is_ok()
    }));
    let message = bind.map(|_| ()).unwrap_err();
    let message = message.downcast_ref::<String>().unwrap();
    assert_eq!(message, "generated code binds `inv.a` (In) as Out");
}

#[test]
fn a_handle_of_the_wrong_width_fails_on_access() {
    let options = ModelOptions::new(Seed::new(1, SeedSource::Env));
    let mut sim = instantiate(&GOOD, &options).unwrap();
    let a = create(&sim, &GOOD, |b| b.input::<Bits<8>>(&inv::Dut::SIGNALS[0])).unwrap();
    let failure = sim.run(async move { a.get() }).unwrap_err();
    assert!(
        failure
            .message
            .contains("signal `inv.a` is 1 bits wide, but its handle's type is 8 bits"),
        "{failure}"
    );
}

#[test]
fn a_clock_edge_at_time_zero_is_captured() {
    // The kernel's first eval runs before any write is applied, so it settles the model at
    // its initial values; the edge applied after it is seen by the next eval (Verilator's
    // first eval after construction never sees an edge, docs/notes/spikes.md §2).
    let (mut sim, dut) = sim::<counter::Dut>();
    let period = sim.precision().duration(10, TimeUnit::Ns).unwrap();
    sim.add_clock(Clock::new(dut.clk.raw(), period)).unwrap();
    let count = sim.run(async move {
        dut.rst.set(0);
        dut.en.set(1);
        dut.clk.rising().await;
        let at_edge = simjig::now();
        ReadOnly::new().await;
        (at_edge, dut.count.get())
    });
    assert_eq!(count.unwrap(), (SimTime::ZERO, Bits::new(1)));
}

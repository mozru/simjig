//! The C ABI end to end: Rust drives hand-instantiated vtables of two tiny Verilator models
//! (`tests/abi_counter/*.sv`, compiled by `build.rs`) through `model::abi` only.
//!
//! What this proves: the Rust mirror matches the C++ layout; ports of every storage class
//! are read and written through the signal table; and `$fatal`, `$error`, `$stop`, `$finish`
//! and `$display` come back as statuses and messages instead of ending the process, both in a
//! plain model (the fatal unwinds) and in a `--timing` model (the fatal is recorded inside a
//! coroutine and reported after eval).

#[path = "../src/model/abi.rs"]
#[expect(dead_code, reason = "the test uses only part of the ABI mirror")]
mod abi;

#[cfg(not(simjig_abi_models))]
#[test]
fn abi_models_were_built() {
    panic!(
        "build.rs could not build the ABI test models: {}",
        env!("SIMJIG_ABI_MODELS_ERROR")
    );
}

#[cfg(simjig_abi_models)]
mod with_models {
    use super::abi::{
        self, Cell, CreateOpts, Decoded, Handle, ModelVtable, MsgKind, OwnedMsg, Signal, Status,
        TIME_NONE,
    };
    use std::ptr::{self, NonNull};

    unsafe extern "C" {
        #[link_name = "simjig_vtable_Vcounter"]
        safe fn counter_vtable() -> *const ModelVtable;
        #[link_name = "simjig_vtable_Vticker"]
        safe fn ticker_vtable() -> *const ModelVtable;
        safe fn simjig_rt_abi_layout(out: *mut u32, cap: usize) -> usize;
        safe fn simjig_rt_abi_fields(count: *mut usize) -> *const CppField;
    }

    /// `simjig_abi_field` (`simjig_rt.h`): one field as the C++ compiler lays it out.
    #[repr(C)]
    struct CppField {
        ty: *const std::ffi::c_char,
        name: *const std::ffi::c_char,
        offset: u32,
        size: u32,
    }

    /// RAII owner of one model handle; the test's only path to the vtable's functions.
    struct Model {
        vt: &'static ModelVtable,
        handle: NonNull<Handle>,
        signals: Vec<Decoded>,
    }

    /// The IR hashes `shim.cpp` gives the two models.
    const COUNTER_HASH: u64 = 0x5eed_c0de_0000_0001;
    const TICKER_HASH: u64 = 0x5eed_c0de_0000_0002;

    /// A model's checked vtable.
    fn checked(vt: *const ModelVtable, ir_hash: u64) -> &'static ModelVtable {
        // SAFETY: the symbol returns a static table exported by a model built in build.rs.
        unsafe { ModelVtable::check(vt, ir_hash) }
            .unwrap_or_else(|e| panic!("vtable rejected: {e:?}"))
    }

    fn counter() -> &'static ModelVtable {
        checked(counter_vtable(), COUNTER_HASH)
    }

    fn ticker() -> &'static ModelVtable {
        checked(ticker_vtable(), TICKER_HASH)
    }

    impl Model {
        fn new(vt: &'static ModelVtable, error_limit: u32) -> (Self, Status) {
            let opts = CreateOpts {
                size: CreateOpts::SIZE,
                flags: 0,
                name: ptr::null(),
                error_limit,
                argc: 0,
                argv: ptr::null(),
            };
            let mut raw = ptr::null_mut();
            // SAFETY: `opts` and `raw` are valid for the call; create never unwinds.
            let status = unsafe { (vt.create)(&raw const opts, &raw mut raw) };
            let Some(handle) = NonNull::new(raw) else {
                panic!("create returned a null handle (status {status})");
            };
            let mut count = 0;
            // SAFETY: `handle` is live; `count` is a valid out-parameter.
            let table = unsafe { (vt.signals)(handle.as_ptr(), &raw mut count) };
            // SAFETY: the table holds `count` rows and lives as long as the handle; copying
            // the rows (plain data and pointers) creates no reference into model memory.
            let rows = unsafe { std::slice::from_raw_parts(table, count as usize) };
            let signals = rows
                .iter()
                .map(|r| r.decode().unwrap_or_else(|e| panic!("bad row: {e:?}")))
                .collect();
            (
                Self {
                    vt,
                    handle,
                    signals,
                },
                Self::status(status),
            )
        }

        fn status(raw: i32) -> Status {
            Status::try_from(raw).unwrap_or_else(|e| panic!("bad status from C: {e:?}"))
        }

        fn eval(&mut self) -> Status {
            // SAFETY: `handle` is live and used by this thread only.
            Self::status(unsafe { (self.vt.eval)(self.handle.as_ptr()) })
        }

        fn final_blocks(&mut self) -> Status {
            // SAFETY: as in eval.
            Self::status(unsafe { (self.vt.final_)(self.handle.as_ptr()) })
        }

        fn set_time(&mut self, time: u64) {
            // SAFETY: as in eval.
            unsafe { (self.vt.set_time)(self.handle.as_ptr(), time) }
        }

        fn next_time_slot(&mut self) -> u64 {
            // SAFETY: as in eval.
            unsafe { (self.vt.next_time_slot)(self.handle.as_ptr()) }
        }

        fn sig(&self, name: &str) -> Decoded {
            *self
                .signals
                .iter()
                // SAFETY: rows come from this live model's table.
                .find(|s| unsafe { s.name() } == name)
                .unwrap_or_else(|| panic!("no signal {name}"))
        }

        fn get(&self, name: &str) -> u64 {
            let sig = self.sig(name);
            assert!(sig.access & abi::ACCESS_READ != 0);
            let Cell::Narrow(cell) = sig.cell else {
                panic!("{name} is wide")
            };
            // SAFETY: a narrow, readable row of this live model; no eval is running.
            unsafe { abi::read_narrow(cell) }
        }

        fn set(&mut self, name: &str, value: u64) {
            let sig = self.sig(name);
            assert!(sig.access & abi::ACCESS_WRITE != 0);
            let Cell::Narrow(cell) = sig.cell else {
                panic!("{name} is wide")
            };
            // SAFETY: a narrow, writable row of this live model; no eval is running.
            unsafe { abi::write_narrow(cell, sig.width, value) }
        }

        fn drain(&mut self) -> Vec<OwnedMsg> {
            let mut buf = [abi::Msg {
                kind: 0,
                line: 0,
                time: 0,
                file: ptr::null(),
                hier: ptr::null(),
                msg: ptr::null(),
                msg_len: 0,
            }; 8];
            let mut all = Vec::new();
            loop {
                // SAFETY: `buf` has room for `buf.len()` records.
                let n = unsafe {
                    (self.vt.drain_msgs)(self.handle.as_ptr(), buf.as_mut_ptr(), buf.len())
                };
                // SAFETY: drain filled `n` records, and no other call intervened.
                all.extend(buf[..n].iter().map(|m| unsafe { m.copy_out() }));
                if n < buf.len() {
                    return all;
                }
            }
        }

        /// One clock cycle: falling edge at `2t`, rising edge at `2t + 1`. Returns the
        /// rising edge's status (FINISH and STOP are sticky, so the falling edge may show them).
        fn cycle(&mut self, t: u64) -> Status {
            self.set("counter.clk", 0);
            self.set_time(2 * t);
            let falling = self.eval();
            assert!(
                matches!(falling, Status::Ok | Status::Stop | Status::Finish),
                "{falling:?}"
            );
            self.set("counter.clk", 1);
            self.set_time(2 * t + 1);
            self.eval()
        }
    }

    impl Drop for Model {
        fn drop(&mut self) {
            // SAFETY: `handle` came from create and is destroyed exactly once.
            unsafe { (self.vt.destroy)(self.handle.as_ptr()) }
        }
    }

    #[test]
    fn rust_mirror_matches_cpp_layout() {
        let mut got = [0u32; 8];
        assert_eq!(simjig_rt_abi_layout(got.as_mut_ptr(), got.len()), 8);
        let want = [
            size_of::<ModelVtable>(),
            align_of::<ModelVtable>(),
            size_of::<Signal>(),
            align_of::<Signal>(),
            size_of::<abi::Msg>(),
            align_of::<abi::Msg>(),
            size_of::<CreateOpts>(),
            align_of::<CreateOpts>(),
        ]
        .map(|v| u32::try_from(v).unwrap());
        assert_eq!(got, want);
    }

    /// Every field of every mirrored struct sits at the C++ `offsetof` with the C++
    /// `sizeof`, and neither side has a field the other lacks.
    #[test]
    fn every_mirror_field_matches_cpp_offsetof() {
        let mut count = 0;
        let table = simjig_rt_abi_fields(&raw mut count);
        // SAFETY: the runtime returns `count` entries of static storage.
        let cpp = unsafe { std::slice::from_raw_parts(table, count) };
        let text = |p| {
            // SAFETY: the table's strings are static C strings.
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_str()
                .unwrap()
                .to_owned()
        };
        let cpp: Vec<(String, String, usize, usize)> = cpp
            .iter()
            .map(|f| (text(f.ty), text(f.name), f.offset as usize, f.size as usize))
            .collect();
        let rust: Vec<(String, String, usize, usize)> = abi::abi_fields()
            .iter()
            .map(|f| (f.ty.to_owned(), f.name.to_owned(), f.offset, f.size))
            .collect();
        assert_eq!(rust, cpp);
        assert_eq!(cpp.len(), 47);
    }

    #[test]
    fn vtable_identifies_its_model() {
        let vt = counter();
        // SAFETY: as in `checked`.
        let wrong = unsafe { ModelVtable::check(counter_vtable(), 7) };
        assert_eq!(
            wrong.err(),
            Some(abi::AbiError::IrHash {
                model: COUNTER_HASH,
                expected: 7
            })
        );
        // SAFETY: the vtable's strings are static C strings.
        let prefix = unsafe { std::ffi::CStr::from_ptr(vt.prefix) };
        assert_eq!(prefix.to_str().unwrap(), "Vcounter");
        // SAFETY: as above.
        let version = unsafe { std::ffi::CStr::from_ptr(vt.verilator_version) };
        assert!(
            version.to_str().unwrap().starts_with("5.052"),
            "{version:?}"
        );
    }

    #[test]
    fn signal_table_describes_every_storage_class() {
        use abi::{Dir::*, Storage::*};
        let (model, status) = Model::new(counter(), 0);
        assert_eq!(status, Status::Ok);
        let rows: Vec<(String, u32, u32, abi::Storage, abi::Dir)> = model
            .signals
            .iter()
            .map(|s| {
                // SAFETY: rows of this live model's table.
                let name = unsafe { s.name() };
                (name, s.width, s.words, s.cell.storage(), s.dir)
            })
            .collect();
        for (i, s) in model.signals.iter().enumerate() {
            assert_eq!(s.id as usize, i);
            assert_eq!(abi::Storage::for_width(s.width), s.cell.storage());
        }
        let want = [
            ("counter.clk", 1, 1, C, In),
            ("counter.rst", 1, 1, C, In),
            ("counter.cmd", 8, 1, C, In),
            ("counter.half_in", 16, 1, S, In),
            ("counter.wide_in", 100, 4, W, In),
            ("counter.count", 32, 1, I, Out),
            ("counter.half_out", 16, 1, S, Out),
            ("counter.quad_out", 40, 2, Q, Out),
            ("counter.wide_out", 100, 4, W, Out),
            ("counter.tiny_in", 5, 1, C, In),
            ("counter.mid_in", 12, 1, S, In),
            ("counter.quad_in", 40, 2, Q, In),
            ("counter.tiny_ext", 8, 1, C, Out),
            ("counter.mid_ext", 16, 1, S, Out),
            ("counter.quad_ext", 64, 2, Q, Out),
            ("counter.all_ones", 1, 1, C, Out),
        ]
        .map(|(n, w, words, st, d)| (n.to_owned(), w, words, st, d));
        assert_eq!(rows, want);
    }

    #[test]
    fn ports_read_and_write_through_the_table() {
        let (mut m, _) = Model::new(counter(), 0);
        m.set("counter.rst", 1);
        assert_eq!(m.cycle(0), Status::Ok);
        assert_eq!(m.get("counter.count"), 0);
        m.set("counter.rst", 0);
        for t in 1..=5 {
            assert_eq!(m.cycle(t), Status::Ok);
        }
        assert_eq!(m.get("counter.count"), 5);
        assert_eq!(m.get("counter.quad_out"), 0xA5_0000_0005);

        m.set("counter.half_in", 0xffff);
        assert_eq!(m.eval(), Status::Ok);
        assert_eq!(m.get("counter.half_out"), 0, "16-bit wrap-around");

        let wide = |name: &str| match m.sig(name).cell {
            Cell::Wide(p) => p,
            Cell::Narrow(_) => panic!("{name} is narrow"),
        };
        let (wide_in, wide_out) = (wide("counter.wide_in"), wide("counter.wide_out"));
        let mut out = [0u32; 4];
        // SAFETY: W rows of the live model, buffers of `words` entries, no eval running.
        unsafe { abi::write_wide(wide_in, 100, &[0x1234_5678, 0, 0xffff_ffff, 0xffff_ffff]) };
        assert_eq!(m.eval(), Status::Ok);
        // SAFETY: as above.
        unsafe { abi::read_wide(wide_in, &mut out) };
        assert_eq!(
            out,
            [0x1234_5678, 0, 0xffff_ffff, 0xf],
            "top word masked to 100 bits"
        );
        // SAFETY: as above.
        unsafe { abi::read_wide(wide_out, &mut out) };
        assert_eq!(out, [!0x1234_5678, 0xffff_ffff, 0, 0]);
    }

    /// Writing all ones to ports narrower than their storage leaves the unused upper bits
    /// zero, as Verilator assumes: the DUT's zero-extended copies hold exactly the port's
    /// width of ones, and so does the read-back.
    #[test]
    fn narrow_writes_leave_unused_storage_bits_clean() {
        let (mut m, _) = Model::new(counter(), 0);
        for port in ["counter.tiny_in", "counter.mid_in", "counter.quad_in"] {
            m.set(port, u64::MAX);
        }
        assert_eq!(m.eval(), Status::Ok);
        let ext = ["counter.tiny_ext", "counter.mid_ext", "counter.quad_ext"].map(|p| m.get(p));
        assert_eq!(
            ext,
            [0x1f, 0xfff, 0xff_ffff_ffff],
            "DUT saw dirty upper bits"
        );
        assert_eq!(m.get("counter.all_ones"), 1);
        let back = ["counter.tiny_in", "counter.mid_in", "counter.quad_in"].map(|p| m.get(p));
        assert_eq!(back, [0x1f, 0xfff, 0xff_ffff_ffff]);
    }

    #[test]
    fn fatal_returns_status_and_message_and_poisons() {
        let (mut m, _) = Model::new(counter(), 0);
        assert_eq!(m.cycle(0), Status::Ok);
        m.set("counter.cmd", 1);
        assert_eq!(m.cycle(1), Status::Fatal);
        let msgs = m.drain();
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert_eq!(msgs[0].kind, Ok(MsgKind::Fatal));
        assert!(msgs[0].msg.contains("counter fatal at count=1"), "{msgs:?}");
        assert!(msgs[0].file.ends_with("counter.sv"), "{msgs:?}");
        assert_eq!(msgs[0].line, 36);
        assert_eq!(msgs[0].time, 3);
        // Unwinding abandoned the eval before the NBA region: the increment never landed.
        assert_eq!(m.get("counter.count"), 1);
        assert_eq!(m.eval(), Status::Poisoned);
        assert_eq!(m.final_blocks(), Status::Poisoned);
    }

    #[test]
    fn error_continues_until_the_error_limit_then_stops() {
        let (mut m, _) = Model::new(counter(), 2);
        m.set("counter.cmd", 2);
        assert_eq!(m.cycle(0), Status::Ok);
        assert_eq!(m.cycle(1), Status::Stop);
        let msgs = m.drain();
        let kinds: Vec<_> = msgs.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [Ok(MsgKind::Error), Ok(MsgKind::Error)]);
        assert!(msgs[1].msg.contains("counter error at count=1"), "{msgs:?}");
        // STOP is not poison: the model still evaluates.
        m.set("counter.cmd", 0);
        assert_eq!(m.cycle(2), Status::Stop);
        assert_eq!(m.get("counter.count"), 3);
    }

    #[test]
    fn stop_finish_and_display_are_reported() {
        let (mut m, _) = Model::new(counter(), 0);
        m.set("counter.cmd", 4);
        assert_eq!(m.cycle(0), Status::Ok);
        let msgs = m.drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            (msgs[0].kind, msgs[0].msg.as_str()),
            (Ok(MsgKind::Display), "count=0\n")
        );

        m.set("counter.cmd", 3);
        assert_eq!(m.cycle(1), Status::Finish);
        assert_eq!(m.drain()[0].kind, Ok(MsgKind::Finish));
        assert_eq!(m.final_blocks(), Status::Finish);

        let (mut m, _) = Model::new(counter(), 0);
        m.set("counter.cmd", 5);
        assert_eq!(m.cycle(0), Status::Stop);
        let msgs = m.drain();
        assert_eq!((msgs[0].kind, msgs[0].line), (Ok(MsgKind::Stop), 40));
    }

    #[test]
    fn fatal_inside_a_timing_coroutine_is_reported_after_eval() {
        let (mut m, status) = Model::new(ticker(), 0);
        assert_eq!(status, Status::Ok);
        m.set("ticker.arm", 1);
        let mut t = 0;
        let status = loop {
            m.set_time(t);
            let status = m.eval();
            if status != Status::Ok {
                break status;
            }
            t = m.next_time_slot();
            assert_ne!(t, TIME_NONE);
        };
        assert_eq!((status, t), (Status::Fatal, 15));
        assert_eq!(m.get("ticker.ticks"), 3);
        let msgs = m.drain();
        assert_eq!(msgs[0].kind, Ok(MsgKind::Fatal), "{msgs:?}");
        assert!(msgs[0].msg.contains("ticker fatal at tick 3"), "{msgs:?}");
        assert_eq!(
            m.next_time_slot(),
            TIME_NONE,
            "poisoned models have no events"
        );
    }

    #[test]
    fn full_ring_drops_oldest_and_counts_them() {
        let (mut m, _) = Model::new(counter(), 0);
        m.set("counter.cmd", 4);
        // The runtime keeps 4096 records (simjig_rt.cpp kRingCapacity).
        for t in 0..4100 {
            assert_eq!(m.cycle(t), Status::Ok);
        }
        let msgs = m.drain();
        assert_eq!(msgs.len(), 4097);
        assert_eq!((msgs[0].kind, msgs[0].line), (Ok(MsgKind::Dropped), 4));
        assert_eq!(msgs[1].msg, "count=4\n");
        assert_eq!(msgs[4096].msg, "count=4099\n");
        assert_eq!(m.drain().len(), 0);
    }

    #[test]
    fn untimed_model_has_no_time_slot() {
        let (mut m, _) = Model::new(counter(), 0);
        // Verilator's own nextTimeSlot() is a fatal here; the ABI answers "none".
        assert_eq!(m.next_time_slot(), TIME_NONE);
        assert_eq!(m.eval(), Status::Ok);
    }

    /// Cost of one raw read and one raw write through the table. Not a correctness test:
    /// run with `cargo test --release -p simjig --test abi_counter -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement; run explicitly in release mode"]
    #[expect(
        clippy::disallowed_methods,
        reason = "wall-clock measurement, not simulation"
    )]
    fn measure_raw_signal_access() {
        use std::hint::black_box;
        use std::time::Instant;
        const N: u32 = 50_000_000;
        let (mut m, _) = Model::new(counter(), 0);
        let narrow = |name: &str| match m.sig(name).cell {
            Cell::Narrow(cell) => cell,
            Cell::Wide(_) => panic!("{name} is wide"),
        };
        let (count, half) = (narrow("counter.count"), narrow("counter.half_in"));
        let start = Instant::now();
        let mut acc = 0u64;
        for _ in 0..N {
            // SAFETY: readable narrow row of the live model; no eval running.
            acc = acc.wrapping_add(unsafe { abi::read_narrow(black_box(count)) });
        }
        let read = start.elapsed().as_secs_f64() * 1e9 / f64::from(N);
        let start = Instant::now();
        for i in 0..N {
            // SAFETY: writable narrow row of the live model; no eval running.
            unsafe { abi::write_narrow(black_box(half), 16, u64::from(i)) };
        }
        let write = start.elapsed().as_secs_f64() * 1e9 / f64::from(N);
        let start = Instant::now();
        for _ in 0..N / 10 {
            black_box(m.eval());
        }
        let eval = start.elapsed().as_secs_f64() * 1e9 / f64::from(N / 10);
        println!("raw read {read:.2} ns, raw write {write:.2} ns, eval {eval:.1} ns ({acc})");
    }
}

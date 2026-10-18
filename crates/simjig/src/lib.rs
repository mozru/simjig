//! Rust testbenches that drive Verilator models.
//!
//! The testbench owns the main loop and simulation time; the Verilator model is a library
//! it advances.
//!
//! The entry point is [`Sim::run`], which runs an async main task against a model. Inside
//! it, tasks ([`spawn`], [`scope`]) await triggers ([`rising`], [`Timer`], [`ReadOnly`],
//! ...) and read and write signals through typed handles ([`In`], [`Out`], ...) or untyped
//! [`RawSig`]s. The scheduler's semantics match cocotb 2.x on Verilator; see the
//! `kernel::sched` module docs for the loop.
//!
//! A generated Verilator model is loaded by its generated `Dut` type, which returns a
//! [`Sim`] plus one typed handle per signal (or a [`LoadError`] if the model and the
//! generated Rust disagree). Tests are usually `#[simjig::test] async fn`s ([`test`]),
//! which add a seed, a time limit, a failure report with a `repro:` line, and a run
//! directory.
//!
//! # Beyond one clocked DUT
//! - **Model options** (on the build script's `simjig_build::Dut`): `.trace_fst(true)` for
//!   FST waves, `.timing(true)` for designs with `#` delays and `@` events, `.coverage(true)`
//!   for coverage counters.
//! - **Waves**: [`TraceOptions`] traces a run; under `#[simjig::test]`,
//!   `SIMJIG_WAVES_ON_FAIL=1` (every failure), `SIMJIG_WAVES=1` or
//!   `SIMJIG_WAVES=window:<t0>..<t1>` replay the test with tracing and list the FST files
//!   in the report ([`WavesReplay`]); `cargo simjig waves <test> --seed <seed> [-p <pkg>]`
//!   does the same in one command (install it once with `cargo install --locked --path
//!   crates/cargo-simjig` from the simjig repository).
//! - **DPI-C imports in Rust**: the generated module has a `Dpi` trait; register an
//!   implementation with `dut.set_dpi(..)` before the first eval. An implementation runs
//!   inside the model's eval, so it cannot read or write signals, await, spawn, or drive
//!   another simjig model (another DUT's signals, another [`Sim`]); a panic fails the run
//!   as [`FailureKind::DpiPanic`]. Two instances of one DPI build cannot share a
//!   simulation; build the design twice under two `Dut::new` names.
//!
//!   ```ignore
//!   struct Adder;
//!   impl dpi::Dpi for Adder {
//!       fn dpi_add(&mut self, x: i32, y: i32) -> i32 { x + y }
//!   }
//!   #[simjig::test]
//!   async fn sums(dut: dpi::Dut) {
//!       dut.set_dpi(Adder);
//!       dut.a.set(2);
//!       dut.b.set(3);
//!       Timer::new(ns(1)).await;
//!       assert_eq!(dut.sum.get().to_u128(), 5);
//!   }
//!   ```
//! - **Several DUTs**: a test may take two to four DUTs, each its own model in one
//!   simulation; [`Mirror`] drives an input of two at once and [`Lockstep`] compares their
//!   outputs per cycle.
//! - **Checkpoints**: [`fork::run_from_checkpoint`] runs several branches of a test from
//!   one state, each in a forked process (Unix, under nextest).
//! - **Coverage**: [`Sim::coverage`], [`Sim::zero_coverage`] and [`Sim::write_coverage`]
//!   (Verilator's `coverage.dat`), or the [`TestCtx`] methods of the same names in a test;
//!   each takes the model index (0 for the first DUT, then the test's DUTs in order).
//!
//! Signal values are the bit-vector types of the `simjig-bits` crate, re-exported as
//! [`simjig::bits`](bits), so a testbench needs no direct dependency on it:
//!
//! ```
//! use simjig::bits::{Bit, Bits};
//!
//! let data = Bits::<8>::new(0x5a);
//! assert_eq!(data.bit(1), Bit::new(true));
//! ```
#![deny(unsafe_code)]

mod dpi;
pub mod fork;
mod harness;
mod kernel;
mod model;
mod signal;

pub use dpi::Chandle;
pub use harness::Result;
pub use harness::ctx::TestCtx;
pub use harness::dut::{DutModel, InitValues, LoadedModel, ModelOptions};
pub use harness::multi::{Divergence, Lockstep, Mirror};
pub use harness::report::{AppliedTimeout, FailureReport, RunIdentity, Signature};
pub use harness::runner::TestOutput;
pub use harness::seed::{Rng, Seed, SeedSource};
pub use harness::waves::{Verdict, WavesFile, WavesReplay};
pub use kernel::clock::{Clock, ClockError, ClockHandle, clock};
pub use kernel::failure::{BlockedTask, Failure, FailureKind, SourceLocation};
pub use kernel::fake::{FakeModel, FakeSig, FakeState};
pub use kernel::models::ModelSetError;
pub use kernel::recorder::{TraceKind, TraceRecord};
pub use kernel::sched::{DELTA_MAX, READ_WRITE_MAX};
pub use kernel::signal::RawSig;
pub use kernel::sim::{CoverageError, Sim};
pub use kernel::state::KernelStats;
pub use kernel::sync::{Event, EventWait, Get, Lock, LockFuture, LockGuard, Put, Queue};
pub use kernel::tasks::{Task, scope, spawn};
pub use kernel::time::{
    Precision, SimDuration, SimTime, TimeDisplay, TimeError, TimeUnit, fs, ms, ns, ps, us,
};
pub use kernel::trigger::{
    Cycles, Edge, Elapsed, NextTimeStep, ReadOnly, ReadWrite, ThenRegion, Timer, TriggerExt,
    YieldNow, changed, cycles, falling, now, now_display, rising, with_timeout, yield_now,
};
pub use model::meta::LoadError;
pub use model::trace::TraceOptions;
pub use model::values::{FieldDiff, InvalidEnum};
pub use signal::{In, InOut, Out, PublicRd, PublicRw, RawWords, Value, Vector};

/// Bit-vector values with SystemVerilog semantics (`Bit`, `Bits<N>`, `WideBits`, the
/// `Packed` trait): the `simjig-bits` crate.
pub use simjig_bits as bits;

/// Marks an `async fn` as a simjig test; see the attribute's own documentation.
pub use simjig_macros::test;

/// Support for the exported macros; not public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::harness::runner::{
        DEFAULT_TIMEOUT_MS, RunEnv, TestMeta, TestOutcome, Timeout, TimeoutArg, assert_test_output,
        run, run_test,
    };
    pub use crate::harness::waves::{WavesRequest, WavesWhen, Window, WindowEnd};
    pub use crate::kernel::combinator::{Fork, MaybeDone, Operand};
    pub use crate::kernel::trigger::log_prefix;

    // Generated model code (simjig-build's codegen) names these: docs/notes/codegen.md,
    // "The `__private` contract", and `model::bind` for the model glue.
    pub use crate::model::bind::{Binder, ModelBinder, create, create_in, instantiate, load};
    pub use crate::model::meta::{Access, ModelMeta, SignalMeta, Storage, Vtable};
    pub use crate::model::values::{FieldDiff, InvalidEnum, pack_field, unpack_field};
    pub use crate::signal::{In, InOut, Out, PublicRd, PublicRw, RawWords, Value, Vector};
    pub use crate::{__dut_model as dut_model, __link_model as link_model};

    /// What generated DPI-C trampolines and `Dut::set_dpi` name (`crate::dpi`).
    pub mod dpi {
        pub use crate::dpi::{
            Abi, Bit, BitVec, Handle, I8, I16, I32, I64, InArg, InOutArg, Logic, LogicVec, OutArg,
            Param, Ret, SvLogicVecVal, U8, U16, U32, U64, call, install,
        };
    }
    pub use crate::__dpi_import as dpi_import;
    pub use simjig_bits as bits;
}

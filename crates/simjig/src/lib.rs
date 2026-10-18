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
//! generated Rust disagree).
#![deny(unsafe_code)]

mod harness;
mod kernel;
mod model;
mod signal;

pub use harness::Result;
pub use harness::ctx::TestCtx;
pub use harness::dut::{DutModel, ModelOptions};
pub use harness::report::{AppliedTimeout, FailureReport, RunIdentity, Signature};
pub use harness::runner::TestOutput;
pub use harness::seed::{Rng, Seed, SeedSource};
pub use kernel::clock::{Clock, ClockError, ClockHandle, clock};
pub use kernel::failure::{BlockedTask, Failure, FailureKind, SourceLocation};
pub use kernel::fake::{FakeModel, FakeSig, FakeState};
pub use kernel::models::ModelSetError;
pub use kernel::recorder::{TraceKind, TraceRecord};
pub use kernel::sched::DELTA_MAX;
pub use kernel::signal::RawSig;
pub use kernel::sim::Sim;
pub use kernel::state::KernelStats;
pub use kernel::sync::{Event, EventWait, Get, Lock, LockFuture, LockGuard, Put, Queue};
pub use kernel::tasks::{Task, scope, spawn};
pub use kernel::time::{
    Precision, SimDuration, SimTime, TimeDisplay, TimeError, TimeUnit, fs, ms, ns, ps, us,
};
pub use kernel::trigger::{
    Edge, Elapsed, NextTimeStep, ReadOnly, ReadWrite, Timer, YieldNow, changed, cycles, falling,
    now, rising, with_timeout, yield_now,
};
pub use model::meta::LoadError;
pub use model::values::{FieldDiff, InvalidEnum};
pub use signal::{In, InOut, Out, PublicRd, PublicRw, RawWords};

/// Marks an `async fn` as a simjig test; see the attribute's own documentation.
pub use simjig_macros::test;

/// Support for the exported macros; not public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::harness::runner::{
        DEFAULT_TIMEOUT_MS, RunEnv, TestMeta, TestOutcome, Timeout, TimeoutArg, assert_test_output,
        run, run_test,
    };
    pub use crate::kernel::combinator::{MaybeDone, boxed};

    // Generated model code (simjig-build's codegen) names these: docs/notes/codegen.md,
    // "The `__private` contract", and `model::bind` for the model glue.
    pub use crate::model::bind::{Binder, ModelBinder, create, instantiate};
    pub use crate::model::meta::{Access, ModelMeta, SignalMeta, Storage, Vtable};
    pub use crate::model::values::{FieldDiff, InvalidEnum, pack_field, unpack_field};
    pub use crate::signal::{In, InOut, Out, PublicRd, PublicRw, RawWords};
    pub use crate::{__dut_model as dut_model, __link_model as link_model};
    pub use simjig_bits as bits;
}

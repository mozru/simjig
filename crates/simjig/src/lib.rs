//! Rust testbenches that drive Verilator models.
//!
//! The testbench owns the main loop and simulation time; the Verilator model is a library
//! it advances.
//!
//! The entry point is [`Sim::run`], which runs an async main task against a model. Inside
//! it, tasks ([`spawn`], [`scope`]) await triggers ([`rising`], [`Timer`], [`ReadOnly`],
//! ...) and read and write signals through [`RawSig`] handles. The scheduler's semantics
//! match cocotb 2.x on Verilator; see the `kernel::sched` module docs for the loop.
#![deny(unsafe_code)]

mod kernel;
mod model;

pub use kernel::clock::{Clock, ClockError};
pub use kernel::failure::{BlockedTask, Failure, FailureKind};
pub use kernel::fake::{FakeModel, FakeSig, FakeState};
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

/// Support for the exported macros; not public API.
#[doc(hidden)]
pub mod __private {
    pub use crate::kernel::combinator::{MaybeDone, boxed};
}

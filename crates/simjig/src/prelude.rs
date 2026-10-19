//! What nearly every testbench uses, for one glob import: `use simjig::prelude::*;`.
//!
//! | Names | For |
//! |---|---|
//! | [`clock`], [`cycles`], [`ns`], [`us`], [`ps`], [`ms`] | clocks and durations |
//! | [`SyncClock`] | the clock of a synchronous test (`#[simjig::test(sync)]`) |
//! | [`spawn`], [`first!`], [`join!`], [`with_timeout`] | concurrency |
//! | [`Timer`], [`ReadWrite`], [`ReadOnly`], [`TriggerExt`] | time and regions (`.read_write()`, `.read_only()`) |
//! | [`Queue`], [`Event`] | task communication |
//! | [`TestCtx`], [`Rng`], [`tb_log!`] | the test's context, seeded streams, logging |
//! | [`Bit`], [`Bits`], [`Value`], [`Vector`], [`Packed`] | signal values and the bounds generic tests use |
//!
//! Edges are methods of the handles (`dut.clk.rising()`), so the free functions
//! [`rising`](crate::rising), [`falling`](crate::falling) and [`changed`](crate::changed)
//! are not here. The list is kept short so that it does not clash with a testbench's own
//! names: a local item or an explicit `use` shadows a glob import, but two glob imports
//! that bring one name from different places make that name ambiguous where it is used.
//! So it leaves out generic names (`Result`, which would shadow `std`'s, `Lock`, `In`,
//! `Out`, `now`, `scope`) and the `test` attribute, which would shadow the built-in
//! `#[test]`; name those through `simjig::`.
//!
//! A sweep module's re-exports (`use <sweep>::*;`) are the same `Value`, `Vector` and
//! `Packed` items, so it can be glob-imported next to this prelude: the same item through
//! two globs is not ambiguous. A design type the sweep shares under one of the names
//! above (a struct `event_t`, say) does clash; name it through the sweep module.
//!
//! ```
//! use simjig::prelude::*;
//! # use simjig::{FakeModel, Precision, Sim};
//!
//! let mut model = FakeModel::new(Precision::NS);
//! model.signal("clk", 1);
//! let mut sim = Sim::from_fake(model);
//! let clk = sim.signal("clk").unwrap();
//! sim.run(async move {
//!     clock(clk, ns(10)).start();
//!     cycles(clk, 3).read_write().await;
//!     tb_log!("three edges, then their ReadWrite region");
//! })
//! .unwrap();
//! ```

pub use crate::bits::{Bit, Bits, Packed};
pub use crate::harness::ctx::TestCtx;
pub use crate::harness::seed::Rng;
pub use crate::kernel::clock::clock;
pub use crate::kernel::sync::{Event, Queue};
pub use crate::kernel::sync_clock::SyncClock;
pub use crate::kernel::tasks::spawn;
pub use crate::kernel::time::{ms, ns, ps, us};
pub use crate::kernel::trigger::{ReadOnly, ReadWrite, Timer, TriggerExt, cycles, with_timeout};
pub use crate::signal::{Value, Vector};
pub use crate::{first, join, tb_log};

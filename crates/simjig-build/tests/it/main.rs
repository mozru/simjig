//! Integration tests for `simjig-build`. They run the real pinned Verilator and C++
//! compiler, so they must run inside `pixi run` (they fail, never skip, without it).
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup failures should panic with the error"
)]

mod cache;
mod emitter;
mod errors;
mod fixture_crate;
mod header;
mod ifaces;
mod noop;
mod pipeline;
mod probes;
mod rebuild;
mod recipe;
mod support;
mod ui;

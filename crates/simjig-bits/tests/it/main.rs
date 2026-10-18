//! Integration tests for `simjig-bits`: one binary, one module per concern.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers unwrap; a failure is a test failure"
)]

mod inference;
mod literals;
mod reference;
mod resize;
mod s3c5m;
mod vectors;

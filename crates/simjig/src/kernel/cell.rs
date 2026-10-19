//! A narrow signal's storage in model memory, and the two raw accesses to it.
//!
//! A Verilator model keeps a signal of at most 64 bits in one `CData`, `SData`, `IData` or
//! `QData` member; [`Narrow`] is a pointer to it, typed by that storage class. This is the
//! one place that reads and writes such a member: the C ABI's decoder (`model::abi`) makes
//! the pointers from a model's signal table, and the kernel's [`Models`](super::models)
//! keeps them to read and write a slot without the backend's dynamic call.
//!
//! Invariants:
//! - A `Narrow` only ever points at a live, aligned member of exactly its storage type
//!   (the shim `static_assert`s the type and size; the decoder checks the class and the
//!   alignment). Its users guarantee the model is alive and that no entry point of it runs
//!   concurrently, which is the contract of [`read_narrow`] and [`write_narrow`].
//! - Model memory is touched only through these raw pointers, at the point of use; no Rust
//!   reference to it is ever created.
//!
//! Why `unsafe` here rather than only in `model` (rule 5): the kernel may not import the
//! model module (it is tested against a fake model), yet reading a port through the
//! backend's dynamic call costs several times the read itself on every access. The pointer
//! type and its two accessors are therefore kernel types, and nothing else moved.
#![expect(
    unsafe_code,
    reason = "raw reads and writes of a model's narrow signal storage (module docs)"
)]

/// A pointer to a `CData`, `SData`, `IData` or `QData` member.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Narrow {
    C(*mut u8),
    S(*mut u16),
    I(*mut u32),
    Q(*mut u64),
}

/// Mask of the bits a `width`-bit value may use in its lowest 64 bits.
fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Reads a signal of at most 64 bits.
///
/// # Safety
/// - `cell` comes from a row of a live model's signal table with `ACCESS_READ`;
/// - no entry point of that model runs concurrently.
#[inline]
pub(crate) unsafe fn read_narrow(cell: Narrow) -> u64 {
    // Each pointer is a live, aligned member of exactly that storage type (the C++ shim
    // static_asserts type and size; `decode` checked the class); the caller guarantees
    // liveness.
    match cell {
        // SAFETY: a live CData (u8).
        Narrow::C(p) => u64::from(unsafe { p.read() }),
        // SAFETY: a live, 2-aligned SData (u16).
        Narrow::S(p) => u64::from(unsafe { p.read() }),
        // SAFETY: a live, 4-aligned IData (u32).
        Narrow::I(p) => u64::from(unsafe { p.read() }),
        // SAFETY: a live, 8-aligned QData (u64).
        Narrow::Q(p) => unsafe { p.read() },
    }
}

/// Writes a signal of at most 64 bits, masking `value` to `width`: Verilator assumes the
/// unused upper bits are zero.
///
/// # Safety
/// As for [`read_narrow`], with `ACCESS_WRITE`, and `width` is the row's width.
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is masked to the width, which fits the storage"
)]
pub(crate) unsafe fn write_narrow(cell: Narrow, width: u32, value: u64) {
    let v = value & mask(width);
    // As in read_narrow; after masking, `v` fits the storage type, so the casts lose nothing.
    match cell {
        // SAFETY: a live, writable CData (u8).
        Narrow::C(p) => unsafe { p.write(v as u8) },
        // SAFETY: a live, writable, 2-aligned SData (u16).
        Narrow::S(p) => unsafe { p.write(v as u16) },
        // SAFETY: a live, writable, 4-aligned IData (u32).
        Narrow::I(p) => unsafe { p.write(v as u32) },
        // SAFETY: a live, writable, 8-aligned QData (u64).
        Narrow::Q(p) => unsafe { p.write(v) },
    }
}

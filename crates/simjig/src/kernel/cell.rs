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

use std::any::Any;
use std::fmt;
use std::rc::Rc;

/// A pointer to a `CData`, `SData`, `IData` or `QData` member.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Narrow {
    C(*mut u8),
    S(*mut u16),
    I(*mut u32),
    Q(*mut u64),
}

/// Mask of the bits a `width`-bit value may use in its lowest 64 bits.
const fn mask(width: u32) -> u64 {
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
pub(crate) unsafe fn write_narrow(cell: Narrow, width: u32, value: u64) {
    // SAFETY: the caller's promise; masked to the width, the value fits the storage.
    unsafe { store_narrow(cell, value & mask(width)) }
}

/// Stores `v`, which fits `cell`'s storage type (masked to the slot's width).
///
/// # Safety
/// As for [`write_narrow`].
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is masked to the width, which fits the storage"
)]
unsafe fn store_narrow(cell: Narrow, v: u64) {
    // As in read_narrow; `v` fits the storage type, so the casts lose nothing.
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

/// A readable narrow slot of a live backend, with what writing it needs: the kernel's way
/// to a port without the backend's dynamic call ([`ModelBackend::direct`]).
///
/// [`ModelBackend::direct`]: super::backend::ModelBackend::direct
#[derive(Debug, Clone, Copy)]
pub(crate) struct Direct {
    cell: Narrow,
    /// The bits the slot's width allows ([`mask`]), applied to every write.
    mask: u64,
    writable: bool,
}

impl Direct {
    /// # Safety
    /// `cell` comes from a row of the backend's live signal table with `ACCESS_READ` (and
    /// `ACCESS_WRITE` if `writable`), `width` is that row's width, and the backend that
    /// hands this out keeps the model alive for as long as it lives itself.
    pub(crate) const unsafe fn new(cell: Narrow, width: u32, writable: bool) -> Self {
        Self {
            cell,
            mask: mask(width),
            writable,
        }
    }

    /// Whether the testbench may write the slot (an input, or a `public_flat_rw` probe).
    pub(crate) const fn writable(self) -> bool {
        self.writable
    }

    /// The slot's value.
    ///
    /// # Safety
    /// The backend that returned this is alive, and none of its entry points runs.
    #[inline]
    pub(crate) unsafe fn read(self) -> u64 {
        // SAFETY: `new`'s contract makes `cell` a readable row of the live model; the caller
        // keeps the backend alive and idle.
        unsafe { read_narrow(self.cell) }
    }

    /// Writes `value`, masked to the slot's width.
    ///
    /// # Safety
    /// As for [`read`](Self::read), and the slot is [`writable`](Self::writable).
    #[inline]
    pub(crate) unsafe fn write(self, value: u64) {
        debug_assert!(self.writable, "invariant: only writable slots are written");
        // SAFETY: as in `read`, and the caller checked that the row has `ACCESS_WRITE`.
        unsafe { store_narrow(self.cell, value & self.mask) }
    }
}

/// The [`Direct`] way to every flat slot that has one: the reads and writes of narrow
/// slots that need no backend call. Its owners ([`Models`](super::models::Models), and the
/// kernel's copy for signal handles) keep the backends that made the `Direct`s alive for as
/// long as the table, and use it only while none of those backends' entry points runs (the
/// kernel is single-threaded and refuses entry during a DPI-C call), which is what makes
/// its safe methods sound.
#[derive(Debug)]
pub(crate) struct DirectTable {
    slots: Box<[Option<Direct>]>,
}

impl DirectTable {
    /// # Safety
    /// Every `Direct` in `slots` comes from a backend that outlives the table, and the
    /// table is used only while none of those backends' entry points runs.
    pub(crate) unsafe fn new(slots: Box<[Option<Direct>]>) -> Self {
        Self { slots }
    }

    /// A copy for another owner.
    ///
    /// # Safety
    /// As [`new`](Self::new), for the copy.
    pub(crate) unsafe fn share(&self) -> Self {
        Self {
            slots: self.slots.clone(),
        }
    }

    /// The `Direct` of flat slot `flat`, if it has one (using it is `unsafe`).
    pub(crate) fn get(&self, flat: u32) -> Option<Direct> {
        self.slots.get(flat as usize).copied().flatten()
    }

    /// The value of flat slot `flat`, if it has a `Direct`.
    #[inline]
    pub(crate) fn read_u64(&self, flat: u32) -> Option<u64> {
        let d = self.slots.get(flat as usize)?.as_ref()?;
        // SAFETY: the table's contract (`new`): the backend is alive and idle.
        Some(unsafe { d.read() })
    }

    /// Reads flat slot `flat` into `out` (one or two words) if it has a `Direct`; returns
    /// whether it did.
    #[inline]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "each word takes 32 bits of the value"
    )]
    pub(crate) fn read(&self, flat: u32, out: &mut [u32]) -> bool {
        let Some(Some(d)) = self.slots.get(flat as usize) else {
            return false;
        };
        if out.len() > 2 {
            return false;
        }
        // SAFETY: the table's contract (`new`): the backend is alive and idle.
        let value = unsafe { d.read() };
        out[0] = value as u32;
        if let Some(high) = out.get_mut(1) {
            *high = (value >> 32) as u32;
        }
        true
    }

    /// Writes `value` (one or two words) to flat slot `flat`, masked to its width, if it
    /// has a writable `Direct`; returns whether it did.
    #[inline]
    pub(crate) fn write(&self, flat: u32, value: &[u32]) -> bool {
        match *value {
            [w] => self.write_u64(flat, u64::from(w)),
            [w0, w1] => self.write_u64(flat, u64::from(w1) << 32 | u64::from(w0)),
            _ => false,
        }
    }

    /// Writes `value` to flat slot `flat`, masked to its width, if it has a writable
    /// `Direct`; returns whether it did.
    #[inline]
    pub(crate) fn write_u64(&self, flat: u32, value: u64) -> bool {
        match self.slots.get(flat as usize) {
            Some(Some(d)) if d.writable() => {
                // SAFETY: as in `read`, and the slot is writable.
                unsafe { d.write(value) };
                true
            }
            _ => false,
        }
    }
}

/// A [`Direct`] that keeps its backend alive itself, through `keep` (a share of what owns
/// the backend): a synchronous run's bound handle reads and writes through it with no
/// table lookup (`SyncClock::bind`).
pub(crate) struct KeptDirect {
    direct: Direct,
    _keep: Rc<dyn Any>,
}

impl fmt::Debug for KeptDirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeptDirect")
            .field("direct", &self.direct)
            .finish_non_exhaustive()
    }
}

impl KeptDirect {
    /// # Safety
    /// `keep` keeps alive the backend that made `direct`, and the holder reads and writes
    /// only while none of that backend's entry points runs.
    pub(crate) unsafe fn new(direct: Direct, keep: Rc<dyn Any>) -> Self {
        Self {
            direct,
            _keep: keep,
        }
    }

    /// The slot's value.
    #[inline]
    pub(crate) fn read(&self) -> u64 {
        // SAFETY: `new`'s contract.
        unsafe { self.direct.read() }
    }

    /// Writes `value`, masked to the slot's width; returns `false`, writing nothing, for a
    /// slot the testbench may not write.
    #[inline]
    pub(crate) fn write(&self, value: u64) -> bool {
        if !self.direct.writable() {
            return false;
        }
        // SAFETY: `new`'s contract, and the slot is writable.
        unsafe { self.direct.write(value) };
        true
    }
}

/// A held model's eval (`eval_held`), callable without its backend: what a synchronous
/// run's clock calls on every edge, so an edge is one call into the model and a status
/// check. Made by the backend that holds the model (`ModelBackend::held_eval`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeldEval {
    call: unsafe fn(*const (), *mut (), u64) -> i32,
    table: *const (),
    handle: *mut (),
}

impl HeldEval {
    /// # Safety
    /// `call(table, handle, ticks)` sets the model's time and evaluates it once, and is
    /// sound whenever the backend that made this is alive and holds its model.
    pub(crate) const unsafe fn new(
        call: unsafe fn(*const (), *mut (), u64) -> i32,
        table: *const (),
        handle: *mut (),
    ) -> Self {
        Self {
            call,
            table,
            handle,
        }
    }

    /// Sets the model's time to `ticks` and evaluates it; the raw status of `eval_held`.
    ///
    /// # Safety
    /// The backend that made this is alive and still holds its model.
    #[inline]
    pub(crate) unsafe fn call(self, ticks: u64) -> i32 {
        // SAFETY: the caller's promise is `new`'s condition.
        unsafe { (self.call)(self.table, self.handle, ticks) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads and writes through every storage class, against local integers (so Miri can
    /// check the accesses): a write masks to the width and leaves the rest of the storage
    /// zero, as Verilator assumes.
    #[test]
    fn a_direct_slot_reads_and_writes_its_storage() {
        let (mut cdata, mut sdata, mut idata, mut qdata) = (0u8, 0u16, 0u32, 0u64);
        let cells = [
            (Narrow::C(&raw mut cdata), 5),
            (Narrow::S(&raw mut sdata), 12),
            (Narrow::I(&raw mut idata), 32),
            (Narrow::Q(&raw mut qdata), 33),
        ];
        for (cell, width) in cells {
            // SAFETY: each pointer is a live, aligned local of its storage type, only
            // accessed through `slot` until the end of this iteration.
            let slot = unsafe { Direct::new(cell, width, true) };
            // SAFETY: as above.
            unsafe { slot.write(u64::MAX) };
            // SAFETY: as above.
            assert_eq!(unsafe { slot.read() }, mask(width), "width {width}");
            // SAFETY: as above.
            unsafe { slot.write(0b1010) };
            // SAFETY: as above.
            assert_eq!(unsafe { slot.read() }, 0b1010, "width {width}");
        }
        assert_eq!(
            (cdata, sdata, idata, qdata),
            (0b1010, 0b1010, 0b1010, 0b1010)
        );
    }
}

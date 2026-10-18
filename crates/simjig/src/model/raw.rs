//! [`RawModel`]: the owner of one live model handle, and the only place outside `abi` that
//! calls through a [`ModelVtable`].
//!
//! Invariants that make every method here safe to call:
//! - `handle` is non-null and live from [`RawModel::create`] until `Drop`, which destroys it
//!   exactly once. The type is `!Send` and `!Sync` (it holds a `NonNull`), so no entry point
//!   ever runs concurrently with another, and none is reentrant (model code never calls back
//!   into Rust: `$display` text goes to the C++ message ring).
//! - `signals` is a copy of the model's table, validated once at creation: every row is
//!   readable, has a non-null pointer, and its `storage` and `words` are exactly what
//!   Verilator uses for its `width`. `read` and `write` index it and check the buffer length
//!   and the write bit before touching model memory, so the `abi` raw accessors' preconditions
//!   hold.
//! - Drained message strings are copied out before the next entry point call.
//!
//! No Rust reference into model memory is ever created; `abi` reads and writes members
//! through raw pointers at the moment of use.
#![expect(
    unsafe_code,
    reason = "calls through the C vtable and the abi raw accessors (DECISION §2 row 12)"
)]

use std::ptr::{self, NonNull};

use super::abi::{
    self, ACCESS_READ, ACCESS_WRITE, CreateOpts, Handle, ModelVtable, Msg, OwnedMsg, Signal,
    Storage,
};

/// One live model: its vtable, its handle and a validated copy of its signal table.
#[derive(Debug)]
pub(crate) struct RawModel {
    vt: &'static ModelVtable,
    handle: NonNull<Handle>,
    signals: Box<[Signal]>,
    names: Box<[String]>,
}

/// Why [`RawModel::create`] failed.
#[derive(Debug)]
pub(crate) enum CreateError {
    /// `create` returned no handle; the raw status code.
    NoHandle(i32),
    /// Row `slot` of the signal table breaks the ABI contract.
    BadRow { slot: usize, problem: String },
}

impl RawModel {
    /// Creates a model through `vt` (which [`ModelVtable::check`] accepted). Returns the model
    /// and `create`'s raw status: a non-OK status still yields a model, to drain its messages
    /// and destroy it.
    pub(crate) fn create(vt: &'static ModelVtable) -> Result<(Self, i32), CreateError> {
        let opts = CreateOpts {
            size: CreateOpts::SIZE,
            // No SIMJIG_CREATE_ECHO: Rust prints the messages, so they reach the test's output.
            flags: 0,
            name: ptr::null(),
            error_limit: 0,
            argc: 0,
            argv: ptr::null(),
        };
        let mut raw = ptr::null_mut();
        // SAFETY: `opts` and `raw` are valid for the duration of the call, and `create` is a
        // noexcept C++ function that never unwinds into Rust.
        let status = unsafe { (vt.create)(&raw const opts, &raw mut raw) };
        let handle = NonNull::new(raw).ok_or(CreateError::NoHandle(status))?;
        let mut count = 0u32;
        // SAFETY: `handle` is live; `count` is a valid out-parameter.
        let table = unsafe { (vt.signals)(handle.as_ptr(), &raw mut count) };
        let rows: Box<[Signal]> = if table.is_null() || count == 0 {
            Box::default()
        } else {
            // SAFETY: the ABI guarantees `count` initialised rows at `table`, valid until
            // destroy; they are copied (plain integers and pointers) before any other call.
            unsafe { std::slice::from_raw_parts(table, count as usize) }.into()
        };
        // SAFETY: the rows come from this live model's table, whose names are static.
        let names = rows.iter().map(|row| unsafe { row.name() }).collect();
        let model = Self {
            vt,
            handle,
            signals: rows,
            names,
        };
        // Validate after `model` owns the handle, so an error destroys it.
        for (slot, row) in model.signals.iter().enumerate() {
            validate(slot, row).map_err(|problem| CreateError::BadRow { slot, problem })?;
        }
        Ok((model, status))
    }

    /// The validated signal table, in slot order.
    pub(crate) fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// The name of every slot, in slot order.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// One `eval`; the raw status code.
    pub(crate) fn eval(&mut self) -> i32 {
        // SAFETY: `handle` is live, and no other entry point runs (module invariants).
        unsafe { (self.vt.eval)(self.handle.as_ptr()) }
    }

    /// Runs the model's `final` blocks; the raw status code.
    pub(crate) fn final_blocks(&mut self) -> i32 {
        // SAFETY: as in `eval`.
        unsafe { (self.vt.final_)(self.handle.as_ptr()) }
    }

    /// Sets the context time, in ticks.
    pub(crate) fn set_time(&mut self, ticks: u64) {
        // SAFETY: as in `eval`.
        unsafe { (self.vt.set_time)(self.handle.as_ptr(), ticks) }
    }

    /// Whether the model has timed events of its own (`--timing`).
    pub(crate) fn events_pending(&self) -> bool {
        // SAFETY: as in `eval`; the call only inspects the model's scheduler.
        unsafe { (self.vt.events_pending)(self.handle.as_ptr()) != 0 }
    }

    /// The next timed event, or [`abi::TIME_NONE`].
    pub(crate) fn next_time_slot(&self) -> u64 {
        // SAFETY: as in `events_pending`; the ABI version never aborts (unlike Verilator's).
        unsafe { (self.vt.next_time_slot)(self.handle.as_ptr()) }
    }

    /// Moves every pending message into `out`, oldest first.
    pub(crate) fn drain(&mut self, out: &mut Vec<OwnedMsg>) {
        let empty = Msg {
            kind: 0,
            line: 0,
            time: 0,
            file: ptr::null(),
            hier: ptr::null(),
            msg: ptr::null(),
            msg_len: 0,
        };
        let mut buf = [empty; 16];
        loop {
            // SAFETY: `buf` has room for `buf.len()` records; `handle` is live.
            let n =
                unsafe { (self.vt.drain_msgs)(self.handle.as_ptr(), buf.as_mut_ptr(), buf.len()) };
            let n = n.min(buf.len());
            // SAFETY: drain filled the first `n` records and no entry point ran since, so
            // their strings are still valid; they are copied before the next call.
            out.extend(buf[..n].iter().map(|m| unsafe { m.copy_out() }));
            if n < buf.len() {
                return;
            }
        }
    }

    /// Copies slot `slot` into `out`, least significant word first.
    ///
    /// # Panics
    /// If `slot` is out of range or `out` is not exactly the slot's word count.
    pub(crate) fn read(&self, slot: usize, out: &mut [u32]) {
        let row = &self.signals[slot];
        assert_eq!(out.len(), row.words as usize, "read buffer size");
        if row.storage == Storage::W as u8 {
            // SAFETY: a validated, readable W row of this live model; `out` has `words` words.
            unsafe { abi::read_wide(row, out) }
        } else {
            // SAFETY: a validated, readable narrow row of this live model.
            let value = unsafe { abi::read_narrow(row) };
            split(value, out);
        }
    }

    /// Writes `value` (least significant word first) into slot `slot`, masked to its width.
    ///
    /// # Panics
    /// If `slot` is out of range, `value` is not exactly the slot's word count, or the slot is
    /// not writable (an output or a `public_flat_rd` signal: writing it is undefined, because
    /// Verilator may have folded it).
    pub(crate) fn write(&mut self, slot: usize, value: &[u32]) {
        let row = &self.signals[slot];
        assert!(
            row.access & ACCESS_WRITE != 0,
            "signal `{}` is read-only to the testbench (an output, or not public_flat_rw); \
             only inputs and public_flat_rw signals can be written",
            self.names[slot]
        );
        assert_eq!(value.len(), row.words as usize, "write buffer size");
        if row.storage == Storage::W as u8 {
            // SAFETY: a validated, writable W row of this live model; `value` has `words`
            // words; the top word is masked by write_wide.
            unsafe { abi::write_wide(row, value) }
        } else {
            let low = u64::from(value[0]);
            let high = value.get(1).map_or(0, |&w| u64::from(w));
            // SAFETY: a validated, writable narrow row; write_narrow masks to the width.
            unsafe { abi::write_narrow(row, high << 32 | low) }
        }
    }
}

impl Drop for RawModel {
    fn drop(&mut self) {
        // SAFETY: `handle` is live and is destroyed exactly once, here; `destroy` accepts
        // poisoned handles (the ABI contract) and never unwinds.
        unsafe { (self.vt.destroy)(self.handle.as_ptr()) }
    }
}

/// Checks one row against the ABI contract that `abi`'s raw accessors rely on.
fn validate(slot: usize, row: &Signal) -> Result<(), String> {
    if row.id as usize != slot {
        return Err(format!("its id is {}", row.id));
    }
    if row.width == 0 {
        return Err("its width is 0".to_owned());
    }
    let storage = Storage::for_width(row.width);
    if row.storage != storage as u8 {
        return Err(format!(
            "storage class {} does not match width {} (expected {storage:?})",
            row.storage, row.width
        ));
    }
    if row.words != row.width.div_ceil(32) {
        return Err(format!("{} words for width {}", row.words, row.width));
    }
    if row.ptr.is_null() {
        return Err("its member pointer is null".to_owned());
    }
    if row.access & ACCESS_READ == 0 {
        return Err("it is not readable".to_owned());
    }
    Ok(())
}

/// Splits a narrow value into `out` (one or two words).
#[expect(
    clippy::cast_possible_truncation,
    reason = "each word takes 32 bits of the value"
)]
fn split(value: u64, out: &mut [u32]) {
    out[0] = value as u32;
    if let Some(high) = out.get_mut(1) {
        *high = (value >> 32) as u32;
    }
}

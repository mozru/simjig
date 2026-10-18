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

use std::ffi::{CString, c_char};
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
    /// A command-line argument contains a NUL byte.
    BadArg(String),
    /// Row `slot` of the signal table breaks the ABI contract.
    BadRow { slot: usize, problem: String },
}

impl RawModel {
    /// Creates a model through `vt` (which [`ModelVtable::check`] accepted), passing `args`
    /// to `VerilatedContext::commandArgs` after a program name. Returns the model and
    /// `create`'s raw status: a non-OK status still yields a model, to drain its messages and
    /// destroy it.
    pub(crate) fn create(
        vt: &'static ModelVtable,
        args: &[String],
    ) -> Result<(Self, i32), CreateError> {
        let mut owned = vec![c"simjig".to_owned()];
        for arg in args {
            owned
                .push(CString::new(arg.as_str()).map_err(|_nul| CreateError::BadArg(arg.clone()))?);
        }
        let pointers: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
        let opts = CreateOpts {
            size: CreateOpts::SIZE,
            // No SIMJIG_CREATE_ECHO: Rust prints the messages, so they reach the test's output.
            flags: 0,
            name: ptr::null(),
            error_limit: 0,
            argc: u32::try_from(pointers.len()).unwrap_or(u32::MAX),
            argv: pointers.as_ptr(),
        };
        let mut raw = ptr::null_mut();
        // SAFETY: `opts`, `raw` and the `argc` NUL-terminated strings behind `argv` (`owned`)
        // are valid for the duration of the call (commandArgs copies them), and `create` is a
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

/// A pure-Rust model behind a real [`ModelVtable`], for testing the backend's handling of
/// statuses, messages, poisoning and teardown without Verilator. One model per thread.
///
/// Slot 0 is a writable 4-bit input (`fake.a`), slot 1 a read-only 8-bit output (`fake.y`).
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::ffi::{CStr, c_void};
    use std::ptr::{self, NonNull};

    use super::super::abi::{
        ABI_VERSION, ACCESS_READ, ACCESS_WRITE, CreateOpts, Dir, Handle, ModelVtable, Msg, Signal,
        Status, Storage,
    };

    /// What the fake model did and will do.
    #[derive(Debug, Default)]
    pub(crate) struct State {
        /// Statuses the next evals return (then OK).
        pub(crate) statuses: VecDeque<Status>,
        /// Messages the next drain returns: kind and text.
        pub(crate) pending: VecDeque<(u32, &'static CStr)>,
        pub(crate) evals: u32,
        pub(crate) finals: u32,
        pub(crate) destroys: u32,
        /// The two members, `a` and `y`.
        pub(crate) members: Box<[u8; 2]>,
        rows: Vec<Signal>,
    }

    thread_local! {
        pub(crate) static STATE: RefCell<State> = RefCell::default();
    }

    const fn row(id: u32, width: u32, dir: Dir, access: u8, name: &'static CStr) -> Signal {
        Signal {
            id,
            width,
            words: 1,
            storage: Storage::C as u8,
            dir: dir as u8,
            access,
            reserved: 0,
            name: name.as_ptr(),
            ptr: ptr::null_mut(),
        }
    }

    extern "C" fn create(_opts: *const CreateOpts, out: *mut *mut Handle) -> i32 {
        STATE.with_borrow_mut(|s| {
            let base = s.members.as_mut_ptr();
            let mut a = row(0, 4, Dir::In, ACCESS_READ | ACCESS_WRITE, c"fake.a");
            let mut y = row(1, 8, Dir::Out, ACCESS_READ, c"fake.y");
            a.ptr = base.cast::<c_void>();
            // SAFETY: `members` has two bytes; offset 1 stays inside it.
            y.ptr = unsafe { base.add(1) }.cast::<c_void>();
            s.rows = vec![a, y];
        });
        // SAFETY: `out` is the caller's valid out-parameter.
        unsafe { out.write(NonNull::<Handle>::dangling().as_ptr()) };
        0
    }

    extern "C" fn destroy(_h: *mut Handle) {
        STATE.with_borrow_mut(|s| s.destroys += 1);
    }

    extern "C" fn eval(_h: *mut Handle) -> i32 {
        STATE.with_borrow_mut(|s| {
            s.evals += 1;
            s.statuses.pop_front().unwrap_or(Status::Ok) as i32
        })
    }

    extern "C" fn final_blocks(_h: *mut Handle) -> i32 {
        STATE.with_borrow_mut(|s| s.finals += 1);
        0
    }

    extern "C" fn set_time(_h: *mut Handle, _t: u64) {}

    extern "C" fn events_pending(_h: *mut Handle) -> u32 {
        0
    }

    extern "C" fn next_time_slot(_h: *mut Handle) -> u64 {
        u64::MAX
    }

    extern "C" fn signals(_h: *mut Handle, count: *mut u32) -> *const Signal {
        STATE.with_borrow(|s| {
            // SAFETY: `count` is the caller's valid out-parameter.
            unsafe { count.write(2) };
            s.rows.as_ptr()
        })
    }

    extern "C" fn drain(_h: *mut Handle, out: *mut Msg, cap: usize) -> usize {
        STATE.with_borrow_mut(|s| {
            let mut n = 0;
            while n < cap {
                let Some((kind, text)) = s.pending.pop_front() else {
                    break;
                };
                let msg = Msg {
                    kind,
                    line: 7,
                    time: 0,
                    file: c"fake.sv".as_ptr(),
                    hier: c"TOP.fake".as_ptr(),
                    msg: text.as_ptr(),
                    msg_len: text.to_bytes().len(),
                };
                // SAFETY: `n < cap`, and `out` has room for `cap` records.
                let slot = unsafe { out.add(n) };
                // SAFETY: `slot` is a valid, writable record of the caller's buffer.
                unsafe { slot.write(msg) };
                n += 1;
            }
            n
        })
    }

    extern "C" fn unsupported(_h: *mut Handle) -> i32 {
        Status::Unsupported as i32
    }

    extern "C" fn trace_open(_h: *mut Handle, _path: *const std::ffi::c_char) -> i32 {
        Status::Unsupported as i32
    }

    extern "C" fn trace_dump(_h: *mut Handle, _t: u64) -> i32 {
        Status::Unsupported as i32
    }

    extern "C" fn cov(_h: *mut Handle, _count: *mut usize) -> *mut u32 {
        ptr::null_mut()
    }

    /// The fake's vtable (prefix `Vfake`, IR hash 7).
    pub(crate) fn vtable() -> ModelVtable {
        ModelVtable {
            abi_version: ABI_VERSION,
            size: 136,
            prefix: c"Vfake".as_ptr(),
            verilator_version: c"fake".as_ptr(),
            ir_hash: 7,
            create,
            destroy,
            eval,
            final_: final_blocks,
            set_time,
            events_pending,
            next_time_slot,
            signals,
            drain_msgs: drain,
            trace_open,
            trace_dump,
            trace_close: unsupported,
            cov_counters: cov,
        }
    }

    /// Resets this thread's fake state.
    pub(crate) fn reset() {
        STATE.with_borrow_mut(|s| *s = State::default());
    }
}

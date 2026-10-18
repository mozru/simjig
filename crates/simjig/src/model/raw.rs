//! [`RawModel`]: the owner of one live model handle, and the only place outside `abi` that
//! calls through a [`ModelVtable`] (or turns a model's vtable pointer into a reference,
//! [`check_vtable`]).
//!
//! Invariants that make every method here safe to call:
//! - `handle` is non-null and live from [`RawModel::create`] until `Drop`, which destroys it
//!   exactly once. The type is `!Send` and `!Sync` (it holds a `NonNull`), so no entry point
//!   ever runs concurrently with another, and none is reentrant (model code never calls back
//!   into Rust: `$display` text goes to the C++ message ring).
//! - `signals` is the model's table decoded once at creation ([`Signal::decode`]): every
//!   row is readable, has a non-null pointer aligned for its storage type, and its storage
//!   class and `words` are exactly what Verilator uses for its `width`. `read` and `write`
//!   index it, match on its typed [`Cell`], and check the buffer length and the write bit
//!   before touching model memory, so the `abi` raw accessors' preconditions hold.
//! - Drained message strings are copied out before the next entry point call.
//! - `coverage` holds the model's `cov_runs` (pointer and length per run), taken once:
//!   the ABI keeps them valid until destroy, so reading a counter is a raw read of model
//!   memory between entry points, like `read`.
//! - A [`TracePort`] reaches the handle only while it is live: the model clears the shared
//!   cell before `destroy`. It is `!Send` too, and its only caller (a fork hook, which runs
//!   in the main task's poll, never inside an eval: the kernel refuses `fork` inside a DPI
//!   call) never runs during another entry point.
//! - The handle is destroyed exactly once: by whoever takes it out of the shared cell,
//!   `Drop` or [`TracePort::destroy_before_exit`]. The latter runs only in a fork child
//!   that `_exit`s right after it, so no entry point reaches the destroyed handle.
//!
//! No Rust reference into model memory is ever created; `abi` reads and writes members
//! through raw pointers at the moment of use.
#![expect(
    unsafe_code,
    reason = "calls through the C vtable and the abi raw accessors (DECISION §2 row 12)"
)]

use std::cell::Cell as StdCell;
use std::ffi::{CStr, CString, c_char};
use std::ptr::{self, NonNull};
use std::rc::Rc;

use super::abi::{
    self, ACCESS_READ, ACCESS_WRITE, AbiError, Cell, CreateOpts, Decoded, Handle, ModelVtable, Msg,
    Narrow, OwnedMsg, Signal,
};
use super::meta::{Vtable, row_problem};

/// Checks the vtable a model's symbol returned ([`ModelVtable::check`]: ABI version, size,
/// IR hash) and only then returns it as a reference.
pub(crate) fn check_vtable(vt: Vtable, ir_hash: u64) -> Result<&'static ModelVtable, AbiError> {
    // SAFETY: a `Vtable` comes only from a model's `simjig_vtable_<prefix>` symbol
    // (`link_model!`, whose contract makes it non-null, aligned and static) or, in unit tests,
    // from `Vtable::leak` (a whole, leaked `ModelVtable`); either way at least `abi_version`,
    // `size` and `size` bytes in all stay valid and unmodified for the program.
    unsafe { ModelVtable::check(vt.as_ptr(), ir_hash) }
}

/// One live model: its vtable, its handle and its validated, decoded signal table.
#[derive(Debug)]
pub(crate) struct RawModel {
    vt: &'static ModelVtable,
    handle: NonNull<Handle>,
    signals: Box<[Decoded]>,
    names: Box<[String]>,
    /// The handle while it is live, shared with [`TracePort`]s; `None` from `Drop` on.
    live: Rc<StdCell<Option<NonNull<Handle>>>>,
    /// The coverage counter runs, once [`RawModel::take_coverage_runs`] fetched them.
    coverage: Option<CovRuns>,
}

/// Coverage counter runs: the first counter and the count of each.
type CovRuns = Box<[(NonNull<u32>, usize)]>;

/// A model's trace entry points after `trace_open`, shared by its tracer and the tracer's
/// fork hook (flush before a fork, reopen in the child, close before the child exits),
/// which cannot borrow the backend the kernel owns. Every call is `None` once the model is
/// destroyed.
#[derive(Debug, Clone)]
pub(crate) struct TracePort {
    vt: &'static ModelVtable,
    live: Rc<StdCell<Option<NonNull<Handle>>>>,
}

impl TracePort {
    /// Writes one sample at `ticks`; the raw status, `None` if the model is gone.
    pub(crate) fn dump(&self, ticks: u64) -> Option<i32> {
        let handle = self.live.get()?;
        // SAFETY: `live` holds the handle only while it is live (cleared before `destroy`),
        // and no other entry point runs (module invariants).
        Some(unsafe { (self.vt.trace_dump)(handle.as_ptr(), ticks) })
    }

    /// Closes the trace (writing its header); the raw status, `None` if the model is gone.
    /// `destroy` also closes it.
    pub(crate) fn close(&self) -> Option<i32> {
        let handle = self.live.get()?;
        // SAFETY: as in `dump`.
        Some(unsafe { (self.vt.trace_close)(handle.as_ptr()) })
    }

    /// Flushes buffered samples to the file; the raw status, `None` if the model is gone.
    pub(crate) fn flush(&self) -> Option<i32> {
        let handle = self.live.get()?;
        // SAFETY: as in `dump`.
        Some(unsafe { (self.vt.trace_flush)(handle.as_ptr()) })
    }

    /// In a fork branch's child that is about to `_exit`: destroys the model, which closes
    /// its trace even when the model is poisoned (`trace_close` refuses a poisoned model).
    /// The child never drops its models, so without this a poisoned model's file has no
    /// header.
    ///
    /// The caller guarantees that the process exits before any other entry point of this
    /// model runs (`fork`'s `at_child_exit` hooks are its last code); `Drop` then never runs.
    pub(crate) fn destroy_before_exit(&self) {
        if let Some(handle) = self.live.take() {
            // SAFETY: `live` held the handle, so it is live; taking it out of the shared
            // cell makes this its only destroy (`Drop` and every port call see `None`), and
            // the caller's contract keeps the model's own methods from running again.
            unsafe { (self.vt.destroy)(handle.as_ptr()) }
        }
    }

    /// In a fork child: detaches the inherited trace and continues it in `path`; the raw
    /// status, `None` if the model is gone.
    pub(crate) fn reopen(&self, path: &CStr) -> Option<i32> {
        let handle = self.live.get()?;
        // SAFETY: as in `dump`; `path` is NUL-terminated and valid for the call, which
        // copies it.
        Some(unsafe { (self.vt.trace_reopen)(handle.as_ptr(), path.as_ptr()) })
    }
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
        let mut model = Self {
            vt,
            handle,
            signals: Box::default(),
            names: Box::default(),
            live: Rc::new(StdCell::new(Some(handle))),
            coverage: None,
        };
        // Validate after `model` owns the handle, so an error destroys it.
        let decoded = rows
            .iter()
            .enumerate()
            .map(|(slot, row)| {
                validate(slot, row).map_err(|problem| CreateError::BadRow { slot, problem })
            })
            .collect::<Result<Box<[Decoded]>, _>>()?;
        // SAFETY: the rows come from this live model's table, whose names are static.
        model.names = decoded.iter().map(|row| unsafe { row.name() }).collect();
        model.signals = decoded;
        Ok((model, status))
    }

    /// Fetches the coverage counter runs of a model verilated with coverage (call once, on
    /// a healthy model); a model without coverage keeps none.
    pub(crate) fn take_coverage_runs(&mut self) {
        if self.vt.features & abi::FEATURE_COVERAGE == 0 {
            return;
        }
        let mut count = 0usize;
        // SAFETY: as in `eval`; `count` is a valid out-parameter.
        let runs = unsafe { (self.vt.cov_runs)(self.handle.as_ptr(), &raw mut count) };
        let runs: &[abi::CovRun] = if runs.is_null() || count == 0 {
            &[]
        } else {
            // SAFETY: the ABI guarantees `count` initialised runs at `runs`, valid until
            // destroy; they are copied out (pointers and lengths) before any other call.
            unsafe { std::slice::from_raw_parts(runs, count) }
        };
        self.coverage = Some(
            runs.iter()
                .filter_map(|run| NonNull::new(run.counters).map(|p| (p, run.len)))
                .collect(),
        );
    }

    /// Every coverage counter, in run order; `None` for a model without coverage.
    pub(crate) fn coverage(&self) -> Option<Vec<u32>> {
        let runs = self.coverage.as_ref()?;
        let mut out = Vec::with_capacity(runs.iter().map(|(_, len)| len).sum());
        for &(counters, len) in runs {
            // SAFETY: `len` live, aligned `u32` counters at `counters` until destroy (the ABI
            // contract), read by value; the model only writes them inside entry points, and
            // none runs now (module invariants).
            out.extend((0..len).map(|i| unsafe { counters.as_ptr().wrapping_add(i).read() }));
        }
        Some(out)
    }

    /// Sets every coverage counter to zero; the raw status (`UNSUPPORTED` without coverage).
    pub(crate) fn cov_zero(&mut self) -> i32 {
        // SAFETY: as in `eval`.
        unsafe { (self.vt.cov_zero)(self.handle.as_ptr()) }
    }

    /// Writes the model's coverage file (`coverage.dat` format) to `path`; the raw status
    /// (`UNSUPPORTED` without coverage, `IO_ERROR` for a file that cannot be written).
    pub(crate) fn cov_write(&mut self, path: &CStr) -> i32 {
        // SAFETY: as in `eval`; `path` is NUL-terminated and valid for the call, which
        // copies it.
        unsafe { (self.vt.cov_write)(self.handle.as_ptr(), path.as_ptr()) }
    }

    /// The validated signal table, in slot order.
    pub(crate) fn signals(&self) -> &[Decoded] {
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

    /// The model's `SIMJIG_FEATURE_*` bits.
    pub(crate) fn features(&self) -> u64 {
        self.vt.features
    }

    /// Opens an FST trace at `path` (closing an open one first), limited to `depth` scope
    /// levels below the top (0: all), skipping dumps before `start` ticks; the raw status.
    pub(crate) fn trace_open(&mut self, path: &CStr, depth: u32, start: u64) -> i32 {
        // SAFETY: as in `eval`; `path` is a NUL-terminated string valid for the call, which
        // copies it.
        unsafe { (self.vt.trace_open_ex)(self.handle.as_ptr(), path.as_ptr(), depth, start) }
    }

    /// A second way to reach this model's trace, for a fork hook.
    pub(crate) fn trace_port(&self) -> TracePort {
        TracePort {
            vt: self.vt,
            live: Rc::clone(&self.live),
        }
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
        match row.cell {
            // SAFETY: a validated, readable W row of this live model; `out` has `words` words.
            Cell::Wide(p) => unsafe { abi::read_wide(p, out) },
            Cell::Narrow(cell) => {
                // SAFETY: a validated, readable narrow row of this live model.
                let value = unsafe { abi::read_narrow(cell) };
                split(value, out);
            }
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
        match row.cell {
            // SAFETY: a validated, writable W row of this live model; `value` has `words`
            // words; the top word is masked by write_wide.
            Cell::Wide(p) => unsafe { abi::write_wide(p, row.width, value) },
            Cell::Narrow(cell) => {
                let low = u64::from(value[0]);
                let high = value.get(1).map_or(0, |&w| u64::from(w));
                // SAFETY: a validated, writable narrow row; write_narrow masks to the width.
                unsafe { abi::write_narrow(cell, row.width, high << 32 | low) }
            }
        }
    }
}

impl Drop for RawModel {
    fn drop(&mut self) {
        if let Some(handle) = self.live.take() {
            // SAFETY: `live` still held the handle, so it is live and was not destroyed;
            // taking it makes this its only destroy. `destroy` accepts poisoned handles (the
            // ABI contract) and never unwinds.
            unsafe { (self.vt.destroy)(handle.as_ptr()) }
        }
    }
}

/// Decodes one row ([`Signal::decode`]: storage class, direction, width and word count)
/// and checks the rest of the ABI contract that `abi`'s raw accessors rely on.
fn validate(slot: usize, row: &Signal) -> Result<Decoded, String> {
    if row.id as usize != slot {
        return Err(format!("its id is {}", row.id));
    }
    let decoded = row.decode().map_err(|e| row_problem(&e))?;
    if row.ptr.is_null() {
        return Err("its member pointer is null".to_owned());
    }
    // `abi`'s accessors read the member as its storage type (`VlWide` as `u32` words), so
    // the pointer must be aligned for it; the shim's static_asserts cannot check addresses.
    let (aligned, align) = match decoded.cell {
        Cell::Narrow(Narrow::C(p)) => (p.is_aligned(), align_of::<u8>()),
        Cell::Narrow(Narrow::S(p)) => (p.is_aligned(), align_of::<u16>()),
        Cell::Narrow(Narrow::I(p)) | Cell::Wide(p) => (p.is_aligned(), align_of::<u32>()),
        Cell::Narrow(Narrow::Q(p)) => (p.is_aligned(), align_of::<u64>()),
    };
    if !aligned {
        return Err(format!(
            "its member pointer {:p} is not {align}-byte aligned, as {:?} storage needs",
            row.ptr,
            decoded.cell.storage()
        ));
    }
    if row.access & ACCESS_READ == 0 {
        return Err("it is not readable".to_owned());
    }
    Ok(decoded)
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
        ABI_VERSION, ACCESS_READ, ACCESS_WRITE, CovRun, CreateOpts, Dir, Handle, ModelVtable, Msg,
        Signal, Status, Storage,
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
        /// Every eval, final, destroy and trace call of the traced vtable, in order.
        pub(crate) calls: Vec<String>,
        /// Statuses the next trace calls of the traced vtable return (then OK).
        pub(crate) trace_statuses: VecDeque<Status>,
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
        STATE.with_borrow_mut(|s| {
            s.destroys += 1;
            s.calls.push("destroy".to_owned());
        });
    }

    extern "C" fn eval(_h: *mut Handle) -> i32 {
        STATE.with_borrow_mut(|s| {
            s.evals += 1;
            s.calls.push("eval".to_owned());
            s.statuses.pop_front().unwrap_or(Status::Ok) as i32
        })
    }

    extern "C" fn final_blocks(_h: *mut Handle) -> i32 {
        STATE.with_borrow_mut(|s| {
            s.finals += 1;
            s.calls.push("final".to_owned());
        });
        0
    }

    /// Records `call` and returns the next queued trace status.
    fn traced(call: String) -> i32 {
        STATE.with_borrow_mut(|s| {
            s.calls.push(call);
            s.trace_statuses.pop_front().unwrap_or(Status::Ok) as i32
        })
    }

    extern "C" fn rec_open(
        _h: *mut Handle,
        path: *const std::ffi::c_char,
        depth: u32,
        start: u64,
    ) -> i32 {
        // SAFETY: the caller passes a NUL-terminated string valid for the call.
        let path = unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned();
        traced(format!("open {path} depth={depth} start={start}"))
    }

    extern "C" fn rec_dump(_h: *mut Handle, t: u64) -> i32 {
        traced(format!("dump {t}"))
    }

    extern "C" fn rec_flush(_h: *mut Handle) -> i32 {
        traced("flush".to_owned())
    }

    extern "C" fn rec_close(_h: *mut Handle) -> i32 {
        traced("close".to_owned())
    }

    extern "C" fn rec_reopen(_h: *mut Handle, path: *const std::ffi::c_char) -> i32 {
        // SAFETY: the caller passes a NUL-terminated string valid for the call.
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        traced(format!("reopen {path}"))
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

    extern "C" fn trace_open_ex(
        _h: *mut Handle,
        _path: *const std::ffi::c_char,
        _depth: u32,
        _flags: u64,
    ) -> i32 {
        Status::Unsupported as i32
    }

    extern "C" fn rng_seed(_h: *mut Handle, _seed: u64) -> i32 {
        Status::Unsupported as i32
    }

    extern "C" fn cov_runs(_h: *mut Handle, count: *mut usize) -> *const CovRun {
        // SAFETY: `count` is the caller's valid out-parameter.
        unsafe { count.write(0) };
        ptr::null()
    }

    /// The fake's vtable (prefix `Vfake`, IR hash 7).
    pub(crate) fn vtable() -> ModelVtable {
        ModelVtable {
            abi_version: ABI_VERSION,
            size: 192,
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
            cov_runs,
            features: 0,
            trace_open_ex,
            trace_flush: unsupported,
            trace_reopen: trace_open,
            cov_zero: unsupported,
            cov_write: trace_open,
            rng_seed,
        }
    }

    /// The fake's vtable with FST support: its trace entries record their calls.
    pub(crate) fn traced_vtable() -> ModelVtable {
        ModelVtable {
            features: super::super::abi::FEATURE_TRACE_FST,
            trace_open_ex: rec_open,
            trace_dump: rec_dump,
            trace_flush: rec_flush,
            trace_close: rec_close,
            trace_reopen: rec_reopen,
            ..vtable()
        }
    }

    /// Resets this thread's fake state.
    pub(crate) fn reset() {
        STATE.with_borrow_mut(|s| *s = State::default());
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use super::super::abi::{Dir, Storage};
    use super::*;

    /// A readable row of `width` bits whose member is at `offset` bytes into `base`.
    fn row_at(width: u32, base: &mut [u64; 4], offset: usize) -> Signal {
        Signal {
            id: 0,
            width,
            words: width.div_ceil(32),
            storage: Storage::for_width(width) as u8,
            dir: Dir::In as u8,
            access: ACCESS_READ,
            reserved: 0,
            name: c"top.x".as_ptr(),
            ptr: base
                .as_mut_ptr()
                .cast::<u8>()
                .wrapping_add(offset)
                .cast::<c_void>(),
        }
    }

    #[test]
    fn validate_rejects_a_member_misaligned_for_its_storage_class() {
        let mut base = [0u64; 4];
        // (width, storage alignment): C 1, S 2, I 4, Q 8, W 4 (u32 words).
        for (width, align) in [(8, 1), (16, 2), (32, 4), (64, 8), (96, 4)] {
            assert!(validate(0, &row_at(width, &mut base, 0)).is_ok(), "{width}");
            assert!(
                validate(0, &row_at(width, &mut base, align)).is_ok(),
                "{width}"
            );
            if align > 1 {
                let error = validate(0, &row_at(width, &mut base, 1)).unwrap_err();
                assert!(
                    error.contains(&format!("not {align}-byte aligned")),
                    "{width}: {error}"
                );
            }
        }
    }
}

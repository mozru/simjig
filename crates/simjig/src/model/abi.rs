//! Rust mirror of the simjig C ABI (`crates/simjig-build/cpp/simjig_abi.h`).
//!
//! A generated model exports one symbol, `simjig_vtable_<prefix>()`, returning a
//! [`ModelVtable`]. Everything the runtime does with the model goes through that table: create,
//! eval, time, the signal table ([`Signal`]) and the message ring ([`Msg`]).
//!
//! Invariants:
//! - The `#[repr(C)]` structs match the header byte for byte. Compile-time asserts below pin
//!   their sizes and every field offset for 64-bit targets; tests compare sizes and every
//!   field's offset and size with what the C++ compiler reports (`simjig_rt_abi_layout`,
//!   `simjig_rt_abi_fields`).
//! - Integers coming from C (status codes, message kinds, storage classes) are kept as
//!   integers and decoded with `TryFrom`: a Rust enum holding an unknown discriminant is UB.
//! - The table is append-only. [`ModelVtable::check`] rejects a model whose `abi_version`
//!   differs or whose `size` is smaller than this mirror, so no field past the model's end is
//!   ever read.
//! - Model memory is only touched through raw pointers at the point of use
//!   ([`read_narrow`], [`write_narrow`], [`read_wide`], [`write_wide`]); no Rust reference to
//!   it is ever created.

use std::ffi::{CStr, c_char, c_void};
use std::marker::{PhantomData, PhantomPinned};
use std::mem::{align_of, offset_of, size_of};

/// `SIMJIG_ABI_VERSION`: bumped only on an incompatible change.
pub(crate) const ABI_VERSION: u32 = 1;

/// `SIMJIG_TIME_NONE`: `next_time_slot` found no pending timed event.
pub(crate) const TIME_NONE: u64 = u64::MAX;

/// `SIMJIG_CREATE_ECHO`: also print captured messages to stdout.
pub(crate) const CREATE_ECHO: u32 = 1;

/// `SIMJIG_ACCESS_READ`.
pub(crate) const ACCESS_READ: u8 = 1;
/// `SIMJIG_ACCESS_WRITE`.
pub(crate) const ACCESS_WRITE: u8 = 2;

/// An integer from C that is not a known value of the named C enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unknown {
    pub(crate) what: &'static str,
    pub(crate) value: i64,
}

/// Declares a Rust enum for a C `#define` enumeration plus its fallible decoding.
macro_rules! c_enum {
    ($(#[$m:meta])* $name:ident: $raw:ty { $($(#[$vm:meta])* $variant:ident = $value:expr,)+ }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr($raw)]
        pub(crate) enum $name { $($(#[$vm])* $variant = $value,)+ }

        impl TryFrom<$raw> for $name {
            type Error = Unknown;
            fn try_from(raw: $raw) -> Result<Self, Unknown> {
                match raw {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(Unknown { what: stringify!($name), value: i64::from(raw) }),
                }
            }
        }
    };
}

c_enum! {
    /// `simjig_status`: the outcome of an entry point that ran model code.
    Status: i32 {
        /// The call completed.
        Ok = 0,
        /// `$finish` has run (sticky).
        Finish = 1,
        /// `$stop`, or `$error` over the error limit, has run (sticky).
        Stop = 2,
        /// `$fatal` or an internal Verilator fatal; the model is now poisoned.
        Fatal = 3,
        /// A C++ exception escaped model code; the model is now poisoned.
        Exception = 4,
        /// The model was already poisoned; nothing ran.
        Poisoned = 5,
        /// The entry point is reserved but not built into this model.
        Unsupported = 6,
        /// A file the call names could not be created; a WARNING message says why.
        IoError = 7,
    }
}

c_enum! {
    /// `SIMJIG_STORAGE_*`: the C++ type Verilator stores a signal in.
    Storage: u8 {
        /// `CData`, 1..=8 bits.
        C = 1,
        /// `SData`, 9..=16 bits.
        S = 2,
        /// `IData`, 17..=32 bits.
        I = 3,
        /// `QData`, 33..=64 bits.
        Q = 4,
        /// `VlWide<words>`, more than 64 bits, least significant word first.
        W = 5,
    }
}

c_enum! {
    /// `SIMJIG_DIR_*`.
    Dir: u8 {
        /// A public internal member, not a port.
        Internal = 0,
        /// Input port.
        In = 1,
        /// Output port.
        Out = 2,
        /// Bidirectional port.
        Inout = 3,
    }
}

c_enum! {
    /// `SIMJIG_MSG_*`: what produced a message record.
    MsgKind: u32 {
        /// `$display`/`$write`/`$warning`/`$info` text or other Verilator output.
        Display = 0,
        /// Internal Verilator warning.
        Warning = 1,
        /// `$error` or a failed assertion.
        Error = 2,
        /// `$stop`.
        Stop = 3,
        /// `$fatal` or an internal Verilator fatal.
        Fatal = 4,
        /// `$finish`.
        Finish = 5,
        /// A C++ exception caught by the barrier.
        Exception = 6,
        /// The ring overflowed; `line` is the number of records lost.
        Dropped = 7,
    }
}

impl Storage {
    /// Verilator's storage rule for a signal of `width` bits.
    pub(crate) fn for_width(width: u32) -> Self {
        match width {
            0..=8 => Self::C,
            9..=16 => Self::S,
            17..=32 => Self::I,
            33..=64 => Self::Q,
            _ => Self::W,
        }
    }
}

/// Opaque `simjig_handle`: zero-sized, `!Send`, `!Sync`, `!Unpin`, never constructed in Rust.
#[repr(C)]
pub(crate) struct Handle {
    _priv: [u8; 0],
    _pin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// `simjig_signal`: one row of a model's signal table.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Signal {
    pub(crate) id: u32,
    pub(crate) width: u32,
    pub(crate) words: u32,
    pub(crate) storage: u8,
    pub(crate) dir: u8,
    pub(crate) access: u8,
    pub(crate) reserved: u8,
    pub(crate) name: *const c_char,
    pub(crate) ptr: *mut c_void,
}

/// `simjig_msg`: a borrowed view of one drained message record.
#[repr(C)]
#[expect(clippy::struct_field_names, reason = "field names mirror simjig_msg")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Msg {
    pub(crate) kind: u32,
    pub(crate) line: u32,
    pub(crate) time: u64,
    pub(crate) file: *const c_char,
    pub(crate) hier: *const c_char,
    pub(crate) msg: *const c_char,
    pub(crate) msg_len: usize,
}

/// `simjig_create_opts`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct CreateOpts {
    pub(crate) size: u32,
    pub(crate) flags: u32,
    pub(crate) name: *const c_char,
    pub(crate) error_limit: u32,
    pub(crate) argc: u32,
    pub(crate) argv: *const *const c_char,
}

impl CreateOpts {
    /// `sizeof(simjig_create_opts)`, the value of its `size` field.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "32 bytes, pinned by the asserts below"
    )]
    pub(crate) const SIZE: u32 = size_of::<Self>() as u32;
}

/// `simjig_model_vtable`. Field order and types match the header exactly.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct ModelVtable {
    pub(crate) abi_version: u32,
    pub(crate) size: u32,
    pub(crate) prefix: *const c_char,
    pub(crate) verilator_version: *const c_char,
    pub(crate) ir_hash: u64,
    pub(crate) create: unsafe extern "C" fn(*const CreateOpts, *mut *mut Handle) -> i32,
    pub(crate) destroy: unsafe extern "C" fn(*mut Handle),
    pub(crate) eval: unsafe extern "C" fn(*mut Handle) -> i32,
    pub(crate) final_: unsafe extern "C" fn(*mut Handle) -> i32,
    pub(crate) set_time: unsafe extern "C" fn(*mut Handle, u64),
    pub(crate) events_pending: unsafe extern "C" fn(*mut Handle) -> u32,
    pub(crate) next_time_slot: unsafe extern "C" fn(*mut Handle) -> u64,
    pub(crate) signals: unsafe extern "C" fn(*mut Handle, *mut u32) -> *const Signal,
    pub(crate) drain_msgs: unsafe extern "C" fn(*mut Handle, *mut Msg, usize) -> usize,
    pub(crate) trace_open: unsafe extern "C" fn(*mut Handle, *const c_char) -> i32,
    pub(crate) trace_dump: unsafe extern "C" fn(*mut Handle, u64) -> i32,
    pub(crate) trace_close: unsafe extern "C" fn(*mut Handle) -> i32,
    pub(crate) cov_counters: unsafe extern "C" fn(*mut Handle, *mut usize) -> *mut u32,
    /// `SIMJIG_FEATURE_*` bits.
    pub(crate) features: u64,
    pub(crate) trace_open_ex: unsafe extern "C" fn(*mut Handle, *const c_char, u32, u64) -> i32,
    pub(crate) trace_flush: unsafe extern "C" fn(*mut Handle) -> i32,
    pub(crate) trace_reopen: unsafe extern "C" fn(*mut Handle, *const c_char) -> i32,
    pub(crate) cov_zero: unsafe extern "C" fn(*mut Handle) -> i32,
    pub(crate) cov_write: unsafe extern "C" fn(*mut Handle, *const c_char) -> i32,
    pub(crate) rng_seed: unsafe extern "C" fn(*mut Handle, u64) -> i32,
}

/// One field of a mirrored struct, named as in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FieldLayout {
    /// The C struct name, e.g. `simjig_signal`.
    pub(crate) ty: &'static str,
    /// The C field name (`final` for the Rust `final_`).
    pub(crate) name: &'static str,
    /// `offset_of!` in the mirror.
    pub(crate) offset: usize,
    /// `size_of` of the field's type in the mirror.
    pub(crate) size: usize,
}

/// The size of the field `field` selects; the closure is never called.
fn field_size<S, F>(_field: fn(&S) -> &F) -> usize {
    size_of::<F>()
}

/// The C name of a mirror field: its Rust name unless given.
macro_rules! c_name {
    ($field:ident) => {
        stringify!($field)
    };
    ($field:ident $c:literal) => {
        $c
    };
}

/// For each mirrored struct and each of its fields, in header order: a compile-time
/// assert that `offset_of!` equals the offset the header implies on LP64/LLP64 (8-byte
/// pointers), and an entry of [`abi_fields`], which a test compares with the C++
/// compiler's `offsetof`/`sizeof` (`simjig_rt_abi_fields`).
macro_rules! abi_fields {
    ($($c_ty:literal $ty:ident = $size:literal {
        $($field:ident $(as $c:literal)? @ $offset:literal,)+
    })+) => {
        #[cfg(target_pointer_width = "64")]
        const _: () = {
            $(
                assert!(size_of::<$ty>() == $size && align_of::<$ty>() == 8);
                $(assert!(offset_of!($ty, $field) == $offset);)+
            )+
        };

        /// Every field of every mirrored struct, in header order.
        pub(crate) fn abi_fields() -> Vec<FieldLayout> {
            vec![$($(FieldLayout {
                ty: $c_ty,
                name: c_name!($field $($c)?),
                offset: offset_of!($ty, $field),
                size: field_size(|s: &$ty| &s.$field),
            },)+)+]
        }
    };
}

abi_fields! {
    "simjig_model_vtable" ModelVtable = 192 {
        abi_version @ 0,
        size @ 4,
        prefix @ 8,
        verilator_version @ 16,
        ir_hash @ 24,
        create @ 32,
        destroy @ 40,
        eval @ 48,
        final_ as "final" @ 56,
        set_time @ 64,
        events_pending @ 72,
        next_time_slot @ 80,
        signals @ 88,
        drain_msgs @ 96,
        trace_open @ 104,
        trace_dump @ 112,
        trace_close @ 120,
        cov_counters @ 128,
        features @ 136,
        trace_open_ex @ 144,
        trace_flush @ 152,
        trace_reopen @ 160,
        cov_zero @ 168,
        cov_write @ 176,
        rng_seed @ 184,
    }
    "simjig_signal" Signal = 32 {
        id @ 0,
        width @ 4,
        words @ 8,
        storage @ 12,
        dir @ 13,
        access @ 14,
        reserved @ 15,
        name @ 16,
        ptr @ 24,
    }
    "simjig_msg" Msg = 48 {
        kind @ 0,
        line @ 4,
        time @ 8,
        file @ 16,
        hier @ 24,
        msg @ 32,
        msg_len @ 40,
    }
    "simjig_create_opts" CreateOpts = 32 {
        size @ 0,
        flags @ 4,
        name @ 8,
        error_limit @ 16,
        argc @ 20,
        argv @ 24,
    }
}
#[cfg(not(target_pointer_width = "64"))]
compile_error!("simjig's C ABI is defined for 64-bit targets only");

/// Why a vtable cannot be used by this runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AbiError {
    /// The model was built against a different `SIMJIG_ABI_VERSION`.
    Version { model: u32, runtime: u32 },
    /// The model's table is shorter than this runtime's mirror.
    Size { model: u32, runtime: usize },
    /// The model's IR hash differs from the generated Rust's.
    IrHash { model: u64, expected: u64 },
    /// A row of the signal table holds an unknown storage class or direction.
    Row { id: u32, unknown: Unknown },
    /// A row's storage class or word count does not match its width.
    RowShape {
        id: u32,
        width: u32,
        storage: Storage,
        words: u32,
    },
}

impl ModelVtable {
    /// Checks that this runtime can use the table at `vt` and that it belongs to the
    /// generated Rust with `ir_hash`; returns it as a reference only then.
    ///
    /// A model built against an older header has a shorter table, so no reference to a
    /// whole [`ModelVtable`] may exist before `size` is known to cover it: the two leading
    /// fields are read through the raw pointer first.
    ///
    /// # Safety
    /// `vt` is non-null, 8-aligned, and points to a table exported by a model (at least its
    /// `abi_version` and `size` fields, and `size` bytes in all) that stays valid and
    /// unmodified for `'a`.
    pub(crate) unsafe fn check<'a>(vt: *const Self, ir_hash: u64) -> Result<&'a Self, AbiError> {
        // SAFETY: `vt` is non-null and aligned; naming the field place creates no reference.
        let field = unsafe { &raw const (*vt).abi_version };
        // SAFETY: every table version starts with `abi_version` (caller's precondition).
        let abi_version = unsafe { field.read() };
        if abi_version != ABI_VERSION {
            return Err(AbiError::Version {
                model: abi_version,
                runtime: ABI_VERSION,
            });
        }
        // SAFETY: as for `abi_version`.
        let field = unsafe { &raw const (*vt).size };
        // SAFETY: `size` follows `abi_version` in every version (caller's precondition).
        let size = unsafe { field.read() };
        if (size as usize) < size_of::<Self>() {
            return Err(AbiError::Size {
                model: size,
                runtime: size_of::<Self>(),
            });
        }
        // SAFETY: the model's table spans `size >= size_of::<Self>()` valid bytes for `'a`.
        let table = unsafe { &*vt };
        if table.ir_hash != ir_hash {
            return Err(AbiError::IrHash {
                model: table.ir_hash,
                expected: ir_hash,
            });
        }
        Ok(table)
    }
}

/// An owned copy of a drained [`Msg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedMsg {
    pub(crate) kind: Result<MsgKind, Unknown>,
    pub(crate) line: u32,
    pub(crate) time: u64,
    pub(crate) file: String,
    pub(crate) hier: String,
    pub(crate) msg: String,
}

/// Copies a C string, replacing invalid UTF-8; null reads as "".
///
/// # Safety
/// `s` is null or points to a NUL-terminated string that stays valid during the call.
unsafe fn c_string(s: *const c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: non-null, and the caller guarantees NUL termination and validity.
    let text = unsafe { CStr::from_ptr(s) };
    text.to_string_lossy().into_owned()
}

impl Msg {
    /// Copies the record out of the model's drain buffer.
    ///
    /// # Safety
    /// `self` was filled by `drain_msgs`, and no entry point has been called on the handle
    /// since (the strings live in the model's buffer until then).
    pub(crate) unsafe fn copy_out(self) -> OwnedMsg {
        let msg = if self.msg.is_null() {
            String::new()
        } else {
            // SAFETY: drain_msgs guarantees `msg` points to `msg_len` initialised bytes, valid
            // until the next entry point call (caller's precondition).
            let bytes = unsafe { std::slice::from_raw_parts(self.msg.cast::<u8>(), self.msg_len) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        OwnedMsg {
            kind: MsgKind::try_from(self.kind),
            line: self.line,
            time: self.time,
            // SAFETY: drain_msgs hands out NUL-terminated strings valid until the next call.
            file: unsafe { c_string(self.file) },
            // SAFETY: as for `file`.
            hier: unsafe { c_string(self.hier) },
            msg,
        }
    }
}

impl Signal {
    /// Decodes the row once, when the table is read: the storage class and direction are
    /// checked here, so accessors match on Rust enums only and never guess a size.
    ///
    /// # Errors
    /// [`AbiError::Row`] for an unknown storage class or direction, and
    /// [`AbiError::RowShape`] when the storage or word count disagrees with the width.
    pub(crate) fn decode(&self) -> Result<Decoded, AbiError> {
        let row = |unknown| AbiError::Row {
            id: self.id,
            unknown,
        };
        let storage = Storage::try_from(self.storage).map_err(row)?;
        let dir = Dir::try_from(self.dir).map_err(row)?;
        if self.width == 0
            || storage != Storage::for_width(self.width)
            || self.words != self.width.div_ceil(32)
        {
            return Err(AbiError::RowShape {
                id: self.id,
                width: self.width,
                storage,
                words: self.words,
            });
        }
        let p = self.ptr;
        let cell = match storage {
            Storage::C => Cell::Narrow(Narrow::C(p.cast())),
            Storage::S => Cell::Narrow(Narrow::S(p.cast())),
            Storage::I => Cell::Narrow(Narrow::I(p.cast())),
            Storage::Q => Cell::Narrow(Narrow::Q(p.cast())),
            Storage::W => Cell::Wide(p.cast()),
        };
        Ok(Decoded {
            id: self.id,
            width: self.width,
            words: self.words,
            dir,
            access: self.access,
            name: self.name,
            cell,
        })
    }
}

/// A signal-table row after [`Signal::decode`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Decoded {
    pub(crate) id: u32,
    pub(crate) width: u32,
    pub(crate) words: u32,
    pub(crate) dir: Dir,
    pub(crate) access: u8,
    name: *const c_char,
    pub(crate) cell: Cell,
}

/// Where a signal's value lives, typed by its storage class.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cell {
    /// At most 64 bits, in one integer.
    Narrow(Narrow),
    /// `VlWide<words>`: 32-bit words, least significant first.
    Wide(*mut u32),
}

/// A pointer to a `CData`, `SData`, `IData` or `QData` member.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Narrow {
    C(*mut u8),
    S(*mut u16),
    I(*mut u32),
    Q(*mut u64),
}

impl Cell {
    /// The storage class this cell was decoded from.
    pub(crate) fn storage(self) -> Storage {
        match self {
            Self::Narrow(Narrow::C(_)) => Storage::C,
            Self::Narrow(Narrow::S(_)) => Storage::S,
            Self::Narrow(Narrow::I(_)) => Storage::I,
            Self::Narrow(Narrow::Q(_)) => Storage::Q,
            Self::Wide(_) => Storage::W,
        }
    }
}

impl Decoded {
    /// The signal's hierarchical name.
    ///
    /// # Safety
    /// The row came from a live model's `signals` table (names are static strings).
    pub(crate) unsafe fn name(&self) -> String {
        // SAFETY: the table's names are NUL-terminated static strings (caller's precondition).
        unsafe { c_string(self.name) }
    }
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

/// Reads a wide signal at `ptr` into `out`, least significant word first.
///
/// # Safety
/// - `ptr` is the [`Cell::Wide`] of a row of a live model's table with `ACCESS_READ`, and
///   `out.len()` is the row's `words`;
/// - no entry point of that model runs concurrently.
pub(crate) unsafe fn read_wide(ptr: *mut u32, out: &mut [u32]) {
    // SAFETY: `ptr` points to a live VlWide<words> (`words` u32s, 4-aligned) that does not
    // overlap `out` (Rust-owned memory).
    unsafe { std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), out.len()) }
}

/// Writes a wide signal at `ptr` from `words`, masking the top word to `width`.
///
/// # Safety
/// As for [`read_wide`], with `ACCESS_WRITE`, `words.len()` the row's `words` and `width`
/// its width.
pub(crate) unsafe fn write_wide(ptr: *mut u32, width: u32, words: &[u32]) {
    let top_bits = width % 32;
    for (i, &w) in words.iter().enumerate() {
        let v = if i + 1 == words.len() && top_bits != 0 {
            w & ((1u32 << top_bits) - 1)
        } else {
            w
        };
        // SAFETY: `i < words`, so the offset stays inside the live VlWide member.
        let word = unsafe { ptr.add(i) };
        // SAFETY: `word` is a live, writable, 4-aligned u32 of that member.
        unsafe { word.write(v) }
    }
}

impl ModelVtable {
    /// The model's Verilator `--prefix`.
    pub(crate) fn prefix(&self) -> String {
        // SAFETY: the ABI makes `prefix` null or a NUL-terminated static string (the
        // generated vtable is a `constexpr` table of string literals).
        unsafe { c_string(self.prefix) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The leading fields of an older, shorter table: 136 bytes, as before the trace and
    /// coverage entries were appended.
    #[repr(C, align(8))]
    struct OldTable {
        abi_version: u32,
        size: u32,
        rest: [u8; 128],
    }

    // Run under Miri: a reference to a whole ModelVtable over this allocation is UB.
    #[test]
    fn a_shorter_table_is_rejected_before_any_reference_exists() {
        let old = Box::new(OldTable {
            abi_version: ABI_VERSION,
            size: 136,
            rest: [0; 128],
        });
        let vt: *const ModelVtable = (&raw const *old).cast();
        // SAFETY: the allocation holds abi_version, size and `size` bytes in all.
        let result = unsafe { ModelVtable::check(vt, 0) };
        assert_eq!(
            result.err(),
            Some(AbiError::Size {
                model: 136,
                runtime: size_of::<ModelVtable>()
            })
        );
        let other = Box::new(OldTable {
            abi_version: ABI_VERSION + 1,
            size: 8,
            rest: [0; 128],
        });
        // SAFETY: as above.
        let result = unsafe { ModelVtable::check((&raw const *other).cast(), 0) };
        assert!(matches!(result, Err(AbiError::Version { model: 2, .. })));
    }

    fn row(width: u32, words: u32, storage: u8, dir: u8, ptr: *mut c_void) -> Signal {
        Signal {
            id: 7,
            width,
            words,
            storage,
            dir,
            access: ACCESS_READ | ACCESS_WRITE,
            reserved: 0,
            name: std::ptr::null(),
            ptr,
        }
    }

    // Run under Miri: before decoding, an unknown class was read as a u64.
    #[test]
    fn rows_decode_once_and_reject_unknown_or_inconsistent_values() {
        let mut byte = Box::new(0u8);
        let p: *mut c_void = (&raw mut *byte).cast();
        let unknown = |what, value| AbiError::Row {
            id: 7,
            unknown: Unknown { what, value },
        };
        assert_eq!(
            row(8, 1, 9, 1, p).decode().err(),
            Some(unknown("Storage", 9))
        );
        assert_eq!(row(8, 1, 1, 4, p).decode().err(), Some(unknown("Dir", 4)));
        let shape = |width, storage, words| AbiError::RowShape {
            id: 7,
            width,
            storage,
            words,
        };
        assert_eq!(
            row(12, 1, 1, 1, p).decode().err(),
            Some(shape(12, Storage::C, 1))
        );
        assert_eq!(
            row(100, 3, 5, 1, p).decode().err(),
            Some(shape(100, Storage::W, 3))
        );
        let ok = row(5, 1, 1, 1, p).decode().unwrap();
        let Cell::Narrow(cell) = ok.cell else {
            panic!("CData row decoded as wide")
        };
        // SAFETY: `cell` points to the live, writable byte above.
        unsafe { write_narrow(cell, ok.width, u64::MAX) };
        // SAFETY: as above.
        assert_eq!(unsafe { read_narrow(cell) }, 0x1f, "masked to 5 bits");
    }
}

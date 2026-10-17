//! The [`Packed`] trait: a value with a fixed SystemVerilog bit width and a Verilator
//! storage representation.
//!
//! Signal handles move values between Rust and model memory only through this trait, so
//! its contract is the whole agreement between generated code and the model.

use core::fmt;

/// A value of a fixed bit width that maps one-to-one onto Verilator's storage for a
/// signal of that width.
///
/// # Contract
///
/// Every implementation, including generated packed structs and enums, must uphold all of
/// the following. Signal handles rely on them; breaking one corrupts model state, because
/// Verilator assumes the unused upper bits of its storage are zero and does not check.
///
/// 1. **Width.** [`WIDTH`](Packed::WIDTH) is at least 1 and equals the SystemVerilog
///    `$bits` of the type.
/// 2. **Storage.** [`Raw`](Packed::Raw) is the C++ type Verilator 5 uses for a signal of
///    `WIDTH` bits:
///
///    | `WIDTH`  | Verilator type | `Raw`                                    |
///    |----------|----------------|------------------------------------------|
///    | 1..=8    | `CData`        | `u8`                                     |
///    | 9..=16   | `SData`        | `u16`                                    |
///    | 17..=32  | `IData`        | `u32`                                    |
///    | 33..=64  | `QData`        | `u64`                                    |
///    | 65 and up| `VlWide<W>`    | `[u32; W]` with `W = WIDTH.div_ceil(32)` |
///
///    `VlWide` words are little-endian: word `i` holds bits `32 * i + 31 ..= 32 * i`.
///    Raw bit `i` is SystemVerilog bit `i` of the packed value (for a packed struct, the
///    first declared member holds the most significant bits, IEEE 1800-2023 §7.2.1).
/// 3. **`to_raw` is clean.** Every raw bit at position `WIDTH` or above is zero.
/// 4. **`from_raw` is total and ignores the upper bits.** It never panics, and
///    `from_raw(r)` depends only on the low `WIDTH` bits of `r`.
/// 5. **Round trip.** `from_raw(x.to_raw()) == x` for every value `x`, and
///    `from_raw(r).to_raw()` equals `r` with the bits at `WIDTH` and above cleared.
///
/// Rule 4 together with rule 5 means every `WIDTH`-bit pattern is a valid value. A type
/// whose encoding is not total (an enum with unused encodings) must still represent every
/// pattern, for example by keeping the raw bits of an unknown encoding.
///
/// # Examples
///
/// ```
/// use simjig_bits::{Bits, Packed};
///
/// let v = Bits::<12>::new(0xabc);
/// let raw: u16 = v.to_raw();
/// assert_eq!(raw, 0xabc);
/// // Bits above WIDTH are ignored.
/// assert_eq!(Bits::<12>::from_raw(0xfabc), v);
/// ```
pub trait Packed: Copy + 'static {
    /// The width in bits (SystemVerilog `$bits`).
    const WIDTH: u32;

    /// Verilator's storage type for a signal of [`WIDTH`](Packed::WIDTH) bits.
    type Raw: Copy + Eq + fmt::Debug + 'static;

    /// Converts to Verilator storage, with every bit at `WIDTH` and above cleared.
    fn to_raw(self) -> Self::Raw;

    /// Converts from Verilator storage, ignoring every bit at `WIDTH` and above.
    fn from_raw(raw: Self::Raw) -> Self;
}

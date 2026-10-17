//! Bit-vector value types with SystemVerilog semantics, as stored by Verilator.
//!
//! - [`Bit`]: one 2-state bit, for 1-bit signals.
//! - [`Bits<N>`]: an unsigned 2-state vector of 1 to 128 bits.
//! - [`WideBits<N, W>`]: an unsigned 2-state vector of more than 128 bits, laid out like
//!   Verilator's `VlWide<W>`.
//! - [`Packed`]: the contract between a value type and Verilator storage. Signal handles
//!   and generated structs and enums build on it.
//!
//! Widths never change implicitly. Operators combine equal widths and wrap; `zext`,
//! `sext` and `trunc` change width explicitly.
//!
//! # Integer literals
//!
//! Signal handles take `impl Into<T>`, so a plain literal must convert into every value
//! type. Each type implements `From` for exactly **one** integer type, `u128`. With a
//! single candidate the compiler infers an untyped literal as `u128` instead of falling
//! back to `i32`, so literals up to 128 bits work at every width and negative literals are
//! rejected at compile time. The conversion checks the range and panics at the caller's
//! line (the panic names the width). Every other integer type converts with `TryFrom`,
//! and `try_new` is the fallible form for `u128`.
//!
//! ```
//! use simjig_bits::{Bit, Bits, WideBits};
//!
//! // A stand-in for a signal handle's setter.
//! fn set<T>(value: impl Into<T>) -> T {
//!     value.into()
//! }
//!
//! let a: Bits<8> = set(0x5a);
//! let b: Bits<64> = set(0xdead_beef_cafe_f00d);
//! let c: WideBits<200, 7> = set(1);
//! let d: Bit = set(1);
//! let e: Bit = set(true);
//! assert_eq!(a.to_string(), "8'h5a");
//! assert_eq!(b, 0xdead_beef_cafe_f00d);
//! assert_eq!((c, d, e), (WideBits::from(1), Bit::ONE, Bit::ONE));
//!
//! // Integers that are not literals use TryFrom.
//! let narrow: u16 = 0x1ff;
//! assert!(Bits::<8>::try_from(narrow).is_err());
//! ```
//!
//! ```should_panic
//! # use simjig_bits::Bits;
//! // Panics with "Bits<8>: value 0x15a does not fit in 8 bits (maximum 0xff)".
//! let _: Bits<8> = 0x15a.into();
//! ```
//!
//! ```compile_fail,E0277
//! # use simjig_bits::Bits;
//! let _: Bits<8> = (-1).into(); // u128 has no negation
//! ```
#![forbid(unsafe_code)]

mod bit;
mod bits;
mod error;
mod packed;
mod wide;
mod words;

pub use bit::Bit;
pub use bits::Bits;
pub use error::OutOfRange;
pub use packed::Packed;
pub use wide::WideBits;

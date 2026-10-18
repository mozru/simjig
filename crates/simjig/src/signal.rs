//! Typed signal handles: [`In`], [`Out`], [`InOut`], [`PublicRd`] and [`PublicRw`].
//!
//! A handle is a [`RawSig`] plus the signal's value type `T` (a [`Packed`] type from
//! `simjig-bits` or a generated struct or enum) and its direction, both in the type. It is
//! `Copy`, resolved through the running kernel on every access, and never holds a reference
//! into model memory. The direction decides the methods:
//!
//! | Handle | Binds | `get` | `set` |
//! |---|---|---|---|
//! | [`In<T>`] | a top-level input | yes | yes |
//! | [`Out<T>`] | a top-level output | yes | no |
//! | [`InOut<T>`] | a top-level inout | yes | yes |
//! | [`PublicRd<T>`] | a `public_flat_rd` internal signal | yes | no |
//! | [`PublicRw<T>`] | a `public_flat_rw` internal signal | yes | yes |
//!
//! Every handle also has [`changed`](In::changed), and for 1-bit types
//! [`rising`](In::rising) and [`falling`](In::falling), which delegate to the kernel triggers.
//!
//! Semantics are the kernel's (see [`RawSig`]): `get` returns the model's current value, and
//! `set` is a deposit applied after the next eval, so a task never reads back its own write
//! before awaiting.
//!
//! # Values
//!
//! `set` takes `impl Into<T>` (DECISION §6), so a literal works (`dut.data.set(0x5a)`): every
//! `simjig-bits` type converts from exactly one integer type, `u128`, which makes the literal
//! infer as `u128`. A `u128` that does not fit the signal's width panics at the caller's line
//! **naming the signal** (`set` checks the width itself before converting, so the conversion's
//! own panic, which knows only the width, never fires). Other conversions (`bool` into `Bit`,
//! a generated enum's constants) go through `Into` unchanged.
//!
//! `get` and `set` require `T: Packed<Raw: RawStorage>`. [`RawStorage`] is implemented for
//! exactly the storage types the [`Packed`] contract allows (`u8`, `u16`, `u32`, `u64` and
//! `[u32; W]`), so the bound holds for every conforming type; it exists because the contract
//! does not state it in the type system.

use std::any::Any;
use std::fmt;
use std::marker::PhantomData;

use simjig_bits::Packed;

use crate::kernel::signal::RawSig;
use crate::kernel::trigger::{self, Edge};
use crate::model::meta::SignalAccess;

mod sealed {
    use crate::kernel::signal::RawSig;

    /// Conversion between a Verilator storage type and the kernel's `u32` words.
    pub trait Words: Copy {
        /// A word buffer of exactly the right length.
        type Buf: AsRef<[u32]> + AsMut<[u32]>;
        /// An all-zero buffer.
        fn zeroed() -> Self::Buf;
        /// The value as words, least significant first.
        fn to_words(self) -> Self::Buf;
        /// The value of `buf`.
        fn from_words(buf: &Self::Buf) -> Self;
    }

    /// Construction of a handle from a slot; crate-only, so a handle's direction cannot be
    /// changed by rebinding its `RawSig`.
    pub trait FromRaw {
        fn from_raw(raw: RawSig) -> Self;
    }
}

/// A Verilator storage type: `u8`, `u16`, `u32`, `u64` or `[u32; W]` (the [`Packed::Raw`]
/// types). Sealed; it only exists to name the bound on [`In::get`] and [`In::set`].
pub trait RawStorage: sealed::Words {}

macro_rules! narrow_words {
    ($($raw:ty => $n:literal),*) => {$(
        impl sealed::Words for $raw {
            type Buf = [u32; $n];
            fn zeroed() -> Self::Buf {
                [0; $n]
            }
            fn to_words(self) -> Self::Buf {
                let wide = u64::from(self);
                let mut buf = [0; $n];
                buf[0] = low_word(wide);
                if let Some(high) = buf.get_mut(1) {
                    *high = low_word(wide >> 32);
                }
                buf
            }
            fn from_words(buf: &Self::Buf) -> Self {
                let high = buf.get(1).map_or(0, |&w| u64::from(w));
                let wide = high << 32 | u64::from(buf[0]);
                // The model keeps the bits above the storage width clear; masking makes the
                // conversion total anyway.
                <$raw>::try_from(wide & u64::from(<$raw>::MAX)).unwrap_or(<$raw>::MAX)
            }
        }
        impl RawStorage for $raw {}
    )*};
}

narrow_words!(u8 => 1, u16 => 1, u32 => 1, u64 => 2);

/// Bits 31..0 of `value`.
fn low_word(value: u64) -> u32 {
    u32::try_from(value & 0xffff_ffff).unwrap_or(u32::MAX)
}

impl<const W: usize> sealed::Words for [u32; W] {
    type Buf = Self;
    fn zeroed() -> Self {
        [0; W]
    }
    fn to_words(self) -> Self {
        self
    }
    fn from_words(buf: &Self) -> Self {
        *buf
    }
}

impl<const W: usize> RawStorage for [u32; W] {}

/// A handle type that generated code binds to a slot with
/// [`Binder::bind`](crate::__private::Binder::bind). Sealed.
pub trait Bind: Copy + sealed::FromRaw {
    /// The access kind the slot must have.
    const ACCESS: SignalAccess;
    /// The width the slot must have.
    const WIDTH: u32;
}

#[track_caller]
fn get<T: Packed<Raw: RawStorage>>(raw: RawSig) -> T {
    let mut buf = <T::Raw as sealed::Words>::zeroed();
    raw.read_words(buf.as_mut());
    T::from_raw(sealed::Words::from_words(&buf))
}

#[track_caller]
fn set<T: Packed<Raw: RawStorage>, V: Into<T> + 'static>(raw: RawSig, value: V) {
    // Monomorphised per `V`, so the type test folds away and other conversions pay nothing.
    if let Some(&literal) = (&value as &dyn Any).downcast_ref::<u128>() {
        check_literal::<T>(raw, literal);
    }
    let value: T = value.into();
    raw.write_words(sealed::Words::to_words(value.to_raw()).as_ref());
}

/// Panics, naming the signal, if `literal` does not fit `T`.
#[track_caller]
fn check_literal<T: Packed>(raw: RawSig, literal: u128) {
    let width = T::WIDTH;
    assert!(
        width >= 128 || literal >> width == 0,
        "signal `{}` is {width} bits wide: value {literal:#x} does not fit (maximum {:#x})",
        raw.name(),
        (1u128 << width) - 1
    );
}

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident, $access:ident) => {
        $(#[$doc])*
        pub struct $name<T> {
            raw: RawSig,
            value: PhantomData<fn() -> T>,
        }

        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T> Copy for $name<T> {}

        impl<T> fmt::Debug for $name<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.raw).finish()
            }
        }

        impl<T> sealed::FromRaw for $name<T> {
            fn from_raw(raw: RawSig) -> Self {
                Self {
                    raw,
                    value: PhantomData,
                }
            }
        }

        impl<T: Packed> Bind for $name<T> {
            const ACCESS: SignalAccess = SignalAccess::$access;
            const WIDTH: u32 = T::WIDTH;
        }

        impl<T: Packed> $name<T> {
            /// The untyped handle, for kernel APIs such as [`Clock::new`](crate::Clock::new).
            #[must_use]
            pub const fn raw(self) -> RawSig {
                self.raw
            }

            /// The signal's hierarchical name.
            ///
            /// # Panics
            /// Outside the handle's running simulation.
            #[track_caller]
            #[must_use]
            pub fn name(self) -> String {
                self.raw.name()
            }

            /// A trigger that resumes after an eval that changed any bit of the signal.
            ///
            /// # Panics
            /// Outside the handle's running simulation.
            #[track_caller]
            pub fn changed(self) -> Edge {
                trigger::changed(self.raw)
            }

            /// A trigger that resumes after an eval in which the signal went from 0 to 1.
            /// Only for 1-bit types; any other width fails to build (`cargo build`, not
            /// `cargo check`: the check runs when the call is monomorphised).
            ///
            /// # Panics
            /// Outside the handle's running simulation.
            #[track_caller]
            pub fn rising(self) -> Edge {
                const { assert!(T::WIDTH == 1, "rising() needs a 1-bit signal; use changed()") };
                trigger::rising(self.raw)
            }

            /// A trigger that resumes after an eval in which the signal went from 1 to 0.
            /// Only for 1-bit types, like [`rising`](Self::rising).
            ///
            /// # Panics
            /// Outside the handle's running simulation.
            #[track_caller]
            pub fn falling(self) -> Edge {
                const { assert!(T::WIDTH == 1, "falling() needs a 1-bit signal; use changed()") };
                trigger::falling(self.raw)
            }
        }

        impl<T: Packed<Raw: RawStorage>> $name<T> {
            /// The signal's current value (writes become visible after the next eval).
            ///
            /// # Panics
            /// Outside the handle's running simulation.
            #[track_caller]
            #[must_use]
            pub fn get(self) -> T {
                get(self.raw)
            }
        }
    };
}

macro_rules! settable {
    ($name:ident) => {
        impl<T: Packed<Raw: RawStorage>> $name<T> {
            /// Deposits `value`; it is applied after the next eval and lasts until the
            /// design or another write changes it.
            ///
            /// # Panics
            /// Outside the handle's running simulation, in the `ReadOnly` phase (as a
            /// `WriteInReadOnly` failure), or if `value` is a `u128` that does not fit the
            /// signal's width (the message names the signal and its width).
            #[track_caller]
            pub fn set(self, value: impl Into<T> + 'static) {
                set(self.raw, value);
            }
        }
    };
}

handle! {
    /// A top-level input of the model: the testbench drives it with [`set`](Self::set) and
    /// can read it back with [`get`](Self::get).
    In, In
}
settable!(In);

handle! {
    /// A top-level output of the model: read-only to the testbench.
    ///
    /// ```
    /// use simjig::Out;
    /// use simjig_bits::Bit;
    ///
    /// fn sample(y: Out<Bit>) -> Bit {
    ///     y.get()
    /// }
    /// ```
    ///
    /// An output cannot be written; this does not compile:
    ///
    /// ```compile_fail
    /// use simjig::Out;
    /// use simjig_bits::Bit;
    ///
    /// fn drive(y: Out<Bit>) {
    ///     y.set(1);
    /// }
    /// ```
    Out, Out
}

handle! {
    /// A top-level inout of the model: the testbench can deposit values and read them.
    InOut, InOut
}
settable!(InOut);

handle! {
    /// An internal signal made visible with `public_flat_rd`: read-only to the testbench.
    ///
    /// A `PublicRd` cannot be written; this does not compile:
    ///
    /// ```compile_fail
    /// use simjig::PublicRd;
    /// use simjig_bits::Bits;
    ///
    /// fn drive(s: PublicRd<Bits<8>>) {
    ///     s.set(1);
    /// }
    /// ```
    PublicRd, PublicRd
}

handle! {
    /// An internal signal made writable with `public_flat_rw`. A write is a deposit: it lasts
    /// only until the signal's driver next evaluates.
    PublicRw, PublicRw
}
settable!(PublicRw);

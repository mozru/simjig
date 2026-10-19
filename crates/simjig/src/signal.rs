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
//! `get` and `set` require `T: Packed<Raw: RawWords>`. [`RawWords`] is implemented for
//! exactly the storage types the [`Packed`] contract allows (`u8`, `u16`, `u32`, `u64` and
//! `[u32; W]`), so the bound holds for every conforming type; it exists because the contract
//! does not state it in the type system. Generated packed structs use the same trait to
//! convert their storage ([`pack_field`](crate::__private::pack_field)).
//!
//! [`Value`] and [`Vector`] name those bounds (and the ones a test compares and prints
//! with) as one trait each, for code generic over the value type: a parameter sweep's
//! associated types are bounded by them.
//!
//! A handle's type parameter is unconstrained, so generated code can name handles of
//! unpacked arrays (`In<[Bits<8>; 4]>`); such handles have no `get`/`set` until the ABI
//! describes unpacked storage.

use std::any::Any;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use simjig_bits::Packed;

use crate::kernel::backend::words;
use crate::kernel::signal::RawSig;
use crate::kernel::state::Kernel;
use crate::kernel::trigger::{self, Edge};

pub(crate) mod sealed {
    use std::rc::Rc;

    use crate::kernel::state::Kernel;

    /// What [`Bind::bind_to`](super::Bind::bind_to) takes: only this crate can make one, so
    /// only `SyncClock::bind` can bind a handle.
    pub struct BindTo {
        pub(crate) kernel: Rc<Kernel>,
        /// Whether the run may read and write the model's storage directly (held).
        pub(crate) direct: bool,
    }

    impl std::fmt::Debug for BindTo {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BindTo")
                .field("direct", &self.direct)
                .finish_non_exhaustive()
        }
    }

    /// A word buffer sized for one storage type, so accesses need no allocation.
    pub trait Buffer: Copy {
        /// A buffer of exactly the storage's word count.
        type Buf: AsRef<[u32]> + AsMut<[u32]>;
        /// An all-zero buffer.
        fn zeroed() -> Self::Buf;
    }
}

/// Verilator storage as little-endian 32-bit words (word 0 holds bits 31..0): implemented
/// for the [`Packed::Raw`] types `u8`, `u16`, `u32` (one word), `u64` (two words) and
/// `[u32; W]` (`W` words). Sealed.
pub trait RawWords: sealed::Buffer {
    /// Writes the value into `out[..n]`, `n` being the storage's word count.
    ///
    /// # Panics
    /// If `out` is shorter than `n`.
    fn to_words(self, out: &mut [u32]);

    /// Reads a value from the first `n` words of `words`, ignoring bits beyond the
    /// storage type.
    ///
    /// # Panics
    /// If `words` is shorter than `n`.
    fn from_words(words: &[u32]) -> Self;
}

macro_rules! narrow_words {
    ($($raw:ty => $n:literal),*) => {$(
        impl sealed::Buffer for $raw {
            type Buf = [u32; $n];
            fn zeroed() -> Self::Buf {
                [0; $n]
            }
        }

        impl RawWords for $raw {
            fn to_words(self, out: &mut [u32]) {
                let wide = u64::from(self);
                out[0] = low_word(wide);
                if $n == 2 {
                    out[1] = low_word(wide >> 32);
                }
            }

            fn from_words(words: &[u32]) -> Self {
                let high = if $n == 2 { u64::from(words[1]) } else { 0 };
                let wide = high << 32 | u64::from(words[0]);
                // Bits beyond the storage type are ignored, so the conversion is total.
                <$raw>::try_from(wide & u64::from(<$raw>::MAX)).unwrap_or(<$raw>::MAX)
            }
        }
    )*};
}

narrow_words!(u8 => 1, u16 => 1, u32 => 1, u64 => 2);

/// Bits 31..0 of `value`.
fn low_word(value: u64) -> u32 {
    u32::try_from(value & 0xffff_ffff).unwrap_or(u32::MAX)
}

impl<const W: usize> sealed::Buffer for [u32; W] {
    type Buf = Self;
    fn zeroed() -> Self {
        [0; W]
    }
}

impl<const W: usize> RawWords for [u32; W] {
    fn to_words(self, out: &mut [u32]) {
        out[..W].copy_from_slice(&self);
    }

    fn from_words(words: &[u32]) -> Self {
        let mut out = [0; W];
        out.copy_from_slice(&words[..W]);
        out
    }
}

/// A value a handle can [`get`](In::get) and [`set`](In::set), compare and print: every
/// type simjig-build generates for a port (`Bit`, `Bits<N>`, `WideBits`, a packed struct,
/// union or enum).
///
/// It names the bounds a generic test body needs, so the body of a parameter sweep
/// (`async fn t<D: fifo::Ports>(dut: D)`) needs no `where` clause: an associated type of a
/// sweep's `Ports` trait is bounded by `Value` (or by [`Vector`], or by a generated
/// `<Struct>Fields` trait, which extends it). Implemented for every type that has the
/// bounds; [`from_u128`](Self::from_u128) and [`to_u128`](Self::to_u128) are provided,
/// an escape hatch to a value's packed bits when its type is opaque (an associated type).
///
/// ```
/// use simjig::Value;
/// use simjig::bits::Bits;
///
/// fn round_trip<T: Value>(bits: u128) -> u128 {
///     T::from_u128(bits).to_u128()
/// }
/// assert_eq!(round_trip::<Bits<12>>(0xabc), 0xabc);
/// ```
pub trait Value: Packed<Raw: RawWords> + Eq + fmt::Debug + Default {
    /// The value whose packed bits are `bits` (`$bits(Self)` wide, the first struct field
    /// most significant, as Verilator stores it).
    ///
    /// # Panics
    /// If `bits` has a bit set at [`Packed::WIDTH`] or above.
    #[track_caller]
    #[must_use]
    fn from_u128(bits: u128) -> Self {
        assert!(
            Self::WIDTH >= 128 || bits >> Self::WIDTH == 0,
            "{bits:#x} does not fit {} bits",
            Self::WIDTH
        );
        let mut buf = <Self::Raw as sealed::Buffer>::zeroed();
        for (i, word) in buf.as_mut().iter_mut().take(4).enumerate() {
            *word = u32::try_from(bits >> (32 * i) & 0xffff_ffff).unwrap_or(u32::MAX);
        }
        Self::from_raw(Self::Raw::from_words(buf.as_ref()))
    }

    /// The value's packed bits as an integer (`Bit` is 0 or 1).
    ///
    /// # Panics
    /// If the value does not fit 128 bits (a bit set above bit 127).
    #[track_caller]
    #[must_use]
    fn to_u128(self) -> u128 {
        let mut buf = <Self::Raw as sealed::Buffer>::zeroed();
        self.to_raw().to_words(buf.as_mut());
        let words = buf.as_ref();
        assert!(
            words.iter().skip(4).all(|&w| w == 0),
            "{self:?} ({} bits) does not fit a u128",
            Self::WIDTH
        );
        words
            .iter()
            .take(4)
            .rev()
            .fold(0, |acc, &w| acc << 32 | u128::from(w))
    }
}

impl<T: Packed<Raw: RawWords> + Eq + fmt::Debug + Default> Value for T {}

/// A bit vector: a [`Value`] that converts from a `u128` literal and compares with one
/// (`Bit`, `Bits<N>` and `WideBits`, not the generated structs and enums).
///
/// In a generic body, `dut.data().set(0x5a)`, `dut.data().get() == 3` and
/// `dut.count().get().to_u128()` ([`Value::to_u128`]) work for every width. Implemented
/// for every type that has the bounds.
pub trait Vector: Value + From<u128> + PartialEq<u128> {}

impl<T: Value + From<u128> + PartialEq<u128>> Vector for T {}

/// Construction of a handle from a slot; crate-only, so a handle's direction cannot be
/// changed by rebinding its `RawSig`.
pub(crate) trait FromRaw {
    fn from_raw(raw: RawSig) -> Self;
}

/// One kernel lookup: checks the width (a handle typed for another width would silently
/// read the wrong bits) and copies the value out.
#[track_caller]
fn get<T: Packed<Raw: RawWords>>(raw: RawSig) -> T {
    let mut buf = <T::Raw as sealed::Buffer>::zeroed();
    raw.with(|k, slot| {
        let width = k.widths[slot as usize];
        assert!(
            width == T::WIDTH && buf.as_ref().len() == words(width),
            "signal `{}` is {width} bits wide, but its handle's type is {} bits",
            k.names[slot as usize],
            T::WIDTH
        );
        if !k.direct.read(slot, buf.as_mut()) {
            k.models.borrow().read(slot, buf.as_mut());
        }
    });
    T::from_raw(RawWords::from_words(buf.as_ref()))
}

#[track_caller]
fn set<T: Packed<Raw: RawWords>, V: Into<T> + 'static>(raw: RawSig, value: V) {
    // Monomorphised per `V`, so the type test folds away and other conversions pay nothing.
    if let Some(&literal) = (&value as &dyn Any).downcast_ref::<u128>() {
        check_literal::<T>(raw, literal);
    }
    debug_assert_eq!(raw.width(), T::WIDTH, "the handle's type width");
    let value: T = value.into();
    let mut buf = <T::Raw as sealed::Buffer>::zeroed();
    value.to_raw().to_words(buf.as_mut());
    raw.write_words(buf.as_ref());
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
    ($(#[$doc:meta])* $name:ident) => {
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

        impl<T> FromRaw for $name<T> {
            fn from_raw(raw: RawSig) -> Self {
                Self {
                    raw,
                    value: PhantomData,
                }
            }
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

        impl<T: Packed<Raw: RawWords>> $name<T> {
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
        impl<T: Packed<Raw: RawWords>> $name<T> {
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

        /// The untyped handle, so kernel APIs that drive a signal take a writable handle
        /// as is: `simjig::clock(dut.clk, ns(10))`. Read-only handles have no such
        /// conversion (a clock on an output cannot compile); use their `raw()`.
        impl<T> From<$name<T>> for RawSig {
            fn from(handle: $name<T>) -> Self {
                handle.raw
            }
        }
    };
}

handle! {
    /// A top-level input of the model: the testbench drives it with [`set`](Self::set) and
    /// can read it back with [`get`](Self::get).
    ///
    /// A writable handle converts into a [`RawSig`], so a kernel clock takes it directly:
    ///
    /// ```
    /// use simjig::{In, clock, ns};
    /// use simjig_bits::Bit;
    ///
    /// fn start(clk: In<Bit>) {
    ///     clock(clk, ns(10)).start();
    /// }
    /// ```
    ///
    /// An output has no such conversion, so a clock on it does not compile:
    ///
    /// ```compile_fail
    /// use simjig::{Out, clock, ns};
    /// use simjig_bits::Bit;
    ///
    /// fn start(q: Out<Bit>) {
    ///     clock(q, ns(10)).start();
    /// }
    /// ```
    In
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
    Out
}

handle! {
    /// A top-level inout of the model: the testbench can deposit values and read them.
    InOut
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
    PublicRd
}

handle! {
    /// An internal signal made writable with `public_flat_rw`. A write is a deposit: it lasts
    /// only until the signal's driver next evaluates.
    PublicRw
}
settable!(PublicRw);

/// A handle bound to a synchronous run ([`SyncClock::bind`](crate::SyncClock::bind)):
/// [`get`](Self::get) reads the model's storage directly, with none of a handle's lookups.
/// Made from a read-only handle ([`Out`], [`PublicRd`]); [`BoundMut`] also writes.
///
/// It reads what the handle would, and checks nothing at each access: the width was checked
/// when it was bound, and the run it belongs to is the one that made it. It is for that run;
/// after it ends it reads the model's final state.
pub struct Bound<T> {
    slot: BoundSlot,
    value: PhantomData<fn() -> T>,
}

/// A handle bound to a synchronous run that also writes: [`set`](Self::set) puts the value
/// into the model's storage at once, as a synchronous test's `set` does, and the next eval
/// takes it. Made from [`In`], [`InOut`] or [`PublicRw`].
pub struct BoundMut<T> {
    slot: BoundSlot,
    value: PhantomData<fn() -> T>,
}

/// What a bound handle reads and writes through: the kernel's direct table (kept alive by
/// the `Rc`), or, for a slot without a direct way, the handle's own path.
struct BoundSlot {
    kernel: Rc<Kernel>,
    flat: u32,
    raw: RawSig,
    direct: bool,
}

impl BoundSlot {
    #[inline]
    fn read<T: Packed<Raw: RawWords>>(&self) -> T {
        let mut buf = <T::Raw as sealed::Buffer>::zeroed();
        if !(self.direct && self.kernel.direct.read(self.flat, buf.as_mut())) {
            return get(self.raw);
        }
        T::from_raw(RawWords::from_words(buf.as_ref()))
    }

    #[inline]
    #[track_caller]
    fn write<T: Packed<Raw: RawWords>, V: Into<T> + 'static>(&self, value: V) {
        if let Some(&literal) = (&value as &dyn Any).downcast_ref::<u128>() {
            check_literal::<T>(self.raw, literal);
        }
        let value: T = value.into();
        let mut buf = <T::Raw as sealed::Buffer>::zeroed();
        value.to_raw().to_words(buf.as_mut());
        if !(self.direct && self.kernel.direct.write(self.flat, buf.as_ref())) {
            self.raw.write_words(buf.as_ref());
        }
    }
}

impl<T: Packed<Raw: RawWords>> Bound<T> {
    /// The signal's current value.
    #[inline]
    #[must_use]
    pub fn get(&self) -> T {
        self.slot.read()
    }
}

impl<T: Packed<Raw: RawWords>> BoundMut<T> {
    /// The signal's current value.
    #[inline]
    #[must_use]
    pub fn get(&self) -> T {
        self.slot.read()
    }

    /// Puts `value` into the model (the next eval takes it).
    ///
    /// # Panics
    /// If `value` is a `u128` that does not fit the signal's width.
    #[inline]
    #[track_caller]
    pub fn set(&self, value: impl Into<T> + 'static) {
        self.slot.write::<T, _>(value);
    }
}

impl<T> fmt::Debug for Bound<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Bound").field(&self.slot.raw).finish()
    }
}

impl<T> fmt::Debug for BoundMut<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BoundMut").field(&self.slot.raw).finish()
    }
}

/// A typed handle a synchronous run can bind ([`SyncClock::bind`](crate::SyncClock::bind)):
/// [`In`], [`Out`], [`InOut`], [`PublicRd`] and [`PublicRw`].
pub trait Bind {
    /// The bound handle: [`BoundMut`] for a writable handle, [`Bound`] for a read-only one.
    type Bound;
    /// Binds the handle; only `SyncClock::bind` can call it.
    #[doc(hidden)]
    fn bind_to(self, to: sealed::BindTo) -> Self::Bound;
}

macro_rules! bind {
    ($bound:ident: $($name:ident),*) => {$(
        impl<T: Packed> Bind for $name<T> {
            type Bound = $bound<T>;

            fn bind_to(self, to: sealed::BindTo) -> Self::Bound {
                let sealed::BindTo { kernel, direct } = to;
                let flat = self.raw.with(|k, flat| {
                    let width = k.widths[flat as usize];
                    assert!(
                        width == T::WIDTH,
                        "signal `{}` is {width} bits wide, but its handle's type is {} bits",
                        k.names[flat as usize],
                        T::WIDTH
                    );
                    flat
                });
                $bound {
                    slot: BoundSlot { kernel, flat, raw: self.raw, direct },
                    value: PhantomData,
                }
            }
        }
    )*};
}

bind!(BoundMut: In, InOut, PublicRw);
bind!(Bound: Out, PublicRd);

#[cfg(test)]
mod tests {
    use simjig_bits::Bit;

    use super::*;
    use crate::kernel::clock::Clock;
    use crate::kernel::fake::FakeModel;
    use crate::kernel::sim::Sim;
    use crate::kernel::time::{Precision, SimDuration};
    use crate::kernel::trigger::{changed, cycles, falling, now, rising};

    /// `Value::to_u128` reads every vector type, `Bit` as 0 or 1, and refuses a wide
    /// value above 128 bits rather than cutting it.
    #[test]
    fn vectors_read_as_integers() {
        use simjig_bits::{Bits, WideBits};
        assert_eq!(Value::to_u128(Bit::ONE), 1);
        assert_eq!(Value::to_u128(Bit::ZERO), 0);
        assert_eq!(Value::to_u128(Bits::<7>::new(0x5a)), 0x5a);
        assert_eq!(
            Value::to_u128(Bits::<64>::new(u128::from(u64::MAX))),
            u128::from(u64::MAX)
        );
        assert_eq!(Value::to_u128(Bits::<128>::new(u128::MAX)), u128::MAX);
        assert_eq!(
            Value::to_u128(WideBits::<200, 7>::from(u128::MAX)),
            u128::MAX
        );
        let wide = std::panic::catch_unwind(|| {
            let mut v = WideBits::<200, 7>::default();
            v.set_bit(128, true);
            Value::to_u128(v)
        });
        assert!(wide.is_err());
    }

    /// `Value::from_u128` is `to_u128`'s inverse at every storage size, and refuses bits
    /// above the width rather than dropping them.
    #[test]
    fn values_are_made_from_their_packed_bits() {
        use simjig_bits::{Bits, WideBits};
        fn round<T: Value>(bits: u128) -> u128 {
            T::from_u128(bits).to_u128()
        }
        assert_eq!(<Bit as Value>::from_u128(1), Bit::ONE);
        assert_eq!(round::<Bits<12>>(0xabc), 0xabc);
        assert_eq!(round::<Bits<40>>(0xab_1234_5678), 0xab_1234_5678);
        assert_eq!(
            round::<Bits<100>>(1 << 99 | 0x1234_5678_9abc),
            1 << 99 | 0x1234_5678_9abc
        );
        assert_eq!(round::<Bits<128>>(u128::MAX), u128::MAX);
        assert_eq!(round::<WideBits<200, 7>>(u128::MAX - 5), u128::MAX - 5);
        assert!(std::panic::catch_unwind(|| <Bits<12> as Value>::from_u128(0x1000)).is_err());
    }

    #[test]
    fn the_free_triggers_take_a_writable_typed_handle() {
        let mut model = FakeModel::new(Precision::NS);
        model.signal("clk", 1);
        let mut sim = Sim::from_fake(model);
        let raw = sim.signal("clk").unwrap();
        sim.add_clock(Clock::new(raw, SimDuration::from_ticks(10)))
            .unwrap();
        let clk = In::<Bit>::from_raw(raw);
        let t = sim.run(async move {
            rising(clk).await;
            falling(clk).await;
            changed(clk).await;
            cycles(clk, 2).await;
            now()
        });
        assert_eq!(t.unwrap().ticks(), 30);
    }
}

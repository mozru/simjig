//! The C ABI of DPI-C arguments and results (IEEE 1800-2023 Annex H, as Verilator 5.052
//! declares it in `V<prefix>__Dpi.h`), and its conversion to the Rust types of the
//! generated `Dpi` trait.
//!
//! Each SV type class has a zero-sized marker implementing [`Abi`]:
//!
//! | Marker | SV | C input | C output/inout | C result | Rust |
//! |---|---|---|---|---|---|
//! | [`I8`]/[`U8`] | `byte` / `byte unsigned` | `char` / `unsigned char` | pointer | same | `i8` / `u8` |
//! | [`I16`]/[`U16`] | `shortint` (`unsigned`) | `short` | pointer | same | `i16` / `u16` |
//! | [`I32`]/[`U32`] | `int` (`unsigned`) | `int` | pointer | same | `i32` / `u32` |
//! | [`I64`]/[`U64`] | `longint` (`unsigned`) | `long long` | pointer | same | `i64` / `u64` |
//! | [`Bit`] / [`Logic`] | `bit` / `logic` | `svBit` / `svLogic` | pointer | same | `Bit` |
//! | [`BitVec<N>`] | `bit [N-1:0]` | `const svBitVecVal*` | `svBitVecVal*` | `svBitVecVal` (N ≤ 32) | `Bits<N>` |
//! | [`LogicVec<N>`] | `logic [N-1:0]` | `const svLogicVecVal*` | `svLogicVecVal*` | none | `Bits<N>` |
//! | [`Handle`] | `chandle` | `void*` | pointer | same | [`Chandle`] |
//!
//! Verilator models are 2-state: an incoming `svLogic` or `svLogicVecVal` never holds X or
//! Z, so only `aval` is read, and outgoing values have `bval = 0` (§H.10.1.2: `aval/bval`
//! `0/0` is `0`, `1/0` is `1`).
//!
//! The direction wrappers [`InArg`], [`OutArg`] and [`InOutArg`] implement [`Param`]: what
//! a trampoline loads from its C argument, passes to the trait method, and stores back.
//! An output buffer from Verilator is uninitialised (`svBitVecVal o__Vcvt[1];`), so an
//! output is always stored, starting from the type's zero value.
//!
//! This is the only module of `dpi` with `unsafe`: reading and writing the pointers C
//! passes. The safety contract of every `unsafe fn` here is the same: the pointer is the
//! one Verilator passed for an argument of exactly this SV type, so it is valid, aligned and
//! holds `ceil(N / 32)` elements for an `N`-bit vector.

use std::ffi::{c_uchar, c_void};
use std::marker::PhantomData;

use simjig_bits::{Bit as BitValue, Bits};

use super::Chandle;

/// `svLogicVecVal` (`svdpi.h`): one 32-bit chunk of a 4-state vector.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvLogicVecVal {
    /// Value bits (`0`/`1`, or with `bval` set, `Z`/`X`).
    pub aval: u32,
    /// Set for `Z` and `X`.
    pub bval: u32,
}

/// The C representation of one SV type class, and its Rust value type.
pub trait Abi {
    /// The value type in the generated trait.
    type Rust: Copy + Default;
    /// The C type of an input argument.
    type In: Copy;
    /// The C type of an output or inout argument.
    type Ptr: Copy;

    /// Converts an input argument.
    ///
    /// # Safety
    /// `c` is what Verilator passed for an input of this type (see the module docs).
    unsafe fn read_in(c: Self::In) -> Self::Rust;

    /// Reads the current value behind an inout argument.
    ///
    /// # Safety
    /// As [`read_in`](Self::read_in), for an inout argument.
    unsafe fn read_ptr(p: Self::Ptr) -> Self::Rust;

    /// Stores `value` through an output or inout argument.
    ///
    /// # Safety
    /// As [`read_in`](Self::read_in), for an output or inout argument.
    unsafe fn write_ptr(p: Self::Ptr, value: Self::Rust);
}

/// A type class that can be a function result.
pub trait Ret: Abi {
    /// The C result type.
    type C: Copy;
    /// Converts a result.
    fn to_ret(value: Self::Rust) -> Self::C;
}

macro_rules! integer {
    ($($(#[$doc:meta])* $marker:ident: $rust:ty;)*) => {$(
        $(#[$doc])*
        #[derive(Debug)]
        pub enum $marker {}

        impl Abi for $marker {
            type Rust = $rust;
            type In = $rust;
            type Ptr = *mut $rust;

            unsafe fn read_in(c: $rust) -> $rust {
                c
            }

            unsafe fn read_ptr(p: *mut $rust) -> $rust {
                // SAFETY: the module contract: `p` is Verilator's valid, aligned argument.
                unsafe { p.read() }
            }

            unsafe fn write_ptr(p: *mut $rust, value: $rust) {
                // SAFETY: the module contract: `p` is Verilator's valid, aligned argument.
                unsafe { p.write(value) }
            }
        }

        impl Ret for $marker {
            type C = $rust;
            fn to_ret(value: $rust) -> $rust {
                value
            }
        }
    )*};
}

// The C types have these exact widths on every target simjig supports (LP64 and LLP64:
// `short` 16, `int` 32, `long long` 64 bits). `byte` is C `char`, whose signedness varies
// by target; a one-byte argument is passed the same either way, and `i8` is SV's value.
integer! {
    /// `byte`: C `char`.
    I8: i8;
    /// `byte unsigned`: C `unsigned char`.
    U8: u8;
    /// `shortint`: C `short`.
    I16: i16;
    /// `shortint unsigned`: C `unsigned short`.
    U16: u16;
    /// `int`: C `int`.
    I32: i32;
    /// `int unsigned`: C `unsigned int`.
    U32: u32;
    /// `longint`: C `long long`.
    I64: i64;
    /// `longint unsigned`: C `unsigned long long`.
    U64: u64;
}

macro_rules! scalar {
    ($($(#[$doc:meta])* $marker:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Debug)]
        pub enum $marker {}

        impl Abi for $marker {
            type Rust = BitValue;
            type In = c_uchar;
            type Ptr = *mut c_uchar;

            unsafe fn read_in(c: c_uchar) -> BitValue {
                BitValue::new(c & 1 == 1)
            }

            unsafe fn read_ptr(p: *mut c_uchar) -> BitValue {
                // SAFETY: the module contract: `p` is Verilator's valid argument.
                unsafe { Self::read_in(p.read()) }
            }

            unsafe fn write_ptr(p: *mut c_uchar, value: BitValue) {
                // SAFETY: the module contract: `p` is Verilator's valid argument.
                unsafe { p.write(c_uchar::from(value.to_bool())) }
            }
        }

        impl Ret for $marker {
            type C = c_uchar;
            fn to_ret(value: BitValue) -> c_uchar {
                c_uchar::from(value.to_bool())
            }
        }
    )*};
}

scalar! {
    /// `bit`: C `svBit`.
    Bit;
    /// `logic`: C `svLogic` (2-state: `0` or `1`).
    Logic;
}

/// `chandle`: C `void*`.
#[derive(Debug)]
pub enum Handle {}

impl Abi for Handle {
    type Rust = Chandle;
    type In = *mut c_void;
    type Ptr = *mut *mut c_void;

    unsafe fn read_in(c: *mut c_void) -> Chandle {
        Chandle::from_ptr(c)
    }

    unsafe fn read_ptr(p: *mut *mut c_void) -> Chandle {
        // SAFETY: the module contract: `p` is Verilator's valid, aligned argument.
        Chandle::from_ptr(unsafe { p.read() })
    }

    unsafe fn write_ptr(p: *mut *mut c_void, value: Chandle) {
        // SAFETY: the module contract: `p` is Verilator's valid, aligned argument.
        unsafe { p.write(value.as_ptr()) }
    }
}

impl Ret for Handle {
    type C = *mut c_void;
    fn to_ret(value: Chandle) -> *mut c_void {
        value.as_ptr()
    }
}

/// The 32-bit words of an `N`-bit vector (`N` ≤ 64 here, so one or two).
const fn words(n: u32) -> usize {
    n.div_ceil(32) as usize
}

/// Word `i` of `value`.
fn word<const N: u32>(value: Bits<N>, i: usize) -> u32 {
    let shifted = value.to_u128() >> (32 * i);
    u32::try_from(shifted & 0xffff_ffff).unwrap_or(u32::MAX)
}

/// `bit [N-1:0]`, `N` from 1 to 64: C `svBitVecVal` words, least significant first.
#[derive(Debug)]
pub struct BitVec<const N: u32>(PhantomData<()>);

impl<const N: u32> Abi for BitVec<N> {
    type Rust = Bits<N>;
    type In = *const u32;
    type Ptr = *mut u32;

    unsafe fn read_in(c: *const u32) -> Bits<N> {
        let mut value = 0u128;
        for i in 0..words(N) {
            // SAFETY: the module contract: `c` holds `words(N)` words, so `c + i` is one.
            let at = unsafe { c.add(i) };
            // SAFETY: as above.
            let w = unsafe { at.read() };
            value |= u128::from(w) << (32 * i);
        }
        Bits::new_truncating(value)
    }

    unsafe fn read_ptr(p: *mut u32) -> Bits<N> {
        // SAFETY: as `read_in`.
        unsafe { Self::read_in(p.cast_const()) }
    }

    unsafe fn write_ptr(p: *mut u32, value: Bits<N>) {
        for i in 0..words(N) {
            // SAFETY: the module contract: `p` holds `words(N)` words, so `p + i` is one.
            let at = unsafe { p.add(i) };
            // SAFETY: as above.
            unsafe { at.write(word(value, i)) };
        }
    }
}

impl<const N: u32> Ret for BitVec<N> {
    type C = u32;
    /// Verilator returns at most 32 bits by value ("DPI function may not return a > 32
    /// bits wide type"), so this is the whole value.
    fn to_ret(value: Bits<N>) -> u32 {
        word(value, 0)
    }
}

/// `logic [N-1:0]`, `N` from 1 to 64: C `svLogicVecVal` chunks, least significant first.
#[derive(Debug)]
pub struct LogicVec<const N: u32>(PhantomData<()>);

impl<const N: u32> Abi for LogicVec<N> {
    type Rust = Bits<N>;
    type In = *const SvLogicVecVal;
    type Ptr = *mut SvLogicVecVal;

    unsafe fn read_in(c: *const SvLogicVecVal) -> Bits<N> {
        let mut value = 0u128;
        for i in 0..words(N) {
            // SAFETY: the module contract: `c` holds `words(N)` chunks, so `c + i` is one.
            let at = unsafe { c.add(i) };
            // SAFETY: as above.
            let chunk = unsafe { at.read() };
            value |= u128::from(chunk.aval) << (32 * i);
        }
        Bits::new_truncating(value)
    }

    unsafe fn read_ptr(p: *mut SvLogicVecVal) -> Bits<N> {
        // SAFETY: as `read_in`.
        unsafe { Self::read_in(p.cast_const()) }
    }

    unsafe fn write_ptr(p: *mut SvLogicVecVal, value: Bits<N>) {
        for i in 0..words(N) {
            let chunk = SvLogicVecVal {
                aval: word(value, i),
                bval: 0,
            };
            // SAFETY: the module contract: `p` holds `words(N)` chunks, so `p + i` is one.
            let at = unsafe { p.add(i) };
            // SAFETY: as above.
            unsafe { at.write(chunk) };
        }
    }
}

/// One argument of a trampoline: loaded from C, passed to the trait method, stored back.
pub trait Param: Sized {
    /// The C parameter type.
    type C: Copy;
    /// What the trait method receives.
    type Pass<'a>
    where
        Self: 'a;

    /// Loads the argument.
    ///
    /// # Safety
    /// `c` is what Verilator passed for this argument (see the module docs).
    unsafe fn load(c: Self::C) -> Self;

    /// The value or reference for the trait method.
    fn pass(&mut self) -> Self::Pass<'_>;

    /// Writes an output back.
    ///
    /// # Safety
    /// As [`load`](Self::load), with the pointer still valid (the trampoline has not
    /// returned).
    unsafe fn store(self);
}

/// An `input` argument: passed by value.
#[derive(Debug)]
pub struct InArg<A: Abi>(A::Rust);

impl<A: Abi> Param for InArg<A> {
    type C = A::In;
    type Pass<'a>
        = A::Rust
    where
        Self: 'a;

    unsafe fn load(c: A::In) -> Self {
        // SAFETY: forwarded from the caller.
        Self(unsafe { A::read_in(c) })
    }

    fn pass(&mut self) -> A::Rust {
        self.0
    }

    unsafe fn store(self) {}
}

/// An `output` argument: the method writes `&mut`, starting from zero.
#[derive(Debug)]
pub struct OutArg<A: Abi> {
    ptr: A::Ptr,
    value: A::Rust,
}

impl<A: Abi> Param for OutArg<A> {
    type C = A::Ptr;
    type Pass<'a>
        = &'a mut A::Rust
    where
        Self: 'a;

    unsafe fn load(ptr: A::Ptr) -> Self {
        Self {
            ptr,
            value: A::Rust::default(),
        }
    }

    fn pass(&mut self) -> &mut A::Rust {
        &mut self.value
    }

    unsafe fn store(self) {
        // SAFETY: forwarded from the caller.
        unsafe { A::write_ptr(self.ptr, self.value) }
    }
}

/// An `inout` argument: the method reads and writes `&mut`, starting from the design's
/// value.
#[derive(Debug)]
pub struct InOutArg<A: Abi> {
    ptr: A::Ptr,
    value: A::Rust,
}

impl<A: Abi> Param for InOutArg<A> {
    type C = A::Ptr;
    type Pass<'a>
        = &'a mut A::Rust
    where
        Self: 'a;

    unsafe fn load(ptr: A::Ptr) -> Self {
        Self {
            ptr,
            // SAFETY: forwarded from the caller.
            value: unsafe { A::read_ptr(ptr) },
        }
    }

    fn pass(&mut self) -> &mut A::Rust {
        &mut self.value
    }

    unsafe fn store(self) {
        // SAFETY: forwarded from the caller.
        unsafe { A::write_ptr(self.ptr, self.value) }
    }
}

/// Defines the `extern "C"` trampoline of one DPI-C import; for generated code only (see
/// the `dpi` module docs).
///
/// ```text
/// ::simjig::__private::dpi_import! {
///     prefix = "Vdpi", imp = dyn Dpi,
///     fn dpi_add => dpi_add(x: InArg ::simjig::__private::dpi::I32,
///                           y: InArg ::simjig::__private::dpi::I32)
///         -> ::simjig::__private::dpi::I32
/// }
/// ```
///
/// The first name is the C symbol, the second the trait method; each argument names its
/// direction wrapper and its [`Abi`] marker; without `->` the C function returns `int` 0
/// (a task, or a `void` function: Verilator ignores the value).
///
/// The `#[no_mangle]` and `unsafe` blocks are written here, not in the generated file, so
/// they do not trip `unsafe_code` in the user's crate.
#[doc(hidden)]
#[macro_export]
macro_rules! __dpi_import {
    (
        prefix = $prefix:literal, imp = $imp:ty,
        fn $c:ident => $method:ident ( $( $arg:ident : $dir:ident $abi:ty ),* $(,)? )
        -> $ret:ty
    ) => {
        #[unsafe(no_mangle)]
        extern "C" fn $c(
            $( $arg: <$crate::__private::dpi::$dir<$abi> as $crate::__private::dpi::Param>::C ),*
        ) -> <$ret as $crate::__private::dpi::Ret>::C {
            $(
                // SAFETY: Verilator passes `$arg` as the import's prototype declares it.
                let mut $arg = unsafe {
                    <$crate::__private::dpi::$dir<$abi> as $crate::__private::dpi::Param>::load($arg)
                };
            )*
            let result = $crate::__private::dpi::call::<$imp, _>(
                $prefix,
                ::core::stringify!($c),
                ::core::default::Default::default(),
                |imp| imp.$method( $( $crate::__private::dpi::Param::pass(&mut $arg) ),* ),
            );
            // SAFETY: the pointers are still the ones Verilator passed.
            $( unsafe { $crate::__private::dpi::Param::store($arg) }; )*
            <$ret as $crate::__private::dpi::Ret>::to_ret(result)
        }
    };
    (
        prefix = $prefix:literal, imp = $imp:ty,
        fn $c:ident => $method:ident ( $( $arg:ident : $dir:ident $abi:ty ),* $(,)? )
    ) => {
        #[unsafe(no_mangle)]
        extern "C" fn $c(
            $( $arg: <$crate::__private::dpi::$dir<$abi> as $crate::__private::dpi::Param>::C ),*
        ) -> ::core::ffi::c_int {
            $(
                // SAFETY: Verilator passes `$arg` as the import's prototype declares it.
                let mut $arg = unsafe {
                    <$crate::__private::dpi::$dir<$abi> as $crate::__private::dpi::Param>::load($arg)
                };
            )*
            $crate::__private::dpi::call::<$imp, _>(
                $prefix,
                ::core::stringify!($c),
                (),
                |imp| imp.$method( $( $crate::__private::dpi::Param::pass(&mut $arg) ),* ),
            );
            // SAFETY: the pointers are still the ones Verilator passed.
            $( unsafe { $crate::__private::dpi::Param::store($arg) }; )*
            0
        }
    };
}

#[cfg(test)]
mod tests {
    use simjig_bits::{Bit as BitValue, Bits};

    use super::SvLogicVecVal;
    use crate::dpi::tests::bound;

    /// Every argument shape a trampoline has: values, vectors of one and two words, 4-state
    /// vectors, scalars, `output` and `inout` arguments, a result and none.
    trait Shapes {
        #[expect(clippy::too_many_arguments, reason = "one argument per shape")]
        fn mix(
            &mut self,
            a: i32,
            v41: Bits<41>,
            l64: Bits<64>,
            s: BitValue,
            o8: &mut Bits<8>,
            io41: &mut Bits<41>,
            ol: &mut Bits<33>,
        ) -> Bits<16>;
        fn task(&mut self, x: i8, n: &mut u64);
    }

    struct Impl;

    impl Shapes for Impl {
        fn mix(
            &mut self,
            a: i32,
            v41: Bits<41>,
            l64: Bits<64>,
            s: BitValue,
            o8: &mut Bits<8>,
            io41: &mut Bits<41>,
            ol: &mut Bits<33>,
        ) -> Bits<16> {
            assert_eq!(a, -5);
            assert_eq!(v41, 0x1_2345_6789_u128);
            assert_eq!(l64, 0xdead_beef_0123_4567_u128);
            assert_eq!(s, true);
            assert_eq!(*o8, 0, "outputs start at zero");
            *o8 = Bits::new(0xab);
            *io41 = io41.wrapping_add(Bits::new(1));
            *ol = Bits::new(0x1_0000_0001);
            Bits::new(0xbeef)
        }

        fn task(&mut self, x: i8, n: &mut u64) {
            *n += u64::from(x.unsigned_abs());
        }
    }

    crate::__dpi_import! {
        prefix = "Vabi", imp = dyn Shapes,
        fn simjig_abi_test_mix => mix(
            a: InArg crate::__private::dpi::I32,
            v41: InArg crate::__private::dpi::BitVec<41>,
            l64: InArg crate::__private::dpi::LogicVec<64>,
            s: InArg crate::__private::dpi::Bit,
            o8: OutArg crate::__private::dpi::BitVec<8>,
            io41: InOutArg crate::__private::dpi::BitVec<41>,
            ol: OutArg crate::__private::dpi::LogicVec<33>,
        ) -> crate::__private::dpi::BitVec<16>
    }

    crate::__dpi_import! {
        prefix = "Vabi", imp = dyn Shapes,
        fn simjig_abi_test_task => task(
            x: InArg crate::__private::dpi::I8,
            n: InOutArg crate::__private::dpi::U64,
        )
    }

    #[test]
    fn trampolines_convert_every_argument_shape() {
        let imp: Box<dyn Shapes> = Box::new(Impl);
        let mut model = bound("Vabi", Some(imp));
        let v41 = [0x2345_6789_u32, 0x1];
        let l64 = [
            SvLogicVecVal {
                aval: 0x0123_4567,
                bval: 0,
            },
            SvLogicVecVal {
                aval: 0xdead_beef,
                bval: 0,
            },
        ];
        // Verilator's output buffers are uninitialised; garbage must be overwritten.
        let mut o8 = [0xffff_ffff_u32];
        let mut io41 = [0xffff_ffff_u32, 0x1ff];
        let mut ol = [SvLogicVecVal { aval: 7, bval: 7 }; 2];
        let (ret, failure) = model.scope(|| {
            simjig_abi_test_mix(
                -5,
                v41.as_ptr(),
                l64.as_ptr(),
                1,
                o8.as_mut_ptr(),
                io41.as_mut_ptr(),
                ol.as_mut_ptr(),
            )
        });
        assert_eq!(failure, None);
        assert_eq!(ret, 0xbeef);
        assert_eq!(o8, [0xab]);
        assert_eq!(io41, [0, 0], "41-bit wrap of all ones plus one");
        assert_eq!(
            ol,
            [
                SvLogicVecVal { aval: 1, bval: 0 },
                SvLogicVecVal { aval: 1, bval: 0 }
            ]
        );
        let mut n = 10_u64;
        let (status, failure) = model.scope(|| simjig_abi_test_task(-3, &raw mut n));
        assert_eq!((status, failure, n), (0, None, 13));
    }

    #[test]
    fn a_failed_call_still_writes_its_outputs() {
        let mut model = bound::<dyn Shapes>("Vabi", None);
        let mut o8 = [0xffff_ffff_u32];
        let mut io41 = [5_u32, 0];
        let mut ol = [SvLogicVecVal { aval: 9, bval: 9 }; 2];
        let (ret, failure) = model.scope(|| {
            simjig_abi_test_mix(
                0,
                [0, 0].as_ptr(),
                [SvLogicVecVal::default(); 2].as_ptr(),
                0,
                o8.as_mut_ptr(),
                io41.as_mut_ptr(),
                ol.as_mut_ptr(),
            )
        });
        assert!(failure.is_some());
        assert_eq!(ret, 0);
        assert_eq!(o8, [0], "zero, not the uninitialised buffer");
        assert_eq!(io41, [5, 0], "an inout keeps the design's value");
        assert_eq!(ol, [SvLogicVecVal::default(); 2]);
    }
}

//! A stand-in for `simjig::__private`: exactly the items generated code names, so the
//! generated Rust compiles and its behaviour can be tested without the runtime.
//!
//! The real runtime must provide the same paths and signatures (`docs/notes/codegen.md`,
//! "The `__private` contract"). Handles here only remember their [`SignalMeta`].

use std::fmt;
use std::marker::PhantomData;

pub use simjig_bits as bits;
use simjig_bits::Packed;

/// Verilator storage of a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    CData,
    SData,
    IData,
    QData,
    VlWide { words: u32 },
    VlUnpacked { elem: &'static Storage, len: u32 },
}

/// How the testbench may access a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    In,
    Out,
    InOut,
    PublicRd,
    PublicRw,
}

/// One entry of `Dut::SIGNALS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalMeta {
    pub name: &'static str,
    pub slot: u32,
    pub width: u32,
    pub storage: Storage,
    pub access: Access,
}

macro_rules! handles {
    ($($name:ident)*) => {$(
        /// A signal handle; the stub only keeps its metadata.
        #[derive(Debug)]
        pub struct $name<T> {
            pub meta: &'static SignalMeta,
            _value: PhantomData<T>,
        }

        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T> Copy for $name<T> {}

        impl<T> $name<T> {
            fn new(meta: &'static SignalMeta) -> Self {
                Self { meta, _value: PhantomData }
            }
        }
    )*};
}
handles!(In Out InOut PublicRd PublicRw);

/// Builds handles while `Dut::__bind` runs.
pub trait Binder {
    fn input<T: 'static>(&mut self, meta: &'static SignalMeta) -> In<T>;
    fn output<T: 'static>(&mut self, meta: &'static SignalMeta) -> Out<T>;
    fn inout<T: 'static>(&mut self, meta: &'static SignalMeta) -> InOut<T>;
    fn public_rd<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRd<T>;
    fn public_rw<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRw<T>;
}

/// A binder that records the slot and access of every call, in call order.
#[derive(Debug, Default)]
pub struct RecordingBinder {
    pub calls: Vec<(u32, Access)>,
}

impl Binder for RecordingBinder {
    fn input<T: 'static>(&mut self, meta: &'static SignalMeta) -> In<T> {
        self.calls.push((meta.slot, Access::In));
        In::new(meta)
    }
    fn output<T: 'static>(&mut self, meta: &'static SignalMeta) -> Out<T> {
        self.calls.push((meta.slot, Access::Out));
        Out::new(meta)
    }
    fn inout<T: 'static>(&mut self, meta: &'static SignalMeta) -> InOut<T> {
        self.calls.push((meta.slot, Access::InOut));
        InOut::new(meta)
    }
    fn public_rd<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRd<T> {
        self.calls.push((meta.slot, Access::PublicRd));
        PublicRd::new(meta)
    }
    fn public_rw<T: 'static>(&mut self, meta: &'static SignalMeta) -> PublicRw<T> {
        self.calls.push((meta.slot, Access::PublicRw));
        PublicRw::new(meta)
    }
}

/// An enum value that names no variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEnum {
    pub type_name: &'static str,
    pub width: u32,
    pub value: u128,
}

impl InvalidEnum {
    pub fn new(type_name: &'static str, width: u32, value: u128) -> Self {
        Self {
            type_name,
            width,
            value,
        }
    }
}

/// One differing field found by a generated `diff`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDiff {
    pub path: String,
    pub left: String,
    pub right: String,
}

impl FieldDiff {
    pub fn new(path: &str, left: &dyn fmt::Debug, right: &dyn fmt::Debug) -> Self {
        Self {
            path: path.to_owned(),
            left: format!("{left:?}"),
            right: format!("{right:?}"),
        }
    }

    #[must_use]
    pub fn within(self, parent: &str) -> Self {
        Self {
            path: format!("{parent}.{}", self.path),
            ..self
        }
    }
}

/// Verilator storage as little-endian 32-bit words.
pub trait RawWords: Copy {
    /// Writes the value into `out[..n]`, `n` = the storage's word count.
    fn to_words(self, out: &mut [u32]);
    /// Reads the value from the first words of `words`, ignoring bits beyond the storage.
    fn from_words(words: &[u32]) -> Self;
}

macro_rules! narrow_raw {
    ($($t:ty)*) => {$(
        impl RawWords for $t {
            fn to_words(self, out: &mut [u32]) {
                out[0] = u32::from(self);
            }
            fn from_words(words: &[u32]) -> Self {
                <$t>::try_from(words[0] & u32::from(<$t>::MAX)).unwrap()
            }
        }
    )*};
}
narrow_raw!(u8 u16 u32);

impl RawWords for u64 {
    fn to_words(self, out: &mut [u32]) {
        out[0] = u32::try_from(self & 0xffff_ffff).unwrap();
        out[1] = u32::try_from(self >> 32).unwrap();
    }
    fn from_words(words: &[u32]) -> Self {
        u64::from(words[0]) | (u64::from(words[1]) << 32)
    }
}

impl<const N: usize> RawWords for [u32; N] {
    fn to_words(self, out: &mut [u32]) {
        out[..N].copy_from_slice(&self);
    }
    fn from_words(words: &[u32]) -> Self {
        std::array::from_fn(|i| words[i])
    }
}

/// A value a handle reads and writes (the runtime's `Value`, with its provided methods
/// for values of up to 256 bits).
pub trait Value: Packed<Raw: RawWords> + Eq + fmt::Debug + Default {
    /// The value whose packed bits are `bits`.
    fn from_u128(bits: u128) -> Self {
        let mut words = [0u32; 8];
        for (i, w) in words.iter_mut().take(4).enumerate() {
            *w = u32::try_from(bits >> (32 * i) & 0xffff_ffff).unwrap();
        }
        Self::from_raw(RawWords::from_words(&words))
    }

    /// The value's packed bits.
    fn to_u128(self) -> u128 {
        let mut words = [0u32; 8];
        self.to_raw().to_words(&mut words);
        words[..4]
            .iter()
            .rev()
            .fold(0, |acc, &w| acc << 32 | u128::from(w))
    }
}

impl<T: Packed<Raw: RawWords> + Eq + fmt::Debug + Default> Value for T {}

/// A bit vector (the runtime's `Vector`).
pub trait Vector: Value + From<u128> + PartialEq<u128> {}

impl<T: Value + From<u128> + PartialEq<u128>> Vector for T {}

fn get(words: &[u32], bit: u32) -> bool {
    words[(bit / 32) as usize] >> (bit % 32) & 1 == 1
}

fn put(words: &mut [u32], bit: u32, value: bool) {
    let mask = 1 << (bit % 32);
    let word = &mut words[(bit / 32) as usize];
    *word = if value { *word | mask } else { *word & !mask };
}

/// Writes `value`'s bits into `dst` at `lsb ..= lsb + T::WIDTH - 1`.
pub fn pack_field<T: Packed>(dst: &mut [u32], lsb: u32, value: T)
where
    T::Raw: RawWords,
{
    let mut src = vec![0u32; T::WIDTH.div_ceil(32) as usize];
    value.to_raw().to_words(&mut src);
    for bit in 0..T::WIDTH {
        put(dst, lsb + bit, get(&src, bit));
    }
}

/// Reads a `T` from `src` at `lsb ..= lsb + T::WIDTH - 1`.
pub fn unpack_field<T: Packed>(src: &[u32], lsb: u32) -> T
where
    T::Raw: RawWords,
{
    let mut dst = vec![0u32; T::WIDTH.div_ceil(32) as usize];
    for bit in 0..T::WIDTH {
        put(&mut dst, bit, get(src, lsb + bit));
    }
    T::from_raw(T::Raw::from_words(&dst))
}

/// The model glue. The real macro implements `DutModel` over the model's vtable; the stub
/// only checks the shape of its arguments and that `Dut` binds.
macro_rules! dut_model {
    (
        $dut:ident,
        prefix = $prefix:literal,
        symbol = $symbol:literal,
        precision_exponent = $exp:literal $(,)?
    ) => {
        const _: (&str, &str, i8) = ($prefix, $symbol, $exp);
        const _: fn(&mut $crate::__private::RecordingBinder) -> $dut = $dut::__bind;
    };
}
pub(crate) use dut_model;

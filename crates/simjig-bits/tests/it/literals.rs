//! Integer-literal ergonomics: a setter taking `impl Into<T>` accepts plain literals, and
//! an out-of-range literal panics at the caller's line naming the width.
//!
//! This file compiling is itself the compile test: every `set(<literal>)` below must infer.

use std::cell::RefCell;
use std::panic::{self, Location};
use std::sync::Once;

use simjig_bits::{Bit, Bits, OutOfRange, WideBits};

/// A stand-in for a signal handle: `set` has the signature signal handles use.
struct Signal<T>(Option<T>);

impl<T> Signal<T> {
    #[track_caller]
    fn set(&mut self, value: impl Into<T>) {
        self.0 = Some(value.into());
    }
}

#[test]
fn literals_infer_at_every_width() {
    let mut b1 = Signal::<Bit>(None);
    b1.set(1);
    assert_eq!(b1.0, Some(Bit::ONE));
    b1.set(false);
    assert_eq!(b1.0, Some(Bit::ZERO));

    let mut b8 = Signal::<Bits<8>>(None);
    b8.set(0x5a);
    assert_eq!(b8.0.unwrap(), 0x5a);

    let mut b32 = Signal::<Bits<32>>(None);
    b32.set(0xdead_beef); // above i32::MAX: no i32 fallback
    assert_eq!(b32.0.unwrap(), 0xdead_beef);

    let mut b128 = Signal::<Bits<128>>(None);
    b128.set(0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff);
    assert_eq!(b128.0.unwrap(), Bits::<128>::MAX);

    let mut wide = Signal::<WideBits<200, 7>>(None);
    wide.set(7);
    assert_eq!(wide.0.unwrap(), 7);

    // A value of the type itself and a Bit into Bits<1> also work.
    b8.set(Bits::<8>::MAX);
    let mut one = Signal::<Bits<1>>(None);
    one.set(Bit::ONE);
    assert_eq!(one.0.unwrap(), 1);
}

thread_local! {
    static LAST_PANIC: RefCell<Option<(String, u32, String)>> = const { RefCell::new(None) };
}

/// Runs `f`, which must panic, and returns the panic's file, line and message. The hook
/// records into a thread-local, so tests running in parallel do not see each other.
fn panic_site(f: impl FnOnce() + panic::UnwindSafe) -> (String, u32, String) {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let site = info.location().map(|l| (l.file().to_owned(), l.line()));
            let message = info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            if let Some((file, line)) = site {
                LAST_PANIC.with(|p| *p.borrow_mut() = Some((file, line, message)));
            }
            previous(info);
        }));
    });
    assert!(panic::catch_unwind(f).is_err(), "expected a panic");
    LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap()
}

#[test]
fn out_of_range_literal_panics_at_the_callers_line() {
    let here = Location::caller().file().to_owned();
    let (file, line, message) = panic_site(|| {
        let mut sig = Signal::<Bits<8>>(None);
        sig.set(0x15a); // the line the panic must point at
    });
    let expected_line = line!() - 2;
    assert_eq!((file.as_str(), line), (here.as_str(), expected_line));
    assert_eq!(
        message,
        "Bits<8>: value 0x15a does not fit in 8 bits (maximum 0xff)"
    );
}

#[test]
fn panicking_constructors_report_the_callers_line() {
    let here = file!();
    let (file, line, message) = panic_site(|| _ = Bits::<4>::new(16));
    assert_eq!((file.as_str(), line), (here, line!() - 1));
    assert_eq!(
        message,
        "Bits<4>: value 0x10 does not fit in 4 bits (maximum 0xf)"
    );

    let (file, line, message) = panic_site(|| _ = Bit::from(2));
    assert_eq!((file.as_str(), line), (here, line!() - 1));
    assert_eq!(
        message,
        "Bit: value 0x2 does not fit in 1 bits (maximum 0x1)"
    );

    let (file, line, _) = panic_site(|| _ = Bits::<8>::ZERO.bit(8));
    assert_eq!((file.as_str(), line), (here, line!() - 1));
}

#[test]
fn try_from_rejects_values_that_do_not_fit() {
    assert_eq!(Bits::<8>::try_from(255u16), Ok(Bits::<8>::MAX));
    assert_eq!(Bits::<8>::try_from(256u16).unwrap_err().width(), 8);
    assert_eq!(Bits::<64>::try_from(u64::MAX), Ok(Bits::<64>::MAX));
    assert_eq!(Bits::<8>::try_new(256).map_err(|e| e.width()), Err(8));
    assert_eq!(
        Bits::<8>::try_from(-1i32).unwrap_err().to_string(),
        "value -1 does not fit in 8 bits (the type is unsigned)"
    );
    assert_eq!(WideBits::<200, 7>::try_from(5u8), Ok(WideBits::from(5)));
    assert!(WideBits::<200, 7>::try_from(-5i64).is_err());
    assert_eq!(Bit::try_from(1u8), Ok(Bit::ONE));
    assert!(Bit::try_from(2u8).is_err());
    assert!(Bit::try_from(-1i8).is_err());
    let err: OutOfRange = Bits::<128>::try_from(-1i128).unwrap_err();
    assert_eq!(err.width(), 128);
}

#[test]
fn bit_compares_with_bool_and_integers() {
    assert!(Bit::ONE == true && Bit::ONE == 1 && Bit::ZERO == 0 && Bit::ZERO != 1);
    assert_eq!(Bit::ONE & Bit::ZERO, Bit::ZERO);
    assert_eq!(Bit::ONE | Bit::ZERO, Bit::ONE);
    assert_eq!(Bit::ONE ^ Bit::ONE, Bit::ZERO);
    assert_eq!(!Bit::ZERO, Bit::ONE);
    assert!(bool::from(Bit::ONE));
    assert_eq!(Bit::from(Bits::<1>::new(1)), Bit::ONE);
    assert_eq!(
        format!("{} {:?} {:x} {:b}", Bit::ONE, Bit::ZERO, Bit::ONE, Bit::ONE),
        "1'b1 1'b0 1 1"
    );
}

#[test]
fn bit_raw_round_trip_ignores_upper_bits() {
    use simjig_bits::Packed;
    for raw in 0..=u8::MAX {
        let bit = Bit::from_raw(raw);
        assert_eq!(bit, raw & 1 == 1);
        assert_eq!(bit.to_raw(), raw & 1);
    }
}

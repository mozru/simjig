//! Every DPI type simjig implements crosses the C ABI both ways: the implementation
//! records what it received, and its outputs and results reach the RTL's ports.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use simjig::{Chandle, ReadOnly, Timer};
use simjig_bits::{Bit, Bits};

use super::{TICK, cycles, dpi_types, sim};

/// The eight integer arguments of `ty_ints`.
type Ints = (i8, i16, i32, i64, u8, u16, u32, u64);

/// What the implementation received, by import.
#[derive(Debug, Default)]
struct Seen {
    ints: Option<Ints>,
    bits: Option<(Bit, Bit, Bit)>,
    vec: Option<(Bits<8>, Bits<41>, Bits<64>, Bits<64>)>,
    voids: u32,
    handles: Vec<Chandle>,
    tasks: Vec<i32>,
}

struct Types {
    seen: Rc<RefCell<Seen>>,
    handle: Chandle,
}

impl dpi_types::Dpi for Types {
    fn ty_bits(&mut self, b: Bit, l: Bit, ob: &mut Bit, ol: &mut Bit, iob: &mut Bit) -> Bit {
        self.seen.borrow_mut().bits = Some((b, l, *iob));
        *ob = !b;
        *ol = l;
        *iob = !*iob;
        Bit::new(b.to_bool() ^ l.to_bool())
    }

    fn ty_byte(&mut self, x: i8) -> i8 {
        x.wrapping_neg()
    }

    fn ty_aliased(&mut self, x: i32) -> i32 {
        x ^ 0x5555_5555
    }

    fn ty_handle(&mut self, c: Chandle) -> Chandle {
        self.seen.borrow_mut().handles.push(c);
        self.handle
    }

    fn ty_ints(
        &mut self,
        b: i8,
        s: i16,
        i: i32,
        l: i64,
        ub: u8,
        us: u16,
        ui: u32,
        ul: u64,
        sum: &mut i64,
    ) -> i32 {
        self.seen.borrow_mut().ints = Some((b, s, i, l, ub, us, ui, ul));
        *sum = l.wrapping_add(1);
        i.wrapping_neg()
    }

    fn ty_logic(&mut self, x: Bit) -> Bit {
        !x
    }

    fn ty_short(&mut self, x: i16) -> i16 {
        x.wrapping_add(1)
    }

    fn ty_task(&mut self, x: i32) {
        self.seen.borrow_mut().tasks.push(x);
    }

    fn ty_ulong(&mut self, x: u64) -> u64 {
        x.rotate_left(8)
    }

    fn ty_vec(
        &mut self,
        v8: Bits<8>,
        l41: Bits<41>,
        s64: Bits<64>,
        o41: &mut Bits<41>,
        io64: &mut Bits<64>,
        ol8: &mut Bits<8>,
    ) -> Bits<16> {
        self.seen.borrow_mut().vec = Some((v8, l41, s64, *io64));
        *o41 = Bits::new_truncating(l41.to_u128() | 1 << 40);
        *io64 = !*io64;
        *ol8 = !v8;
        Bits::new_truncating(s64.to_u128() >> 48)
    }

    fn ty_void(&mut self, o: &mut i32) {
        *o = 0x1234_5678;
        self.seen.borrow_mut().voids += 1;
    }
}

const A: u32 = 0x8182_f3f5;
const B: u64 = 0xfedc_ba98_7654_3210;

/// The output ports after one settled run.
#[derive(Debug)]
struct Ports {
    ints_ret: Bits<32>,
    ints_sum: Bits<64>,
    bits_out: Bits<4>,
    vec_ret: Bits<16>,
    vec_o41: Bits<41>,
    vec_io64: Bits<64>,
    vec_ol8: Bits<8>,
    r_byte: Bits<8>,
    r_short: Bits<16>,
    r_ulong: Bits<64>,
    r_logic: Bit,
    void_out: Bits<32>,
    aliased: Bits<32>,
    handle_ok: Bit,
}

/// Drives `a = A`, `b = B` and two clock cycles; returns the ports, what the
/// implementation saw, and the chandle it handed out.
fn run() -> (Ports, Seen, Chandle) {
    let (mut sim, dut) = sim::<dpi_types::Dut>();
    let seen = Rc::new(RefCell::new(Seen::default()));
    // Any stable non-null address will do: the design only stores and compares it.
    let token = Box::leak(Box::new(0u64));
    let handle = Chandle::from_ptr(std::ptr::from_mut(token).cast::<c_void>());
    dut.set_dpi(Types {
        seen: Rc::clone(&seen),
        handle,
    });
    let ports = sim
        .run(async move {
            dut.a.set(u128::from(A));
            dut.b.set(u128::from(B));
            Timer::new(TICK).await;
            cycles(dut.clk, 2).await;
            ReadOnly::new().await;
            Ports {
                ints_ret: dut.ints_ret.get(),
                ints_sum: dut.ints_sum.get(),
                bits_out: dut.bits_out.get(),
                vec_ret: dut.vec_ret.get(),
                vec_o41: dut.vec_o41.get(),
                vec_io64: dut.vec_io64.get(),
                vec_ol8: dut.vec_ol8.get(),
                r_byte: dut.r_byte.get(),
                r_short: dut.r_short.get(),
                r_ulong: dut.r_ulong.get(),
                r_logic: dut.r_logic.get(),
                void_out: dut.void_out.get(),
                aliased: dut.aliased.get(),
                handle_ok: dut.handle_ok.get(),
            }
        })
        .unwrap();
    drop(sim);
    let seen = Rc::try_unwrap(seen).unwrap().into_inner();
    (ports, seen, handle)
}

#[test]
#[expect(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "the expected values are SV's own reinterpretations of `a` and `b`"
)]
fn integers_carry_svs_reinterpretation_of_the_bits() {
    let (ports, seen, _) = run();
    let i = A as i32;
    let expected = (
        A as u8 as i8,
        A as u16 as i16,
        i,
        B as i64,
        A as u8,
        A as u16,
        A,
        B,
    );
    assert_eq!(seen.ints, Some(expected));
    assert_eq!(ports.ints_ret, u128::from(i.wrapping_neg().cast_unsigned()));
    assert_eq!(
        ports.ints_sum,
        u128::from(B.wrapping_add(1)),
        "longint output"
    );
    assert_eq!(ports.r_byte, 0x0b, "byte result: -(-11)");
    assert_eq!(ports.r_short, 0xf3f6, "shortint result");
    assert_eq!(
        ports.r_ulong,
        u128::from(B.rotate_left(8)),
        "longint unsigned result"
    );
    assert_eq!(
        ports.aliased,
        u128::from(A ^ 0x5555_5555),
        "through the C alias"
    );
}

#[test]
fn scalars_cross_in_out_and_inout() {
    let (ports, seen, _) = run();
    // a[0] = 1, a[1] = 0, a[2] = 1.
    assert_eq!(seen.bits, Some((Bit::ONE, Bit::ZERO, Bit::ONE)));
    assert_eq!(
        ports.bits_out, 0b1000,
        "{{ret = b ^ l, ob = !b, ol = l, iob = !iob}}"
    );
    assert_eq!(ports.r_logic, Bit::ZERO, "logic result: !a[0]");
}

#[test]
fn vectors_of_one_and_two_words_cross_in_out_and_inout() {
    let (ports, seen, _) = run();
    let a41 = Bits::<41>::new(u128::from(A));
    let b64 = Bits::<64>::new(u128::from(B));
    assert_eq!(seen.vec, Some((Bits::new(0xf5), a41, b64, b64)));
    assert_eq!(ports.vec_ret, 0xfedc, "bit [15:0] result");
    assert_eq!(ports.vec_o41, u128::from(A) | 1 << 40, "bit [40:0] output");
    assert_eq!(ports.vec_io64, u128::from(!B), "logic [63:0] inout");
    assert_eq!(ports.vec_ol8, 0x0a, "logic [7:0] output");
}

#[test]
fn void_functions_tasks_and_chandles_work() {
    let (ports, seen, handle) = run();
    assert_eq!(ports.void_out, 0x1234_5678);
    assert!(seen.voids >= 1, "the comb block ran ty_void");
    assert_eq!(ports.handle_ok, Bit::ONE);
    assert_eq!(
        seen.handles,
        [Chandle::NULL, handle],
        "null first, then ours back"
    );
    assert_eq!(
        seen.tasks,
        [A.cast_signed(); 2],
        "one task call per posedge"
    );
}

//! (f) DPI-C round trips through the public API: the design calls Rust (`dpi_types`, a
//! `set_dpi` implementation whose outputs reach the ports, and which sees the design's
//! arguments), and the same `dpi` design with its imports left to C (`dpi_c`,
//! `Dut::dpi_in_c` plus `tests/duts/dpi/dpi_impl.c`), which links beside Rust DPI models.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::bits::{Bit, Bits};
use simjig::{Chandle, clock, ns, read_only, timer};

use crate::{dpi_c, dpi_types};

/// Inverts or offsets what it gets; records the task's argument.
struct Echo {
    tasks: Rc<RefCell<Vec<i32>>>,
}

impl dpi_types::Dpi for Echo {
    fn ty_bits(&mut self, b: Bit, l: Bit, ob: &mut Bit, ol: &mut Bit, iob: &mut Bit) -> Bit {
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
    fn ty_handle(&mut self, _c: Chandle) -> Chandle {
        Chandle::from_ptr(std::ptr::NonNull::<u8>::dangling().as_ptr().cast())
    }
    fn ty_ints(
        &mut self,
        _b: i8,
        _s: i16,
        i: i32,
        l: i64,
        _ub: u8,
        _us: u16,
        _ui: u32,
        _ul: u64,
        sum: &mut i64,
    ) -> i32 {
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
        self.tasks.borrow_mut().push(x);
    }
    fn ty_ulong(&mut self, x: u64) -> u64 {
        x.rotate_left(8)
    }
    fn ty_vec(
        &mut self,
        v8: Bits<8>,
        _l41: Bits<41>,
        _s64: Bits<64>,
        o41: &mut Bits<41>,
        io64: &mut Bits<64>,
        ol8: &mut Bits<8>,
    ) -> Bits<16> {
        *o41 = Bits::new(0x1_2345_6789);
        *io64 = Bits::new(io64.to_u128() ^ 0xffff);
        *ol8 = Bits::new(v8.to_u128() ^ 0xff);
        Bits::new(v8.to_u128() << 8)
    }
    fn ty_void(&mut self, o: &mut i32) {
        *o = 77;
    }
}

#[simjig::test(timeout = "1us")]
async fn the_design_calls_rust_and_gets_its_results(dut: dpi_types::Dut) {
    let tasks = Rc::new(RefCell::new(Vec::new()));
    dut.set_dpi(Echo {
        tasks: Rc::clone(&tasks),
    });
    dut.a.set(0x0102_0381);
    dut.b.set(0x10);
    timer(ns(1)).await;
    read_only().await;
    assert_eq!(
        dut.ints_ret.get(),
        Bits::new(u128::from(0x0102_0381u32.wrapping_neg()))
    );
    assert_eq!(dut.ints_sum.get(), Bits::new(0x11));
    // a[0] = 1, a[1] = 0, a[2] = 0: ob = 0, ol = 0, iob flipped to 1, result 1.
    assert_eq!(dut.bits_out.get(), Bits::new(0b1001));
    assert_eq!(dut.vec_ret.get(), Bits::new(0x8100));
    assert_eq!(dut.vec_o41.get(), Bits::new(0x1_2345_6789));
    assert_eq!(dut.vec_io64.get(), Bits::new(0x10 ^ 0xffff));
    assert_eq!(dut.vec_ol8.get(), Bits::new(0x81 ^ 0xff));
    assert_eq!(
        dut.r_byte.get(),
        Bits::new(u128::from(0x81u8.wrapping_neg()))
    );
    assert_eq!(dut.r_short.get(), Bits::new(0x0382));
    assert_eq!(dut.r_ulong.get(), Bits::new(0x1000));
    assert_eq!(dut.r_logic.get(), Bit::ZERO);
    assert_eq!(dut.void_out.get(), Bits::new(77));
    assert_eq!(dut.aliased.get(), Bits::new(0x0102_0381 ^ 0x5555_5555));

    // The context task and the chandle run once per posedge.
    timer(ns(1)).await;
    clock(dut.clk, ns(10)).start();
    for _ in 0..3 {
        dut.clk.rising().await;
    }
    read_only().await;
    assert_eq!(dut.handle_ok.get(), Bit::ONE);
    assert_eq!(*tasks.borrow(), [0x0102_0381; 3]);
}

#[simjig::test(timeout = "1us")]
async fn imports_left_to_c_run_the_c_implementation(dut: dpi_c::Dut) {
    // No `set_dpi` exists for this model: both imports are `dpi_impl.c`'s.
    dut.a.set(40);
    dut.b.set(2);
    clock(dut.clk, ns(10)).start();
    for _ in 0..4 {
        dut.clk.rising().await;
    }
    read_only().await;
    assert_eq!(dut.sum.get(), Bits::new(42));
    // dpi_tick returns its call count, starting at 1; `calls` registers it.
    assert_eq!(dut.calls.get(), Bits::new(4));
    timer(ns(1)).await;
    dut.a.set(u128::from(u32::MAX));
    timer(ns(1)).await;
    read_only().await;
    assert_eq!(dut.sum.get(), Bits::new(1), "C's wrapping add");
}

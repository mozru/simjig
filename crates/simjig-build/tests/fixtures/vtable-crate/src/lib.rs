//! Calls the models' exported vtables through a hand-written mirror of `simjig_abi.h`.
//! The simjig runtime crate is deliberately not used: this checks the build pipeline only.
#![cfg(test)]
#![expect(unsafe_code, reason = "calling the C ABI directly is the point of this crate")]

use core::ffi::{CStr, c_char, c_void};

/// `simjig_model_vtable`, up to `signals` (enough for these tests).
#[repr(C)]
pub struct Vtable {
    abi_version: u32,
    size: u32,
    prefix: *const c_char,
    verilator_version: *const c_char,
    ir_hash: u64,
    create: unsafe extern "C" fn(*const c_void, *mut *mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    eval: unsafe extern "C" fn(*mut c_void) -> i32,
    final_: unsafe extern "C" fn(*mut c_void) -> i32,
    set_time: unsafe extern "C" fn(*mut c_void, u64),
    events_pending: unsafe extern "C" fn(*mut c_void) -> u32,
    next_time_slot: unsafe extern "C" fn(*mut c_void) -> u64,
    signals: unsafe extern "C" fn(*mut c_void, *mut u32) -> *const Signal,
}

/// `simjig_signal`.
#[repr(C)]
pub struct Signal {
    id: u32,
    width: u32,
    words: u32,
    storage: u8,
    dir: u8,
    access: u8,
    reserved: u8,
    name: *const c_char,
    ptr: *mut c_void,
}

const OK: i32 = 0;
const TIME_NONE: u64 = u64::MAX;

mod counter {
    include!(concat!(env!("OUT_DIR"), "/simjig/counter.rs"));
}
mod timing_dut {
    include!(concat!(env!("OUT_DIR"), "/simjig/timing_dut.rs"));
}
mod counter_fst {
    include!(concat!(env!("OUT_DIR"), "/simjig/counter_fst.rs"));
}
mod inv {
    include!(concat!(env!("OUT_DIR"), "/simjig/inv.rs"));
}
mod flop {
    include!(concat!(env!("OUT_DIR"), "/simjig/flop.rs"));
}

/// A live model behind a vtable.
struct Model {
    vt: &'static Vtable,
    h: *mut c_void,
}

impl Model {
    fn new(vt: *const Vtable, prefix: &str) -> Self {
        // SAFETY: the symbol returns a pointer to a static table, checked below.
        let vt = unsafe { &*vt };
        assert_eq!(vt.abi_version, 1);
        assert!(vt.size as usize >= size_of::<Vtable>());
        let mut h = core::ptr::null_mut();
        // SAFETY: NULL options are allowed; `h` is a valid out pointer.
        assert_eq!(unsafe { (vt.create)(core::ptr::null(), &raw mut h) }, OK);
        let m = Self { vt, h };
        assert_eq!(m.prefix(), prefix, "the symbol of one model returns its own table");
        m
    }

    fn prefix(&self) -> &str {
        // SAFETY: a static NUL-terminated string.
        unsafe { CStr::from_ptr(self.vt.prefix) }.to_str().unwrap()
    }

    fn eval(&self) -> i32 {
        // SAFETY: a live handle.
        unsafe { (self.vt.eval)(self.h) }
    }

    fn signal(&self, name: &str) -> &Signal {
        let mut n = 0;
        // SAFETY: a live handle; the table has `n` entries.
        let table = unsafe {
            let p = (self.vt.signals)(self.h, &raw mut n);
            core::slice::from_raw_parts(p, n as usize)
        };
        table
            .iter()
            // SAFETY: static NUL-terminated names.
            .find(|s| unsafe { CStr::from_ptr(s.name) }.to_str() == Ok(name))
            .unwrap_or_else(|| panic!("no signal {name}"))
    }

    /// Narrow (at most 32-bit) signals only.
    fn get(&self, name: &str) -> u32 {
        let s = self.signal(name);
        // SAFETY: `ptr` points at a live member of the storage the descriptor names.
        unsafe {
            match s.storage {
                1 => u32::from(*s.ptr.cast::<u8>()),
                2 => u32::from(*s.ptr.cast::<u16>()),
                3 => *s.ptr.cast::<u32>(),
                other => panic!("storage {other}"),
            }
        }
    }

    fn set(&self, name: &str, v: u8) {
        let s = self.signal(name);
        assert_eq!((s.storage, s.width), (1, 1), "{name}: 1-bit inputs only");
        // SAFETY: a live CData member.
        unsafe { *s.ptr.cast::<u8>() = v };
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: created by this vtable, destroyed once.
        unsafe { (self.vt.destroy)(self.h) };
    }
}

#[test]
fn counter_vtable_end_to_end() {
    let m = Model::new(counter::vtable(), counter::PREFIX);
    assert_eq!(m.signal("counter.count").width, 12, "WIDTH=12 reached the model");
    let clock = |n: u32| {
        for _ in 0..n {
            m.set("counter.clk", 1);
            assert_eq!(m.eval(), OK);
            m.set("counter.clk", 0);
            assert_eq!(m.eval(), OK);
        }
    };
    m.set("counter.rst", 1);
    clock(1);
    m.set("counter.rst", 0);
    m.set("counter.en", 1);
    clock(5000);
    // WIDTH=12 wraps at 4096.
    assert_eq!(m.get("counter.count"), 5000 % 4096);
}

#[test]
fn timing_model_schedules_its_own_events() {
    let m = Model::new(timing_dut::vtable(), timing_dut::PREFIX);
    assert_eq!(m.eval(), OK);
    // SAFETY: a live handle.
    let next = unsafe { (m.vt.next_time_slot)(m.h) };
    // `initial forever #5 iclk = ~iclk` with timeprecision 1ps: the first edge at 5 ns.
    assert_eq!(next, 5000);
    let mut t = 0;
    while t < 50_000 {
        // SAFETY: a live handle.
        t = unsafe { (m.vt.next_time_slot)(m.h) };
        assert_ne!(t, TIME_NONE);
        // SAFETY: a live handle.
        unsafe { (m.vt.set_time)(m.h, t) };
        assert_eq!(m.eval(), OK);
    }
    assert_eq!(m.get("timing_dut.ticks"), 5, "5 rising iclk edges by 50 ns");
}

#[test]
fn fst_tracing_links_and_writes_a_trace() {
    let m = Model::new(counter_fst::vtable(), counter_fst::PREFIX);
    assert_eq!(m.eval(), OK);
    let path = std::env::temp_dir().join(format!("simjig-fixture-{}.fst", std::process::id()));
    let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { counter_fst::fst_dump(c_path.as_ptr()) }, 0);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    // An FST file starts with its header block (FST_BL_HDR = 0), and the value-change
    // blocks are compressed, so a valid file exercised the lz4/zlib calls.
    assert!(bytes.len() > 100, "{} bytes", bytes.len());
    assert_eq!(bytes[0], 0);
}

#[test]
fn two_models_share_one_binary() {
    let inv = Model::new(inv::vtable(), inv::PREFIX);
    let flop = Model::new(flop::vtable(), flop::PREFIX);
    assert_ne!(inv.prefix(), flop.prefix());
    inv.set("inv.a", 0);
    assert_eq!(inv.eval(), OK);
    assert_eq!(inv.get("inv.y"), 1);
    // The first eval settles initial values; the edge comes after it.
    flop.set("flop.d", 1);
    assert_eq!(flop.eval(), OK);
    assert_eq!(flop.get("flop.q"), 0);
    flop.set("flop.clk", 1);
    assert_eq!(flop.eval(), OK);
    assert_eq!(flop.get("flop.q"), 1);
    // Each model has its own context: evaluating one leaves the other alone.
    inv.set("inv.a", 1);
    assert_eq!(inv.eval(), OK);
    assert_eq!((inv.get("inv.y"), flop.get("flop.q")), (0, 1));
}

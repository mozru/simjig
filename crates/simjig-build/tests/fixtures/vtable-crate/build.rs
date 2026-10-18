//! Builds five models into one crate, each through the full simjig-build pipeline with
//! the real code generator:
//! - `counter` (tests/duts/counter, WIDTH=12): the end-to-end vtable check;
//! - `timing_dut` with `--timing`: C++20 coroutines;
//! - `counter_fst`: the counter again, with `--trace-fst` (lz4 and zlib);
//! - `inv` and `flop`: two models with different prefixes in one binary.
//!
//! All five share one libverilated. The emitter wraps `codegen_emitter` and appends a
//! hand-written `extern` block for the vtable symbol (the runtime crate would own that).

use simjig_build::svtypes::ir::Design;
use simjig_build::{Dut, DutConfig, Emitted, Error, codegen_emitter};

fn main() -> Result<(), Error> {
    let dut = |name: &str, dir: &str| {
        Dut::new(name)
            .sources([format!("duts/{dir}/{dir}.sv")])
            .top(dir)
            .emitter(emit)
    };
    dut("counter", "counter").param("WIDTH", 12).build()?;
    dut("timing_dut", "timing_dut").timing(true).build()?;
    dut("counter_fst", "counter").trace_fst(true).build()?;
    dut("inv", "inv").build()?;
    dut("flop", "flop").build()?;
    Ok(())
}

/// The generated files, plus `PREFIX` and `ffi::vtable()` in the Rust; for `--trace-fst`
/// models also `<name>_fst_dump(path)` in the shim, which writes a short FST trace.
fn emit(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
    let mut e = codegen_emitter(design, config)?;
    let prefix = config.prefix();
    let name = &config.name;
    let rust = e.rust.as_mut().expect("codegen emits Rust");
    *rust += &format!(
        "\npub const PREFIX: &str = {prefix:?};\n\
         pub mod ffi {{\n    unsafe extern \"C\" {{\n\
         \x20       #[link_name = \"simjig_vtable_{prefix}\"]\n\
         \x20       pub safe fn vtable() -> *const crate::Vtable;\n"
    );
    if config.trace_fst {
        let shim = e.shim_cpp.as_mut().expect("codegen emits a shim");
        *shim += &format!(
            "\n#include \"verilated_fst_c.h\"\n\
             extern \"C\" int {name}_fst_dump(const char* path) {{\n\
             \x20   VerilatedContext ctx;\n    ctx.traceEverOn(true);\n\
             \x20   {prefix} m{{&ctx, \"top\"}};\n    VerilatedFstC tfp;\n\
             \x20   m.trace(&tfp, 99);\n    tfp.open(path);\n\
             \x20   for (int i = 0; i < 8; ++i) {{\n\
             \x20       m.clk = i & 1; m.rst = i < 2; m.en = 1; m.eval();\n\
             \x20       tfp.dump(ctx.time()); ctx.timeInc(1);\n    }}\n\
             \x20   tfp.close();\n    m.final();\n    return 0;\n}}\n"
        );
        *rust += &format!(
            "        #[link_name = \"{name}_fst_dump\"]\n\
             \x20       pub unsafe fn fst_dump(path: *const core::ffi::c_char) -> i32;\n"
        );
    }
    *rust += "    }\n}\n";
    Ok(e)
}

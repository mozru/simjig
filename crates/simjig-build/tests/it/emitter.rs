//! The emitter seam: what an emitter returns reaches Verilator, the compiler and `OUT_DIR`.

use std::path::Path;
use std::process::Command;

use simjig_build::svtypes::ir::Design;
use simjig_build::{
    Dut, DutConfig, Emitted, Error, HeaderPort, PortDir, Storage, Wrapper, ir_ports,
};

use crate::support::{Sandbox, dut};

/// Wraps `counter` in `counter_wrap`, which ties `en` high, and exports a vtable.
#[expect(clippy::unnecessary_wraps, reason = "the `Emitter` signature")]
fn wrapping_emitter(design: &Design, config: &DutConfig) -> Result<Emitted, Error> {
    let prefix = config.prefix();
    assert_eq!(
        design.top_module().name,
        "counter",
        "the IR is the user's top"
    );
    let wrapper = "module counter_wrap (input logic clk, input logic rst, \
                   output logic [7:0] count);\n  counter u (.clk, .rst, .en(1'b1), .count);\n\
                   endmodule\n";
    let shim = format!(
        "#include \"{prefix}.h\"\n#include \"simjig_model.h\"\n\
         namespace {{ struct T {{ using Model = {prefix};\n\
         static constexpr const char* kPrefix = \"{prefix}\";\n\
         static constexpr uint64_t kIrHash = 0; static constexpr bool kTiming = false;\n\
         static void bind({prefix}& m, simjig::SignalTable& t) {{\n\
         SIMJIG_SIGNAL(t, m.count, \"count\", 8, SIMJIG_DIR_OUT, SIMJIG_ACCESS_READ); }} }}; }}\n\
         SIMJIG_EXPORT_VTABLE({prefix}, T)\n"
    );
    let port = |name: &str, dir, msb| HeaderPort {
        name: name.into(),
        dir,
        msb,
        lsb: 0,
        storage: Storage::C8,
    };
    Ok(Emitted {
        rust: Some(format!("pub const PREFIX: &str = {prefix:?};\n")),
        shim_cpp: Some(shim),
        // A waiver the build needs: without it, -Wall's DECLFILENAME is fatal.
        vlt: Some("`verilator_config\nlint_off -rule DECLFILENAME\n".into()),
        wrapper: Some(Wrapper {
            module: "counter_wrap".into(),
            contents: wrapper.into(),
        }),
        ports: Some(vec![
            port("clk", PortDir::In, 0),
            port("rst", PortDir::In, 0),
            port("count", PortDir::Out, 7),
        ]),
    })
}

#[test]
fn emitted_files_reach_verilator_the_compiler_and_out_dir() {
    let s = Sandbox::new();
    let env = s.env();
    let built = Dut::new("counter")
        .sources([dut("counter").join("counter.sv")])
        .top("counter")
        .verilator_args(["-Wall"])
        .emitter(wrapping_emitter)
        .build_in(&env)
        .unwrap();

    // The wrapper is the top: the header has its ports (no `en`).
    let names: Vec<&str> = built.header_ports.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["clk", "rst", "count"]);
    // The IR still describes the user's top.
    assert_eq!(built.design.top_module().name, "counter");

    let rust = built.rust_file.as_deref().unwrap();
    assert_eq!(rust, env.out_dir.join("simjig/counter.rs"));
    assert!(
        std::fs::read_to_string(rust)
            .unwrap()
            .contains(&built.prefix)
    );
    let shim = env
        .out_dir
        .join(format!("simjig/{}_shim.cpp", built.prefix));
    assert!(shim.is_file());

    // The shim was compiled into the model archive.
    let archive = env.out_dir.join(format!(
        "simjig-{p}/libsimjig_model_{p}.a",
        p = built.prefix
    ));
    let nm = Command::new("nm").arg(&archive).output().unwrap();
    let symbols = String::from_utf8_lossy(&nm.stdout);
    assert!(
        symbols.contains(&format!("simjig_vtable_{}", built.prefix)),
        "{symbols}"
    );
}

#[test]
fn a_wrapper_without_ports_is_rejected() {
    #[expect(clippy::unnecessary_wraps, reason = "the `Emitter` signature")]
    fn emitter(_: &Design, _: &DutConfig) -> Result<Emitted, Error> {
        Ok(Emitted {
            wrapper: Some(Wrapper {
                module: "w".into(),
                contents: "module w; endmodule\n".into(),
            }),
            ..Emitted::default()
        })
    }
    let s = Sandbox::new();
    let err = Dut::new("inv")
        .sources([dut("inv").join("inv.sv")])
        .top("inv")
        .emitter(emitter)
        .build_in(&s.env())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "generating code for DUT `inv`: the emitter generated wrapper `w` but not its ports \
         (`Emitted::ports`)"
    );
    assert_eq!(s.verilations(), 0, "nothing was verilated");
}

/// With the default (no-op) emitter, the header is checked against the IR's ports.
fn ir_matches_header(s: &Sandbox, name: &str, sources: &[&Path], top: &str) {
    let built = Dut::new(name)
        .sources(sources)
        .top(top)
        .build_in(&s.env())
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut want = ir_ports(&built.design);
    let mut got = built.header_ports.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want, "{name}");
}

#[test]
fn ir_ports_match_the_header() {
    let s = Sandbox::new();
    let rtl = s.rtl();
    // Every storage class and direction, a `[5:2]` range and an `inout`.
    ir_matches_header(&s, "ports", &[&rtl.join("ports.sv")], "ports");
    let d = dut("struct_pipe");
    let pkg = d.join("struct_pipe_pkg.sv");
    ir_matches_header(
        &s,
        "struct_pipe",
        &[&pkg, &d.join("struct_pipe.sv")],
        "struct_pipe",
    );
    ir_matches_header(&s, "wide", &[&dut("wide").join("wide.sv")], "wide");
}

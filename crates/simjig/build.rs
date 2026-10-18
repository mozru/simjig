//! Builds the Verilator test models that simjig's own integration tests link against, each
//! through the real pipeline (`simjig_build::Dut`): the type IR, the generated Rust and C++
//! shim, verilation and compilation, exactly as a user's build script does.
//!
//! - `tests/abi_counter.rs` drives two models (`abi_counter`, `abi_ticker`) through the raw
//!   C ABI. `OUT_DIR/test_models.rs` gives every model's prefix and IR hash, and
//!   `OUT_DIR/test_vtables/<name>.rs` declares its vtable symbol as `vtable()` (a
//!   `link_name` must be a literal), returning the includer's `Vtable` type (a pointer).
//! - `tests/model_tests`, `tests/e2e_*`, `tests/multi_dut` and `tests/fork_checkpoints` include the generated bindings with
//!   `simjig::include_dut!` (`OUT_DIR/simjig/<name>.rs`).
//! - `tests/dpi_tests` adds the DPI-C models `dpi` (`tests/duts/dpi`) and `dpi_types`
//!   (every DPI type simjig implements), whose generated code includes the `Dpi` trait and
//!   the trampolines.
//!
//! Cargo has no test-only build scripts, so two rules keep this away from simjig's users:
//! - models are built only when `SIMJIG_TEST_MODELS=1`, which the repository's
//!   `.cargo/config.toml` sets; a crate that depends on simjig never builds them;
//! - nor does trybuild's nested build (`tests/harness_tests/ui.rs`), which reads the same
//!   config file but links no model (see [`in_trybuild`]);
//! - every link directive simjig-build emits is rewritten to `cargo::rustc-link-arg-tests`,
//!   so the archives reach only simjig's test binaries, never the library.
//!
//! If a model cannot be built (no Verilator, say), the script still succeeds so the
//! library builds anywhere, and each test binary fails with the reason instead of being
//! skipped.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use simjig_build::{CppRecipe, Dut, ProbeAccess, ProbeType, XInitial, codegen};

/// The cocotb goldens were recorded with `--timescale 1ns/1ps` (tests/conformance/README.md).
const TIMESCALE: [&str; 2] = ["--timescale", "1ns/1ps"];

/// A design in the shared `tests/duts/<name>` corpus, with the goldens' timescale.
fn shared(name: &str, files: &[&str]) -> Dut {
    let dir = format!("../../tests/duts/{name}");
    Dut::new(name)
        .sources(files.iter().map(|f| format!("{dir}/{f}")))
        .top(name)
        .verilator_args(TIMESCALE)
}

/// The shared design `design` built as the model `name`.
fn shared_as(name: &str, design: &str) -> Dut {
    let dir = format!("../../tests/duts/{design}");
    Dut::new(name)
        .sources([format!("{dir}/{design}.sv")])
        .top(design)
        .verilator_args(TIMESCALE)
}

/// Read-only probes of the internal variables `vars` of the top `module`.
fn probes(dut: Dut, module: &str, width: u32, vars: &[&str]) -> Dut {
    vars.iter().fold(dut, |dut, var| {
        dut.probe(
            format!("{module}.{var}"),
            module,
            ProbeType::Bits { width },
            ProbeAccess::Read,
        )
    })
}

/// The designs that exercise interfaces with probes, coverage and DPI-C.
fn feature_duts() -> Vec<Dut> {
    vec![
        shared("iface_top", &["iface_pkg.sv", "bus_if.sv", "iface_top.sv"])
            .bind_iface("in_bus", "bus_if.dst")
            .bind_iface("out_bus", "bus_if.src")
            .probe(
                "iface_top.full",
                "iface_top",
                ProbeType::Bits { width: 1 },
                ProbeAccess::Read,
            )
            .probe(
                "iface_top.data_q",
                "iface_top",
                ProbeType::Bits { width: 16 },
                ProbeAccess::Read,
            ),
        // The counter with coverage counters (tests/e2e_stage3).
        shared_as("counter_cov", "counter").coverage(true),
        Dut::new("dpi")
            .sources(["../../tests/duts/dpi/dpi.sv"])
            .top("dpi"),
        // The same design with both imports in C (the reference implementation).
        Dut::new("dpi_c")
            .sources(["../../tests/duts/dpi/dpi.sv"])
            .top("dpi")
            .dpi_in_c(["dpi_add", "dpi_tick"])
            .recipe(CppRecipe {
                model_sources: vec!["../../tests/duts/dpi/dpi_impl.c".into()],
                ..CppRecipe::default()
            }),
        Dut::new("dpi_types")
            .sources(["tests/dpi_tests/dpi_types.sv"])
            .top("dpi_types"),
    ]
}

fn duts() -> Vec<Dut> {
    let async_fifo = shared("async_fifo", &["async_fifo.sv"])
        .param("WIDTH", 8)
        .param("DEPTH", 8);
    vec![
        Dut::new("abi_counter")
            .sources(["tests/abi_counter/counter.sv"])
            .top("counter"),
        Dut::new("abi_ticker")
            .sources(["tests/abi_counter/ticker.sv"])
            .top("ticker")
            .timing(true),
        Dut::new("hello")
            .sources(["tests/model_tests/hello.sv"])
            .top("hello")
            .verilator_args(TIMESCALE),
        // A register without reset, built with and without randomizable initial values
        // (tests/model_tests/init.rs).
        Dut::new("no_reset")
            .sources(["tests/model_tests/no_reset.sv"])
            .top("no_reset")
            .verilator_args(TIMESCALE),
        Dut::new("no_reset_fast")
            .sources(["tests/model_tests/no_reset.sv"])
            .top("no_reset")
            .verilator_args(TIMESCALE)
            .x_initial(XInitial::Fast),
        shared("inv", &["inv.sv"]),
        shared("flop", &["flop.sv"]),
        shared("counter", &["counter.sv"]),
        // A second revision of `counter` (same top, different RTL) for the lockstep tests.
        Dut::new("counter_rev2")
            .sources(["tests/multi_dut/counter_rev2.sv"])
            .top("counter")
            .verilator_args(TIMESCALE),
        probes(
            shared("comb_chain", &["comb_chain.sv"]),
            "comb_chain",
            8,
            &["s1", "s2", "s3"],
        ),
        shared("fatal", &["fatal.sv"]),
        shared("wide", &["wide.sv"]),
        // timing_dut declares its own timeunit and precision.
        Dut::new("timing_dut")
            .sources(["../../tests/duts/timing_dut/timing_dut.sv"])
            .top("timing_dut")
            .timing(true),
        shared("struct_pipe", &["struct_pipe_pkg.sv", "struct_pipe.sv"]),
        shared("fifo", &["fifo.sv"])
            .param("WIDTH", 8)
            .param("DEPTH", 4),
        probes(
            async_fifo,
            "async_fifo",
            4,
            &["wbin", "rgray_w2", "rbin", "wgray_r2"],
        ),
        shared("derived_clk", &["derived_clk.sv"]),
        // FST-traced builds of two shared designs, for the waves tests (tests/waves_tests).
        shared_as("counter_fst", "counter").trace_fst(true),
        shared_as("fifo_fst", "fifo")
            .param("WIDTH", 8)
            .param("DEPTH", 4)
            .trace_fst(true),
        // A DPI-C design and the `$fatal` design, FST-traced: a model poisoned in a fork
        // branch still finishes the branch's waves (tests/waves_tests).
        Dut::new("dpi_fst")
            .sources(["../../tests/duts/dpi/dpi.sv"])
            .top("dpi")
            .verilator_args(TIMESCALE)
            .trace_fst(true),
        shared_as("fatal_fst", "fatal").trace_fst(true),
    ]
    .into_iter()
    .chain(feature_duts())
    .collect()
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(simjig_test_models)");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=SIMJIG_TEST_MODELS");
    if std::env::var_os("SIMJIG_TEST_MODELS").is_none_or(|v| v != "1") {
        let why = "SIMJIG_TEST_MODELS is not 1 (the repository's .cargo/config.toml sets it)";
        println!("cargo::rustc-env=SIMJIG_TEST_MODELS_ERROR={why}");
        return;
    }
    if std::env::var_os("OUT_DIR").is_some_and(|out| in_trybuild(Path::new(&out))) {
        let why = "trybuild's nested build links no test model";
        println!("cargo::rustc-env=SIMJIG_TEST_MODELS_ERROR={why}");
        return;
    }
    match build_models() {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            println!("cargo::rustc-cfg=simjig_test_models");
        }
        Err(err) => {
            let err = err.replace('\n', " ");
            println!("cargo::warning=simjig test models not built: {err}");
            println!("cargo::rustc-env=SIMJIG_TEST_MODELS_ERROR={err}");
        }
    }
}

/// Whether `out_dir` lies in trybuild's target directory (`<target>/tests/trybuild`, which
/// trybuild 1.x always uses for its nested cargo).
///
/// That build compiles simjig only as the library its compile-fail cases depend on, so
/// models built there are never linked, and on a cold target verilating all of them takes
/// longer than nextest lets the UI test run. Detected from the path rather than by the UI
/// test setting `SIMJIG_TEST_MODELS=0` (an existing variable wins over `[env]`): that would
/// need the `unsafe` `std::env::set_var` in a test binary whose other tests read the
/// environment on other threads, and every way of running the test (cargo test, nextest, an
/// IDE) passes through this path.
fn in_trybuild(out_dir: &Path) -> bool {
    let parts: Vec<_> = out_dir.components().collect();
    parts
        .windows(2)
        .any(|w| w[0].as_os_str() == "tests" && w[1].as_os_str() == "trybuild")
}

/// Builds every model; returns the `cargo::` lines to print (printed only when all of them
/// built, so a half-built set links nothing).
fn build_models() -> Result<Vec<String>, String> {
    let env = simjig_build::BuildEnv::from_cargo().map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    let mut consts = String::from("// @generated by crates/simjig/build.rs\n");
    for dut in duts() {
        let config = dut.config().clone();
        let built = dut.build_in(&env).map_err(|e| e.to_string())?;
        lines.extend(tests_only(&built.directives));
        let lowered =
            codegen::lower(&built.design, &config.codegen_config()).map_err(|e| e.to_string())?;
        let upper = config.name.to_uppercase();
        let vtables = env.out_dir.join("test_vtables");
        std::fs::create_dir_all(&vtables).map_err(|e| format!("{}: {e}", vtables.display()))?;
        let link = vtables.join(format!("{}.rs", config.name));
        std::fs::write(&link, vtable_decl(&built.prefix))
            .map_err(|e| format!("{}: {e}", link.display()))?;
        writeln!(
            consts,
            "pub(crate) const {upper}_PREFIX: &str = {:?};\n\
             pub(crate) const {upper}_IR_HASH: u64 = {};",
            built.prefix,
            grouped_hex(lowered.ir_hash)
        )
        .map_err(|e| e.to_string())?;
    }
    let path = env.out_dir.join("test_models.rs");
    std::fs::write(&path, consts).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(lines)
}

/// `h` as a hex literal in groups of four digits (clippy's `unreadable_literal`).
fn grouped_hex(h: u64) -> String {
    let group = |shift: u32| (h >> shift) & 0xffff;
    format!(
        "0x{:04x}_{:04x}_{:04x}_{:04x}",
        group(48),
        group(32),
        group(16),
        group(0)
    )
}

/// `vtable()`: the model's exported vtable, typed as the including module's `Vtable`.
fn vtable_decl(prefix: &str) -> String {
    format!(
        "// @generated by crates/simjig/build.rs\n\
         unsafe extern \"C\" {{\n\
         \x20   #[link_name = \"simjig_vtable_{prefix}\"]\n\
         \x20   pub(crate) safe fn vtable() -> Vtable;\n\
         }}\n"
    )
}

/// simjig-build's directives with every link directive narrowed to test binaries:
/// `rustc-link-search` + `rustc-link-lib=static=NAME` become the archive's path, a plain
/// `rustc-link-lib=NAME` becomes `-L<dirs> -lNAME`, and `rustc-link-arg` its `-tests` form.
/// On ELF an archive only satisfies references made before it; simjig-build lists each
/// model's archive before the shared runtime, and each DUT repeats the runtime.
fn tests_only(directives: &[String]) -> Vec<String> {
    let mut search: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for d in directives {
        let rest = d.strip_prefix("cargo::").unwrap_or(d);
        if let Some(dir) = rest.strip_prefix("rustc-link-search=native=") {
            search.push(PathBuf::from(dir));
        } else if let Some(name) = rest.strip_prefix("rustc-link-lib=static=") {
            let file = format!("lib{name}.a");
            let archive = search
                .iter()
                .rev()
                .map(|dir| dir.join(&file))
                .find(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from(&file));
            out.push(format!("cargo::rustc-link-arg-tests={}", archive.display()));
        } else if let Some(name) = rest.strip_prefix("rustc-link-lib=") {
            for dir in &search {
                out.push(format!("cargo::rustc-link-arg-tests=-L{}", dir.display()));
            }
            out.push(format!("cargo::rustc-link-arg-tests=-l{name}"));
        } else if let Some(arg) = rest.strip_prefix("rustc-link-arg=") {
            out.push(format!("cargo::rustc-link-arg-tests={arg}"));
        } else {
            out.push(d.clone());
        }
    }
    out
}

//! Builds every spike model and links them into ONE binary per test file.
//!
//! The point of doing it by hand (no `make`, no `verilator --build`) is experiment 2:
//! - each model's generated C++ and its shim go into their own archive,
//!   `libspk_<prefix>.a`, compiled with the `VM_*` defines of that model's
//!   `<prefix>_classes.mk`;
//! - libverilated (plus the spike runtime) is compiled ONCE into `libspk_verilated.a`,
//!   with the union of the features the models need (timing, FST, DPI), and linked once.
//!
//! A second, unlinked copy of `Vcounter` (same prefix, separate Verilator run) goes into
//! `libdup_Vcounter.a` so the symbol-collision test has a negative control.
//!
//! Must run inside `pixi run` (Verilator 5.052 and the pinned C++ compiler on PATH).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::thread;

/// One Verilator model of the spikes.
struct Model {
    prefix: &'static str,
    top: &'static str,
    shim: &'static str,
    args: &'static [&'static str],
    /// Relative to the repository root, in compile order.
    sources: &'static [&'static str],
}

const MODELS: &[Model] = &[
    Model {
        prefix: "Vcounter",
        top: "counter",
        shim: "counter.cpp",
        args: &["--trace-fst"],
        sources: &["tests/duts/counter/counter.sv"],
    },
    Model {
        prefix: "Vbigfifo",
        top: "fifo",
        shim: "bigfifo.cpp",
        // 2**23 x 64 bit = 64 MiB of model memory.
        args: &["-GDEPTH=8388608", "-GWIDTH=64"],
        sources: &["tests/duts/fifo/fifo.sv"],
    },
    Model {
        prefix: "Vrng_a",
        top: "rng_disp",
        shim: "rng_disp.cpp",
        args: &[],
        sources: &["tests/spikes/rtl/rng_disp.sv"],
    },
    Model {
        prefix: "Vrng_b",
        top: "rng_disp",
        shim: "rng_disp.cpp",
        args: &[],
        sources: &["tests/spikes/rtl/rng_disp.sv"],
    },
    Model {
        prefix: "Vfatal",
        top: "fatal",
        shim: "fatal.cpp",
        args: &[],
        sources: &["tests/duts/fatal/fatal.sv"],
    },
    Model {
        prefix: "Vdpi",
        top: "dpi",
        shim: "dpi.cpp",
        args: &[],
        sources: &["tests/duts/dpi/dpi.sv"],
    },
    Model {
        prefix: "Vtiming",
        top: "timing_dut",
        shim: "timing.cpp",
        args: &["--timing"],
        sources: &["tests/duts/timing_dut/timing_dut.sv"],
    },
    Model {
        prefix: "Vderived",
        top: "derived_clk",
        shim: "derived.cpp",
        args: &[],
        sources: &[
            "tests/spikes/rtl/derived_clk.vlt",
            "tests/duts/derived_clk/derived_clk.sv",
        ],
    },
    Model {
        prefix: "Vglitch",
        top: "glitch",
        shim: "glitch.cpp",
        args: &[],
        sources: &["tests/spikes/rtl/glitch.vlt", "tests/spikes/rtl/glitch.sv"],
    },
];

/// libverilated sources: the union of every model's `VM_GLOBAL_FAST/SLOW`.
const VERILATED_SOURCES: &[&str] = &[
    "verilated.cpp",
    "verilated_threads.cpp",
    "verilated_timing.cpp",
    "verilated_fst_c.cpp",
    "verilated_dpi.cpp",
];

/// Paths and tools shared by every step.
struct Env {
    root: PathBuf,
    out: PathBuf,
    lib: PathBuf,
    include: PathBuf,
    cxx: String,
    ar: String,
    conda: Option<PathBuf>,
}

/// One C++ compile: source, object, extra flags.
struct Job {
    src: PathBuf,
    obj: PathBuf,
    flags: Vec<String>,
}

fn main() {
    let e = environment();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=shim");
    println!("cargo:rerun-if-changed=rtl");
    for m in MODELS {
        for s in m.sources {
            println!("cargo:rerun-if-changed={}", e.root.join(s).display());
        }
    }
    fs::create_dir_all(&e.lib).expect("create lib dir");

    let mut archives = Vec::new();
    for m in MODELS {
        archives.push((
            format!("libspk_{}.a", m.prefix),
            model_jobs(&e, m, m.prefix),
        ));
    }
    // Negative control for the collision test: same design, same prefix, second run.
    archives.push((
        "libdup_Vcounter.a".to_owned(),
        model_jobs(&e, &MODELS[0], "dup"),
    ));
    archives.push(("libspk_verilated.a".to_owned(), verilated_jobs(&e)));

    let all: Vec<&Job> = archives.iter().flat_map(|(_, jobs)| jobs).collect();
    compile_all(&e, &all);
    for (name, jobs) in &archives {
        archive(&e, name, jobs);
    }
    emit_link(&e);
}

fn environment() -> Env {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..").canonicalize().expect("repo root");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let vroot = run_capture(Command::new("verilator").args(["--getenv", "VERILATOR_ROOT"]));
    Env {
        lib: out.join("lib"),
        include: PathBuf::from(vroot.trim()).join("include"),
        cxx: env::var("CXX").unwrap_or_else(|_| "c++".to_owned()),
        ar: env::var("AR").unwrap_or_else(|_| "ar".to_owned()),
        conda: env::var_os("CONDA_PREFIX").map(PathBuf::from),
        root,
        out,
    }
}

/// Flags every translation unit shares, including the `$display` routing hook. It must
/// be the same in every TU: `VL_PRINTF` is also used by inline functions in the headers.
fn common_flags(e: &Env) -> Vec<String> {
    let mut f: Vec<String> = [
        "-std=gnu++20",
        "-Os",
        "-w",
        "-fPIC",
        "-faligned-new",
        "-DVERILATOR=1",
        "-DVL_PRINTF=spk_printf",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let shim = e.root.join("tests/spikes/shim");
    f.push("-include".to_owned());
    f.push(shim.join("spk_runtime.h").display().to_string());
    f.push(format!("-I{}", shim.display()));
    f.push(format!("-I{}", e.include.display()));
    f.push(format!("-I{}", e.include.join("vltstd").display()));
    if let Some(c) = &e.conda {
        f.push(format!("-isystem{}", c.join("include").display()));
    }
    if run_capture(Command::new(&e.cxx).arg("--version")).contains("clang") {
        f.push("-fbracket-depth=4096".to_owned());
    }
    f
}

/// Verilates `m` into OUT_DIR/<dir> and returns the compile jobs for its C++ and shim.
fn model_jobs(e: &Env, m: &Model, dir: &str) -> Vec<Job> {
    let mdir = e.out.join(dir);
    let mut cmd = Command::new("verilator");
    cmd.current_dir(&e.root)
        .args([
            "--cc",
            "--threads",
            "1",
            "-Wno-fatal",
            "--top-module",
            m.top,
        ])
        .arg("--prefix")
        .arg(m.prefix)
        .arg("--Mdir")
        .arg(&mdir)
        .args(m.args)
        .args(m.sources);
    run_capture(&mut cmd);

    let mk = fs::read_to_string(mdir.join(format!("{}_classes.mk", m.prefix))).expect("read mk");
    let mut flags = common_flags(e);
    for var in [
        "VM_COVERAGE",
        "VM_SC",
        "VM_TIMING",
        "VM_TRACE",
        "VM_TRACE_FST",
        "VM_TRACE_VCD",
        "VM_TRACE_SAIF",
        "VM_VPI",
    ] {
        flags.push(format!("-D{var}={}", mk_scalar(&mk, var)));
    }
    flags.push(format!("-I{}", mdir.display()));
    flags.push(format!("-DSPK_TOP={}", m.prefix));
    flags.push(format!("-DSPK_HEADER=\"{}.h\"", m.prefix));
    // Internals (rootp->...) for the shims that reach past the ports.
    flags.push(format!("-DSPK_ROOT_HEADER=\"{}___024root.h\"", m.prefix));

    let mut jobs = Vec::new();
    for var in [
        "VM_CLASSES_FAST",
        "VM_CLASSES_SLOW",
        "VM_SUPPORT_FAST",
        "VM_SUPPORT_SLOW",
    ] {
        for class in mk_list(&mk, var) {
            jobs.push(Job {
                src: mdir.join(format!("{class}.cpp")),
                obj: mdir.join(format!("{class}.o")),
                flags: flags.clone(),
            });
        }
    }
    jobs.push(Job {
        src: e.root.join("tests/spikes/shim").join(m.shim),
        obj: mdir.join("spk_shim.o"),
        flags,
    });
    jobs
}

fn verilated_jobs(e: &Env) -> Vec<Job> {
    let dir = e.out.join("verilated");
    fs::create_dir_all(&dir).expect("create verilated dir");
    let mut flags = common_flags(e);
    // The union of the models' features; VM_* only gate WAVES and coverage here.
    for d in [
        "VM_COVERAGE=0",
        "VM_SC=0",
        "VM_TIMING=1",
        "VM_TRACE=1",
        "VM_TRACE_FST=1",
        "VM_TRACE_VCD=0",
        "VM_TRACE_SAIF=0",
        "VM_VPI=0",
    ] {
        flags.push(format!("-D{d}"));
    }
    let mut jobs: Vec<Job> = VERILATED_SOURCES
        .iter()
        .map(|s| Job {
            src: e.include.join(s),
            obj: dir.join(s.replace(".cpp", ".o")),
            flags: flags.clone(),
        })
        .collect();
    jobs.push(Job {
        src: e.root.join("tests/spikes/shim/runtime.cpp"),
        obj: dir.join("runtime.o"),
        flags,
    });
    jobs
}

/// Compiles every job, as many at once as there are cores.
fn compile_all(e: &Env, jobs: &[&Job]) {
    let queue = Mutex::new(jobs.iter());
    let n = thread::available_parallelism().map_or(4, |n| n.get());
    thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| {
                loop {
                    let Some(job) = queue.lock().expect("queue").next() else {
                        break;
                    };
                    let mut cmd = Command::new(&e.cxx);
                    cmd.args(&job.flags)
                        .arg("-c")
                        .arg(&job.src)
                        .arg("-o")
                        .arg(&job.obj);
                    run_capture(&mut cmd);
                }
            });
        }
    });
}

fn archive(e: &Env, name: &str, jobs: &[Job]) {
    let path = e.lib.join(name);
    let _ = fs::remove_file(&path);
    let mut cmd = Command::new(&e.ar);
    cmd.arg("rcs").arg(&path);
    for j in jobs {
        cmd.arg(&j.obj);
    }
    run_capture(&mut cmd);
}

fn emit_link(e: &Env) {
    println!("cargo:rustc-link-search=native={}", e.lib.display());
    // Order matters for single-pass linkers: models, then libverilated, then system libs.
    // `-bundle` keeps them as plain `-l` flags on the final link, in this order.
    for m in MODELS {
        println!("cargo:rustc-link-lib=static:-bundle=spk_{}", m.prefix);
    }
    println!("cargo:rustc-link-lib=static:-bundle=spk_verilated");
    if let Some(c) = &e.conda {
        println!("cargo:rustc-link-search=native={}", c.join("lib").display());
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,{}",
            c.join("lib").display()
        );
    }
    println!("cargo:rustc-link-lib=dylib=lz4");
    println!("cargo:rustc-link-lib=dylib=z");
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
        // Same as verilated.mk: these weak hooks may stay undefined.
        println!(
            "cargo:rustc-link-arg=-Wl,-U,__Z15vl_time_stamp64v,-U,__Z13sc_time_stampv,-U,_vlog_startup_routines"
        );
    } else {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=pthread");
    }
    println!("cargo:rustc-env=SPK_LIB_DIR={}", e.lib.display());
    println!("cargo:rustc-env=SPK_CXX={}", e.cxx);
}

/// `VAR = value` from a Verilator `_classes.mk`.
fn mk_scalar(mk: &str, var: &str) -> String {
    mk.lines()
        .find_map(|l| {
            l.strip_prefix(var)?
                .trim()
                .strip_prefix('=')
                .map(|v| v.trim().to_owned())
        })
        .unwrap_or_else(|| "0".to_owned())
}

/// The names in a `VAR += \` continuation list from a Verilator `_classes.mk`.
fn mk_list(mk: &str, var: &str) -> Vec<String> {
    let mut lines = mk
        .lines()
        .skip_while(|l| !l.starts_with(&format!("{var} +=")));
    lines.next();
    lines
        .map_while(|l| l.strip_suffix('\\').map(|n| n.trim().to_owned()))
        .filter(|n| !n.is_empty())
        .collect()
}

fn run_capture(cmd: &mut Command) -> String {
    let out = cmd
        .output()
        .unwrap_or_else(|err| panic!("spawn {cmd:?}: {err}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

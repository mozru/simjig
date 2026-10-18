//! The `build.rs`-facing description of one design under test, and the build pipeline.
//!
//! [`Dut`] records configuration only. [`Dut::build_in`] then runs the pipeline in a
//! fixed order:
//! 1. locate and version-check Verilator;
//! 2. claim the prefix (see `pipeline::claim`): two configurations must never share one;
//! 3. the JSON pass (`svtypes::verilator::json_pass`) with the same sources, `-I`, `-D`,
//!    `-G`, extra arguments and `--timing` setting (one builder, `DutConfig::design`),
//!    giving the type IR. A DUT with
//!    interface ports takes two passes through generated tops (`pipeline::design_ir`).
//!    The IR is cached (`front`): a rerun that changes nothing runs no JSON pass;
//! 4. the [`Emitter`] turns the IR into the Rust bindings, the C++ shim, a `.vlt` file
//!    and a layout check (see [`crate::emit`] for where each goes), and the layout check
//!    is linted, unless it passed for the same IR before (`front`);
//! 5. verilate (`--cc`) through the content-hash cache, with the `.vlt` and wrapper;
//! 6. cross-check the header's ports against the IR's (or the emitter's), each probe
//!    against Verilator's scope table (`syms`), the DPI-C prototypes in `V<prefix>__Dpi.h`
//!    against the IR's, and check that no other DUT of this build script exports the same
//!    DPI names (`script`);
//! 7. compile and link the C++ (see `compile`), and collect the `cargo::` directives.
//!
//! The IR comes before verilation because the emitters' outputs are Verilator inputs.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use svtypes::ir::{Design, DpiImport};

use crate::codegen::{CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType};
use crate::compile::{self, CppRecipe};
use crate::emit::{self, Emitter};
use crate::front::Front;
use crate::hash::KeyHasher;
use crate::header::{self, HeaderPort};
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error, cache, manifest, pipeline, script};

/// A file generated before verilation (a `.vlt` control file or a wrapper module). It is
/// stored content-addressed in the shared cache, so its path is the same in every crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GeneratedInput {
    /// File name, with the extension Verilator uses to pick its role (`.vlt`, `.sv`).
    pub file_name: String,
    /// Full contents.
    pub contents: String,
}

/// How the model is verilated, decided once ([`DutConfig::features`]) and used by every
/// step that must agree with Verilator: the model build, the JSON pass and layout check
/// (`--timing`), the shim (`kTiming`, `kTraceFst`, `kCoverage`) and the link (FST writer,
/// coverage runtime). The compile step checks the result against the `VM_*` switches
/// Verilator wrote, so an option file (`-f`) that changes one fails the build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    /// `--timing` ([`Dut::timing`]); raw `--timing`/`--no-timing` are rejected.
    pub timing: bool,
    /// `--trace-fst` ([`Dut::trace_fst`]); raw trace switches are rejected.
    pub trace_fst: bool,
    /// Coverage counters: [`Dut::coverage`], then the coverage flags among the extra
    /// arguments, by Verilator's rule (see [`DutConfig::features`]).
    pub coverage: bool,
}

/// The coverage kinds of Verilator 5.052 (`V3Options::coverage(bool)` sets all five; the
/// model has counters while any is on).
const COVERAGE_KINDS: [&str; 5] = ["line", "toggle", "expr", "fsm", "user"];

/// An option's name without its one or two leading dashes (Verilator accepts both), or
/// `None` for a file or an option value.
fn option_name(arg: &str) -> Option<&str> {
    arg.strip_prefix("--")
        .or_else(|| arg.strip_prefix('-'))
        .filter(|name| !name.is_empty())
}

/// Coverage after the builder switch and then `args`, last word winning per kind.
fn coverage_after(builder: bool, args: &[String]) -> bool {
    let mut kinds = [builder; COVERAGE_KINDS.len()];
    for name in args.iter().filter_map(|a| option_name(a)) {
        let (on, rest) = match name.strip_prefix("no-") {
            Some(rest) => (false, rest),
            None => (true, name),
        };
        if rest == "coverage" {
            kinds = [on; COVERAGE_KINDS.len()];
        } else if let Some(kind) = rest.strip_prefix("coverage-")
            && let Some(i) = COVERAGE_KINDS.iter().position(|k| *k == kind)
        {
            kinds[i] = on;
        }
    }
    kinds.contains(&true)
}

/// Why a parameter override (`-G`, `-pvalue+`) is refused, among the arguments or in an
/// option file.
const PARAM_HINT: &str = "pass parameters with `.param(name, value)`: a raw override \
     reaches every Verilator run, and the layout check, whose top has no such parameter, \
     fails (`Parameters from the command line were not found in the design`)";

fn is_param_override(arg: &str) -> bool {
    option_name(arg).is_some_and(|name| name.starts_with('G') || name.starts_with("pvalue+"))
}

/// Why a raw Verilator argument is refused, if it is: each has a builder switch (or a
/// fixed value) that every Verilator run and the generated code must agree on, or is set
/// by the pipeline itself.
fn refused_arg(arg: &str, next: Option<&str>) -> Option<&'static str> {
    let name = option_name(arg)?;
    if is_param_override(arg) {
        return Some(PARAM_HINT);
    }
    // `--prefix=Vx` is `--prefix Vx`.
    let name = name.split_once('=').map_or(name, |(n, _)| n);
    match name {
        "timing" | "no-timing" => Some(
            "use `.timing(true)` or `.timing(false)`: the JSON pass, the layout check and \
             the shim (which must know whether the model runs coroutines) all follow it",
        ),
        "trace" | "no-trace" | "trace-fst" | "trace-vcd" | "trace-saif" | "trace-threads"
        | "trace-fst-thread" => Some(
            "use `.trace_fst(true)`: simjig writes FST waves through the shim, which must \
             know the model is traced, and on the testbench's one thread",
        ),
        "threads" if next != Some("1") => Some(
            "simjig runs each model on one thread (its context has `threads(1)`, so runs \
             are deterministic and the testbench owns the only thread); a model built for \
             more needs a thread pool",
        ),
        "top-module" | "top" => Some(
            "set the top with `.top(\"module\")`: the JSON pass, the generated wrapper and \
             the model build each pass their own `--top-module`",
        ),
        "prefix" => Some(
            "the pipeline names the model `V<name>_<hash>` from `Dut::new(name)` and the \
             configuration, so variants never share C++ symbols; rename the DUT instead",
        ),
        "Mdir" => Some(
            "the pipeline puts Verilator's output in the shared content-addressed cache; \
             set the cache with `SIMJIG_CACHE_DIR`",
        ),
        "cc" | "exe" | "build" | "main" | "binary" | "lib-create" | "o" => Some(
            "the pipeline verilates with `--cc` and compiles and links the model itself; \
             add C++ sources with `.recipe(CppRecipe { model_sources, .. })`",
        ),
        "sc" => Some(
            "simjig models are C++ (`--cc`): the shim and the runtime drive a C++ model, not \
             a SystemC one",
        ),
        "hierarchical" => Some(
            "the pipeline compiles one model per DUT, and a hierarchical block is a separate \
             model whose variables the DUT's scope table does not hold",
        ),
        _ => None,
    }
}

/// Everything recorded about a DUT. Emitters read this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DutConfig {
    /// Name given to [`Dut::new`]; a C identifier.
    pub name: String,
    /// Source files, in order.
    pub sources: Vec<PathBuf>,
    /// `` `include `` search directories (`-I`).
    pub include_dirs: Vec<PathBuf>,
    /// Preprocessor defines (`-D`).
    pub defines: Vec<(String, String)>,
    /// Top-level parameter overrides (`-G`).
    pub params: Vec<(String, String)>,
    /// Top module. Required by [`Dut::build`]: the JSON pass and the emitters need it.
    pub top: Option<String>,
    /// Internal signals to expose.
    pub probes: Vec<Probe>,
    /// Interface-port bindings; non-empty makes the generated wrapper the model top.
    pub ifaces: Vec<IfaceBinding>,
    /// `--timing` instead of `--no-timing`.
    pub timing: bool,
    /// `--trace-fst`.
    pub trace_fst: bool,
    /// `--coverage`.
    pub coverage: bool,
    /// Extra Verilator arguments, passed last.
    pub verilator_args: Vec<String>,
    /// Generated Verilator inputs, passed before the sources.
    pub generated: Vec<GeneratedInput>,
    /// C names of the DPI-C imports the user implements in C or C++ ([`Dut::dpi_in_c`]);
    /// the code generator emits no Rust trampoline for them.
    pub dpi_in_c: Vec<String>,
}

impl DutConfig {
    /// Timing, FST tracing and coverage as Verilator will build them (see [`Features`]).
    ///
    /// Coverage is [`Dut::coverage`] followed by the extra arguments: `--coverage` and
    /// `--no-coverage` switch every kind (line, toggle, expr, fsm, user), `--coverage-<kind>`
    /// and `--no-coverage-<kind>` one, the last word winning; coverage is on while any kind
    /// is. Options that only tune coverage (`--coverage-max-width`) do not turn it on.
    pub fn features(&self) -> Features {
        Features {
            timing: self.timing,
            trace_fst: self.trace_fst,
            coverage: coverage_after(self.coverage, &self.verilator_args),
        }
    }

    /// The design's DPI-C imports that Rust implements: all of them except
    /// [`dpi_in_c`](Self::dpi_in_c).
    ///
    /// # Errors
    /// [`Error::Config`] if `dpi_in_c` names an import the design does not have.
    pub fn rust_dpi_imports(&self, design: &Design) -> Result<Vec<DpiImport>, Error> {
        if let Some(unknown) = self
            .dpi_in_c
            .iter()
            .find(|c| !design.dpi_imports.iter().any(|i| &i.c_name == *c))
        {
            let known: Vec<&str> = design
                .dpi_imports
                .iter()
                .map(|i| i.c_name.as_str())
                .collect();
            return Err(Error::Config {
                dut: self.name.clone(),
                msg: format!(
                    "`dpi_in_c` names `{unknown}`, but the design imports no DPI-C function \
                     with that C name (its imports: {})",
                    if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    }
                ),
            });
        }
        Ok(design
            .dpi_imports
            .iter()
            .filter(|i| !self.dpi_in_c.contains(&i.c_name))
            .cloned()
            .collect())
    }

    /// The Verilator `--prefix`: `V<name>_<hash8>`, unique per configuration so several
    /// variants of one design can link into one binary.
    ///
    /// It covers every setting that changes the model's code (sources by path, flags,
    /// params, probes, bindings, user-supplied generated inputs) but not file contents
    /// or emitter outputs, so emitters can compute it before generating anything.
    pub fn prefix(&self) -> String {
        // Every entry is tagged with its list, so an entry cannot move to another list (a
        // define `W=8` to a parameter `W=8`) without changing the prefix.
        let mut h = KeyHasher::new("simjig-prefix-v2");
        h.str("name").str(&self.name);
        h.str("top").str(self.top.as_deref().unwrap_or(""));
        for s in &self.sources {
            h.str("source").path(s);
        }
        for d in &self.include_dirs {
            h.str("include").path(d);
        }
        for (k, v) in &self.defines {
            h.str("define").str(k).str(v);
        }
        for (k, v) in &self.params {
            h.str("param").str(k).str(v);
        }
        for p in &self.probes {
            h.str("probe").str(&format!("{p:?}"));
        }
        for b in &self.ifaces {
            h.str("iface").str(&format!("{b:?}"));
        }
        h.str("switches").field(&[
            u8::from(self.timing),
            u8::from(self.trace_fst),
            u8::from(self.coverage),
        ]);
        for a in &self.verilator_args {
            h.str("arg").str(a);
        }
        for g in &self.generated {
            h.str("generated").str(&g.file_name).str(&g.contents);
        }
        format!("V{}_{}", self.name, &h.finish()[..8])
    }

    /// The Verilator arguments, without `-Mdir`. `top` is the module to verilate (the
    /// user's top or the emitted wrapper); `generated` are the on-disk paths of the
    /// generated inputs, passed before the sources.
    pub(crate) fn verilator_args(
        &self,
        prefix: &str,
        top: &str,
        generated: &[PathBuf],
    ) -> Vec<String> {
        let mut a: Vec<String> = ["--cc", "--MMD", "--prefix", prefix]
            .map(str::to_owned)
            .into();
        if self.features().trace_fst {
            a.push("--trace-fst".to_owned());
        }
        if self.coverage {
            a.push("--coverage".to_owned());
        }
        let sources = generated.iter().chain(&self.sources).cloned().collect();
        a.extend(
            self.design(top, sources, self.ifaces.is_empty())
                .design_args(),
        );
        a
    }

    /// The design as every Verilator run sees it (the JSON pass, the model build, the
    /// layout check): one [`svtypes::verilator::JsonPassOpts`], so the three spell
    /// `--timing`, the defines, `-I`, `-G` and extra arguments with one builder
    /// ([`JsonPassOpts::design_args`](svtypes::verilator::JsonPassOpts::design_args)).
    ///
    /// `params` says whether the DUT's parameters go on the command line (`-G`). They do
    /// not with interface ports: the wrapper is the top there, so the parameters go into
    /// the wrapper (`IfaceTop::params`), nor for the layout check, whose types are
    /// spelled with elaborated widths.
    pub(crate) fn design(
        &self,
        top: &str,
        sources: Vec<PathBuf>,
        params: bool,
    ) -> svtypes::verilator::JsonPassOpts {
        svtypes::verilator::JsonPassOpts {
            sources,
            top: top.to_owned(),
            timing: self.features().timing,
            defines: self
                .defines
                .iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect(),
            params: if params {
                self.params.clone()
            } else {
                Vec::new()
            },
            include_dirs: self.include_dirs.clone(),
            extra_args: self.verilator_args.clone(),
            ..svtypes::verilator::JsonPassOpts::default()
        }
    }

    /// What [`crate::codegen::lower`] needs besides the IR.
    pub fn codegen_config(&self) -> CodegenConfig {
        CodegenConfig {
            probes: self.probes.clone(),
            iface_top: self.iface_top(),
        }
    }

    /// The wrapper plan's inputs, when the DUT has interface ports.
    pub(crate) fn iface_top(&self) -> Option<IfaceTop> {
        (!self.ifaces.is_empty()).then(|| IfaceTop {
            dut: self.top.clone().unwrap_or_default(),
            params: self.params.clone(),
            bindings: self.ifaces.clone(),
        })
    }

    /// The JSON pass inputs: the same design as the model build sees ([`Self::design`]),
    /// run by the Verilator that builds the model. `generated` (the stored user-generated
    /// SystemVerilog inputs, [`Self::sv_inputs`]) come before the user's sources, as in the
    /// model build; `extra` sources (generated tops) after them.
    pub(crate) fn json_pass_opts(
        &self,
        top: &str,
        generated: &[PathBuf],
        extra: &[PathBuf],
        verilator: &Verilator,
    ) -> svtypes::verilator::JsonPassOpts {
        let sources = generated
            .iter()
            .chain(&self.sources)
            .chain(extra)
            .cloned()
            .collect();
        svtypes::verilator::JsonPassOpts {
            exe: verilator.exe.clone(),
            env: verilator.child_env(),
            ..self.design(top, sources, self.ifaces.is_empty())
        }
    }

    /// The user-generated inputs ([`Dut::verilator_input`]) that are design sources, not
    /// control files: every Verilator run needs them, while a `.vlt` only shapes the model.
    pub(crate) fn sv_inputs(&self) -> impl Iterator<Item = &GeneratedInput> {
        self.generated.iter().filter(|g| {
            !Path::new(&g.file_name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vlt"))
        })
    }

    /// Checks the configuration and returns the top module.
    fn validate(&self) -> Result<&str, Error> {
        let fail = |msg: String| {
            Err(Error::Config {
                dut: self.name.clone(),
                msg,
            })
        };
        let is_ident = !self.name.is_empty()
            && !self.name.starts_with(|c: char| c.is_ascii_digit())
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !is_ident {
            return fail("the name must be a C identifier (letters, digits, `_`)".to_owned());
        }
        if self.sources.is_empty() {
            return fail("no sources; add them with `.sources([..])`".to_owned());
        }
        let args = &self.verilator_args;
        for (i, arg) in args.iter().enumerate() {
            if let Some(why) = refused_arg(arg, args.get(i + 1).map(String::as_str)) {
                return fail(format!("Verilator argument `{arg}`: {why}"));
            }
        }
        if let Some((file, word)) = crate::optfile::find(args, is_param_override) {
            return fail(format!(
                "Verilator option file `{}` has `{word}`: {PARAM_HINT}",
                file.display()
            ));
        }
        if self
            .sources
            .iter()
            .chain(&self.include_dirs)
            .any(|p| p.as_os_str().is_empty())
        {
            return fail("a source or include path is empty".to_owned());
        }
        match self.top.as_deref() {
            Some(top) if !top.is_empty() => Ok(top),
            _ => fail("no top module; set it with `.top(\"module\")`".to_owned()),
        }
    }
}

/// What a build produced. Emitters and the runtime glue read this.
#[derive(Debug, Clone)]
pub struct Built {
    /// The Verilator `--prefix`; the model class is `<prefix>` in `<prefix>.h`.
    pub prefix: String,
    /// The Verilator output directory, inside the shared cache. Read-only. When an input
    /// changed while Verilator read it, the output is not cached and this is
    /// `OUT_DIR/simjig-unpublished/verilated-<prefix>/obj` (the build warns).
    pub obj_dir: PathBuf,
    /// The ports declared in `<prefix>.h`.
    pub header_ports: Vec<HeaderPort>,
    /// Whether verilation was skipped because the cache had the output.
    pub cache: CacheStatus,
    /// The Verilator that was used.
    pub verilator: Verilator,
    /// The type IR of the user's design, from the JSON pass.
    pub design: Design,
    /// `OUT_DIR/simjig/<name>.rs`, when the emitter generated Rust.
    pub rust_file: Option<PathBuf>,
    /// The `cargo::` lines this build needs, in order. [`Dut::build`] prints them.
    pub directives: Vec<String>,
}

/// Whether the Verilator output came from the cache.
///
/// `#[non_exhaustive]`, like the other public enums that may grow: a `match` outside
/// `simjig-build` needs a wildcard arm.
///
/// ```compile_fail,E0004
/// fn ran(status: simjig_build::CacheStatus) -> bool {
///     match status {
///         simjig_build::CacheStatus::Hit => false,
///         simjig_build::CacheStatus::Miss => true,
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStatus {
    /// Found in the cache; Verilator did not run.
    Hit,
    /// Verilator ran. Its output was added to the cache, unless an input changed while
    /// Verilator read it: then it serves this build only, from
    /// `OUT_DIR/simjig-unpublished/` (see [`Built::obj_dir`]).
    Miss,
}

impl Built {
    /// The generated model header, `<obj_dir>/<prefix>.h`.
    pub fn header_path(&self) -> PathBuf {
        verilator::output_file(&self.obj_dir, &self.prefix, ".h")
    }
}

/// A design under test, configured from `build.rs`.
///
/// ```no_run
/// use simjig_build::{Dut, ProbeAccess, ProbeType};
/// // In build.rs:
/// Dut::new("fifo")
///     .sources(["rtl/fifo.sv"])
///     .top("fifo")
///     .param("DEPTH", 16)
///     .probe("fifo.u_ctl.state", "fifo_ctl", ProbeType::Bits { width: 2 }, ProbeAccess::Read)
///     .build()?;
/// # Ok::<(), simjig_build::Error>(())
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct Dut {
    config: DutConfig,
    recipe: CppRecipe,
    emitter: Emitter,
}

impl Default for Dut {
    fn default() -> Self {
        Self {
            config: DutConfig::default(),
            recipe: CppRecipe::default(),
            emitter: emit::DEFAULT_EMITTER,
        }
    }
}

impl Dut {
    /// Starts a DUT named `name` (a C identifier; it becomes part of the model prefix).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            config: DutConfig {
                name: name.into(),
                ..DutConfig::default()
            },
            ..Self::default()
        }
    }

    /// Adds source files. Relative paths are made absolute against the current directory
    /// (in a build script, the package root) right away, so [`DutConfig::prefix`] is the
    /// same before and during the build.
    pub fn sources(mut self, paths: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        self.config.sources.extend(paths.into_iter().map(absolute));
        self
    }

    /// Adds `` `include `` search directories (made absolute like [`Dut::sources`]).
    pub fn include_dirs(mut self, dirs: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        self.config
            .include_dirs
            .extend(dirs.into_iter().map(absolute));
        self
    }

    /// Defines a preprocessor macro (`-Dkey=value`).
    pub fn define(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.defines.push((key.into(), value.into()));
        self
    }

    /// Overrides a top-level parameter (`-Gname=value`). String parameters must include
    /// their SystemVerilog quotes: `.param("MODE", "\"fast\"")`.
    pub fn param(mut self, name: impl Into<String>, value: impl Display) -> Self {
        self.config.params.push((name.into(), value.to_string()));
        self
    }

    /// Sets the top module.
    pub fn top(mut self, module: impl Into<String>) -> Self {
        self.config.top = Some(module.into());
        self
    }

    /// Exposes an internal signal. `path` starts at the top module
    /// (`counter.u_core.state`); `module` is the module that declares the variable and
    /// `ty` its type, because the type IR has no types of internal variables to look them
    /// up in. The build checks the path, module and access against the IR and the width
    /// against Verilator's scope table. A path enters a generate block by the name
    /// Verilator gives its scope: the label (`top.g_x.u.x`), `genblk<N>` for an unnamed
    /// block (`top.genblk2.u.x`, IEEE 1800-2023 §27.6), or `label[i]` for one iteration
    /// of a generate-for loop (`top.g[0].u.x`). Paths through instance arrays are not
    /// supported.
    pub fn probe(
        mut self,
        path: impl Into<String>,
        module: impl Into<String>,
        ty: ProbeType,
        access: ProbeAccess,
    ) -> Self {
        self.config.probes.push(Probe {
            path: path.into(),
            module: module.into(),
            ty,
            access,
        });
        self
    }

    /// Binds an interface port of the top to the interface the generated wrapper
    /// instantiates for it: `iface` is `interface` or `interface.modport`.
    pub fn bind_iface(self, port: impl Into<String>, iface: &str) -> Self {
        let (interface, modport) = match iface.split_once('.') {
            Some((i, m)) => (i.to_owned(), Some(m.to_owned())),
            None => (iface.to_owned(), None),
        };
        self.bind_iface_with(IfaceBinding {
            port: port.into(),
            interface,
            params: Vec::new(),
            modport,
        })
    }

    /// Binds an interface port with full control (interface parameter overrides).
    pub fn bind_iface_with(mut self, binding: IfaceBinding) -> Self {
        self.config.ifaces.push(binding);
        self
    }

    /// Enables `--timing` (delays, events, `fork`). Off by default: `--no-timing`.
    pub fn timing(mut self, on: bool) -> Self {
        self.config.timing = on;
        self
    }

    /// Enables FST waveform tracing support (`--trace-fst`).
    pub fn trace_fst(mut self, on: bool) -> Self {
        self.config.trace_fst = on;
        self
    }

    /// Enables coverage counters (`--coverage`).
    pub fn coverage(mut self, on: bool) -> Self {
        self.config.coverage = on;
        self
    }

    /// Leaves the DPI-C imports with these C names to C: the generated Rust has no trait
    /// method and no trampoline for them, so a C or C++ definition (for example in
    /// [`CppRecipe::model_sources`]) is the only one and links without a duplicate symbol.
    /// The other imports are still implemented in Rust through `Dut::set_dpi`. Naming a C
    /// name the design does not import fails the build.
    pub fn dpi_in_c(mut self, c_names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config
            .dpi_in_c
            .extend(c_names.into_iter().map(Into::into));
        self
    }

    /// Appends raw Verilator arguments (for example `-Wno-WIDTH`).
    pub fn verilator_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.config
            .verilator_args
            .extend(args.into_iter().map(Into::into));
        self
    }

    /// Adds a generated Verilator input (emitter seam).
    pub fn verilator_input(
        mut self,
        file_name: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        let (file_name, contents) = (file_name.into(), contents.into());
        self.config.generated.push(GeneratedInput {
            file_name,
            contents,
        });
        self
    }

    /// Adds to the C++ compile recipe (the C ABI recipe itself is always applied).
    pub fn recipe(mut self, recipe: CppRecipe) -> Self {
        self.recipe = recipe;
        self
    }

    /// Replaces the code generator (default: [`emit::DEFAULT_EMITTER`]).
    pub fn emitter(mut self, emitter: Emitter) -> Self {
        self.emitter = emitter;
        self
    }

    /// The configuration recorded so far.
    pub fn config(&self) -> &DutConfig {
        &self.config
    }

    /// Builds from a Cargo build script: reads the environment Cargo provides, runs
    /// [`Dut::build_in`], and prints the `cargo::` directives.
    ///
    /// On failure it prints no directive of this DUT, only the error as `cargo::error=`
    /// lines, and returns it.
    #[expect(clippy::print_stdout, reason = "build scripts talk to Cargo on stdout")]
    pub fn build(self) -> Result<Built, Error> {
        let result = BuildEnv::from_cargo().and_then(|env| self.build_in(&env));
        for line in cargo_lines(&result) {
            println!("{line}");
        }
        result
    }

    /// Runs the whole pipeline (see the module docs) in an explicit environment and
    /// returns the directives instead of printing them.
    pub fn build_in(self, env: &BuildEnv) -> Result<Built, Error> {
        let top = self.config.validate()?;
        let verilator = Verilator::locate(env)?;
        let cache = cache::Cache::open(env.cache_dir())?;
        let prefix = self.config.prefix();
        pipeline::claim(env, &cache, &self.config, &prefix)?;
        let (front, ir, placed) = self.front_end(env, &cache, &verilator, top)?;
        let mut generated = ir.wrapper.iter().cloned().collect::<Vec<_>>();
        for g in placed.inputs.iter().chain(&self.config.generated) {
            generated.push(cache.store_input(g)?);
        }
        let args = self.config.verilator_args(&prefix, &ir.top, &generated);
        let name = &self.config.name;
        let model = cache.verilate(
            name,
            &verilator,
            &args,
            &self.config.include_dirs,
            &env.out_dir,
        )?;
        let mut warnings: Vec<String> = env.unshared_cache_warning().into_iter().collect();
        warnings.extend(
            model
                .changed
                .as_ref()
                .map(|p| manifest::changed_warning(name, "verilating", p)),
        );
        let header_ports = check_model(
            env,
            &self.config,
            &prefix,
            &model.obj_dir,
            &placed,
            &self.config.rust_dpi_imports(&ir.design)?,
        )?;
        // After the checks: an IR the model contradicts is not recorded.
        let front_changed = front.record(&cache, &ir, placed.layout_check.as_ref(), &model.deps)?;
        warnings.extend(
            front_changed.map(|p| manifest::changed_warning(name, "the front end ran", &p)),
        );
        let mut directives = compile::compile(&compile::Inputs {
            env,
            config: &self.config,
            cache: &cache,
            verilator: &verilator,
            prefix: &prefix,
            obj_dir: &model.obj_dir,
            recipe: &self.recipe,
            model_sources: &placed.model_sources,
        })?;
        directives.extend(rerun_directives(
            &verilator,
            &model.deps,
            &cache.root().join("inputs"),
            &self.recipe,
        ));
        directives.extend(warnings);
        directives.extend(untested_warning(&verilator));
        Ok(Built {
            prefix,
            obj_dir: model.obj_dir,
            header_ports,
            cache: model.status,
            verilator,
            design: ir.design,
            rust_file: placed.rust,
            directives,
        })
    }
}

/// What [`Dut::build`] prints for `result`: the directives of a build, or the error alone.
///
/// The error is printed as `cargo::error=` lines (one per line of its message), which Cargo
/// shows as `error:` lines before anything else of the failed build script, so it is not
/// buried under thousands of `rerun-if-changed` lines (Cargo dumps the script's whole
/// stdout when the script fails). It is also the last stdout line, after the directives of
/// the DUTs this script built before, and the script's `Error:` on stderr follows.
fn cargo_lines(result: &Result<Built, Error>) -> Vec<String> {
    match result {
        Ok(built) => built.directives.clone(),
        Err(error) => error
            .to_string()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!("cargo::error={l}"))
            .collect(),
    }
}

/// The warning for a Verilator accepted only because of `SIMJIG_UNTESTED_VERILATOR=1`.
fn untested_warning(verilator: &Verilator) -> Option<String> {
    verilator.untested.then(|| {
        format!(
            "cargo::warning=simjig: {} is not a supported Verilator (supported: {}); \
             building anyway because SIMJIG_UNTESTED_VERILATOR=1",
            verilator.version_line,
            svtypes::version::supported_list()
        )
    })
}

/// Cross-checks the DPI-C trampolines' prototypes against `V<prefix>__Dpi.h`, which
/// Verilator writes only for a design with DPI functions.
fn check_dpi_header(imports: &[DpiImport], obj_dir: &Path, prefix: &str) -> Result<(), Error> {
    if imports.is_empty() {
        return Ok(());
    }
    let path = verilator::output_file(obj_dir, prefix, "__Dpi.h");
    let text = std::fs::read_to_string(&path).map_err(Error::io("reading", &path))?;
    crate::codegen::check_dpi_header(imports, &text).map_err(|m| Error::Parse {
        path,
        line: m.line,
        msg: m.message,
    })
}

impl Dut {
    /// The IR (cached or from the JSON pass), the emitted files placed, and the layout
    /// check run unless it passed for this IR before.
    fn front_end(
        &self,
        env: &BuildEnv,
        cache: &cache::Cache,
        verilator: &Verilator,
        top: &str,
    ) -> Result<(Front, pipeline::Ir, emit::Placed), Error> {
        let sv_inputs = self
            .config
            .sv_inputs()
            .map(|g| cache.store_input(g))
            .collect::<Result<Vec<_>, _>>()?;
        let (front, cached) = Front::lookup(cache, &self.config, top, &sv_inputs, verilator)?;
        let ir = match cached {
            Some(ir) => ir,
            None => pipeline::design_ir(&self.config, top, &sv_inputs, verilator, cache)?,
        };
        let emitted = (self.emitter)(&ir.design, &self.config)?;
        let placed = emit::place(emitted, &ir.design, &self.config, &env.out_dir, cache)?;
        if let Some(check) = &placed.layout_check
            && !front.checked(cache, check)
        {
            pipeline::layout_check(&self.config, &sv_inputs, verilator, cache, check)?;
        }
        Ok((front, ir, placed))
    }
}

/// Checks the verilated model against what the emitter placed: the header's ports, each
/// probe against the scope table (`syms`), the DPI-C trampolines' prototypes, and the DPI
/// exports against the other DUTs of this build script. Returns the header's ports.
fn check_model(
    env: &BuildEnv,
    config: &DutConfig,
    prefix: &str,
    obj_dir: &Path,
    placed: &emit::Placed,
    dpi_imports: &[DpiImport],
) -> Result<Vec<HeaderPort>, Error> {
    let header_ports = header::read_header_ports(&verilator::output_file(obj_dir, prefix, ".h"))?;
    header::check_ports(&header_ports, &placed.ports)?;
    crate::syms::check_probes(&config.name, obj_dir, prefix, &placed.probes)?;
    check_dpi_header(dpi_imports, obj_dir, prefix)?;
    let dpi_header = verilator::output_file(obj_dir, prefix, "__Dpi.h");
    script::claim_dpi_exports(env, config, prefix, header::read_dpi_exports(&dpi_header)?)?;
    Ok(header_ports)
}

/// `std::path::absolute` fails only on an empty path, which `validate` then rejects.
fn absolute(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Reruns on every file Verilator read, the recipe's files, the Verilator executable
/// (replaced in place by an upgrade, while `PATH` stays the same) and the tracked
/// variables.
///
/// The `deps` under `generated` (the cache's `inputs/`: the `.vlt`, the wrapper, stored
/// `verilator_input`s) are left out: they derive from the configuration and the sources,
/// which are tracked already, and this run may have just written them, after the build
/// script started, which Cargo would take as a change on the next build.
fn rerun_directives(
    verilator: &Verilator,
    deps: &[PathBuf],
    generated: &Path,
    recipe: &CppRecipe,
) -> Vec<String> {
    let files = std::iter::once(&verilator.exe)
        .chain(deps.iter().filter(|d| !d.starts_with(generated)))
        .chain(&recipe.global_sources)
        .chain(&recipe.model_sources)
        .chain(&recipe.include_dirs);
    let mut out: Vec<String> = files
        .map(|p| format!("cargo::rerun-if-changed={}", p.display()))
        .collect();
    out.extend(
        BuildEnv::TRACKED_VARS
            .iter()
            .map(|v| format!("cargo::rerun-if-env-changed={v}")),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DutConfig {
        Dut::new("fifo")
            .sources(["/r/fifo.sv"])
            .include_dirs(["/r/inc"])
            .define("K", "V")
            .param("DEPTH", 16)
            .top("fifo")
            .config
    }

    #[test]
    fn a_failed_build_prints_only_the_error_as_cargo_errors() {
        // A config error is found before anything is verilated.
        let Err(error) = Dut::new("fifo").top("fifo").config.validate() else {
            panic!("an empty source list is refused")
        };
        let lines = cargo_lines(&Err(error));
        assert_ne!(lines, Vec::<String>::new());
        assert!(
            lines.iter().all(|l| l.starts_with("cargo::error=")),
            "no directive of a failed DUT is printed: {lines:?}"
        );
        assert!(
            lines[0].contains("invalid DUT `fifo` configuration"),
            "{lines:?}"
        );
        // Every non-empty line of a multi-line message is its own `cargo::error`.
        let multi = Error::Config {
            dut: "fifo".into(),
            msg: "first\n\n  add .bind_iface(\"in\", \"req_if.sink\")".into(),
        };
        assert_eq!(
            cargo_lines(&Err(multi)),
            [
                "cargo::error=invalid DUT `fifo` configuration: first",
                "cargo::error=  add .bind_iface(\"in\", \"req_if.sink\")",
            ]
        );
    }

    #[test]
    fn reruns_track_the_resolved_verilator_not_path() {
        let verilator = Verilator {
            exe: PathBuf::from("/opt/v/bin/verilator"),
            version: svtypes::version::VerilatorVersion::new(5, 52),
            version_line: "Verilator 5.052".into(),
            include_dir: PathBuf::from("/opt/v/share/verilator/include"),
            verilator_root: None,
            untested: false,
        };
        let stored = PathBuf::from("/c/inputs/ab/V.vlt");
        let reruns = rerun_directives(
            &verilator,
            &[stored.clone(), PathBuf::from("/r/a.sv")],
            Path::new("/c/inputs"),
            &CppRecipe::default(),
        );
        assert!(reruns.contains(&"cargo::rerun-if-changed=/r/a.sv".to_owned()));
        assert!(
            !reruns.contains(&format!("cargo::rerun-if-changed={}", stored.display())),
            "stored generated inputs are not reruns: {reruns:?}"
        );
        assert!(reruns.contains(&"cargo::rerun-if-changed=/opt/v/bin/verilator".to_owned()));
        assert!(reruns.contains(&"cargo::rerun-if-env-changed=SIMJIG_VERILATOR".to_owned()));
        assert!(
            !reruns.contains(&"cargo::rerun-if-env-changed=PATH".to_owned()),
            "{reruns:?}"
        );
    }

    #[test]
    fn verilator_args_follow_the_config() {
        let c = Dut {
            config: cfg(),
            ..Dut::default()
        }
        .trace_fst(true)
        .verilator_args(["-Wno-WIDTH"])
        .config;
        let args = c.verilator_args("Vfifo_1234abcd", "fifo", &[PathBuf::from("/c/p.vlt")]);
        assert_eq!(
            args.join(" "),
            "--cc --MMD --prefix Vfifo_1234abcd --trace-fst --no-timing --top-module fifo \
             -I/r/inc -DK=V -GDEPTH=16 /c/p.vlt /r/fifo.sv -Wno-WIDTH"
        );
        let timing = DutConfig {
            timing: true,
            ..cfg()
        };
        assert!(
            timing
                .verilator_args("V", "fifo", &[])
                .contains(&"--timing".to_owned())
        );
        assert!(
            !timing
                .verilator_args("V", "fifo", &[])
                .contains(&"--no-timing".to_owned())
        );
    }

    /// One config per setting, each differing from `cfg()` in that setting only.
    fn variants() -> Vec<DutConfig> {
        vec![
            DutConfig {
                timing: true,
                ..cfg()
            },
            DutConfig {
                trace_fst: true,
                ..cfg()
            },
            DutConfig {
                coverage: true,
                ..cfg()
            },
            DutConfig { top: None, ..cfg() },
            DutConfig {
                params: vec![("DEPTH".into(), "32".into())],
                ..cfg()
            },
            DutConfig {
                defines: vec![],
                ..cfg()
            },
            DutConfig {
                include_dirs: vec![],
                ..cfg()
            },
            DutConfig {
                verilator_args: vec!["-O0".into()],
                ..cfg()
            },
            DutConfig {
                probes: vec![Probe {
                    path: "fifo.u.s".into(),
                    module: "u".into(),
                    ty: ProbeType::Bits { width: 4 },
                    access: ProbeAccess::Read,
                }],
                ..cfg()
            },
            DutConfig {
                probes: vec![Probe {
                    path: "fifo.u.s".into(),
                    module: "u".into(),
                    ty: ProbeType::Bits { width: 4 },
                    access: ProbeAccess::ReadWrite,
                }],
                ..cfg()
            },
            DutConfig {
                generated: vec![GeneratedInput {
                    file_name: "a.vlt".into(),
                    contents: "x".into(),
                }],
                ..cfg()
            },
            DutConfig {
                ifaces: vec![IfaceBinding {
                    port: "s".into(),
                    interface: "i".into(),
                    params: vec![],
                    modport: None,
                }],
                ..cfg()
            },
        ]
    }

    #[test]
    fn prefix_is_stable_and_tracks_every_setting() {
        let base = cfg().prefix();
        assert!(
            base.starts_with("Vfifo_") && base.len() == "Vfifo_".len() + 8,
            "{base}"
        );
        assert_eq!(base, cfg().prefix(), "deterministic");
        let mut seen = vec![base.clone()];
        for v in variants() {
            let p = v.prefix();
            assert!(!seen.contains(&p), "{v:?} collides");
            seen.push(p);
        }
    }

    /// Each list is tagged, so moving an entry from one list to another (a define to a
    /// parameter, a source to an include directory) changes the prefix.
    #[test]
    fn prefix_fields_do_not_run_into_each_other() {
        let empty = DutConfig {
            sources: vec![],
            include_dirs: vec![],
            defines: vec![],
            params: vec![],
            ..cfg()
        };
        let define = DutConfig {
            defines: vec![("W".into(), "8".into())],
            ..empty.clone()
        };
        let param = DutConfig {
            params: vec![("W".into(), "8".into())],
            ..empty.clone()
        };
        assert_ne!(define.prefix(), param.prefix());
        let source = DutConfig {
            sources: vec!["/r/a".into()],
            ..empty.clone()
        };
        let include = DutConfig {
            include_dirs: vec!["/r/a".into()],
            ..empty.clone()
        };
        assert_ne!(source.prefix(), include.prefix());
        let arg = DutConfig {
            verilator_args: vec!["x".into()],
            ..empty.clone()
        };
        let generated = DutConfig {
            generated: vec![GeneratedInput {
                file_name: "x".into(),
                contents: String::new(),
            }],
            ..empty
        };
        assert_ne!(arg.prefix(), generated.prefix());
    }

    #[test]
    fn builder_records_probes_and_bindings() {
        let d = Dut::new("top")
            .probe(
                "top.u_ctl.state",
                "ctl",
                ProbeType::Named("pkg::state_t".into()),
                ProbeAccess::Read,
            )
            .bind_iface("s_axis", "axis_if.sink")
            .bind_iface("m_axis", "axis_if");
        assert_eq!(
            d.config().probes,
            [Probe {
                path: "top.u_ctl.state".into(),
                module: "ctl".into(),
                ty: ProbeType::Named("pkg::state_t".into()),
                access: ProbeAccess::Read,
            }]
        );
        let modports: Vec<_> = d
            .config()
            .ifaces
            .iter()
            .map(|b| (&b.interface, &b.modport))
            .collect();
        assert_eq!(
            modports,
            [
                (&"axis_if".to_owned(), &Some("sink".to_owned())),
                (&"axis_if".to_owned(), &None)
            ]
        );
    }

    fn with_args(args: &[&str]) -> DutConfig {
        DutConfig {
            verilator_args: args.iter().map(|a| (*a).to_owned()).collect(),
            ..cfg()
        }
    }

    /// Coverage follows Verilator's own rule (`V3Options`, 5.052): `--coverage` switches every
    /// kind, `--coverage-<kind>` one, the last word wins, and coverage is on while any kind
    /// is. Options that only tune coverage do not turn it on.
    #[test]
    fn coverage_is_on_exactly_when_verilator_turns_a_kind_on() {
        let on = |args: &[&str]| with_args(args).features().coverage;
        assert!(on(&["--coverage-line"]));
        assert!(on(&["-coverage-toggle"]));
        assert!(on(&["--coverage"]));
        assert!(on(&["--no-coverage-line", "--coverage-user"]));
        assert!(!on(&[]));
        assert!(!on(&["--coverage-max-width", "8"]));
        assert!(!on(&["--coverage-underscore"]));
        assert!(!on(&["--coverage", "--no-coverage"]));
        assert!(!on(&["--coverage-line", "--no-coverage-line"]));
        assert!(on(&[
            "--coverage",
            "--no-coverage-line",
            "--no-coverage-toggle",
            "--no-coverage-user",
            "--no-coverage-expr"
        ]));
        let builder = DutConfig {
            coverage: true,
            ..with_args(&["--no-coverage"])
        };
        assert!(
            !builder.features().coverage,
            "the extra arguments come last"
        );
        assert!(
            DutConfig {
                coverage: true,
                ..cfg()
            }
            .features()
            .coverage
        );
    }

    #[test]
    fn raw_flags_that_have_a_builder_switch_are_rejected() {
        let msg = |args: &[&str]| with_args(args).validate().unwrap_err().to_string();
        for (args, hint) in [
            (&["-GDEPTH=4"][..], "`.param(name, value)`"),
            (&["-pvalue+DEPTH=4"], "`.param(name, value)`"),
            (&["--timing"], "`.timing(true)`"),
            (&["-no-timing"], "`.timing(true)`"),
            (&["--trace-fst"], "`.trace_fst(true)`"),
            (&["--trace"], "`.trace_fst(true)`"),
            (&["--trace-vcd"], "`.trace_fst(true)`"),
            (&["--trace-threads", "2"], "`.trace_fst(true)`"),
            (&["--threads", "4"], "one thread"),
            (&["--top-module", "x"], "`.top(\"module\")`"),
            (&["--top", "x"], "`.top(\"module\")`"),
            (&["--prefix", "Vx"], "`Dut::new(name)`"),
            (&["-Mdir", "obj"], "`SIMJIG_CACHE_DIR`"),
            (&["--Mdir", "obj"], "`SIMJIG_CACHE_DIR`"),
            (&["--cc"], "`.recipe(CppRecipe"),
            (&["--exe"], "`.recipe(CppRecipe"),
            (&["--build"], "`.recipe(CppRecipe"),
            (&["--main"], "`.recipe(CppRecipe"),
            (&["--binary"], "`.recipe(CppRecipe"),
            (&["--lib-create", "x"], "`.recipe(CppRecipe"),
            (&["-o", "x"], "`.recipe(CppRecipe"),
            (&["--sc"], "C++ (`--cc`)"),
            (&["--hierarchical"], "one model per DUT"),
            (&["--prefix=Vx"], "`Dut::new(name)`"),
            (&["--Mdir=obj"], "`SIMJIG_CACHE_DIR`"),
            (&["--top=x"], "`.top(\"module\")`"),
            (&["--top-module=x"], "`.top(\"module\")`"),
        ] {
            let m = msg(args);
            assert!(m.contains(hint) && m.contains(args[0]), "{args:?}: {m}");
            assert!(!m.contains("  "), "one space between words: {m}");
        }
        for ok in [
            &["--threads", "1"][..],
            &["--trace-depth", "3"],
            &["--trace-structs"],
            &["--coverage-line"],
            &["-Wno-WIDTH"],
            &["-O3"],
            &["--output-split", "1"],
        ] {
            assert!(with_args(ok).validate().is_ok(), "{ok:?}");
        }
    }

    /// A parameter override inside an option file reaches every Verilator run like one
    /// among the arguments, so it is refused the same way.
    #[test]
    fn parameter_overrides_in_option_files_are_rejected() {
        let t = tempfile::tempdir().unwrap();
        let opts = t.path().join("opts.f");
        let path = opts.display().to_string();
        for (text, word) in [
            ("-Wall\n-GDEPTH=4\n", "-GDEPTH=4"),
            ("-pvalue+W=2", "-pvalue+W=2"),
        ] {
            std::fs::write(&opts, text).unwrap();
            let m = with_args(&["-f", &path])
                .validate()
                .unwrap_err()
                .to_string();
            assert!(
                m.contains(&format!("option file `{path}` has `{word}`"))
                    && m.contains("`.param(name, value)`"),
                "{m}"
            );
        }
        std::fs::write(&opts, "// -GDEPTH=4\n-Wall\n").unwrap();
        assert!(with_args(&["-f", &path]).validate().is_ok());
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let msg = |c: DutConfig| c.validate().unwrap_err().to_string();
        assert_eq!(
            msg(DutConfig {
                name: "my-fifo".into(),
                ..cfg()
            }),
            "invalid DUT `my-fifo` configuration: the name must be a C identifier (letters, digits, `_`)"
        );
        assert_eq!(
            msg(DutConfig {
                sources: vec![],
                ..cfg()
            }),
            "invalid DUT `fifo` configuration: no sources; add them with `.sources([..])`"
        );
        assert_eq!(
            msg(DutConfig { top: None, ..cfg() }),
            "invalid DUT `fifo` configuration: no top module; set it with `.top(\"module\")`"
        );
    }
}

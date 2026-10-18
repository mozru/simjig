//! The `build.rs`-facing description of one design under test, and the build pipeline.
//!
//! [`Dut`] records configuration only. [`Dut::build_in`] then runs the pipeline in a
//! fixed order:
//! 1. locate and version-check Verilator;
//! 2. claim the prefix (see `pipeline::claim`): two configurations must never share one;
//! 3. the JSON pass (`svtypes::verilator::json_pass`) with the same sources, `-I`, `-D`,
//!    `-G` and extra arguments, always `--no-timing`, giving the type IR. A DUT with
//!    interface ports takes two passes through generated tops (`pipeline::design_ir`);
//! 4. the [`Emitter`] turns the IR into the Rust bindings, the C++ shim, a `.vlt` file
//!    and a layout check (see [`crate::emit`] for where each goes), and the layout check
//!    is linted;
//! 5. verilate (`--cc`) through the content-hash cache, with the `.vlt` and wrapper;
//! 6. cross-check the header's ports against the IR's (or the emitter's);
//! 7. compile and link the C++ (see `compile`), and collect the `cargo::` directives.
//!
//! The IR comes before verilation because the emitters' outputs are Verilator inputs.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use svtypes::ir::Design;

use crate::codegen::{CodegenConfig, IfaceBinding, IfaceTop, Probe, ProbeAccess, ProbeType};
use crate::compile::{self, CppRecipe};
use crate::emit::{self, Emitter};
use crate::hash::KeyHasher;
use crate::header::{self, HeaderPort};
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error, cache, pipeline};

/// A file generated before verilation (a `.vlt` control file or a wrapper module). It is
/// stored content-addressed in the shared cache, so its path is the same in every crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GeneratedInput {
    /// File name, with the extension Verilator uses to pick its role (`.vlt`, `.sv`).
    pub file_name: String,
    /// Full contents.
    pub contents: String,
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
}

impl DutConfig {
    /// The Verilator `--prefix`: `V<name>_<hash8>`, unique per configuration so several
    /// variants of one design can link into one binary.
    ///
    /// It covers every setting that changes the model's code (sources by path, flags,
    /// params, probes, bindings, user-supplied generated inputs) but not file contents
    /// or emitter outputs, so emitters can compute it before generating anything.
    pub fn prefix(&self) -> String {
        let mut h = KeyHasher::new("simjig-prefix-v1");
        h.str(&self.name);
        h.str(self.top.as_deref().unwrap_or(""));
        for s in &self.sources {
            h.path(s);
        }
        for d in &self.include_dirs {
            h.path(d);
        }
        for (k, v) in self.defines.iter().chain(&self.params) {
            h.str(k).str(v);
        }
        for p in &self.probes {
            h.str(&format!("{p:?}"));
        }
        for b in &self.ifaces {
            h.str(&format!("{b:?}"));
        }
        h.field(&[
            u8::from(self.timing),
            u8::from(self.trace_fst),
            u8::from(self.coverage),
        ]);
        for a in &self.verilator_args {
            h.str(a);
        }
        for g in &self.generated {
            h.str(&g.file_name).str(&g.contents);
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
        a.push(
            if self.timing {
                "--timing"
            } else {
                "--no-timing"
            }
            .to_owned(),
        );
        if self.trace_fst {
            a.push("--trace-fst".to_owned());
        }
        if self.coverage {
            a.push("--coverage".to_owned());
        }
        a.extend(["--top-module".to_owned(), top.to_owned()]);
        a.extend(
            self.include_dirs
                .iter()
                .map(|d| format!("-I{}", d.display())),
        );
        a.extend(self.defines.iter().map(|(k, v)| format!("-D{k}={v}")));
        // With interface ports the wrapper is the top, so `-G` would reach the wrapper:
        // the DUT's parameters go into the wrapper instead (`IfaceTop::params`).
        if self.ifaces.is_empty() {
            a.extend(self.params.iter().map(|(k, v)| format!("-G{k}={v}")));
        }
        a.extend(
            generated
                .iter()
                .chain(&self.sources)
                .map(|p| p.display().to_string()),
        );
        a.extend(self.verilator_args.iter().cloned());
        a
    }

    /// What [`crate::codegen::lower`] needs besides the IR.
    pub fn codegen_config(&self) -> CodegenConfig {
        CodegenConfig {
            probes: self.probes.clone(),
            iface_top: self.iface_top(),
            verilator_args: self.verilator_args.clone(),
            control_files: self
                .generated
                .iter()
                .filter(|g| {
                    Path::new(&g.file_name)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("vlt"))
                })
                .map(|g| g.contents.clone())
                .collect(),
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

    /// The JSON pass inputs: the same design as the model build sees. `extra` sources
    /// (generated tops) come after the user's; `-G` is passed only without a wrapper.
    pub(crate) fn json_pass_opts(
        &self,
        top: &str,
        extra: &[PathBuf],
    ) -> svtypes::verilator::JsonPassOpts {
        let params = if self.ifaces.is_empty() {
            self.params.clone()
        } else {
            Vec::new()
        };
        svtypes::verilator::JsonPassOpts {
            sources: self.sources.iter().chain(extra).cloned().collect(),
            top: top.to_owned(),
            defines: self
                .defines
                .iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect(),
            params,
            include_dirs: self.include_dirs.clone(),
            extra_args: self.verilator_args.clone(),
        }
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
    /// The Verilator output directory, inside the shared cache. Read-only.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Found in the cache; Verilator did not run.
    Hit,
    /// Verilator ran and the output was added to the cache.
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
    /// `ty` its type, because the type IR has no internal variables to look them up in.
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
    #[expect(clippy::print_stdout, reason = "build scripts talk to Cargo on stdout")]
    pub fn build(self) -> Result<Built, Error> {
        let built = self.build_in(&BuildEnv::from_cargo()?)?;
        for line in &built.directives {
            println!("{line}");
        }
        Ok(built)
    }

    /// Runs the whole pipeline (see the module docs) in an explicit environment and
    /// returns the directives instead of printing them.
    pub fn build_in(self, env: &BuildEnv) -> Result<Built, Error> {
        let top = self.config.validate()?;
        let verilator = Verilator::locate(env)?;
        let cache = cache::Cache::open(env.cache_dir())?;
        let prefix = self.config.prefix();
        pipeline::claim(env, &cache, &self.config, &prefix)?;
        let ir = pipeline::design_ir(&self.config, top, &verilator, &cache)?;
        let emitted = (self.emitter)(&ir.design, &self.config)?;
        let placed = emit::place(emitted, &ir.design, &self.config, &env.out_dir)?;
        if let Some(check) = &placed.layout_check {
            pipeline::layout_check(&self.config, &verilator, &cache, check)?;
        }
        let mut generated = ir.wrapper.into_iter().collect::<Vec<_>>();
        for g in placed.inputs.iter().chain(&self.config.generated) {
            generated.push(cache.store_input(g)?);
        }
        let args = self.config.verilator_args(&prefix, &ir.top, &generated);
        let model = cache.verilate(&verilator, &args, &self.config.include_dirs)?;
        let header_ports =
            header::read_header_ports(&verilator::output_file(&model.obj_dir, &prefix, ".h"))?;
        header::check_ports(&header_ports, &placed.ports)?;
        let mut directives = compile::compile(&compile::Inputs {
            env,
            cache: &cache,
            verilator: &verilator,
            prefix: &prefix,
            obj_dir: &model.obj_dir,
            recipe: &self.recipe,
            model_sources: &placed.model_sources,
        })?;
        directives.extend(rerun_directives(&model.deps, &self.recipe));
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

/// `std::path::absolute` fails only on an empty path, which `validate` then rejects.
fn absolute(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn rerun_directives(deps: &[PathBuf], recipe: &CppRecipe) -> Vec<String> {
    let files = deps
        .iter()
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
            "--cc --MMD --prefix Vfifo_1234abcd --no-timing --trace-fst --top-module fifo \
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

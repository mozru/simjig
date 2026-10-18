//! The `build.rs`-facing description of one design under test, and the build pipeline.
//!
//! [`Dut`] records configuration only. [`Dut::build`] then runs the pipeline in a fixed
//! order: locate Verilator, verilate through the content-hash cache, cross-check the
//! header ports, compile the C++ and emit Cargo directives.
//!
//! Seams for the emitters (which are not written yet): they read [`DutConfig`] (probes,
//! interface bindings, the model prefix) and plug results back in through three inputs,
//! all of which are hashed into the cache keys:
//! - [`Dut::verilator_input`]: generated Verilator inputs (`.vlt`, wrapper SV);
//! - [`Dut::recipe`]: additions to the C ABI [`CppRecipe`] (shim, defines);
//! - [`Dut::expect_ports`]: the ports the IR expects, checked against the header.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use crate::compile::{self, CppRecipe};
use crate::hash::KeyHasher;
use crate::header::{self, HeaderPort};
use crate::verilator::{self, Verilator};
use crate::{BuildEnv, Error, cache};

/// How a probed internal signal may be accessed from Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    /// Read only (`public_flat_rd`).
    Rd,
    /// Read and write (`public_flat_rw`).
    Rw,
}

/// An internal signal the testbench wants to see, e.g. `u_ctl.state`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Probe {
    /// Hierarchical path below the top module.
    pub path: String,
    /// Requested access.
    pub access: Access,
}

/// Binds an interface-typed top-level port to the interface that the generated wrapper
/// instantiates for it (DECISION.md §2 #9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IfaceBinding {
    /// The top-level port name, e.g. `s_axis`.
    pub port: String,
    /// The interface (or `interface.modport`) to instantiate, e.g. `axis_if`.
    pub iface: String,
}

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
    /// Top module; Verilator picks it when `None`.
    pub top: Option<String>,
    /// Internal signals to expose.
    pub probes: Vec<Probe>,
    /// Interface-port bindings.
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
    /// params, probes, bindings) but not file contents or generated inputs, so emitters
    /// can compute it before generating anything.
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
            h.str(&p.path).str(&format!("{:?}", p.access));
        }
        for b in &self.ifaces {
            h.str(&b.port).str(&b.iface);
        }
        h.field(&[
            u8::from(self.timing),
            u8::from(self.trace_fst),
            u8::from(self.coverage),
        ]);
        for a in &self.verilator_args {
            h.str(a);
        }
        format!("V{}_{}", self.name, &h.finish()[..8])
    }

    /// The Verilator arguments, without `-Mdir`. `generated` are the on-disk paths of
    /// [`DutConfig::generated`].
    pub(crate) fn verilator_args(&self, prefix: &str, generated: &[PathBuf]) -> Vec<String> {
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
        if let Some(top) = &self.top {
            a.extend(["--top-module".to_owned(), top.clone()]);
        }
        a.extend(
            self.include_dirs
                .iter()
                .map(|d| format!("-I{}", d.display())),
        );
        a.extend(self.defines.iter().map(|(k, v)| format!("-D{k}={v}")));
        a.extend(self.params.iter().map(|(k, v)| format!("-G{k}={v}")));
        a.extend(
            generated
                .iter()
                .chain(&self.sources)
                .map(|p| p.display().to_string()),
        );
        a.extend(self.verilator_args.iter().cloned());
        a
    }

    fn validate(&self) -> Result<(), Error> {
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
        Ok(())
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
/// use simjig_build::{Access, Dut};
/// // In build.rs:
/// Dut::new("fifo")
///     .sources(["rtl/fifo.sv"])
///     .param("DEPTH", 16)
///     .probe("u_ctl.state", Access::Rd)
///     .build()?;
/// # Ok::<(), simjig_build::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct Dut {
    config: DutConfig,
    recipe: CppRecipe,
    expected_ports: Option<Vec<HeaderPort>>,
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

    /// Exposes an internal signal (a hierarchical path below the top).
    pub fn probe(mut self, path: impl Into<String>, access: Access) -> Self {
        let path = path.into();
        self.config.probes.push(Probe { path, access });
        self
    }

    /// Binds an interface-typed top-level port to the interface to instantiate for it.
    pub fn bind_iface(mut self, port: impl Into<String>, iface: impl Into<String>) -> Self {
        let (port, iface) = (port.into(), iface.into());
        self.config.ifaces.push(IfaceBinding { port, iface });
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

    /// Requires the header's ports to equal `ports` (seam for the IR cross-check).
    pub fn expect_ports(mut self, ports: Vec<HeaderPort>) -> Self {
        self.expected_ports = Some(ports);
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

    /// Runs the whole pipeline in an explicit environment and returns the directives
    /// instead of printing them.
    pub fn build_in(self, env: &BuildEnv) -> Result<Built, Error> {
        self.config.validate()?;
        let verilator = Verilator::locate(env)?;
        let cache = cache::Cache::open(env.cache_dir())?;
        let prefix = self.config.prefix();
        let generated = self
            .config
            .generated
            .iter()
            .map(|g| cache.store_input(g))
            .collect::<Result<Vec<_>, _>>()?;
        let args = self.config.verilator_args(&prefix, &generated);
        let model = cache.verilate(&verilator, &args, &self.config.include_dirs)?;
        let header_ports =
            header::read_header_ports(&verilator::output_file(&model.obj_dir, &prefix, ".h"))?;
        if let Some(expected) = &self.expected_ports {
            header::check_ports(&header_ports, expected)?;
        }
        let mut directives = compile::compile(&compile::Inputs {
            env,
            cache: &cache,
            verilator: &verilator,
            prefix: &prefix,
            obj_dir: &model.obj_dir,
            recipe: &self.recipe,
            model_sources: &[],
        })?;
        directives.extend(rerun_directives(&model.deps, &self.recipe));
        Ok(Built {
            prefix,
            obj_dir: model.obj_dir,
            header_ports,
            cache: model.status,
            verilator,
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
        let args = c.verilator_args("Vfifo_1234abcd", &[PathBuf::from("/c/p.vlt")]);
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
                .verilator_args("V", &[])
                .contains(&"--timing".to_owned())
        );
        assert!(
            !timing
                .verilator_args("V", &[])
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
                    path: "u.s".into(),
                    access: Access::Rd,
                }],
                ..cfg()
            },
            DutConfig {
                probes: vec![Probe {
                    path: "u.s".into(),
                    access: Access::Rw,
                }],
                ..cfg()
            },
            DutConfig {
                ifaces: vec![IfaceBinding {
                    port: "s".into(),
                    iface: "i".into(),
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
        // Generated inputs are derived from the config, so they must not change it.
        let generated = DutConfig {
            generated: vec![GeneratedInput {
                file_name: "a.vlt".into(),
                contents: "x".into(),
            }],
            ..cfg()
        };
        assert_eq!(generated.prefix(), base);
    }

    #[test]
    fn builder_records_probes_and_bindings() {
        let d = Dut::new("top")
            .probe("u_ctl.state", Access::Rd)
            .bind_iface("s_axis", "axis_if");
        assert_eq!(
            d.config().probes,
            [Probe {
                path: "u_ctl.state".into(),
                access: Access::Rd
            }]
        );
        assert_eq!(
            d.config().ifaces,
            [IfaceBinding {
                port: "s_axis".into(),
                iface: "axis_if".into()
            }]
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
    }
}

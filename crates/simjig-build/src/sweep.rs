//! Parameter sweeps: several [`Dut`]s of one DUT module, built as one Rust module.
//!
//! Each member is an ordinary DUT: its own model, prefix, cache entries, `rerun-if-changed`
//! lines and `cfg(simjig_dut_<name>)`. What the sweep adds is only Rust: after every
//! member built, each one is lowered again from the IR its build used (the lowering is a
//! pure function of the IR and the configuration, so this is the lowering its model was
//! checked against), [`codegen::lower_sweep`] decides what they share, and the sweep
//! rewrites each member's `OUT_DIR/simjig/<member>.rs` (importing the shared types) and
//! writes `OUT_DIR/simjig/<sweep>.rs`, which includes the members. `include_dut!(<sweep>)`
//! then brings in the whole sweep, and `<sweep>::Ports` is implemented by every member's
//! `Dut`.
//!
//! Invariants:
//! - the sweep's name is claimed in the build script's registry before any member builds
//!   (`script::claim_sweep`): it names a file in `OUT_DIR/simjig/`, so no DUT may have it,
//!   and a DUT belongs to at most one sweep (its file is rewritten for that sweep);
//! - a member with a custom [`Emitter`](crate::Emitter) is refused: the sweep regenerates
//!   the member's Rust with the code generator, which must be what built its model;
//! - a failed sweep (a member failed, or the sweep itself) replaces the sweep's file with
//!   a `compile_error!` stub, as a failed DUT's is (`emit::stub_failed_sweep`).

use std::path::PathBuf;

use crate::dut::DutConfig;
use crate::{BuildEnv, Built, Dut, Error, codegen, emit, script};

/// Builds several configurations of one DUT module and generates one Rust module for them,
/// so a test body is written once over the sweep's `Ports` trait.
///
/// ```no_run
/// // build.rs
/// use simjig_build::{Dut, Sweep};
///
/// let fifo = |name: &str, width: u32, depth: u32| {
///     Dut::new(name)
///         .sources(["rtl/sync_fifo.sv"])
///         .top("sync_fifo")
///         .param("WIDTH", width)
///         .param("DEPTH", depth)
/// };
/// Sweep::new("sync_fifo")
///     .dut(fifo("fifo_8x4", 8, 4))
///     .dut(fifo("fifo_32x16", 32, 16))
///     .build()?;
/// # Ok::<(), simjig_build::Error>(())
/// ```
///
/// ```ignore
/// // a test
/// simjig::include_dut!(sync_fifo); // sync_fifo::{Ports, fifo_8x4, fifo_32x16, ..}
///
/// #[simjig::test(duts = [sync_fifo::fifo_8x4::Dut, sync_fifo::fifo_32x16::Dut])]
/// async fn resets_empty<D: sync_fifo::Ports>(dut: D) {
///     dut.rst().set(1);
///     // ...
/// }
/// ```
///
/// What the sweep module holds is decided by [`codegen::lower_sweep`]: the generated
/// types equal in every member are defined once (so `fifo_8x4::Item` is
/// `sync_fifo::Item`), and `Ports` has one method per signal, with an associated type
/// (`D::WrData`, bounded by `simjig::Vector` or `simjig::Value`) where the members'
/// types differ.
#[derive(Debug, Clone)]
#[must_use]
pub struct Sweep {
    name: String,
    duts: Vec<Dut>,
}

/// What a sweep build produced.
#[derive(Debug, Clone)]
pub struct BuiltSweep {
    /// Each member's build, in sweep order. Their `rust_file`s now hold the member
    /// modules the sweep module includes.
    pub duts: Vec<Built>,
    /// `OUT_DIR/simjig/<name>.rs`, the sweep module.
    pub rust_file: PathBuf,
    /// The sweep's own `cargo::` lines (its `cfg(simjig_dut_<name>)`), after the members'.
    pub directives: Vec<String>,
}

impl Sweep {
    /// Starts a sweep named `name`: a C identifier, which names its Rust module
    /// (`include_dut!(<name>)`) and its gate `cfg(simjig_dut_<name>)`, and which no DUT of
    /// the build script may have.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            duts: Vec::new(),
        }
    }

    /// Adds a configuration: a DUT of the same module as the others, whose name names
    /// its module inside the sweep's (`<sweep>::<name>::Dut`).
    pub fn dut(mut self, dut: Dut) -> Self {
        self.duts.push(dut);
        self
    }

    /// Builds from a Cargo build script, like [`Dut::build`]: builds every member (each
    /// printing its directives, or on failure its cause as `cargo::warning=` lines and its
    /// `rerun-if-changed` lines), then the sweep module, and prints the sweep's
    /// directives. On success it sets `cfg(simjig_dut_<name>)`; on failure it only
    /// declares it. A failure of the sweep itself warns its cause first, as a member's
    /// does; a member's failure is warned once, by the member.
    ///
    /// # Errors
    /// The first member's error, or the sweep's: its name taken, members of different
    /// modules, a member with a custom emitter, or a name collision in the sweep module.
    /// The sweep module is then a stub whose `compile_error!` names the error.
    #[expect(clippy::print_stdout, reason = "build scripts talk to Cargo on stdout")]
    pub fn build(self) -> Result<BuiltSweep, Error> {
        let env = BuildEnv::from_cargo().inspect_err(crate::dut::warn_cause)?;
        let configs = self.configs();
        let name = self.name.clone();
        let mut duts = Vec::new();
        let mut rest = self.duts.clone().into_iter();
        // Whether the error is a member's, which `Dut::build` has warned already.
        let mut member_failed = false;
        let result = self.check(&env).and_then(|()| {
            for dut in rest.by_ref() {
                duts.push(dut.build().inspect_err(|_| member_failed = true)?);
            }
            finish(&env, &name, &duts, &configs)
        });
        let built = result.inspect_err(|e| {
            if !member_failed {
                crate::dut::warn_cause(e);
            }
            // Members not built after an earlier failure still rerun the script when their
            // inputs change.
            for dut in rest {
                for line in dut.failed_reruns(Some(&env.out_dir)) {
                    println!("{line}");
                }
            }
            for line in emit::cfg_directives(&name, false) {
                println!("{line}");
            }
            fail(&env, &name, e);
        })?;
        for line in &built.directives {
            println!("{line}");
        }
        Ok(built)
    }

    /// Runs the sweep in an explicit environment, building each member with
    /// [`Dut::build_in`], and returns the directives instead of printing them.
    ///
    /// # Errors
    /// As [`Sweep::build`].
    pub fn build_in(self, env: &BuildEnv) -> Result<BuiltSweep, Error> {
        let configs = self.configs();
        let result = self.check(env).and_then(|()| {
            let duts = self
                .duts
                .iter()
                .map(|d| d.clone().build_in(env))
                .collect::<Result<Vec<_>, _>>()?;
            finish(env, &self.name, &duts, &configs)
        });
        result.inspect_err(|e| fail(env, &self.name, e))
    }

    fn configs(&self) -> Vec<DutConfig> {
        self.duts.iter().map(|d| d.config().clone()).collect()
    }

    /// The checks that need no build: the name, the members' emitters, the claim.
    fn check(&self, env: &BuildEnv) -> Result<(), Error> {
        let fail = |msg: String| {
            Err(Error::Config {
                dut: self.name.clone(),
                msg,
            })
        };
        if !crate::dut::is_c_ident(&self.name) {
            return fail("a sweep's name must be a C identifier (letters, digits, `_`)".into());
        }
        if self.duts.is_empty() {
            return fail("a sweep needs at least one DUT; add them with `.dut(..)`".into());
        }
        if let Some(d) = self.duts.iter().find(|d| d.has_custom_emitter()) {
            return fail(format!(
                "DUT `{}` has a custom emitter, but a sweep generates its members' Rust with \
                 the code generator; build it outside the sweep",
                d.config().name
            ));
        }
        let members: Vec<String> = self.duts.iter().map(|d| d.config().name.clone()).collect();
        script::claim_sweep(env, &self.name, &members)
    }
}

/// After every member built: lowers the sweep and writes its files.
fn finish(
    env: &BuildEnv,
    name: &str,
    duts: &[Built],
    configs: &[DutConfig],
) -> Result<BuiltSweep, Error> {
    let mut members = Vec::new();
    for (built, config) in duts.iter().zip(configs) {
        let lowered = codegen::lower(&built.design, &config.codegen_config())
            .map_err(|e| Error::codegen(&config.name, e))?;
        members.push((config.name.clone(), lowered));
    }
    let sweep = codegen::lower_sweep(name, &members).map_err(|e| Error::codegen(name, e))?;
    let mut files = Vec::new();
    for ((built, config), (_, lowered)) in duts.iter().zip(configs).zip(&members) {
        let (opts, dpi) = emit::rust_options(&built.design, config)?;
        let text = codegen::emit_sweep_member(lowered, &opts, &sweep)
            + &codegen::emit_dpi(&dpi, lowered, &opts.prefix)
                .map_err(|e| Error::codegen(&config.name, e))?;
        files.push((config.name.clone(), text));
    }
    let rust_file = emit::place_sweep(&env.out_dir, name, &codegen::emit_sweep(&sweep), &files)?;
    Ok(BuiltSweep {
        duts: duts.to_vec(),
        rust_file,
        directives: emit::cfg_directives(name, true),
    })
}

/// A failed sweep's module becomes a stub, unless a DUT of the name owns the file.
fn fail(env: &BuildEnv, name: &str, error: &Error) {
    if crate::dut::is_c_ident(name) && !script::claimed(&env.out_dir, name) {
        emit::stub_failed_sweep(&env.out_dir, name, error);
    }
}

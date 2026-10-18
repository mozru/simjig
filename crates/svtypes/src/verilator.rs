//! Runs `verilator --json-only` and decodes its output.
//!
//! The pass always adds `--no-timing` (`--json-only` turns `--timing` on otherwise, which
//! changes elaboration and needs coroutine support the model build may not have) and
//! `-Wno-fatal` (lint policy belongs to the model build, which sees the same warnings;
//! the JSON pass only reads types). It runs [`JsonPassOpts::exe`], which defaults to the
//! `verilator` on `PATH`; a build that located its own Verilator passes that one, so the
//! IR and the model come from the same tool.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::{Error, Result};
use crate::frontend;
use crate::ir::Design;

/// Inputs of one JSON pass.
#[derive(Clone, Debug)]
pub struct JsonPassOpts {
    /// The `verilator` executable to run (default: `verilator`, looked up on `PATH`).
    pub exe: PathBuf,
    /// Environment variables set for the Verilator process (for example
    /// `VERILATOR_ROOT`), on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// SystemVerilog source files.
    pub sources: Vec<PathBuf>,
    /// Top module name (`--top-module`).
    pub top: String,
    /// Preprocessor defines (`+define+NAME` or `+define+NAME=VALUE`).
    pub defines: Vec<(String, Option<String>)>,
    /// Top-level parameter overrides (`-GNAME=VALUE`); values are SystemVerilog
    /// expressions, e.g. `8` or `4'hc`.
    pub params: Vec<(String, String)>,
    /// Include directories (`-I`).
    pub include_dirs: Vec<PathBuf>,
    /// Further arguments, passed through before the sources.
    pub extra_args: Vec<String>,
}

impl Default for JsonPassOpts {
    fn default() -> Self {
        Self {
            exe: PathBuf::from("verilator"),
            env: Vec::new(),
            sources: Vec::new(),
            top: String::new(),
            defines: Vec::new(),
            params: Vec::new(),
            include_dirs: Vec::new(),
            extra_args: Vec::new(),
        }
    }
}

/// Where a JSON pass wrote its output.
#[derive(Clone, Debug)]
pub struct JsonFiles {
    /// The `.tree.json` file.
    pub tree: PathBuf,
    /// The `.tree.meta.json` file.
    pub meta: PathBuf,
    /// The `verilator --version` line of the tool that wrote them.
    pub version: String,
}

/// Elaborates the design with Verilator and returns its type IR.
///
/// # Examples
/// ```no_run
/// use svtypes::verilator::{JsonPassOpts, json_pass};
///
/// let design = json_pass(&JsonPassOpts {
///     sources: vec!["rtl/top.sv".into()],
///     top: "top".into(),
///     params: vec![("WIDTH".into(), "16".into())],
///     ..JsonPassOpts::default()
/// })?;
/// for port in &design.top_module().ports {
///     println!("{} is {:?} bits", port.name, design.ty(port.ty).width);
/// }
/// # Ok::<(), svtypes::error::Error>(())
/// ```
///
/// # Errors
/// [`Error::Tool`] when Verilator cannot be started or fails (with its command line and a
/// stderr excerpt), [`Error::Io`] on temp-dir trouble, and the errors of
/// [`frontend::parse`].
pub fn json_pass(opts: &JsonPassOpts) -> Result<Design> {
    let dir = TempDir::new()?;
    let files = write_json_files(opts, dir.path())?;
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|source| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        })
    };
    frontend::parse(&read(&files.tree)?, &read(&files.meta)?, &files.version)
}

/// Runs the JSON pass, writing `design.tree.json` and `design.tree.meta.json` into
/// `out_dir` (which must exist). Used to regenerate fixtures.
///
/// # Errors
/// As [`json_pass`].
pub fn write_json_files(opts: &JsonPassOpts, out_dir: &Path) -> Result<JsonFiles> {
    let version = run(verilator(opts).arg("--version"))?;
    let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    let tree = out_dir.join("design.tree.json");
    let meta = out_dir.join("design.tree.meta.json");
    run(&mut command(opts, out_dir, &tree, &meta))?;
    Ok(JsonFiles {
        tree,
        meta,
        version,
    })
}

/// A command running `opts.exe` in `opts.env`.
fn verilator(opts: &JsonPassOpts) -> Command {
    let mut cmd = Command::new(&opts.exe);
    cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));
    cmd
}

fn command(opts: &JsonPassOpts, out_dir: &Path, tree: &Path, meta: &Path) -> Command {
    let mut cmd = verilator(opts);
    cmd.args([
        "--json-only",
        "--no-timing",
        "-Wno-fatal",
        "--top-module",
        &opts.top,
    ]);
    cmd.arg("-Mdir").arg(out_dir);
    cmd.arg("--json-only-output").arg(tree);
    cmd.arg("--json-only-meta-output").arg(meta);
    for dir in &opts.include_dirs {
        cmd.arg(format!("-I{}", dir.display()));
    }
    for (name, value) in &opts.defines {
        match value {
            Some(v) => cmd.arg(format!("+define+{name}={v}")),
            None => cmd.arg(format!("+define+{name}")),
        };
    }
    for (name, value) in &opts.params {
        cmd.arg(format!("-G{name}={value}"));
    }
    cmd.args(&opts.extra_args);
    cmd.args(&opts.sources);
    cmd
}

/// Runs `cmd`; any failure becomes [`Error::Tool`].
fn run(cmd: &mut Command) -> Result<Output> {
    let line = command_line(cmd);
    let out = cmd.output().map_err(|e| Error::Tool {
        command: line.clone(),
        status: None,
        stderr: format!(
            "could not start `{}`: {e} (is it installed? try `pixi run`)",
            cmd.get_program().to_string_lossy()
        ),
    })?;
    if out.status.success() {
        return Ok(out);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout)
    } else {
        stderr
    };
    Err(Error::Tool {
        command: line,
        status: out.status.code(),
        stderr: excerpt(&text, 40),
    })
}

/// The last `lines` lines of `text`.
fn excerpt(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let skip = all.len().saturating_sub(lines);
    let tail = all[skip..].join("\n");
    if skip > 0 {
        format!("[... {skip} lines omitted ...]\n{tail}")
    } else {
        tail
    }
}

fn command_line(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| {
            let a = a.to_string_lossy();
            if a.contains([' ', '\'', '"']) {
                format!("'{}'", a.replace('\'', r"'\''"))
            } else {
                a.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A directory under the system temp dir, removed on drop.
#[derive(Debug)]
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("svtypes-{}-{n}", std::process::id()));
            // `create_dir` is atomic: failing with AlreadyExists means another process
            // owns that name, so try the next one.
            match fs::create_dir(&path) {
                Ok(()) => return Ok(TempDir(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(Error::Io {
                        context: format!("creating {}", path.display()),
                        source,
                    });
                }
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort: a leftover temp dir is harmless and Drop cannot report.
        fs::remove_dir_all(&self.0).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_keeps_the_tail() {
        let text = (1..=50)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let e = excerpt(&text, 3);
        assert_eq!(e, "[... 47 lines omitted ...]\n48\n49\n50");
        assert_eq!(excerpt("a\nb", 3), "a\nb");
    }

    #[test]
    fn command_has_the_required_flags() {
        let opts = JsonPassOpts {
            exe: "/opt/v/bin/verilator".into(),
            env: vec![("VERILATOR_ROOT".into(), "/opt/v".into())],
            sources: vec!["a.sv".into()],
            top: "t".into(),
            defines: vec![("X".into(), None), ("Y".into(), Some("2".into()))],
            params: vec![("P".into(), "4'hc".into())],
            include_dirs: vec!["inc".into()],
            extra_args: vec!["--x-assign".into(), "0".into()],
        };
        let cmd = command(
            &opts,
            Path::new("o"),
            Path::new("o/t.json"),
            Path::new("o/m.json"),
        );
        let env: Vec<_> = cmd.get_envs().collect();
        assert_eq!(
            env,
            [(
                std::ffi::OsStr::new("VERILATOR_ROOT"),
                Some(std::ffi::OsStr::new("/opt/v"))
            )]
        );
        assert_eq!(
            command_line(&cmd),
            "/opt/v/bin/verilator --json-only --no-timing -Wno-fatal --top-module t -Mdir o \
             --json-only-output o/t.json --json-only-meta-output o/m.json -Iinc +define+X \
             +define+Y=2 '-GP=4'\\''hc' --x-assign 0 a.sv"
        );
    }
}

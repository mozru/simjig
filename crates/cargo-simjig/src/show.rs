//! `cargo simjig show <dut>`: the Rust module that `simjig::include_dut!(<dut>)` includes.
//!
//! simjig-build writes each DUT's bindings to `OUT_DIR/simjig/<dut>.rs` (its `emit.rs`),
//! under a hashed directory of the target dir, and `include_dut!` splices the file in as
//! `mod <dut>`. To
//! learn the generated names (`Status::EMPTY`, `StatusVariant`, `r#in`) without hunting
//! for that directory, this command asks Cargo where the build scripts' `OUT_DIR`s are:
//! it runs `cargo check --tests --message-format=json` (which runs the build scripts, as
//! a test build would) and reads the `out_dir` of each `build-script-executed` message of
//! one package: the one `-p` names, else the one whose directory `cargo locate-project`
//! reports, else (in a virtual workspace's root) every local package (a `path+file`
//! package id). So a dependency's own build script, which may build test models of its
//! own, is left out. Cargo's progress and errors still go to stderr.
//!
//! It prints the file, after a `//` line naming its path, or only the path (`--path`).
//! Several packages of a workspace can have a DUT of one name: each match is printed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::json;

/// What `cargo simjig show` was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowArgs {
    /// The DUT name, as given to `simjig_build::Dut::new` and `include_dut!`.
    pub dut: String,
    /// The Cargo package whose build script builds it.
    pub package: Option<String>,
    /// Print only the path of the generated file.
    pub path_only: bool,
}

/// The usage text of the subcommand.
pub const USAGE: &str = concat!(
    "  show      print the Rust module that `include_dut!(<dut>)` includes\n",
    "            cargo simjig show <dut> [-p <package>] [--path]\n",
    "            <dut>     the name given to `simjig_build::Dut::new`\n",
    "            -p        the package whose build script builds it (default: every package)\n",
    "            --path    print only the path of the generated file\n",
    "            --help    print this help",
);

impl ShowArgs {
    /// Parses the arguments after `show`.
    ///
    /// # Errors
    /// A message for a missing DUT name, a second one, `-p` without a value, or an unknown
    /// flag.
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        let mut parsed = Self {
            dut: String::new(),
            package: None,
            path_only: false,
        };
        let mut iter = args.iter();
        while let Some(&arg) = iter.next() {
            match arg {
                "-p" | "--package" => {
                    let value = iter.next().ok_or("--package needs a value")?;
                    parsed.package = Some((*value).to_owned());
                }
                "--path" => parsed.path_only = true,
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                dut if parsed.dut.is_empty() => dut.clone_into(&mut parsed.dut),
                extra => return Err(format!("one DUT at a time; got a second one, {extra}")),
            }
        }
        if parsed.dut.is_empty() {
            return Err("name the DUT, as `simjig_build::Dut::new` names it".to_owned());
        }
        Ok(parsed)
    }

    /// The `cargo check` that runs the build scripts and reports their `OUT_DIR`s; `cargo`
    /// is the program to run (Cargo sets `CARGO` for its subcommands).
    #[must_use]
    pub fn command(&self, cargo: OsString) -> Command {
        let mut command = Command::new(cargo);
        command.args(["check", "--tests", "--message-format=json"]);
        if let Some(package) = &self.package {
            command.args(["-p", package]);
        }
        command
    }
}

/// Whose build scripts [`out_dirs`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packages<'a> {
    /// The package of this name (`-p`).
    Named(&'a str),
    /// The package in this directory.
    At(&'a Path),
    /// Every package in a local directory (`path+file`).
    Local,
}

/// The `OUT_DIR` of every build script of `packages` in Cargo's JSON messages (`stdout`),
/// in order.
#[must_use]
pub fn out_dirs(stdout: &str, packages: Packages<'_>) -> Vec<PathBuf> {
    stdout
        .lines()
        .filter(|l| json::find_str(l, "reason").as_deref() == Some("build-script-executed"))
        .filter(|l| {
            let id = json::find_str(l, "package_id").unwrap_or_default();
            match packages {
                Packages::Named(name) => package_name(&id) == Some(name),
                Packages::At(dir) => local_dir(&id).is_some_and(|d| Path::new(d) == dir),
                Packages::Local => local_dir(&id).is_some(),
            }
        })
        .filter_map(|l| json::find_str(l, "out_dir"))
        .map(PathBuf::from)
        .collect()
}

/// The directory of a local package's ID (`path+file:///w/q#0.0.0` is `/w/q`).
fn local_dir(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("path+file://")?;
    Some(rest.split_once('#').map_or(rest, |(dir, _)| dir))
}

/// The directory of the package Cargo would pick here (`cargo locate-project`), if any.
fn current_package_dir(cargo: &OsString) -> Option<PathBuf> {
    let out = Command::new(cargo)
        .args(["locate-project", "--message-format", "plain"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let manifest = String::from_utf8(out.stdout).ok()?;
    let dir = Path::new(manifest.trim_end()).parent()?;
    out.status.success().then(|| dir.to_path_buf())
}

/// The package name in a Cargo package ID specification: `…#name@1.0`, or, when the name
/// is the last path segment, `path+file:///w/name#1.0` (Cargo book, "Package ID
/// specifications").
fn package_name(id: &str) -> Option<&str> {
    let (url, fragment) = id.rsplit_once('#')?;
    match fragment.split_once('@') {
        Some((name, _)) => Some(name),
        None => url.rsplit('/').next(),
    }
}

/// The generated files for `dut` under `out_dirs`.
///
/// # Errors
/// When there is none: a message naming the DUTs that are generated, if any.
pub fn find(dut: &str, out_dirs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let found: Vec<PathBuf> = out_dirs
        .iter()
        .map(|d| d.join("simjig").join(format!("{dut}.rs")))
        .filter(|f| f.is_file())
        .collect();
    if !found.is_empty() {
        return Ok(found);
    }
    let mut known: Vec<String> = out_dirs.iter().flat_map(|d| duts_in(d)).collect();
    known.sort();
    known.dedup();
    Err(if known.is_empty() {
        format!(
            "no DUT `{dut}`: no build script here runs simjig-build (`simjig_build::Dut::new` \
             in `build.rs`); run this in the crate that calls `include_dut!`, or pass -p"
        )
    } else {
        format!(
            "no DUT `{dut}`; the DUTs built here are: {}",
            known.join(", ")
        )
    })
}

/// The DUT names that simjig-build wrote under one `OUT_DIR`.
fn duts_in(out_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(out_dir.join("simjig")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "rs"))
        .filter_map(|p| Some(p.file_stem()?.to_str()?.to_owned()))
        .collect()
}

/// Runs `cargo simjig show` with `args`: 0 when the DUT was found, 1 when it was not, and
/// Cargo's exit code when the build failed.
#[must_use]
#[expect(clippy::print_stdout, reason = "the command's output")]
pub fn run(args: &ShowArgs) -> ExitCode {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = match args
        .command(cargo.clone())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("error: cannot run cargo: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !output.status.success() {
        eprintln!("error: the build failed (see above), so the DUT bindings may not exist");
        return output
            .status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let here = current_package_dir(&cargo);
    let dirs = match (&args.package, &here) {
        (Some(name), _) => out_dirs(&stdout, Packages::Named(name)),
        (None, Some(dir)) => match out_dirs(&stdout, Packages::At(dir)) {
            // A virtual workspace's root is no package: every member counts.
            dirs if dirs.is_empty() => out_dirs(&stdout, Packages::Local),
            dirs => dirs,
        },
        (None, None) => out_dirs(&stdout, Packages::Local),
    };
    let files = match find(&args.dut, &dirs) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    for file in files {
        if args.path_only {
            println!("{}", file.display());
            continue;
        }
        match std::fs::read_to_string(&file) {
            Ok(text) => print!("// {}\n{text}", file.display()),
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", file.display());
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse_in_any_order() {
        assert_eq!(
            ShowArgs::parse(&["--path", "fifo", "-p", "quickstart"]),
            Ok(ShowArgs {
                dut: "fifo".into(),
                package: Some("quickstart".into()),
                path_only: true,
            })
        );
        assert!(ShowArgs::parse(&[]).unwrap_err().contains("name the DUT"));
        assert!(
            ShowArgs::parse(&["a", "b"])
                .unwrap_err()
                .contains("one DUT")
        );
        assert!(
            ShowArgs::parse(&["a", "-p"])
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            ShowArgs::parse(&["a", "-x"])
                .unwrap_err()
                .contains("unknown option")
        );
    }

    #[test]
    fn the_command_checks_the_tests_with_json_messages() {
        let args = ShowArgs::parse(&["fifo", "-p", "q"]).unwrap();
        let command = args.command("cargo".into());
        let words: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            words,
            ["check", "--tests", "--message-format=json", "-p", "q"]
        );
    }

    /// Lines as Cargo 1.99 prints them (`cargo check --message-format=json`), shortened.
    #[test]
    fn out_dirs_come_from_the_packages_build_script_messages_only() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/q#0.0.0","#,
            r#""out_dir":"/not/an/artifact"}"#,
            "\n",
            r#"{"reason":"build-script-executed","package_id":"path+file:///w/q#0.0.0","#,
            r#""linked_libs":[],"linked_paths":[],"cfgs":[],"env":[],"#,
            r#""out_dir":"/t/debug/build/q-1a2b/out"}"#,
            "\n",
            r#"{"reason":"build-script-executed","package_id":"path+file:///w/x#tb@0.1.0","#,
            r#""out_dir":"C:\\t\\b \"x\"\\out"}"#,
            "\n",
            r#"{"reason":"build-script-executed","#,
            r#""package_id":"registry+https://github.com/rust-lang/crates.io-index#simjig@0.1.0","#,
            r#""out_dir":"/t/debug/build/simjig-9f/out"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
        );
        let q = PathBuf::from("/t/debug/build/q-1a2b/out");
        let tb = PathBuf::from(r#"C:\t\b "x"\out"#);
        assert_eq!(out_dirs(stdout, Packages::Local), [q.clone(), tb.clone()]);
        assert_eq!(
            out_dirs(stdout, Packages::Named("q")),
            std::slice::from_ref(&q)
        );
        assert_eq!(out_dirs(stdout, Packages::Named("tb")), [tb]);
        assert_eq!(out_dirs(stdout, Packages::At(Path::new("/w/q"))), [q]);
        assert_eq!(
            out_dirs(stdout, Packages::At(Path::new("/w"))),
            [] as [PathBuf; 0]
        );
        assert_eq!(
            out_dirs(stdout, Packages::Named("simjig")),
            [PathBuf::from("/t/debug/build/simjig-9f/out")]
        );
    }

    #[test]
    fn find_returns_the_file_or_names_the_duts_built() {
        let root = std::env::temp_dir().join(format!("simjig-show-{}", std::process::id()));
        let (a, b) = (root.join("a/out"), root.join("b/out"));
        std::fs::create_dir_all(a.join("simjig")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("simjig/fifo.rs"), "pub struct Dut;").unwrap();
        std::fs::write(a.join("simjig/inv.rs"), "").unwrap();
        let dirs = [a.clone(), b];
        assert_eq!(find("fifo", &dirs), Ok(vec![a.join("simjig/fifo.rs")]));
        assert_eq!(
            find("fif", &dirs),
            Err("no DUT `fif`; the DUTs built here are: fifo, inv".into())
        );
        assert!(find("fifo", &[]).unwrap_err().contains("no build script"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

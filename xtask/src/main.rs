//! Repository automation.
//!
//! - `cargo xtask setup` installs every pinned tool (see [`setup`]).
//! - `cargo xtask doctor [--json]` runs the `cargo simjig doctor` checks.
//! - `cargo xtask ci` runs the same gate locally and in GitHub Actions, so a green local
//!   run means a green CI run.
#![forbid(unsafe_code)]

mod setup;

use std::env;
use std::process::{Command, ExitCode};

use cargo_simjig::doctor::{self, Format};

/// One step of the gate: a program, its arguments, and extra environment variables.
struct Step {
    program: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
}

const CI: &[Step] = &[
    Step {
        program: "cargo",
        args: &["fmt", "--all", "--check"],
        env: &[],
    },
    Step {
        program: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        env: &[],
    },
    Step {
        program: "cargo",
        args: &[
            "nextest",
            "run",
            "--workspace",
            "--all-features",
            "--profile",
            "ci",
            "--no-tests=pass",
        ],
        env: &[],
    },
    Step {
        program: "cargo",
        args: &["test", "--workspace", "--doc"],
        env: &[],
    },
    Step {
        program: "cargo",
        args: &["doc", "--workspace", "--no-deps"],
        env: &[("RUSTDOCFLAGS", "-D warnings")],
    },
    Step {
        program: "cargo",
        args: &["deny", "check"],
        env: &[],
    },
    Step {
        program: "typos",
        args: &[],
        env: &[],
    },
    // Called directly: `cargo machete` mistakes its own subcommand name for a path.
    Step {
        program: "cargo-machete",
        args: &[],
        env: &[],
    },
    // The C-level tests of the shared runtime (`pixi run cpp-tests`): tracing, coverage
    // and per-model isolation, which no Rust test reaches.
    Step {
        program: "bash",
        args: &[CPP_TESTS],
        env: &[],
    },
];

/// `crates/simjig-build/cpp/tests/run.sh`, wherever `cargo xtask` is run from.
const CPP_TESTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../crates/simjig-build/cpp/tests/run.sh"
);

/// Runs `program` with `args` and extra `env`, echoing the command line first.
fn run(program: &str, args: &[&str], env: &[(&str, &str)]) -> Result<(), String> {
    let line = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("==> {line}");
    let status = Command::new(program)
        .args(args)
        .envs(env.iter().copied())
        .status()
        .map_err(|e| format!("could not start `{program}`: {e} (is it installed?)"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{line}` failed with {status}"))
    }
}

const USAGE: &str = "usage: cargo xtask <ci | setup | doctor [--json]>";

fn ci() -> Result<(), String> {
    for step in CI {
        run(step.program, step.args, step.env)?;
    }
    eprintln!("==> all checks passed");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["ci"] => ci(),
        ["setup"] => setup::setup(),
        ["doctor"] => return doctor::run(Format::Text),
        ["doctor", "--json"] => return doctor::run(Format::Json),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn ci_runs_the_cpp_tests() {
        assert!(Path::new(super::CPP_TESTS).is_file());
        assert!(
            super::CI
                .iter()
                .any(|s| s.program == "bash" && s.args == [super::CPP_TESTS])
        );
    }

    /// ARCHITECTURE.md: `unsafe` exists only in named modules of `simjig`; every other
    /// crate root (library or binary) forbids it.
    #[test]
    fn every_crate_root_but_simjig_forbids_unsafe() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut roots = vec![root.join("xtask/src/main.rs")];
        for krate in std::fs::read_dir(root.join("crates")).unwrap() {
            let dir = krate.unwrap().path();
            if dir.file_name().is_some_and(|n| n == "simjig") {
                continue;
            }
            roots.extend(
                ["src/lib.rs", "src/main.rs"]
                    .map(|f| dir.join(f))
                    .into_iter()
                    .filter(|p| p.is_file()),
            );
        }
        assert!(roots.len() >= 7, "{roots:?}");
        let missing: Vec<_> = roots
            .iter()
            .filter(|p| {
                !std::fs::read_to_string(p)
                    .unwrap()
                    .contains("#![forbid(unsafe_code)]")
            })
            .collect();
        assert!(
            missing.is_empty(),
            "no #![forbid(unsafe_code)] in {missing:?}"
        );
    }
}

//! Repository automation.
//!
//! `cargo xtask ci` runs the same gate locally and in GitHub Actions, so a green local run
//! means a green CI run.

use std::env;
use std::process::{Command, ExitCode};

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
];

fn run(step: &Step) -> Result<(), String> {
    let line = std::iter::once(step.program)
        .chain(step.args.iter().copied())
        .collect::<Vec<_>>();
    eprintln!("==> {}", line.join(" "));
    let status = Command::new(step.program)
        .args(step.args)
        .envs(step.env.iter().copied())
        .status()
        .map_err(|e| format!("could not start `{}`: {e} (is it installed?)", step.program))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{}` failed with {status}", line.join(" ")))
    }
}

fn main() -> ExitCode {
    let task = env::args().nth(1);
    let Some("ci") = task.as_deref() else {
        eprintln!("usage: cargo xtask ci");
        return ExitCode::FAILURE;
    };
    let steps = CI;
    for step in steps {
        if let Err(msg) = run(step) {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    }
    eprintln!("==> all checks passed");
    ExitCode::SUCCESS
}

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

fn main() -> ExitCode {
    let task = env::args().nth(1);
    let Some("ci") = task.as_deref() else {
        eprintln!("usage: cargo xtask ci");
        return ExitCode::FAILURE;
    };
    let steps = CI;
    for step in steps {
        if let Err(msg) = run(step.program, step.args, step.env) {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    }
    eprintln!("==> all checks passed");
    ExitCode::SUCCESS
}

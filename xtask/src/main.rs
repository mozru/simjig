//! Repository automation.
//!
//! - `cargo xtask setup` installs every pinned tool (see [`setup`]).
//! - `cargo xtask doctor [--json]` runs the `cargo simjig doctor` checks.
//! - `cargo xtask ci` runs the same gate locally and in GitHub Actions, so a green local
//!   run means a green CI run.
//! - `cargo xtask miri` runs the kernel and ABI tests under Miri (nightly). It is not part
//!   of `ci`: it takes minutes.

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

const USAGE: &str = "usage: cargo xtask <ci | miri | setup | doctor [--json]>";

/// Miri's flags. Isolation is off because the test harness reads the environment and the
/// clock. Leaks are ignored because some are deliberate: failure locations are interned for
/// the life of the process, and the backend tests leak their fake vtables into
/// thread-locals, which Miri (unlike a `static`) does not count as reachable. Strict
/// provenance holds the pointer code (wakers, signal rows) to the strictest model.
const MIRI_FLAGS: &str = "-Zmiri-disable-isolation -Zmiri-ignore-leaks -Zmiri-strict-provenance";

/// Runs, under Miri, every test of the pure-Rust `unsafe` code: the library's unit tests
/// (waker, slab, the signal-table validation and the backend over a fake vtable) and the
/// kernel integration tests against `FakeModel`. Tests that link a Verilator model cannot
/// run in Miri; `SIMJIG_TEST_MODELS=0` keeps `build.rs` from building them.
///
/// The toolchain is `nightly` unless `SIMJIG_MIRI_TOOLCHAIN` names another
/// (`rustup toolchain install nightly --component miri`).
fn miri() -> Result<(), String> {
    let toolchain = env::var("SIMJIG_MIRI_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_owned());
    let toolchain = format!("+{toolchain}");
    run(
        "cargo",
        &[
            &toolchain,
            "miri",
            "test",
            "-p",
            "simjig",
            "--lib",
            "--test",
            "kernel_tests",
        ],
        &[("MIRIFLAGS", MIRI_FLAGS), ("SIMJIG_TEST_MODELS", "0")],
    )?;
    eprintln!("==> miri passed");
    Ok(())
}

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
        ["miri"] => miri(),
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

    /// ARCHITECTURE.md: the workspace denies `unsafe_code`; every member inherits the
    /// workspace lints or, like the examples, lists `unsafe_code = "deny"` itself.
    #[test]
    fn every_workspace_member_denies_unsafe() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        let workspace = read(&root.join("Cargo.toml"));
        let (_, rust_lints) = workspace.split_once("[workspace.lints.rust]\n").unwrap();
        let rust_lints = rust_lints.split("\n[").next().unwrap();
        assert!(
            rust_lints.contains("\nunsafe_code = \"deny\""),
            "{rust_lints}"
        );
        let mut manifests = vec![root.join("xtask/Cargo.toml")];
        for dir in ["crates", "examples"] {
            for member in std::fs::read_dir(root.join(dir)).unwrap() {
                manifests.push(member.unwrap().path().join("Cargo.toml"));
            }
        }
        assert!(manifests.len() >= 9, "{manifests:?}");
        let lax: Vec<_> = manifests
            .iter()
            .filter(|m| {
                let text = read(m);
                let (_, lints) = text.split_once("\n[lints").unwrap_or_default();
                !(lints.starts_with("]\nworkspace = true")
                    || lints.contains("\nunsafe_code = \"deny\""))
            })
            .collect();
        assert!(lax.is_empty(), "unsafe_code not denied in {lax:?}");
    }

    /// The top README's Rust blocks headed `// <path>` show `examples/quickstart`: each
    /// block, after that line, is in that file of the quickstart, line for line
    /// (indentation aside), so CI builds and runs what the README shows.
    #[test]
    fn the_readme_tests_are_the_quickstart() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        let readme = read(&root.join("README.md"));
        let quickstart = root.join("examples/quickstart");
        let flat = |text: &str| -> String {
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            lines.join("\n")
        };
        let mut files = Vec::new();
        for block in readme.split("\n```rust\n").skip(1) {
            let block = block.split("```").next().unwrap();
            let (first, code) = block.split_once('\n').unwrap();
            let Some(path) = first.strip_prefix("// ") else {
                continue;
            };
            let Ok(file) = std::fs::read_to_string(quickstart.join(path)) else {
                continue;
            };
            assert!(
                flat(&file).contains(&flat(code)),
                "README block not in examples/quickstart/{path}:\n{code}"
            );
            files.push(path.to_owned());
        }
        assert_eq!(
            files,
            [
                "tests/flop.rs",
                "build.rs",
                "src/lib.rs",
                "tests/scoreboard.rs",
                "tests/flop_sync.rs"
            ]
        );
    }

    /// The `key = value` lines of the TOML table `[name]` in `text`, comments dropped.
    fn table(text: &str, name: &str) -> std::collections::BTreeMap<String, String> {
        let header = format!("\n[{name}]\n");
        let Some((_, rest)) = text.split_once(&header) else {
            return std::collections::BTreeMap::new();
        };
        rest.lines()
            .take_while(|l| !l.starts_with('['))
            .filter_map(|l| l.split_once(" = "))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    }

    /// An example crate's own `[lints.*]` tables are the workspace's, except that tests
    /// may unwrap and need no docs (Cargo cannot extend `workspace = true` locally).
    #[test]
    fn example_lints_mirror_the_workspace() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let workspace = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let relaxed = ["missing_docs", "unwrap_used", "expect_used"];
        let mut checked = 0;
        for example in std::fs::read_dir(root.join("examples")).unwrap() {
            let manifest = example.unwrap().path().join("Cargo.toml");
            let text = std::fs::read_to_string(&manifest).unwrap();
            for kind in ["rust", "clippy", "rustdoc"] {
                let mut want = table(&workspace, &format!("workspace.lints.{kind}"));
                for (key, level) in &mut want {
                    if relaxed.contains(&key.as_str()) {
                        "\"allow\"".clone_into(level);
                    }
                }
                let have = table(&text, &format!("lints.{kind}"));
                assert_eq!(have, want, "{} [lints.{kind}]", manifest.display());
                checked += 1;
            }
        }
        assert!(checked >= 3, "no example checked");
    }
}

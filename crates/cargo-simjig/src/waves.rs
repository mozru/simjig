//! `cargo simjig waves <test> [--seed <seed>]`: waves of one test run, in one command.
//!
//! It runs the test once with `cargo nextest run`, with `SIMJIG_SEED` and `SIMJIG_WAVES`
//! set. The simjig harness in the test binary does the rest (`simjig::harness::waves`): it
//! replays the run with FST tracing over the last part of the run (or `--window`), writes
//! `waves.fst` and `waves.gtkw` into the run directory, checks that the replay reproduced
//! the run, and prints the paths. So the command is exactly the failure report's `waves:`
//! line, without having to copy it.
//!
//! The exit code is nextest's: a failing test still fails after its waves are written.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

/// What `cargo simjig waves` was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavesArgs {
    /// The libtest name of the test (`module::test`), as a failure report prints it.
    pub test: String,
    /// The root seed (`SIMJIG_SEED`); the test's own seed when absent.
    pub seed: Option<String>,
    /// The Cargo package that holds the test.
    pub package: Option<String>,
    /// `<t0>..<t1>` (`SIMJIG_WAVES=window:<t0>..<t1>`); the harness's default window when
    /// absent.
    pub window: Option<String>,
    /// Arguments after `--`, passed to nextest.
    pub nextest: Vec<String>,
}

/// The usage text of the subcommand.
pub const USAGE: &str = "\
  waves     replay one test with FST waves into its run directory
            cargo simjig waves <test> [--seed <seed>] [-p <package>]
                                      [--window <t0>..<t1>] [-- <nextest args>]
            <test>    the test's name as its failure report prints it (`module::test`)
            --seed    the seed from the report (default: the test's own seed)
            --window  the span to trace, e.g. 2us..3us (default: the last 10% of the
                      run or 1 us, whichever is longer)";

impl WavesArgs {
    /// Parses the arguments after `waves`.
    ///
    /// # Errors
    /// A message for a missing test name, a flag without its value, or an unknown flag.
    pub fn parse(args: &[&str]) -> Result<Self, String> {
        let mut parsed = Self {
            test: String::new(),
            seed: None,
            package: None,
            window: None,
            nextest: Vec::new(),
        };
        let mut iter = args.iter();
        while let Some(&arg) = iter.next() {
            let mut value = |flag: &str| {
                iter.next()
                    .map(|v| (*v).to_owned())
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match arg {
                "--seed" => parsed.seed = Some(value("--seed")?),
                "-p" | "--package" => parsed.package = Some(value("--package")?),
                "--window" => parsed.window = Some(value("--window")?),
                "--" => {
                    parsed.nextest = iter.map(|v| (*v).to_owned()).collect();
                    break;
                }
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                test if parsed.test.is_empty() => test.clone_into(&mut parsed.test),
                extra => return Err(format!("one test at a time; got a second one, {extra}")),
            }
        }
        if parsed.test.is_empty() {
            return Err("name the test, as its failure report prints it".to_owned());
        }
        Ok(parsed)
    }

    /// The `cargo nextest run` command that runs the test with waves; `cargo` is the
    /// program to run (Cargo sets `CARGO` for its subcommands).
    #[must_use]
    pub fn command(&self, cargo: OsString) -> Command {
        let mut command = Command::new(cargo);
        command.args(["nextest", "run", "--no-capture", "--run-ignored", "all"]);
        if let Some(package) = &self.package {
            command.args(["-p", package]);
        }
        command.args(["-E", &format!("test(={})", self.test)]);
        command.args(&self.nextest);
        let waves = self
            .window
            .as_ref()
            .map_or_else(|| "1".to_owned(), |w| format!("window:{w}"));
        command.env("SIMJIG_WAVES", waves);
        match &self.seed {
            Some(seed) => command.env("SIMJIG_SEED", seed),
            None => command.env_remove("SIMJIG_SEED"),
        };
        command
    }
}

/// Runs `cargo simjig waves` with `args`; the exit code is nextest's.
#[must_use]
pub fn run(args: &WavesArgs) -> ExitCode {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    match args.command(cargo).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("cargo simjig waves: nextest exited with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("cargo simjig waves: cannot run cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse_in_any_order() {
        let args = WavesArgs::parse(&[
            "--seed",
            "0x1f",
            "fifo::backpressure",
            "-p",
            "tb",
            "--window",
            "2us..3us",
            "--",
            "--profile",
            "ci",
        ])
        .unwrap();
        assert_eq!(
            args,
            WavesArgs {
                test: "fifo::backpressure".to_owned(),
                seed: Some("0x1f".to_owned()),
                package: Some("tb".to_owned()),
                window: Some("2us..3us".to_owned()),
                nextest: vec!["--profile".to_owned(), "ci".to_owned()],
            }
        );
        for bad in [&[][..], &["--seed"], &["a", "b"], &["a", "--colour"]] {
            assert!(WavesArgs::parse(bad).is_err(), "{bad:?}");
        }
    }

    fn rendered(waves: &WavesArgs) -> (Vec<String>, Vec<(String, Option<String>)>) {
        let command = waves.command(OsString::from("cargo"));
        let words = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut env: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        env.sort();
        (words, env)
    }

    #[test]
    fn the_command_is_the_reports_waves_line() {
        let args = WavesArgs::parse(&["m::t", "--seed", "0x2a", "-p", "tb"]).unwrap();
        let (words, env) = rendered(&args);
        assert_eq!(
            words.join(" "),
            "nextest run --no-capture --run-ignored all -p tb -E test(=m::t)"
        );
        assert_eq!(
            env,
            [
                ("SIMJIG_SEED".to_owned(), Some("0x2a".to_owned())),
                ("SIMJIG_WAVES".to_owned(), Some("1".to_owned())),
            ]
        );
    }

    #[test]
    fn a_window_and_no_seed_set_the_variables_accordingly() {
        let args = WavesArgs::parse(&["m::t", "--window", "..5us"]).unwrap();
        let (_, env) = rendered(&args);
        assert_eq!(
            env,
            [
                ("SIMJIG_SEED".to_owned(), None),
                ("SIMJIG_WAVES".to_owned(), Some("window:..5us".to_owned())),
            ]
        );
    }
}

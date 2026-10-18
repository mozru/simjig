//! `cargo simjig waves <test> [--seed <seed>]`: waves of one test run, in one command.
//!
//! It runs the test once with `cargo nextest run`, with `SIMJIG_SEED` and `SIMJIG_WAVES`
//! set. The simjig harness in the test binary does the rest (`simjig::harness::waves`): it
//! replays the run with FST tracing over the last part of the run (or `--window`), writes
//! `waves.fst` and `waves.gtkw` into the run directory, checks that the replay reproduced
//! the run, and prints the paths. So the command is exactly the failure report's `waves:`
//! line, without having to copy it.
//!
//! The exit code is nextest's: a failing test still fails after its waves are written, and
//! the command then says so in one line instead of reporting an error of its own.
//!
//! `--window` is checked here, before anything is built: `<t0>..<t1>`, each end a time such
//! as `0`, `2us`, `1500 ns` or `1.5us` (a decimal is converted to a whole number of a
//! smaller unit, as the harness takes it), and either end may be left out.

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

/// The usage text of the subcommand (each line keeps its indent: a `\` continuation
/// would strip it).
pub const USAGE: &str = concat!(
    "  waves     replay one test with FST waves into its run directory\n",
    "            cargo simjig waves <test> [--seed <seed>] [-p <package>]\n",
    "                                      [--window <t0>..<t1>] [-- <nextest args>]\n",
    "            <test>    the test's name as the FAIL line prints it: the test function's\n",
    "                      name, or `module::test` for a test in a module (matched exactly)\n",
    "            --seed    the seed from the report (default: the test's own seed)\n",
    "            -p        the package that holds the test (default: every package)\n",
    "            --window  the span to trace, e.g. 2us..3us or 1.5us.. (default: the last\n",
    "                      10% of the run or 1 us, whichever is longer)\n",
    "            --help    print this help",
);

/// nextest's exit code when the run worked and a test failed.
const NEXTEST_TESTS_FAILED: i32 = 100;

impl WavesArgs {
    /// Whether the arguments after `waves` ask for its help (`-h`, `--help`, before any
    /// `--`).
    #[must_use]
    pub fn wants_help(args: &[&str]) -> bool {
        args.iter()
            .take_while(|a| **a != "--")
            .any(|a| matches!(*a, "-h" | "--help"))
    }

    /// Parses the arguments after `waves`.
    ///
    /// # Errors
    /// A message for a missing test name, a flag without its value, an unknown flag, or a
    /// window that is not `<t0>..<t1>`.
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
                "--window" => parsed.window = Some(parse_window(&value("--window")?)?),
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
            return Err("name the test, as the FAIL line of its report prints it".to_owned());
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
        Ok(status) => {
            if let Some(line) = ending(status.code()) {
                eprintln!("{line}");
            }
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map_or(ExitCode::FAILURE, ExitCode::from)
        }
        Err(error) => {
            eprintln!("cargo simjig waves: cannot run cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The line printed after nextest exits with `code` (`None`: killed by a signal): nothing
/// for a pass, a note for a failing test (its report and waves are above; the command
/// worked), an error otherwise.
#[must_use]
pub fn ending(code: Option<i32>) -> Option<String> {
    match code {
        Some(0) => None,
        Some(NEXTEST_TESTS_FAILED) => Some(
            "cargo simjig waves: the test failed; its report, with the waves files, is above"
                .to_owned(),
        ),
        Some(code) => Some(format!(
            "cargo simjig waves: error: nextest exited with code {code} (the test did not run; \
             see nextest's output above)"
        )),
        None => Some("cargo simjig waves: error: nextest was killed by a signal".to_owned()),
    }
}

/// The units a window time may use, largest first: symbol and power of ten of a second.
const UNITS: [(&str, i32); 6] = [
    ("s", 0),
    ("ms", -3),
    ("us", -6),
    ("ns", -9),
    ("ps", -12),
    ("fs", -15),
];

/// Checks `--window`'s `<t0>..<t1>` and returns it with every time as a whole number of a
/// unit (`1.5us` becomes `1500ns`), the form `SIMJIG_WAVES=window:` takes.
///
/// # Errors
/// A message naming the part that is not a time, and the accepted forms.
pub fn parse_window(text: &str) -> Result<String, String> {
    let forms = "a window is <t0>..<t1>, each a time such as 0, 2us, 1500ns or 1.5us, \
                 either end optional (2us.., ..3us)";
    let (from, to) = text
        .split_once("..")
        .ok_or_else(|| format!("--window {text:?}: {forms}"))?;
    let end = |part: &str| -> Result<String, String> {
        let part = part.trim();
        if part.is_empty() {
            return Ok(String::new());
        }
        parse_time(part).map_err(|why| format!("--window {text:?}: {why}; {forms}"))
    };
    Ok(format!("{}..{}", end(from)?, end(to)?))
}

/// `<number>[ ]<unit>` with an optional fraction and `_` separators, or a bare `0`, as a
/// whole number of the largest unit that holds it exactly.
fn parse_time(text: &str) -> Result<String, String> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '_' || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number = number.replace('_', "");
    let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{text:?} is not a time"));
    }
    let unit = unit.trim();
    if unit.is_empty() {
        return if digits.bytes().all(|b| b == b'0') {
            Ok("0".to_owned())
        } else {
            Err(format!("{text:?} has no unit (fs ps ns us ms s)"))
        };
    }
    let Some(&(_, exponent)) = UNITS.iter().find(|(symbol, _)| *symbol == unit) else {
        return Err(format!(
            "{unit:?} in {text:?} is not a unit (fs ps ns us ms s)"
        ));
    };
    // The fraction's significant digits decide how many units (of 10^3) to step down.
    let steps = fraction.trim_end_matches('0').len().div_ceil(3);
    let kept = fraction.len().min(3 * steps);
    let exponent = exponent - 3 * i32::try_from(steps).unwrap_or(i32::MAX);
    let Some(&(symbol, _)) = UNITS.iter().find(|(_, e)| *e == exponent) else {
        return Err(format!("{text:?} is finer than 1 fs"));
    };
    let scaled = format!(
        "{whole}{}{}",
        &fraction[..kept],
        "0".repeat(3 * steps - kept)
    );
    let value: u64 = scaled
        .parse()
        .map_err(|_overflow| format!("{text:?} is too large"))?;
    Ok(format!("{value}{symbol}"))
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

    #[test]
    fn windows_are_checked_and_decimals_become_whole_numbers() {
        for (given, sent) in [
            ("2us..3us", "2us..3us"),
            ("1.5us..", "1500ns.."),
            ("..0.25 ns", "..250ps"),
            ("0..1_000ns", "0..1000ns"),
            (".5ms..1.0ms", "500us..1ms"),
            ("1.1234ns..", "1123400fs.."),
        ] {
            assert_eq!(parse_window(given).as_deref(), Ok(sent), "{given}");
        }
        for (bad, why) in [
            ("3us", "a window is <t0>..<t1>"),
            ("1xs..", "\"xs\" in \"1xs\" is not a unit"),
            ("5..", "\"5\" has no unit"),
            ("1.2.3us..", "is not a time"),
            ("0.0000001fs..", "finer than 1 fs"),
        ] {
            let error = parse_window(bad).unwrap_err();
            assert!(error.contains(why), "{bad}: {error}");
            assert!(error.contains("either end optional"), "{bad}: {error}");
        }
        let error = WavesArgs::parse(&["m::t", "--window", "2us"]).unwrap_err();
        assert!(error.starts_with("--window \"2us\""), "{error}");
    }

    #[test]
    fn help_is_asked_for_before_the_nextest_arguments() {
        assert!(WavesArgs::wants_help(&["--help"]));
        assert!(WavesArgs::wants_help(&["m::t", "-h"]));
        assert!(!WavesArgs::wants_help(&["m::t", "--", "--help"]));
    }

    #[test]
    fn the_usage_keeps_its_indent() {
        let lines: Vec<&str> = USAGE.lines().collect();
        assert!(lines[0].starts_with("  waves     "), "{USAGE}");
        let options = lines[1].find('[').unwrap();
        assert_eq!(lines[2].find('['), Some(options), "{USAGE}");
        assert!(
            lines[2..].iter().all(|l| l.starts_with("            ")),
            "{USAGE}"
        );
    }

    #[test]
    fn a_failing_test_is_not_reported_as_an_error_of_the_command() {
        assert_eq!(ending(Some(0)), None);
        let failed = ending(Some(100)).unwrap();
        assert!(
            failed.contains("the test failed") && !failed.contains("error"),
            "{failed}"
        );
        assert!(
            ending(Some(101))
                .unwrap()
                .contains("error: nextest exited with code 101")
        );
        assert!(ending(None).unwrap().contains("error"));
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

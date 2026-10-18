//! `cargo simjig <command>`.
//!
//! Cargo runs `cargo-simjig simjig <args>` for `cargo simjig <args>`; running the binary
//! directly (`cargo-simjig <args>`) works too. Help that was asked for (`--help`, `-h`,
//! `help [<command>]`, `<command> --help`) goes to stdout and exits 0; a usage error
//! goes to stderr and exits 2.

use std::env;
use std::io::{self, Write as _};
use std::process::ExitCode;

use cargo_simjig::doctor::{self, Format};
use cargo_simjig::show::{self, ShowArgs};
use cargo_simjig::waves::{self, WavesArgs};

const HEADER: &str = "\
usage: cargo simjig <command> [<args>]
       cargo simjig help [<command>]     (or cargo simjig <command> --help)

commands:";

/// The help of every command.
fn usage() -> String {
    format!(
        "{HEADER}\n{}\n{}\n{}",
        doctor::USAGE,
        waves::USAGE,
        show::USAGE
    )
}

/// The help of `command`, if it is one.
fn command_usage(command: &str) -> Option<&'static str> {
    match command {
        "doctor" => Some(doctor::USAGE),
        "waves" => Some(waves::USAGE),
        "show" => Some(show::USAGE),
        _ => None,
    }
}

/// Prints help that was asked for; fails only when stdout cannot be written.
fn help(text: &str) -> ExitCode {
    match writeln!(io::stdout().lock(), "{text}") {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn usage_error(error: &str, text: &str) -> ExitCode {
    eprintln!("error: {error}\n\n{text}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("simjig") {
        args.remove(0);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let is_help = |a: &&str| matches!(*a, "-h" | "--help");
    match args.as_slice() {
        [] => usage_error("no command given", &usage()),
        ["--help" | "-h" | "help"] => help(&usage()),
        ["help", command] => match command_usage(command) {
            Some(text) => help(text),
            None => usage_error(&format!("no command `{command}`"), &usage()),
        },
        ["waves", rest @ ..] if WavesArgs::wants_help(rest) => help(waves::USAGE),
        ["waves", rest @ ..] => match WavesArgs::parse(rest) {
            Ok(waves_args) => waves::run(&waves_args),
            Err(error) => usage_error(&error, waves::USAGE),
        },
        ["show", rest @ ..] if rest.iter().any(is_help) => help(show::USAGE),
        ["show", rest @ ..] => match ShowArgs::parse(rest) {
            Ok(show_args) => show::run(&show_args),
            Err(error) => usage_error(&error, show::USAGE),
        },
        ["doctor", rest @ ..] if rest.iter().any(is_help) => help(doctor::USAGE),
        ["doctor"] => doctor::run(Format::Text),
        ["doctor", "--json"] => doctor::run(Format::Json),
        ["doctor", ..] => usage_error("doctor takes only `--json`", doctor::USAGE),
        [command, ..] => usage_error(&format!("no command `{command}`"), &usage()),
    }
}

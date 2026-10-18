//! `cargo simjig <command>`.
//!
//! Cargo runs `cargo-simjig simjig <args>` for `cargo simjig <args>`; running the binary
//! directly (`cargo-simjig <args>`) works too.

#![forbid(unsafe_code)]

use std::env;
use std::io::{self, Write as _};
use std::process::ExitCode;

use cargo_simjig::doctor::Report;

const USAGE: &str = "\
usage: cargo simjig doctor [--json]

commands:
  doctor    check that Verilator, the C++ toolchain and nextest are usable
            --json   print one JSON object instead of text
            exits 1 if any check fails";

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("simjig") {
        args.remove(0);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let json = match args.as_slice() {
        ["doctor"] => false,
        ["doctor", "--json"] => true,
        ["--help" | "-h" | "help"] => {
            eprintln!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let report = Report::gather();
    let text = if json {
        report.to_json()
    } else {
        report.to_string()
    };
    if let Err(error) = io::stdout().lock().write_all(text.as_bytes()) {
        eprintln!("error: cannot write the report: {error}");
        return ExitCode::FAILURE;
    }
    if report.has_failures() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

//! `cargo simjig <command>`.
//!
//! Cargo runs `cargo-simjig simjig <args>` for `cargo simjig <args>`; running the binary
//! directly (`cargo-simjig <args>`) works too.

#![forbid(unsafe_code)]

use std::env;
use std::process::ExitCode;

use cargo_simjig::doctor::{self, Format};

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
    let format = match args.as_slice() {
        ["doctor"] => Format::Text,
        ["doctor", "--json"] => Format::Json,
        ["--help" | "-h" | "help"] => {
            eprintln!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    doctor::run(format)
}

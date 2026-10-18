//! `cargo simjig <command>`.
//!
//! Cargo runs `cargo-simjig simjig <args>` for `cargo simjig <args>`; running the binary
//! directly (`cargo-simjig <args>`) works too.

#![forbid(unsafe_code)]

use std::env;
use std::process::ExitCode;

use cargo_simjig::doctor::{self, Format};
use cargo_simjig::waves::{self, WavesArgs};

const USAGE: &str = "\
usage: cargo simjig doctor [--json]
       cargo simjig waves <test> [--seed <seed>] [options]

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
    if let ["waves", rest @ ..] = args.as_slice() {
        if WavesArgs::wants_help(rest) {
            eprintln!("{}", waves::USAGE);
            return ExitCode::SUCCESS;
        }
        return match WavesArgs::parse(rest) {
            Ok(waves_args) => waves::run(&waves_args),
            Err(error) => {
                eprintln!("error: {error}\n\n{}", waves::USAGE);
                ExitCode::from(2)
            }
        };
    }
    let format = match args.as_slice() {
        ["doctor"] => Format::Text,
        ["doctor", "--json"] => Format::Json,
        ["--help" | "-h" | "help"] => {
            eprintln!("{USAGE}\n{}", waves::USAGE);
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}\n{}", waves::USAGE);
            return ExitCode::from(2);
        }
    };
    doctor::run(format)
}

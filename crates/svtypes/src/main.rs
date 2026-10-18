//! `svtypes dump <sv files> --top X [-G NAME=VALUE] [-D NAME[=VALUE]] [-I DIR] [-- ARGS]`:
//! runs the Verilator JSON pass and prints the design's type IR as JSON.

use std::io::Write as _;
use std::process::ExitCode;

use svtypes::verilator::{JsonPassOpts, json_pass};

const USAGE: &str = "usage: svtypes dump <sv files> --top X [-G NAME=VALUE] [-D NAME[=VALUE]] [-I DIR] [-- VERILATOR ARGS]";

fn parse_args(args: &[String]) -> Result<JsonPassOpts, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    if cmd != "dump" {
        return Err(format!("unknown command `{cmd}`\n{USAGE}"));
    }
    let mut opts = JsonPassOpts::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--top" => opts.top = value("--top")?,
            "-G" => {
                let v = value("-G")?;
                let (n, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("-G {v}: expected NAME=VALUE"))?;
                opts.params.push((n.into(), val.into()));
            }
            "-D" => {
                let v = value("-D")?;
                opts.defines.push(match v.split_once('=') {
                    Some((n, val)) => (n.into(), Some(val.into())),
                    None => (v, None),
                });
            }
            "-I" => opts.include_dirs.push(value("-I")?.into()),
            "--" => opts.extra_args.extend(it.by_ref().cloned()),
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option `{flag}`\n{USAGE}"));
            }
            source => opts.sources.push(source.into()),
        }
    }
    if opts.top.is_empty() || opts.sources.is_empty() {
        return Err(format!("dump needs --top and at least one source\n{USAGE}"));
    }
    Ok(opts)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = parse_args(&args).and_then(|opts| {
        let design = json_pass(&opts).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&design).map_err(|e| e.to_string())?;
        writeln!(std::io::stdout().lock(), "{json}").map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("svtypes: {msg}");
            ExitCode::FAILURE
        }
    }
}

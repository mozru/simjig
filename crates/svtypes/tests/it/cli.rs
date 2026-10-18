//! The `svtypes` command line.

use std::process::Command;

/// `--help` (and `-h`, `help`) prints the usage on stdout and succeeds.
#[test]
fn help_prints_usage() {
    for flag in ["--help", "-h", "help"] {
        let out = Command::new(env!("CARGO_BIN_EXE_svtypes"))
            .arg(flag)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert!(
            stdout.starts_with("usage: svtypes dump "),
            "{flag}: {stdout}"
        );
    }
}

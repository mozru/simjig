//! Runs `cargo xtask doctor` in this repository, inside its pixi environment, and checks
//! that it reports the same checks as `cargo simjig doctor`.

use std::env;
use std::path::Path;
use std::process::Command;

#[test]
fn doctor_runs_the_simjig_checks() {
    let binary = env!("CARGO_BIN_EXE_xtask");
    let mut command = if env::var_os("PIXI_ENVIRONMENT_NAME").is_some() {
        Command::new(binary)
    } else {
        let mut pixi = Command::new("pixi");
        pixi.args(["run", "--locked", binary]);
        pixi
    };
    let output = command
        .args(["doctor", "--json"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
        .output()
        .unwrap_or_else(|error| panic!("cannot start xtask: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.starts_with("{\"ok\":true,\"checks\":["), "{stdout}");
    assert!(
        stdout.contains("{\"check\":\"verilator-root\",\"status\":\"ok\""),
        "{stdout}"
    );
}

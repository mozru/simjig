# Contributing

## Tools

```sh
cargo xtask setup               # pixi install --locked, then the pinned cargo tools
pixi run cargo xtask doctor     # checks Verilator, VERILATOR_ROOT, CXX, ccache, FST, nextest
```

Three pins make the environment reproducible: `rust-toolchain.toml` (Rust; rustup
installs it on the first cargo command), `pixi.lock` (Verilator, the C++ compiler, ccache,
make), and the `DEV_TOOLS` table in `xtask/src/setup.rs` (cargo-nextest, cargo-deny,
cargo-machete, typos-cli). `cargo xtask setup` is idempotent: it skips tools already at
their pinned version. To bump a cargo tool, edit `DEV_TOOLS` and run setup again.

Run commands that need Verilator through `pixi run` (e.g. `pixi run cargo xtask ci`).
Set `PIXI_NO_TELEMETRY=1` to stop pixi's anonymous usage ping.

`cargo xtask doctor` runs the same checks as `cargo simjig doctor`, the command simjig
users run (`cargo install --path crates/cargo-simjig`). Each problem is printed as `WARN`
or `FAIL` with a one-line fix; any `FAIL` exits non-zero, and `--json` prints one JSON
object for CI.

**The checkout path must not contain spaces.** Verilator's launcher script breaks on them
(for example `~/Library/Application Support/...`).

## The gate

```sh
cargo xtask ci
```

This runs, in order:
1. fmt check
2. clippy `-D warnings`
3. nextest (profile `ci`)
4. doctests
5. rustdoc `-D warnings`
6. cargo-deny
7. typos
8. cargo-machete

GitHub Actions runs exactly the same command, so a green local run means green CI.

## Self-hosted runner

CI runs on a self-hosted macOS arm64 runner (labels `self-hosted, macOS, ARM64`).

### Setup
1. On GitHub: **Settings → Actions → Runners → New self-hosted runner → macOS / ARM64**.
   Follow the download and `./config.sh` steps shown there.
2. Enter the registration token yourself. Never paste it into a chat or commit it.
3. Run it as a service: `./svc.sh install && ./svc.sh start`.
4. Run `cargo xtask setup` as the runner's user. The workflow adds `~/.cargo/bin` to
   `PATH`.

### Safety rules
- The workflow refuses pull requests from forks, because they would run arbitrary code on
  this machine.
- Also set **Settings → Actions → General → Fork pull request workflows → Require approval
  for all outside collaborators**.
- Keep the repository private, or keep these protections on if it becomes public.
- Give the runner its own macOS user account, with no access to your personal files or
  keychains.

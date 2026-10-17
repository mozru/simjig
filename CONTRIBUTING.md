# Contributing

## Tools

```sh
rustup show                     # installs the pinned toolchain from rust-toolchain.toml
cargo install --locked cargo-nextest cargo-deny cargo-machete typos-cli
pixi install --locked           # Verilator, C++ compiler, ccache, pinned in pixi.lock
```

Run commands that need Verilator through `pixi run` (e.g. `pixi run cargo xtask ci`).
Set `PIXI_NO_TELEMETRY=1` to stop pixi's anonymous usage ping.

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
4. Install the tools above for the runner's user. The workflow adds `~/.cargo/bin` to
   `PATH`.

### Safety rules
- The workflow refuses pull requests from forks, because they would run arbitrary code on
  this machine.
- Also set **Settings → Actions → General → Fork pull request workflows → Require approval
  for all outside collaborators**.
- Keep the repository private, or keep these protections on if it becomes public.
- Give the runner its own macOS user account, with no access to your personal files or
  keychains.

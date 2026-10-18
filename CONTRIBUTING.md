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

## Miri

```sh
rustup toolchain install nightly --component miri   # once
cargo xtask miri
```

This runs the pure-Rust `unsafe` code under Miri: simjig's unit tests (the waker, the
slab, signal-table validation, the Verilator backend over a fake vtable) and the kernel
integration tests (`tests/kernel_tests`, against `FakeModel`). Tests that link a Verilator
model cannot run under Miri, so it sets `SIMJIG_TEST_MODELS=0`. The flags are
`-Zmiri-disable-isolation` (the test harness reads the environment and the clock),
`-Zmiri-ignore-leaks` (interned failure locations and the backend tests' fake vtables are
leaked on purpose; the kernel tests check separately that every table is empty after a
run) and `-Zmiri-strict-provenance`. Set `SIMJIG_MIRI_TOOLCHAIN` to use a nightly other than
`nightly`.

It takes minutes, so it is not part of `cargo xtask ci`. Run it after changing `unsafe`
code, the executor or the waker.

## Linux

CI runs on macOS only. To run the gate on Linux from a macOS arm64 host, use the Lima VM
in `ci/lima/simjig.yaml` (Ubuntu 26.04 LTS arm64, Virtualization.framework, 4 CPUs,
8 GiB RAM, a sparse 40 GiB disk):

```sh
ci/lima/run.sh                                                    # pixi run cargo xtask ci
ci/lima/run.sh pixi run -e conformance conformance-regen --check
ci/lima/run.sh bash -c 'rustup toolchain install nightly --component miri && pixi run cargo xtask miri'
```

It needs `limactl` (Lima 2.x) on `PATH`. The script creates and starts the VM `simjig` if
needed, clones the checkout's **committed `HEAD`** into the guest's `~/simjig` (the home
mount is read-only, and `target/` and `.pixi/` stay on the guest's disk), installs pixi,
rustup and the pinned cargo tools there, and runs the command. Uncommitted changes are
not synced: commit first. The first run takes about 15 minutes (the cargo tools build
from source). Stop the VM with `limactl stop simjig`; remove it with
`limactl delete simjig`.

This covers linux-aarch64 only. `pixi.lock` also locks linux-64, but nothing runs it.

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

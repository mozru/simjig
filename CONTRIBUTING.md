# Contributing

simjig is Rust testbenches that drive Verilator models: the testbench owns the loop and
time; the model is a library. `ARCHITECTURE.md` has the code map and the single home of
each concept.

## Constitution
Priorities, in order: correctness > determinism/debuggability > performance > ergonomics.
Name the trade-off when two of them conflict.

1. **Prove it with a failing test.** Every fix or behaviour change adds a test that fails
   without it.
2. **Evidence, not confidence.** Report the commands you ran and their results, and say
   what you did not verify.
3. **One logical change per commit, every commit green.** Refactors go in separate commits
   from behaviour changes.
4. **Never break users silently.** Changes to the API, generated code or simulation
   semantics need a reviewed snapshot or semver diff and a CHANGELOG entry.
5. **Justify complexity in writing.** A new dependency, public abstraction, unsafe/FFI
   surface or executor-semantics change needs a "why not simpler" note.
6. **Simple shape.** Flat and deep modules; no pass-through layers. Concrete code first,
   and abstract only on the second real use. One place per concept, recorded in
   `ARCHITECTURE.md`. Functions stay at or under 70 lines.
7. **Write for a reader who wasn't there.** Each module opens with a `//!` explaining its
   concept and invariants. Comments explain domain and why, citing the rule (e.g.
   IEEE 1800 §x). Never restate the code.

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

GitHub Actions runs exactly the same command (see CI below), so a green local run means
green CI.

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

CI runs the gate on linux-aarch64 too (see CI). To run it on Linux from a macOS arm64
host, use the Lima VM in `ci/lima/simjig.yaml` (Ubuntu 26.04 LTS arm64, Virtualization.framework, 4 CPUs,
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

This covers linux-aarch64 only. `pixi.lock` also locks linux-64 (x86_64), but neither CI
nor the VM runs it.

## CI

`.github/workflows/ci.yml` runs `pixi run cargo xtask ci` on GitHub-hosted runners,
`macos-15` (arm64) and `ubuntu-24.04-arm` (linux-aarch64), for pushes to `main` and for
every pull request, forks included. It installs pixi and the locked environment, the Rust
toolchain from `rust-toolchain.toml` and the pinned cargo tools (`cargo xtask setup`),
caching the pixi environment, the cargo tools and the cargo registry. The workflow's token
is read-only and it uses no secrets. Miri and the conformance check
(`conformance-regen --check`) are not part of it; run them locally before changing
`unsafe` code or the scheduler.

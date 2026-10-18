# n3-prelude: prelude, region suffixes, lib-target layout, review nits

## Why not simpler (constitution rule 5)
- **`TriggerExt` (sealed) and `ThenRegion<T, R>`.** The simplest form would be two free
  functions (`read_write_after(trigger)`), but the request was the suffix form
  `dut.clk.rising().read_write()`, which needs a trait or inherent methods on three
  types. One sealed trait with default methods is one place instead of six inherent
  methods; sealing keeps it off `Event`/`Lock`/`Queue`, which can resume a task in a
  region where "the same timestep's ReadWrite" may already be past. `ThenRegion` is a
  hand-written two-state future rather than an `async` block so that the type is
  nameable (the trait method returns it) and both halves deregister in `Drop`. It reuses
  `ReadWrite`/`ReadOnly` unchanged: no scheduler change, the goldens are untouched.
- **`Cycles`** replaces `cycles`' `impl Future` (an `async` block) with a named future
  so it can implement the sealed trait. Same behaviour: an edge per remaining count,
  created on poll, so the width check still runs inside the simulation.
- **`simjig::prelude`** is a list of re-exports, no new items. It leaves out `Result`,
  `test`, `Lock`, `In`/`Out`, `now` and `scope` (its rustdoc says why).

## Lints
`unsafe_code = "deny"` in `[workspace.lints.rust]` replaces every `#![forbid(unsafe_code)]`.
The opt-ins use `#[expect(unsafe_code, reason = ..)]`, not `#[allow]`: the workspace warns on
`clippy::allow_attributes`, and `expect` fails if the unsafe code goes away. `bench/simjig`
is its own workspace, so its `Cargo.toml` states `unsafe_code = "deny"` itself.

## Nits
- `Dut::sources`: compares `fs::canonicalize(p).unwrap_or(p)`, stores the path as named.
- `toolchain_from`: the compiler's prefix is read from the resource directory (what
  precedes `lib/clang` or `lib/gcc`) and Clang's `InstalledDir`, not from the
  executable's path, so a conda GCC (whose configured `--prefix` is the build machine's)
  works. A real build of `examples/quickstart` still emits no `rerun-if-changed` under the
  conda `include/c++/v1`, `lib/clang` or the SDK.
- Sweep naming: two passes in `Builder::build` (`Nested`, `Builder::expand`).

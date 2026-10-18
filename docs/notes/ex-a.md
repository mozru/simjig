# ex-a: `examples/lib` (sync_fifo, skid_buffer)

Library modules with showcase testbenches, written against the public API only, as a
user would. This file records the friction found doing that.

## Friction and API gaps

### 1. Interface parameters need `bind_iface_with` and a struct literal
`stream_if` has a `WIDTH` parameter. `.param("WIDTH", 16)` sets the module's only; the
generated wrapper instantiates `stream_if` with its default (8), and verilating fails on
width warnings in the DUT under the wrapper (`WIDTHTRUNC` on `assign down.data =
out_data`). These are on by default, so it fails without `-Wall` too (checked with
`verilator --lint-only` on a hand-written top). The fix is the full struct:

```rust
let stream = |port: &str, modport: &str| IfaceBinding {
    port: port.into(),
    interface: "stream_if".into(),
    params: vec![("WIDTH".into(), SKID_WIDTH.to_string())],
    modport: Some(modport.into()),
};
Dut::new("skid_buffer")
    .param("WIDTH", SKID_WIDTH)
    .bind_iface_with(stream("up", "sink"))
    .bind_iface_with(stream("down", "source"))
```

Suggestions: a builder form (`.bind_iface("up", "stream_if.sink").iface_param("WIDTH",
16)`, or `bind_iface_params(port, iface, [(k, v)])`), and a diagnostic: when verilating
the wrapper fails with a width warning on an interface port's signal, say that interface
parameters are not inherited from `.param`.

### 2. No shared shape for two models with the same ports
`sync_fifo::Dut` and `item_fifo::Dut` have the same port names and handle kinds; only the
payload type differs. A driver/monitor that serves both needs a hand-written struct of
handles and one constructor per model (`Fifo<T>`, `Fifo::raw`, `Fifo::items` in
`tests/sync_fifo.rs`, about 40 lines). The same happens for two parameter variants of one
module (each `Dut::new` name is its own type, even with identical port types). Not a
blocker; a generated `From`, or a trait per port shape, would be a design question, so it
is only noted.

### 3. Parameters are `i32` constants
`sync_fifo::Dut::DEPTH` is an `i32` (SV `int`, correct), so every use as a size needs
`usize::try_from(sync_fifo::Dut::DEPTH).unwrap()`. Fine, but noisy in a testbench.

### 4. No random `Bits<N>` from `Rng`
`Bits::new(u128::from(rng.below(1 << 16)))` for each random field. A `rng.bits::<N>()`
(or `Bits::random(&mut rng)`) would shorten stimulus code. The skid buffer and the FIFO
each have such a helper.

### 5. `clippy::unwrap_used` in test helpers
The workspace lints deny `unwrap`/`expect` outside `#[test]` functions, and async helpers
of a `#[simjig::test]` are not test functions, so each test file needs
`#![expect(clippy::unwrap_used, ...)]` (as simjig's own test crates do). The quickstart
does not show this because its tests have no helpers; a user copying the workspace lints
meets it at once.

### 6. A stale `cargo-simjig` gives a usage text, not "unknown command"
`~/.cargo/bin/cargo-simjig` here predated `show`; `cargo simjig show item_fifo` printed
the usage (doctor, waves) with no line saying that `show` is unknown or that the binary
may be older than the checkout. I ran the workspace's own build
(`target/debug/cargo-simjig simjig show ..`) rather than reinstalling into the shared
`~/.cargo/bin`.

### 7. Running `cargo simjig show` outside pixi poisons simjig's own test models
Not a user-crate problem, but easy to hit in this repository: `show` runs
`cargo check --tests`, which ran `crates/simjig/build.rs` without Verilator on `PATH`. That
script caches "simjig test models not built" and only reruns on `SIMJIG_TEST_MODELS` or
its own source changing, so later `pixi run` builds keep the failure until
`crates/simjig/build.rs` is touched. simjig-build's DUTs do not have this problem
(`BuildEnv::TRACKED_VARS` includes `PATH`). `crates/simjig/build.rs` could print
`cargo::rerun-if-env-changed=PATH` (and the other tracked variables) when it fails.

## Verilator facts (5.052)

- An elaboration-time `$error` or `$fatal` in a generate block is a *warning*
  (`USERERROR`, `USERFATAL`): fatal by default, but `-Wno-fatal` lets a bad `DEPTH`
  elaborate. `sync_fifo` uses `$fatal(1, ..)`; its comment says so. The test
  `depth_must_be_a_power_of_two` runs `verilator --lint-only -Wall` directly.
- Every DUT here is built with `-Wall` through `.verilator_args(["-Wall"])`; the generated
  wrappers for the interface ports add no warnings.

## Checks the tests were shown to catch
Each mutation was applied to the RTL by hand, the crate's tests run, and the RTL restored:

| Mutation | Failing tests |
|---|---|
| `do_push = push && !full` (no push with pop when full) | both random tests, `push_and_pop_in_one_cycle` (`count`, "full: a push with a pop is taken") |
| registered read loads every cycle | `registered_read_waits_for_a_pop`, `fills_to_full_and_drains_to_empty`, `push_and_pop_in_one_cycle`, the 8-bit random test (`rd`) |
| `do_pop = pop` (pop when empty) | all but the FWFT and lint tests, including every fork branch |
| skid: `up.ready = 1` | random and stall tests ("3 beats in flight") |
| skid: `up.ready = !out_valid` | all three skid tests |
| skid: the skid register never reaches the output | random and stall tests ("beat 1 out") |

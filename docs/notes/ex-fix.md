# ex-fix: verification review fixes for the library examples

Scope: `examples/lib/` (RTL, `build.rs`, tests, README). Answers the verification-engineer
review: R1 (no mid-run reset), R2 (async FIFO reset semantics), nits N1 to N6.

## R1: reset during traffic

Verilator zero-initialises every flop, so a reset only at t=0 shows nothing: the state
reset is meant to clear is already zero. Each module now has a test that resets it from a
state the zero start never shows, checks the outputs right after, and carries on with
traffic against a restarted model. Each monitor or model honours reset itself (a reset
edge discards what is in flight), so the same checks run before and after.

| Module | Test | State when reset comes |
|---|---|---|
| `sync_fifo`, `item_fifo` | `a_reset_during_traffic_empties_the_fifo`, `a_reset_during_item_traffic_empties_the_fifo` | random traffic; >= 2 entries, pushes and pops both not a multiple of `2*DEPTH` (both pointers away from 0) |
| `skid_buffer` | `a_reset_with_the_skid_full_empties_both_registers` | after 100 random beats: both registers full, `up` offering a third |
| `round_robin_arbiter` | `a_reset_during_traffic_points_the_priority_at_0` | after 200 random cycles: pointer moved to 3, every requester requesting |
| `async_fifo` | `both_resets_mid_traffic_empty_both_sides` | random traffic; >= 2 entries, read pointer away from 0, write pointer >= 4 past 0 mod `2*DEPTH` |

### Mutation results

Each mutant was applied by hand to the RTL, the module's test binary run
(`pixi run cargo nextest run -p simjig-examples-lib --test <module> --no-fail-fast`), and
the file restored with `git checkout`, with the tests of commit `bdbbd4c` (default seed). For every reset mutant, the binary's other tests still pass with it: only the new test kills it.

| Mutant | Before (per review) | Now | Killed by |
|---|---|---|---|
| (a) `sync_fifo`: reset leaves `rd_ptr` (delete `rd_ptr <= '0;`) | survives | killed | both new sync_fifo reset tests: monitor `count` 7 vs 0 at the edge after reset |
| (b) `skid_buffer`: reset leaves `skid_valid` | survives | killed | new reset test: `up.ready after reset` 0 vs 1 |
| `skid_buffer`: reset leaves `out_valid` | (not listed) | killed | new reset test: `down.valid after reset` |
| (c) `async_fifo`: `rrst_n` leaves `{rq2_wgray, rq1_wgray}` | survives | killed | new reset test: `rempty` low at the first `rclk` edge after release |
| `async_fifo`: `rrst_n` leaves `rbin`, `rgray` | (not listed) | killed | same check |
| `round_robin_arbiter`: `if (!rst_n) ptr <= ptr;` | survived (every old test) | killed | new reset test: monitor `gnt` 8 vs 1 right after reset |
| `skid_buffer`: `up.ready = !skid_valid \|\| down.ready` (N2) | n/a | killed | `outputs_do_not_follow_inputs_within_a_cycle` (and 3 other tests) |
| `skid_buffer`: `down.valid = out_valid \|\| up.valid` (N2) | n/a | killed | 5 tests, including the N2 test |
| `async_fifo`: `wrst_n` leaves `{wq2_rgray, wq1_rgray}` | n/a | **survives** | see below |

The surviving write-side mutant: after reset, the stale `wq2_rgray` can only make `wfull`
read high (never low) for the two `wclk` edges before the synchronizer refills, and only
when the old read pointer is exactly `DEPTH` ahead of the new write pointer at that edge.
The effect is pessimistic (a refused write, no loss), so the scoreboard cannot see it; the
new test's "`wfull` low for two edges after release" check catches it only in that phase.
A directed test could pin it (reset with the read pointer at `DEPTH`, writer idle); not done.

### Randomized initial state: not done

Verilator's `--x-initial unique` is already the default, but it randomises only when the
runtime `randReset` is 2, set by the plusarg `+verilator+rand+reset+2`
(`verilated.cpp`, `VL_RAND_RESET_*`; default 0 = zeros). simjig passes exactly one
plusarg, `+verilator+seed+<n>` (`ModelOptions::verilator_args`,
`crates/simjig/src/harness/dut.rs`), and a test cannot add another. Supporting it would
be a `ModelOptions` change in `crates/simjig`, outside this brief. The seed side would be
deterministic (the seed already comes from the test's seed), so it is worth doing there.
The mid-run resets above cover the same risk from the other direction.

## R2: async FIFO resets

- `rtl/async_fifo.sv` header and the README now say both resets must be asserted
  together (overlapping) and each released on its own clock, and why.
- The TB's `reset_both` sets both low at the same instant and releases each after three
  cycles of its own clock, at a falling edge.
- Confirmed by simulation: with the new test's resetter changed to assert `wrst_n` alone
  (temporary edit, reverted), it fails under `SIMJIG_SEED` 1 and 2 with "a read with
  nothing written: rempty was low on an empty FIFO" and under seed 3 with "read #1 out of
  order or corrupted".
- Traffic now ends when the writer is done and the scoreboard empty, and checks
  `reads + flushed == writes` (plus `writes == ITEMS` when nothing reset). That replaced
  the `drained` event, which counted to `ITEMS` and could not survive a flush.

## Nits

- **N1**: README and the TB's module doc say the CDC structure is verified by review and
  CDC lint, not by these 2-state tests.
- **N2**: `outputs_do_not_follow_inputs_within_a_cycle` toggles `down.ready`, `up.valid`
  and `up.data` 1 ns apart after a falling edge, skid empty and full, checking the three
  outputs in `ReadOnly` after each.
- **N3**: every driver, reset release included, drives at the falling edge (the async
  FIFO at each domain's own). The arbiter's `drive()` returns the `gnt` its rising edge
  saw and leaves the caller at the next falling edge, which removed the `ReadOnly`
  settle. sync_fifo and skid_buffer released reset just after a rising edge; now at a
  falling edge too (one extra idle cycle after reset; no test counts it).
- **N4**: `timeunit 1ns; timeprecision 1ps;` in every module, interface and package;
  `$fatal` for every elaboration check (the arbiter and async FIFO used `$error`);
  `skid_buffer` gains checks for `WIDTH < 1` and for a `WIDTH` that differs from its
  interfaces' (`$bits(up.data)`); each parameterized module has a
  `parameters_are_checked_at_elaboration` test through `tests/common/mod.rs`. The README
  has a conventions table with each module's reset polarity.
- **N5**: `Fifo<T>` keeps its shape; its doc now says why it exists.
- **N6**: `full_after_depth_writes` checks `rempty == 0`.

## Surprise: the generated interface wrapper has no `timeunit`

With `timeunit` in every RTL unit, the `skid_buffer` build stopped with
`%Warning-TIMESCALEMOD` on `simjig_wrap_skid_buffer.sv`: the wrapper simjig-build
generates for `bind_iface` ports declares no `timeunit`, and Verilator refuses the mix.
Worked around in `build.rs` with `.verilator_args(["--timescale", "1ns/1ps"])`, which the
root README already suggests. Better fixed in simjig-build: the generated wrapper could
carry the design's `timeunit`, or the README could name this case. Reported, not changed
(outside this brief).

# simjig-examples-lib

Library-quality SystemVerilog modules, each with a simjig testbench written the way we
recommend writing one. Start from [`examples/quickstart`](../quickstart) for the smallest
project; come here for complete testbenches.

```sh
pixi run cargo nextest run -p simjig-examples-lib
pixi run cargo simjig show sync_fifo -p simjig-examples-lib   # a DUT's generated Rust
```

| File | What |
|---|---|
| `rtl/` | the modules, lint-clean under Verilator `-Wall` |
| `build.rs` | one function per module, each building that module's DUTs (every one with `-Wall`) |
| `tests/<module>.rs` | one test binary per module |

## The testbench shape

Every testbench here has the same parts:

- **A clock and a reset helper.** `clock(dut.clk, ns(10)).start()`, then `reset(dut)`,
  which holds reset for two edges with every input idle and releases it at a falling
  edge.
- **One timing rule.** Monitors sample on the rising edge; drivers, reset release
  included, drive on the falling edge (in the async FIFO, each domain's own). At a rising
  edge of a testbench clock, signals still hold their pre-edge values: exactly what the
  flops capture. A monitor that samples there sees every handshake the design takes, and
  a value driven half a cycle away from the edge can never race it.
- **A monitor and a reference model.** The monitor is a detached task that checks the
  design against a plain-Rust model on every edge, then applies the edge to the model. It
  runs in every test, directed or random, so each is checked the same way; the test reads
  the model (an `Rc<RefCell<..>>`) at the end for its coverage counts.
- **Seeded random stimulus.** Each driver draws from its own stream
  (`ctx.rng("producer")`), so a failure replays from the report's seed, and changing one
  driver does not change another's stimulus. Rates change in bursts, so queues fill and
  drain instead of hovering at half full.
- **Directed corner cases** for what random traffic reaches rarely or must be pinned
  exactly (latency, a cycle count).
- **`with_timeout`** around every wait that should end ("the FIFO drains within 1 us"),
  so a hang fails with a message instead of running to the test's budget.
- **Parameters set once.** Each module's function in `build.rs` sets its parameters with
  `.param(..)`; the generated `Dut` carries them as constants (`Dut::N`, `Dut::DEPTH`), so
  the testbench never repeats them.

## Conventions

Every RTL unit (module, interface, package) declares `timeunit 1ns; timeprecision 1ps;`,
and every parameterized module checks its parameters at elaboration with `$fatal`, which
a `parameters_are_checked_at_elaboration` test pins. Reset polarity follows each design's
usual style, so it differs:

| Module | Reset | Elaboration checks |
|---|---|---|
| `sync_fifo` / `item_fifo` | `rst`: synchronous, active high | `DEPTH` a power of 2, at least 2; `WIDTH` at least 1 |
| `skid_buffer` | `rst`: synchronous, active high | `WIDTH` at least 1, and equal to the interfaces' `WIDTH` |
| `round_robin_arbiter` | `rst_n`: synchronous, active low | `N` at least 2 |
| `async_fifo` | `wrst_n`, `rrst_n`: asynchronous, active low, one per clock domain, asserted together | `DEPTH` a power of 2, at least 2; `WIDTH` at least 1 |

## sync_fifo

`rtl/sync_fifo.sv`: a synchronous FIFO with `push`/`pop`, `full`, `empty` and `count`.

| Parameter | Meaning |
|---|---|
| `WIDTH` | bits per entry, at least 1 |
| `DEPTH` | entries: a power of two, at least 2 (`$fatal` at elaboration otherwise) |
| `FWFT` | 1: first-word fall-through (`rd_data` shows the head while `!empty`); 0: `rd_data` is loaded by a pop and holds until the next |

A full FIFO takes a push only in a cycle that also pops; an empty FIFO ignores a pop.
`rtl/item_fifo.sv` wraps it over the packed struct `fifo_pkg::item_t`
(`rtl/fifo_pkg.sv`), so its testbench drives and compares the generated `Item` struct
(`item.kind`, `item.tag`, `item.data`) and the enum `Kind` instead of raw bits.

`build.rs` builds two models: `sync_fifo` (8 bits, 4 deep, registered read) and
`item_fifo` (`Item`, 4 deep, FWFT). `tests/sync_fifo.rs` copies either model's handles into
one `Fifo<T>` struct, so one driver, monitor and model serve both. The monitor's model
honours `rst` too: a reset edge discards its entries, so any test may reset mid-run.

| Test | Checks |
|---|---|
| `random_traffic_matches_the_model`, `random_items_match_the_model` | 2000 cycles of bursty pushes and pops against the model (`count`, `full`, `empty`, `rd` every edge); then a drain; every corner reached (full, empty, a dropped push, push with pop while full) |
| `fills_to_full_and_drains_to_empty` | `count` and `full` while filling, the dropped fifth push, order, a pop of an empty FIFO |
| `push_and_pop_in_one_cycle` | when empty, in between, and when full |
| `registered_read_waits_for_a_pop`, `first_word_falls_through` | the two read modes |
| `a_reset_during_traffic_empties_the_fifo`, `a_reset_during_item_traffic_empties_the_fifo` | `rst` for one edge mid-traffic, with entries inside and both pointers away from zero: right after it `count` is 0, `empty` set, `full` clear, a registered `rd` zero; the model restarts empty and traffic goes on, checked as before |
| `a_reset_beats_push_and_pop`, `a_reset_beats_item_push_and_pop` | directed: two entries inside, both pointers away from zero, `push` and `pop` held high across the reset edge: after it `count` 0, `empty` set, `full` clear, a registered `rd` zero (not the entry the pop would have read); then one push and pop work |
| `random_continuations_from_full` | fills once, then `fork::run_from_checkpoint` runs four random branches from that full state, each with its own seed (needs nextest) |
| `parameters_are_checked_at_elaboration` | runs `verilator --lint-only -Wall` on other parameter sets, and on a DEPTH or WIDTH it must refuse (`$fatal`) |

## skid_buffer

`rtl/skid_buffer.sv`: a valid/ready pipeline register (two entries: output and skid) that
cuts every combinational path between its sides, moving one beat per cycle while `down`
is ready. Its ports are `stream_if` modports (`rtl/stream_if.sv`): `up` is a
`stream_if.sink`, `down` a `stream_if.source`.

A Verilator top cannot have interface ports, so `build.rs` binds each port to an
interface instance in the generated wrapper. `stream_if` has its own `WIDTH` parameter,
which `.param("WIDTH", ..)` on the module does not reach, so the binding sets it:

```rust
let stream = |port: &str, modport: &str| IfaceBinding {
    port: port.into(),
    interface: "stream_if".into(),
    params: vec![("WIDTH".into(), "16".into())],
    modport: Some(modport.into()),
};
Dut::new("skid_buffer")
    .sources(["rtl/stream_if.sv", "rtl/skid_buffer.sv"])
    .top("skid_buffer")
    .param("WIDTH", 16)
    .bind_iface_with(stream("up", "sink"))
    .bind_iface_with(stream("down", "source"))
    .build()?;
```

Every RTL unit here declares `timeunit 1ns; timeprecision 1ps;`, but the wrapper simjig
generates for the interface ports does not, so `build.rs` also passes
`--timescale 1ns/1ps` to give it one (Verilator stops on a mix with `TIMESCALEMOD`).

For an interface without parameters, `.bind_iface("up", "stream_if.sink")` is enough. In
the test, `dut.up` and `dut.down` are bundles typed from the modports: `dut.up.valid` is
an `In<Bit>` the testbench writes, `dut.up.ready` an `Out<Bit>` it reads.

| Test | Checks |
|---|---|
| `random_valid_and_ready_lose_nothing` | 1000 beats with random `valid` (held until taken) and bursty random `ready`: no loss, no duplicate, order kept, `down` stable while stalled, at most two beats inside, and both sides saw backpressure |
| `full_throughput_while_ready_is_high` | with `ready` high and a beat every cycle, `up.ready` never drops and the beats leave on consecutive edges |
| `a_stall_fills_the_skid_then_drains_in_order` | `up.ready` drops only once both registers hold a beat, and returns two edges after the release |
| `a_reset_with_the_skid_full_empties_both_registers` | after random traffic, both registers full and `up` offering a third beat: one `rst` edge, with `up.valid` and `down.ready` both high across it, leaves `down.valid` low and `up.ready` high; the board restarts empty and random traffic flows again |
| `outputs_do_not_follow_inputs_within_a_cycle` | with the skid empty and full, toggling `down.ready`, `up.valid` and `up.data` mid-cycle leaves `up.ready`, `down.valid` and `down.data` unchanged (read in `ReadOnly`): no combinational path |
| `parameters_are_checked_at_elaboration` | `$fatal` on a WIDTH below 1, or one that differs from the `stream_if` instances' own `WIDTH` |

## round_robin_arbiter

`rtl/round_robin_arbiter.sv`, tested in `tests/round_robin_arbiter.rs`; built with `N = 5`
(not a power of 2, so the pointer's wrap is exercised).

- `req[N-1:0]` in, one-hot `gnt[N-1:0]` out, combinational from `req` and a rotating
  priority pointer. At each edge with a grant the pointer moves just past the winner.
- `hold`: while high, the pointer stays on the winner, so the current grantee keeps the
  grant for as long as it requests (a lock for multi-cycle transfers).
- Synchronous, active-low `rst_n` points the priority at requester 0.

The testbench's monitor checks every cycle that `gnt` is one-hot or zero, goes only to a
requester and is non-zero whenever something requests, and compares it with a Rust model
of the pointer (`PointerModel`). The tests add:

| Test | Property |
|---|---|
| `grants_match_the_pointer_model` | random `req` and `hold`, 2000 cycles, against the model |
| `all_requesting_grants_each_once_per_n_cycles` | fairness: every window of `N` cycles grants each requester once |
| `a_held_request_is_granted_within_n_cycles` | starvation bound under random arrivals, and the bound is reached |
| `hold_keeps_the_grant_with_its_owner` | directed lock and release |
| `a_reset_during_traffic_points_the_priority_at_0` | `rst_n` for one edge mid-traffic with the pointer at 3, every requester requesting and `hold` high across the edge: the next grant goes to 0, and random traffic then matches the restarted model |
| `parameters_are_checked_at_elaboration` | `$fatal` on N below 2 |

## async_fifo

`rtl/async_fifo.sv`, tested in `tests/async_fifo.rs`; built with `WIDTH = 8`, `DEPTH = 8`.

- A dual-clock FIFO: Gray-code pointers, each one bit wider than the address, crossed by
  2-flop synchronizers (Cummings, SNUG 2002). `DEPTH` must be a power of 2 (at least 2);
  elaboration fails otherwise.
- Write side on `wclk`: a write happens at an edge with `winc && !wfull`.
- Read side on `rclk`: `rdata` shows the oldest entry while `!rempty` (first-word
  fall-through); `rinc && !rempty` pops it.
- Each side has its own asynchronous, active-low reset (`wrst_n`, `rrst_n`). **Assert
  both together** (their low periods must overlap), as in Cummings' design, and release
  each synchronously to its own clock. Each reset clears its own pointer and the
  synchronizer that brings the other side's pointer in, so one alone leaves the pointers
  inconsistent: `wrst_n` alone zeroes the write pointer while the read pointer keeps its
  value, `rempty` drops once the zero crosses, and the read side pops entries nobody
  wrote. With both asserted, both sides are empty from the moment the second falls.
- `wfull` and `rempty` are pessimistic: each sees the other side's pointer three of its
  own edges after it moved.

The testbench starts two kernel clocks with unrelated periods and resets the FIFO with
`reset_both`: both resets fall at once, and each rises at a falling edge of its own clock
(a `join!` of two `release` calls). It then runs four tasks: a writer and a reader
with random stall runs, and a write monitor and a read monitor around a scoreboard
`Queue` with room for exactly `DEPTH` entries. A write accepted on a full FIFO overflows
the scoreboard; a read on an empty one finds it empty; every read is checked for order and
data, and the test waits (under `with_timeout`) until all 1000 items are read. A reset
empties the scoreboard, as it empties the FIFO.

These tests are 2-state and zero-delay: a synchronizer never goes metastable and a
pointer is never sampled mid-change, so they check the protocol (order, loss, both flags'
latency, reset), not the clock-domain crossing. The CDC structure (Gray-coded pointers,
two-flop synchronizers, no other signal crossing) is verified by review and by CDC lint.

| Test | Clocks (write / read) |
|---|---|
| `traffic_with_a_7ns_writer_and_an_11ns_reader` | 7 ns / 11 ns; fills up and runs dry |
| `traffic_with_an_11ns_writer_and_a_7ns_reader` | 11 ns / 7 ns; fills up and runs dry |
| `traffic_with_a_fast_writer_and_a_slow_reader` | 3.1 ns / 29 ns; fills up |
| `traffic_with_a_slow_writer_and_a_fast_reader` | 29 ns / 3.1 ns; runs dry |
| `both_resets_mid_traffic_empty_both_sides` | 7 ns / 11 ns; both resets asserted together mid-traffic, with entries inside and both pointers away from zero: `rempty` high and `wfull` low at once and for two edges of each clock after release; then traffic runs on, every later write read back |
| `full_after_depth_writes` | directed: exactly `DEPTH` writes before `wfull`, and `rempty` low |
| `flags_see_the_other_side_three_edges_later` | directed: the synchronizer latency of both flags |
| `parameters_are_checked_at_elaboration` | `$fatal` on a DEPTH that is not a power of 2 (or below 2), or a WIDTH below 1 |

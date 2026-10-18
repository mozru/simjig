# simjig-examples-lib

Library-quality SystemVerilog modules, each with a simjig testbench that shows the idioms:
a seeded `Rng` per stimulus stream (`ctx.rng("...")`), kernel clocks (`simjig::clock`), a
reset helper, and a driver/monitor split with the checks in the monitors.

```sh
pixi run cargo nextest run -p simjig-examples-lib
```

Each design has its own function in `build.rs`, which sets its parameters with
`.param(..)`; the generated `Dut` carries them as constants (`Dut::N`, `Dut::DEPTH`), so
the testbench never repeats them.

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

## async_fifo

`rtl/async_fifo.sv`, tested in `tests/async_fifo.rs`; built with `WIDTH = 8`, `DEPTH = 8`.

- A dual-clock FIFO: Gray-code pointers, each one bit wider than the address, crossed by
  2-flop synchronizers (Cummings, SNUG 2002). `DEPTH` must be a power of 2 (at least 2);
  elaboration fails otherwise.
- Write side on `wclk`: a write happens at an edge with `winc && !wfull`.
- Read side on `rclk`: `rdata` shows the oldest entry while `!rempty` (first-word
  fall-through); `rinc && !rempty` pops it.
- Each side has its own asynchronous, active-low reset (`wrst_n`, `rrst_n`).
- `wfull` and `rempty` are pessimistic: each sees the other side's pointer three of its
  own edges after it moved.

The testbench starts two kernel clocks with unrelated periods, resets each domain on its
own clock (`join!` of two `reset_domain` calls), and runs four tasks: a writer and a reader
with random stall runs, and a write monitor and a read monitor around a scoreboard
`Queue` with room for exactly `DEPTH` entries. A write accepted on a full FIFO overflows
the scoreboard; a read on an empty one finds it empty; every read is checked for order and
data, and the test waits (under `with_timeout`) until all 1000 items are read.

| Test | Clocks (write / read) |
|---|---|
| `traffic_with_a_7ns_writer_and_an_11ns_reader` | 7 ns / 11 ns; fills up and runs dry |
| `traffic_with_an_11ns_writer_and_a_7ns_reader` | 11 ns / 7 ns; fills up and runs dry |
| `traffic_with_a_fast_writer_and_a_slow_reader` | 3.1 ns / 29 ns; fills up |
| `traffic_with_a_slow_writer_and_a_fast_reader` | 29 ns / 3.1 ns; runs dry |
| `full_after_depth_writes` | directed: exactly `DEPTH` writes before `wfull` |
| `flags_see_the_other_side_three_edges_later` | directed: the synchronizer latency of both flags |

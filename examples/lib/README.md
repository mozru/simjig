# simjig-examples-lib

Library-quality SystemVerilog modules, each with a simjig testbench that shows the idioms:
a seeded `Rng` per stimulus stream (`ctx.rng("...")`), kernel clocks (`simjig::clock`), a
reset helper, and a driver/monitor split with the checks in the monitors.

```sh
pixi run cargo nextest run -p simjig-examples-lib
```

Each design has its own function in `build.rs`, which sets its parameters with
`.param(..)`; the generated `Dut` carries them as constants (`Dut::N`), so
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

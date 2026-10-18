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
  which holds reset for two edges with every input idle.
- **One timing rule.** Tasks drive on the falling edge and sample on the rising edge. At a
  rising edge of a testbench clock, signals still hold their pre-edge values: exactly what
  the flops capture. A monitor that samples there sees every handshake the design takes.
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
one `Fifo<T>` struct, so one driver, monitor and model serve both.

| Test | Checks |
|---|---|
| `random_traffic_matches_the_model`, `random_items_match_the_model` | 2000 cycles of bursty pushes and pops against the model (`count`, `full`, `empty`, `rd` every edge); then a drain; every corner reached (full, empty, a dropped push, push with pop while full) |
| `fills_to_full_and_drains_to_empty` | `count` and `full` while filling, the dropped fifth push, order, a pop of an empty FIFO |
| `push_and_pop_in_one_cycle` | when empty, in between, and when full |
| `registered_read_waits_for_a_pop`, `first_word_falls_through` | the two read modes |
| `random_continuations_from_full` | fills once, then `fork::run_from_checkpoint` runs four random branches from that full state, each with its own seed (needs nextest) |
| `depth_must_be_a_power_of_two` | runs `verilator --lint-only -Wall` on other parameter sets, and on DEPTHs it must refuse |

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

For an interface without parameters, `.bind_iface("up", "stream_if.sink")` is enough. In
the test, `dut.up` and `dut.down` are bundles typed from the modports: `dut.up.valid` is
an `In<Bit>` the testbench writes, `dut.up.ready` an `Out<Bit>` it reads.

| Test | Checks |
|---|---|
| `random_valid_and_ready_lose_nothing` | 1000 beats with random `valid` (held until taken) and bursty random `ready`: no loss, no duplicate, order kept, `down` stable while stalled, at most two beats inside, and both sides saw backpressure |
| `full_throughput_while_ready_is_high` | with `ready` high and a beat every cycle, `up.ready` never drops and the beats leave on consecutive edges |
| `a_stall_fills_the_skid_then_drains_in_order` | `up.ready` drops only once both registers hold a beat, and returns two edges after the release |

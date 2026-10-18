# simjig

Rust testbenches that drive Verilator models. The testbench owns the loop and simulation
time; the model is a library. Types and a typed `Dut` are generated from the
SystemVerilog, so ports, packed structs, enums and interface bundles are Rust types.

See `ARCHITECTURE.md` for the code map and `docs/architecture/DECISION.md` for the design.

## Quick start

The canonical starting point is [`examples/quickstart`](examples/quickstart): two tiny
designs, a build script and tests. Copy it to start a project.

```rust
// build.rs
fn main() -> Result<(), simjig_build::Error> {
    simjig_build::Dut::new("flop")
        .sources(["rtl/flop.sv"])
        .top("flop")
        .build()?;
    Ok(())
}
```

```rust
// tests/flop.rs
use simjig::{ReadOnly, clock, ns};
use simjig_bits::Bit;

simjig::include_dut!(flop); // the generated `flop::Dut`

#[simjig::test(timeout = "1us")]
async fn q_follows_d(dut: flop::Dut) {
    clock(dut.clk, ns(10)).start();
    dut.rst.set(1);
    dut.clk.rising().await;
    dut.clk.falling().await;
    dut.rst.set(0);
    dut.d.set(1);
    dut.clk.rising().await;
    ReadOnly::new().await; // the values the edge produced
    assert_eq!(dut.q.get(), Bit::ONE);
}
```

```sh
pixi install --locked                       # Verilator 5.052 and the C++ toolchain
pixi run cargo nextest run -p quickstart
```

A failing test prints a report with the seed, the simulation time, the failing task and a
one-line `repro:` command that reruns it with the same seed.

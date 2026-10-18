# quickstart

The smallest simjig project. Copy this directory to start your own.

| File | What |
|---|---|
| `rtl/inv.sv`, `rtl/flop.sv` | the designs |
| `build.rs` | one `simjig_build::Dut` per design: verilate, generate bindings, link |
| `tests/inv.rs` | a combinational test: `set`, `Timer`, `get` |
| `tests/flop.rs` | a clocked test: `simjig::clock`, `rising`, `ReadOnly` |

`Cargo.toml` needs `simjig` and `simjig-bits` for the tests and `simjig-build` for the
build script. The package has no library target on purpose: the build script's link
directives go to the library when there is one, and an integration test that never names
that library does not link it, so the model would be missing (see the glue notes,
`docs/notes/glue.md`). Verilator 5.052 must be on `PATH` (in this repository: `pixi run`).

```sh
pixi run cargo nextest run -p quickstart
```

Each test file names a design with `simjig::include_dut!(inv);`, which brings in the
module `inv` with `inv::Dut`: one typed handle per port (`In<Bit>` for inputs, `Out<..>`
for outputs). A `#[simjig::test]` takes the `Dut` as its argument and runs as an async
function in simulation time.

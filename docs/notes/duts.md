# Notes: shared DUTs and cocotb conformance goldens (agent "duts")

## What exists
- `tests/duts/<name>/`: 13 designs, each with a README. `pixi run duts-lint` lints all of
  them with `-Wall` (at several parameter values where a design has them). The only
  waiver is `SYNCASYNCNET` in `derived_clk`, explained in the source.
- `pixi run -e conformance duts-smoke`: cocotb smoke tests for the DUTs that have no
  golden (`inv`, `wide`, `struct_pipe`, `timing_dut`, `dpi`, `derived_clk`,
  `iface_top_wrap`, and `fatal` once per kind).
- `tests/conformance/`: 13 golden scenarios on `flop`, `counter`, `comb_chain`, `fifo`,
  `async_fifo` and `derived_clk`. The format and every observed rule are in
  `tests/conformance/README.md`; that file is the input to the scheduler spec.
- `pixi.toml`: a `conformance` feature and environment (Python 3.12 and cocotb 2.1.0
  from PyPI), kept out of the default environment. The default environment gained only
  the `duts-lint` task.

## Surprises (the scheduler spec depends on these)
1. **Flops do NOT show their new value right after `RisingEdge(clk)` in cocotb on
   Verilator.** This contradicts `docs/PLAN.md` §5 step 2 ("flops already show their new
   values, as in cocotb on Verilator"). cocotb applies the clock toggle as a write. The
   edge callback fires after the write is applied but before the eval that clocks the
   flops. Post-edge values appear from `ReadOnly` on, or at the next trigger in a later
   eval. This holds in both `COCOTB_TRUST_INERTIAL_WRITES` modes. The director should
   decide whether simjig copies cocotb (sampled pre-edge values, which are race-free for
   monitors) or keeps the PLAN wording. In either case the PLAN sentence is factually
   wrong about cocotb.
2. **A design-driven edge behaves the opposite way.** `RisingEdge(div_clk)` on a derived
   clock and `ValueChange(count)` see post-eval values, because they fire after the eval
   that produced them. A simjig "trigger on derived clock is sampled" rule must state which
   of the two it means.
3. **A Timer due at the same time as a clock edge runs before the edge.** It sees `clk=0`
   and pre-edge state. `First(edge, Timer)` resolves to the Timer on a tie, and
   `with_timeout` raises on a tie. A trigger armed from that Timer wake-up still catches
   the same-time edge.
4. **A write right after `RisingEdge` is not captured by that edge**, and a task cannot
   read back its own write before awaiting. Writes are applied after the next
   `eval_step`. This matches the write-log design (DECISION.md table 2, row 2).
5. **Wake order at coincident edges is not clock-declaration order.** PLAN §5 says
   coincident edges are applied in clock-declaration order. cocotb applies them together
   (every woken task sees both clocks high), but wakes waiters in what looks like
   trigger-priming order. The `rclk` waiter ran before the `wclk` waiter although `wclk`
   was started first. If simjig uses declaration order, `two_clocks` will not match line
   for line. Either the Rust diff compares per-task streams for that scenario, or the
   kernel mimics priming order. The director needs to decide.
   **Resolved:** the kernel mimics priming order, one trigger group at a time; see the
   conformance README, "Callback order" (`tests/conformance/README.md`).
6. cocotb's Verilator runner and Makefile **default `COCOTB_TRUST_INERTIAL_WRITES=1`**,
   so a plain `os.environ.pop` does not give mode 0. `regen.py` pins it explicitly. With
   mode 0, only `ReadWrite` visibility changes (see the README).
7. Awaiting `ReadWrite` from `ReadWrite` with nothing pending, and no clock running, ends
   the simulation. The second `ReadWrite` callback goes on Verilator's "future" list and
   the step never loops again.
8. `$finish` at a 15 ns edge ends the simulation at **20 ns** (exit 0). Verilator's main
   loop checks `gotFinish()` only after advancing time and running the next step's timed
   callbacks. `$error` stops the simulation like `$fatal`/`$stop` (exit 1), because
   Verilator's default error limit is 1.
9. Verilator compiles `.c` DPI sources with the C++ compiler, so a plain C file without
   `extern "C"` fails to link.
10. Verilator rejects interface ports on the top, so `iface_top` needs a wrapper even for
    cocotb. `iface_top_wrap.sv` is a hand-written reference for the generated one.

## Decisions
- Every scenario runs in its own simulator process. Running the tests of a module
  back to back carries time and model state from one test to the next, which would make
  each golden depend on test order.
- Time is logged in ps, with timescale 1ns/1ps for every build. For these designs, ticks
  of the time precision and ps coincide (DECISION.md table 2, row 8).
- The log keeps the five requested keys (`time`, `phase`, `trigger`, `signal`, `value`)
  and adds `task` and `op`. Without `task`, the interleaving of concurrent tasks is
  ambiguous; without `op`, a write cannot be told apart from a read.
- Trigger labels are fixed strings set by the scenario, never cocotb `repr`s, so the
  Rust side can produce them.
- cocotb comes from PyPI because conda-forge builds it for linux-64 only. On
  linux-aarch64 PyPI has no wheel, so pixi builds the locked sdist (not verified on that
  platform).

## Not verified
- linux-64 and linux-aarch64 runs (only osx-arm64 was run). The lock resolves for all
  three platforms.
- The cause of the coincident-edge wake order (surprise 5) was a hypothesis; it is now
  confirmed (conformance README, "Callback order").
- `fatal` with `AT_START` (time 0) under cocotb: only linted, not run.

# ex-b: round-robin arbiter and async FIFO examples

`examples/lib` (package `simjig-examples-lib`): `rtl/round_robin_arbiter.sv`,
`rtl/async_fifo.sv`, their testbenches in `tests/`, one `build.rs` function each.

## Checking the testbenches catch bugs
Each RTL mutant below was applied by hand, the suite run, and the RTL restored:

| Mutant | Caught by |
|---|---|
| arbiter: `ptr <= ptr + 1` instead of `winner + 1` | the pointer model (`grants_match_the_pointer_model`, `a_held_request_...`) |
| FIFO: writes ignore `wfull` | scoreboard overflow, data mismatch, `full_after_depth_writes` |
| FIFO: `rempty` from one synchronizer flop | `flags_see_the_other_side_three_edges_later` (`rempty`) |
| FIFO: `wfull` from one synchronizer flop | the same test (`wfull`) |
| FIFO: binary pointers instead of Gray | scoreboard overflow in three traffic tests |

A one-flop synchronizer is functionally correct in a 2-state simulation, so only the
exact-latency test can see it.

## Friction
1. **The test body and a monitor wake on the same edge in either order.** The monitor logs
   at each `rising()`; the driver's last `drive(..).await` returns at the same edge, and
   reading the log right away missed that edge's sample in every test (1999 of 2000).
   The fix is a `ReadOnly` first, which is the documented semantics, but it is an easy
   trap for any driver/monitor split:

   ```rust
   async fn settled(log: &Log) -> Vec<Sample> {
       ReadOnly::new().await; // after every task the edge woke
       log.borrow().clone()
   }
   ```

2. **Durations cannot be constants.** `ns()`/`ps()` convert to ticks of the running
   simulation's precision, so they panic outside a run and are not `const fn`. A table of
   clock configurations has to be a function:

   ```rust
   const DIRECTED: Clocks = Clocks { wclk: ns(7), .. }; // E0015: non-const fn
   fn directed_clocks() -> Clocks { Clocks { wclk: ns(7), rclk: ns(11), rclk_phase: ps(1300) } }
   ```

   A unit-carrying value (`(7, TimeUnit::Ns)`) converted at `clock()` would allow both.
3. **`Elapsed` prints raw ticks.** `with_timeout(us(400), ..)` fails with "timed out after
   400000000 ticks", while the rest of simjig prints physical units
   (`SimTime::display(precision)`).
4. **`expect` in shared test helpers.** `clippy.toml`'s `allow-expect-in-tests` covers
   `#[test]` functions, not the plain `async fn` helpers an integration test file calls,
   so `with_timeout(..).await.expect(..)` in `traffic()` fails `-D warnings`. I matched on
   the `Err` and panicked with the progress instead (a better message anyway). Repository
   lint configuration, not simjig API.
5. **Narrowing `Bits` reads.** `Bits<N>` has `to_u128` only; a `u32` mask needs
   `u32::try_from(..).expect(..)` (and point 4). The arbiter keeps its masks as `u128`.
6. **Environment:** the `cargo simjig` in `~/.cargo/bin` predates `show` (it printed the
   old usage), so I read the generated module from `target/debug/build/*/out/simjig/`.

What worked well: `.param(..)` plus the generated `Dut::N`/`Dut::DEPTH` constants, so
`Bits<{ Dut::WIDTH }>` follows the build script; `Queue::try_put` with capacity `DEPTH`
as an overflow check; `join!` for the two per-domain resets; `clock(..).phase(..)`.

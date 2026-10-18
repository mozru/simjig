# s3c5-ux: usability review cycle 5 (and the cycle 5 maintainer review's overlap)

## The dropped-unrun-`Task` check
The rule (`kernel::tasks::settle_unrun_drops`): a handle dropped before its task ever ran,
never awaited, and not as part of a larger cancellation is noted, and the poll that
dropped it settles it when the poll ends:
- in the poll in which `main` returns: nothing (the trace still records
  `DroppedUnpolled`);
- otherwise: one `Setup` failure naming every handle that poll dropped, with each
  `spawn` line, saying "by `let _ = spawn(..)`, or by leaving the handle's scope early".

Why the exemption is sound: `run_ready` polls nothing once `main` completes, so a task
spawned in `main`'s last poll would not have run even if detached. The test's result is
the same as the correct program's. No other task gets the exemption: a handle dropped in a
child's last poll, had it been detached, would have run under `main`.

Options considered:
- **Accurate message only.** Adopted as part of the rule. On its own it leaves the
  maintainer review's bug: `Setup` is recorded first and masks the test's `Err`.
- **Opt-in `cancel_on_drop()`.** Not added: once `main`'s early return is exempt, the
  remaining case (an early return in a child or a helper whose caller goes on) is rare,
  and `.cancel()` or spawning after the checks fixes it. It would be a new public API with
  a second spelling of "cancel" (constitution rule 5). It can be added later without
  breaking anything.
- **Defer to the end of the run, failing only if `main` returned `Ok` and nothing else
  failed** (the maintainer review's suggestion). `Sim::run<T>` cannot tell `Ok` from
  `Err` for a generic `T` without a new bound on `T`, which would break every
  `sim.run(async { 42 })`. It also reports a missing driver's later symptom (a timeout, an
  assertion) instead of its cause. The rule above passes both of its tests
  (`kernel_tests/s3c5m.rs`), since the `Err` is returned in `main`'s last poll.
- **Defer to the end of the step, failing only if the parent did not fail or return an
  error.** Unsound as stated: the kernel cannot see a parent's `Err`, and exempting any
  task's last poll would let `let _ = spawn(..)` in a child pass silently.

Not covered (open): `scope("s", async { let m = spawn(..); check()?; .. }).await?` in
`main`. The drop happens in the scope task's poll, which is not `main`'s, so it still fails
as `Setup`. The message names the fix.

## Integer `PartialEq` from the left
Kept only `PartialEq<Bits<N>/WideBits/Bit> for u128`. Checked empirically
(`simjig-bits/tests/it/inference.rs` fails with 10×E0283 + 1×E0284 against 888b8d2):
- an impl for `u32`, `u64`, `usize`, `u8`, `i32` or `bool` gives `T: PartialEq<?U>` a
  second candidate for that `T`, and std code with an inferred right-hand side stops
  compiling;
- `i32` (the literal fallback) is one of those: `f_i32 == From::from(2i8)` breaks;
- with only `u128`, `4 == x` picks the single impl and infers `u128`.
`u128` users get the same break (`total_u128 == xs.iter().sum()`); that is the trade-off
for `assert_eq!(4, x.get())`.

`From<u8/u16/u32/u64> for Bits<N>`: rejected. A standalone check (scratch `t.rs`) shows
`set::<Bits<8>>(1)` fails with `Bits<8>: From<i32>` not satisfied once a second `From`
exists. The README says `u128::from(byte)` instead.

## `cycles(clk, n)`
`n: impl TryInto<u64>`, not `impl Into<u64>`: `usize` has no `Into<u64>`, and an
unsuffixed literal falls back to `i32`, which has none either. A negative count panics at
the call (`cycles` is now a `#[track_caller]` `fn` returning the future).

## `Dut::build` and `cargo::error`
Checked with cargo 1.99.1: a script that prints `cargo::error=` and exits 0 fails with
"build script logged errors". A script whose `main` returns `Err` shows `--- stdout`
(the directives) and then `--- stderr` (`Error: ...`) last. So `Dut::build` prints nothing on
failure. With `?` or `unwrap`, the error is the last thing Cargo shows.

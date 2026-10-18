# s3-usability: notes

Fixes from the stage/3 usability review (RTL designer, cocotb engineer, Rust developer new
to HDL).

## 1. `let _ = spawn(..)` fails the run
- Rule: a `Task` handle dropped while its task has never been polled, and the handle was
  never polled (awaited), fails the run as `FailureKind::Setup` at the `spawn` call
  (`Task::spawned_at`, captured in `spawn` before entering `state::with_current`, whose
  closure would hide `#[track_caller]`). Help names `.detach()`, `.await`, `.cancel()`.
- Exempt (`tasks::cascading`): the run ended, the dropping task is being cancelled, a
  panic is unwinding, and new: a combinator cancelling an operand
  (`Kernel::cancelling_operand`, set by `combinator::cancel_operand` around both
  `Operand::cancel` calls). Without the last one, `first!(async {}, spawn(..))` failed
  (checked by disabling it: `cancel::deliberate_and_cascading_drops_...` fails).
- Why not the broader rule ("any handle dropped without await/detach/cancel"): it would
  also fail `let _mon = spawn(..)` held until scope end and handles kept in structs for
  RAII cancellation, i.e. remove drop as a cancellation mechanism (DECISION §1). The false
  pass in the review is only the never-ran case. A handle held to the end of `main`
  whose task never ran does fail (it is the same mistake).
- Why `Setup` and not a new `FailureKind`: the harness matches `FailureKind`
  exhaustively in `harness::report` and `harness::waves`; `Setup` ("the test is set up in
  a way that cannot run, and the design is not at fault") fits and keeps the change out
  of those files.
- Tests changed: `fork_checkpoints rules::another_running_task_refuses_the_fork` and the
  swarm's `CancelChild` cancelled fresh tasks by drop; they call `.cancel()` now.
- clippy's `let_underscore_future` also catches `let _ = spawn(..)` for users who run it.

## 2. `cargo simjig show`
- Runs `cargo check --tests --message-format=json [-p]`; reads `out_dir` from
  `build-script-executed` messages. `--tests` because a test-only package (quickstart) has
  no lib target, so plain `check` would not run its build script.
- Which package: `-p` by name (parsed from the package ID spec), else the directory of
  `cargo locate-project`, else every `path+file` package (virtual workspace root). Without
  this, `-p quickstart` also listed simjig's own test models (its build script).
- Not tested end to end in CI (a nested `cargo check` of the workspace from a test is slow
  and contends for the target dir); checked by hand, see the final report.

## 3. Build errors
- Cargo 1.99 with `cargo::error=` lines: prints `error: <pkg>: <line>` right after
  `Compiling`, before the `--- stdout` dump; with exit 0 it prints no dump at all
  ("build script logged errors"), with exit 1 the dump follows. `Dut::build` cannot exit
  0 for the user's `main`, so the dump stays, but the error is first and also the last
  stdout line. The reviewer's 1,270 lines are the earlier DUT's directives (quickstart:
  `inv` built, `flop` broken: 1,269 lines of stderr, error first).
- Holding all DUTs' directives until the script ends would need an exit hook; not done.

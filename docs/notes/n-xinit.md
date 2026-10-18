# n-xinit: random initial register state

Goal: let a test start a model from random initial values, so a register that reset never
clears fails, while every default build and zero-initialized run stays exactly as it was.

## What Verilator 5.052 does (checked against the installed 5.052 and the 5.053 source)

- `--x-initial unique` is already the **default** (`V3Options.cpp`: `m_xInitial = "unique"`).
  With it, each variable without an initializer is set in the root constructor
  (`V<prefix>___024root___ctor_var_reset`, called from the root's constructor) by
  `VL_SCOPED_RAND_RESET_{I,Q,W}(width, __VscopeHash, salt)`, where `__VscopeHash` hashes the
  instance name and `salt` the variable's name. Inputs are included (checked in a
  verilated module: `clk`, `rst_n`, `d` all reset this way).
- `VL_SCOPED_RAND_RESET_*` (`verilated.cpp`, the same in 5.052 and 5.053) returns 0 when the
  thread context's `randReset()` is 0 (the default), all ones for 1, and for 2 a value from
  `VlRNG{randSeed() ^ scopeHash ^ salt}`: deterministic per (seed, scope, variable), with no
  draw from the `$urandom` stream.
- `--x-initial 0` and `fast` compile the values in (`noreset = 0;` in the generated
  constructor), so nothing at run time can randomize them.
- `+verilator+rand+reset+2` and `+verilator+seed+N` set `randReset` and `randSeed` on the
  context (`commandArgs`). simjig already passes `+verilator+seed+<verilator_seed>`, which
  also seeds `$urandom` (`Runtime::reseed(context_seed())` in create).
- `--x-assign unique` is a different thing: explicit `X` constants become XTEMP variables
  reset with `VL_SCOPED_RAND_RESET_ASSIGN_*`, which ignore `randReset` and are always random.
  Not exposed (out of scope); it stays reachable through `verilator_args`.

So the build side needs no flag at all for the default; it needs a way to say "not
unique", so the runtime can refuse instead of silently giving zeros.

## Why not just the plusargs

Passing `+verilator+rand+reset+2` would work for one model, but `randSeed` is also the
`$urandom` seed, and `harness::multi` gives every model of a simulation the **same**
Verilator seed on purpose (lockstep runs need equal `$urandom` draws). Two instances of one
design (same instance name `TOP`, same variable names) would then start from identical
values, and per-model streams (the brief) would need per-model `$urandom` seeds, changing
default multi-DUT behaviour. Restoring `$urandom` with the existing `rng_seed` slot after
create is not exact either: a constructor that draws (class objects) would have drawn from
the other stream.

## Decision: two appended `simjig_create_opts` fields, no new vtable slot

- `simjig_create_opts` gains `init` (`SIMJIG_INIT_ZERO` 0 / `SIMJIG_INIT_RANDOM` 2, numbered
  as `randReset`) and `init_seed`, appended after size 32 (now 40). A slot called after
  create is too late: the values are set in the constructor. Append-only, so
  `SIMJIG_ABI_VERSION` stays 1 and the vtable stays 192 bytes; `check_vtable` is unchanged.
  A caller compiled before the fields passes `size` 32 and gets zeros (C++ test
  `random_init_draws_from_its_seed_alone` covers it).
- The shim (`Vtable::construct_randomized`, `simjig_model.h`) sets `randReset(2)` and
  `randSeed(init_seed)` for the construction alone and restores both afterwards (RAII, so a
  throwing constructor restores too). `$urandom` was already seeded from the plusarg seed
  before, so it is the zero run's stream (C++ test, and `random_initial_values_leave_urandom_alone`
  in Rust). Each `randSeed` bumps Verilator's global seed epoch; the shim calls
  `simjig::thread_rng()` after each so a constructor draw would not reset the model's
  private stream. **Not verified by a failing test**: none of the test designs draws in its
  constructor, and removing those two calls passes every test.
- New feature bit `SIMJIG_FEATURE_RANDOM_INIT` (8): set unless the traits declare
  `kXInitialUnique = false` (default true, Verilator's default). Codegen emits that line only
  for `XInitial::Zero`/`Fast`, so default shims, their snapshots and archive keys are
  unchanged. `VerilatorModel::load` refuses `init = random` on a model without the bit
  (`LoadError::NoRandomInit`, a `Setup` failure with help).
- `simjig_build::XInitial { Unique (default, no flag), Zero, Fast }` and `Dut::x_initial`.
  The prefix digest hashes it only when non-default (default prefixes unchanged). Raw
  `--x-initial` is refused in `verilator_args` and in option files (`refused_arg`), so the
  shim's trait cannot disagree with the command line (there is no `VM_*` switch to check
  it against, unlike trace and coverage). `--x-initial-edge` is not affected.

Trade-off named (rule: correctness > determinism > performance > ergonomics): refusing raw
`--x-initial` breaks a user who passed it (ergonomics), in exchange for never running a
"random" test on zeros (correctness).

## Run side

- `simjig::InitValues { Zero (default), Random }` (`harness::dut`), `ModelOptions::{init,
  init_seed, with_init, random_init}`.
- Seeds: `Seed::init_seed(model)` is stream `"init.model-<model>"`, kept in `1..=i32::MAX`
  (`randSeed` is an `int`; 0 is fine there but kept positive like `verilator_seed`).
  `ModelOptions::new` uses model 0's, `for_model(i)` model i's, so multi-DUT models start
  apart while `verilator_seed` (and so `$urandom`) stays shared. Adding a stream changes no
  other stream (seed tree).
- Choice: `#[simjig::test(init = random | zero)]` wins, else `SIMJIG_INIT=random|zero`, else
  zero (`runner::init_values`). The argument wins so a `SIMJIG_INIT=random` sweep cannot
  break a test pinned to zeros.
- Recorded: report header `seed=..  init=random  t=..`, repro `SIMJIG_INIT=random
  SIMJIG_SEED=..` (both only for random runs, so zero reports are byte-identical), and
  `summary.json` `"init"` and `"init_seed"` (model 0's, `null` for zero). The waves replay
  child gets `SIMJIG_INIT` explicitly.
- Fork checkpoints: initial values are drawn once, at create; branches inherit the parent's
  state and draw nothing again (`the_attribute_randomizes_and_branches_inherit_it`).
- `FakeModel`s ignore `init` (as they ignore `trace`).

## Tests

| Test | Shows |
|---|---|
| C++ `random_init_draws_from_its_seed_alone` | same seed same values, other seed others, zero and a size-32 caller get zeros, `$urandom` equal to the zero run's (fails with the shim change disabled) |
| C++ `vtable_describes_the_build` | feature bit set by default, clear for a `kXInitialUnique = false` model (`Vtc_hier`, now verilated `--x-initial fast`) |
| `model_tests::init::*` (`no_reset.sv`, built as `no_reset` and `no_reset_fast`) | zero default; random differs across seeds 1..4 and repeats for one seed; `$urandom` unchanged; `XInitial::Fast` refused with help; two instances start apart but share `$urandom`; the attribute end to end and fork inheritance |
| `harness_tests::runner::random_initial_values_are_chosen_and_recorded` | precedence, header, repro, `summary.json` |
| `harness_tests::reports::a_random_init_run_says_so_in_its_header_and_repro` | report text |
| `simjig-build` `x_initial_is_a_flag_and_a_prefix_only_off_the_default`, codegen `only_a_compiled_in_x_initial_adds_a_traits_line` | build side, default unchanged |
| `harness::dut`/`seed` unit tests, `model::verilator` `random_init_reaches_create_only_on_a_model_that_supports_it` | options and ABI plumbing |

## Mutation: `sync_fifo` without `rd_ptr <= '0;`

Every `examples/lib` test now runs with `init = random` (all 34 pass). With the mutant applied
(by hand, then restored), `sync_fifo` tests killed: **11 of 12** with random init (most fail at
t=20 to 30 ns, right after the first reset: `count` nonzero), against **4 of 12** with zeros
(the four mid-run reset tests from ex-fix). The survivor is
`parameters_are_checked_at_elaboration`, which runs no traffic.

## Not done / not verified

- `--x-assign` is not exposed.
- The epoch-sync calls in `construct_randomized` (above).
- Mutants other than `sync_fifo`'s `rd_ptr` were not rerun under random init.
- macro expansion snapshots change by one line each (`init: None`); this is the only
  diff in a default build's generated output.

# abi: the C ABI, the C++ runtime and the fatal-unwinding spike

Checked against the installed Verilator 5.052 (conda-forge,
`.pixi/envs/default/share/verilator/include`) and the 5.053-devel source.

## Files
| File | What |
|---|---|
| `crates/simjig-build/cpp/simjig_abi.h` | The C ABI: vtable, signal descriptor, message record, create options, status codes. |
| `crates/simjig-build/cpp/simjig_vl_config.h` | Force-included into every C++ translation unit: `VL_USER_*`, `VL_TIME_CONTEXT`, `VL_PRINTF`. |
| `crates/simjig-build/cpp/simjig_rt.{h,cpp}` | Hand-written runtime: hooks, message ring, exception barrier, model-independent entries. |
| `crates/simjig-build/cpp/simjig_model.h` | `simjig::Vtable<Traits>`, `SIMJIG_SIGNAL`, `SIMJIG_EXPORT_VTABLE`: what the generator instantiates per model. |
| `crates/simjig-build/cpp/spike/` | Historical: the unwinding spike behind the results below (SV models, hand shim, C++ driver, `run.sh [asan\|ubsan]`). It prints and asserts nothing, so no gate runs it; the behaviours it showed are tested in `crates/simjig/tests/abi_counter.rs` (`fatal_*`, `error_*`, `stop_finish_and_display_are_reported`). Kept to rerun under sanitizers. |
| `crates/simjig/src/model/abi.rs` | Rust mirror: `#[repr(C)]` structs, decoded enums, layout asserts, raw read/write. |
| `crates/simjig/build.rs`, `tests/abi_counter.rs`, `tests/abi_counter/` | Integration test models (one plain, one `--timing`), the hand-written shim, 10 tests. |

## The ABI (`simjig_abi.h`, `SIMJIG_ABI_VERSION 1`)
- One symbol per model: `const simjig_model_vtable* simjig_vtable_<prefix>(void)`.
- `simjig_model_vtable` (136 bytes): `abi_version`, `size`, `prefix`, `verilator_version`
  (`VERILATOR_VERSION` of the headers the model was compiled with), `ir_hash`, then `create`,
  `destroy`, `eval`, `final`, `set_time`, `events_pending`, `next_time_slot`, `signals`,
  `drain_msgs`, `trace_open`, `trace_dump`, `trace_close`, `cov_counters`. Tracing and
  coverage are stubs (`SIMJIG_UNSUPPORTED`, `NULL`); their slots are reserved.
- Append-only: `abi_version` changes only on an incompatible change, and `size` tells a newer
  runtime which entries exist. The Rust side rejects a different version or a `size` smaller
  than its mirror (`ModelVtable::check`), so it never reads past the model's table.
- `simjig_signal` (32 bytes): `id`, `width`, `words` (`ceil(width/32)`), `storage`
  (C/S/I/Q/W), `dir` (internal/in/out/inout), `access` (read/write bits), `name`, `ptr`.
- `simjig_msg` (48 bytes): `kind`, `line`, `time`, `file`, `hier`, `msg`, `msg_len`. Kinds:
  DISPLAY, WARNING, ERROR, STOP, FATAL, FINISH, EXCEPTION, DROPPED.
- Status: OK 0, FINISH 1, STOP 2, FATAL 3, EXCEPTION 4, POISONED 5, UNSUPPORTED 6.
  FINISH and STOP are sticky and do not poison; FATAL and EXCEPTION poison.
- Status codes and kinds are plain integers (`int32_t` / `#define`), not C enums, so their
  size is fixed. Rust keeps them as integers and decodes them with `TryFrom`, because a Rust
  enum holding an unknown discriminant is UB.
- Each handle owns its own `VerilatedContext`. Every entry point makes it Verilator's thread
  context (`Verilated::threadContextp`), so several models on one thread stay separate.
- `next_time_slot` returns `SIMJIG_TIME_NONE` when nothing is pending, and always without
  `--timing`. Verilator's own `nextTimeSlot()` is a `VL_FATAL_MT` in both cases.

## Compile recipe
This is what `crates/simjig/build.rs` does now and what `simjig-build` should do.

1. **Every** C++ translation unit gets the same flags: libverilated, the generated model,
   the shim and `simjig_rt.cpp`.
   - `-include <cpp>/simjig_vl_config.h -I<cpp>`
   - `-I$VERILATOR_ROOT/include -I$VERILATOR_ROOT/include/vltstd`
   - `-std=gnu++20 -Os` (coroutines are needed for `--timing`; `-Os` is Verilator's
     `OPT_FAST`/`OPT_GLOBAL`)
   - `-DVERILATOR=1 -DVM_COVERAGE=0 -DVM_SC=0 -DVM_TIMING=1 -DVM_TRACE=0 -DVM_TRACE_FST=0
     -DVM_TRACE_VCD=0 -DVM_TRACE_SAIF=0 -DVM_VPI=0`
   - `-w` (Verilator's `CFG_CXXFLAGS_NO_UNUSED` exists only to silence generated-code
     warnings)
2. `simjig_vl_config.h` defines:
   - `VL_USER_FINISH`, `VL_USER_STOP`, `VL_USER_STOP_MAYBE`, `VL_USER_FATAL` and
     `VL_USER_WARN`, which drop Verilator's default hook definitions from `verilated.cpp`;
   - a declaration of `vl_stop_maybe`. 5.052 declares it nowhere else, so `verilated.cpp`
     does not compile with `VL_USER_STOP_MAYBE` without it;
   - `VL_TIME_CONTEXT`, so time comes only from the context. Without it, a context time of 0
     falls back to the weak `sc_time_stamp()`, which is an undefined symbol at link time on
     macOS;
   - `VL_PRINTF`/`VL_VPRINTF` set to `simjig_vl_printf`/`simjig_vl_vprintf`. All
     `$display`/`$write`/`$fatal` text goes through `VL_PRINTF_MT`, which ends in `VL_PRINTF`.
3. **libverilated archive**, shared by every model:
   - `verilated.cpp`, `verilated_threads.cpp`, `verilated_timing.cpp` and `simjig_rt.cpp`;
   - the hooks live in `simjig_rt.cpp`, so they are linked exactly once.

   In 5.052 the runtime sources read only `VM_SC` and `VM_VPI` (and `VM_TRACE` only to set a
   compatibility macro). Generated model code reads no `VM_*` at all (checked with grep). So
   one archive per (Verilator version, compiler, `simjig_vl_config.h` hash) serves every
   model, timing or not. That is the cache key for DECISION §2 #10. Add
   `verilated_fst_c.cpp` (with `-lz`, and lz4 per `verilated.mk`) and `verilated_cov.cpp`
   when tracing and coverage stop being stubs; they will need `VM_TRACE*`/`VM_COVERAGE` to
   match the model.
4. **Per model archive**: `verilator --cc [--timing|--no-timing] --prefix V<name>`, then every
   generated `.cpp` (not `__ALL.cpp`; that file is made by make) plus the generated shim.
5. **Link**: the model archives first, then libverilated (ELF archive order), then `-lc++`
   (macOS) or `-lstdc++ -lpthread` (Linux). The test uses `cargo::rustc-link-arg-tests`, so
   nothing reaches the library or its users.

With `verilator --build` (the spike), passing `-CFLAGS "-include ... -std=gnu++20"` is enough:
`VM_USER_CFLAGS` reaches `verilated.cpp` too. Pitfall: an empty `-LDFLAGS ""` swallows the
next argument.

## Fatal routing (what the hooks see, 5.052)
| SV | Generated code | Hook |
|---|---|---|
| `$fatal` | `VL_WRITEF_NX("[t] %Fatal: f.sv:N: ...")`, then `VL_STOP_MT(f, N, "", false)` | `vl_stop_maybe(maybe=false)` |
| `$error` | `VL_WRITEF_NX("[t] %Error: ...")`, then `VL_STOP_MT(f, N, "")` | `vl_stop_maybe(maybe=true)` |
| `$stop` | `VL_STOP_MT(f, N, "")` | `vl_stop_maybe(maybe=true)` |
| `$finish` | `VL_FINISH_MT(f, N, "")` | `vl_finish` |
| `$warning`/`$info`/`$display` | `VL_WRITEF_NX` | `VL_PRINTF` only |
| internal (`$readmem`, convergence, null deref) | `VL_FATAL_MT` | `vl_fatal` |

Consequences:
- **`$fatal` never reaches `vl_fatal`.** The `hier` argument is always `""`; the `%m` scope is
  only in the printed text.
- **`$stop` and `$error` look the same to the hook.** The runtime tells them apart by the
  `%Error:` text printed just before. That text then becomes the ERROR/FATAL record, so it
  is not also kept as a DISPLAY record.
- `$error` counts toward the error limit, as in Verilator (`create_opts.error_limit`; 0 keeps
  Verilator's default of 1, or `+verilator+error+limit`). Below the limit the model
  continues with OK; at the limit it returns STOP.
- `$stop` always returns STOP. Verilator ignores it below the error limit; simjig follows
  IEEE 1800-2023 §20.2, where `$stop` suspends the simulation. **This is a deviation.**
- `$finish` returns FINISH; Verilator keeps evaluating, and so does simjig.
- A `vl_fatal` caller may be followed by `VL_UNREACHABLE` (for example `nullPointerError` and
  `didNotConverge`). Returning from `vl_fatal` is therefore UB, so it always throws.

## Spike results (`pixi run bash crates/simjig-build/cpp/spike/run.sh`, historical)
| Case | Result |
|---|---|
| `$fatal`, plain model | **Unwinds cleanly.** FATAL plus the message, and the statements after `$fatal` do not run. The NBA update is abandoned (`cnt` stays 0). Afterwards eval/final return POISONED, and destroy works. |
| internal fatal (`$readmemh` syntax error), plain model | Unwinds cleanly: FATAL, message `$readmem file syntax error`, file `bad.hex`. |
| `$stop` | STOP, sticky; the model keeps evaluating. |
| `$error`, limit 1 (default) | STOP on the first one. |
| `$error`, limit 3 | OK, OK, then STOP on the third. Each one is an ERROR record. |
| `$finish` | FINISH, sticky; `final` runs. |
| `$display` | Captured as a DISPLAY record, with the newline. `$write("a"); $write("b\n")` becomes one record. |
| `$warning` | A DISPLAY record (`%Warning:` text); it never reaches a hook. |
| `$fatal` inside a `--timing` coroutine | **Unwinding would abort:** `VlPromise::unhandled_exception()` is `std::abort()` in `verilated_timing.h`. So this falls back: the runtime does not throw for models with `kTiming`. It records, returns from the hook, and eval returns FATAL and poisons. Cost: statements after `$fatal` run until the coroutine next suspends ("after fatal" was printed). |
| internal `vl_fatal` inside a coroutine | **Aborts the process (exit 134).** Returning is UB (see above) and throwing aborts. The runtime writes `simjig: fatal: <file>:<line>: <msg>` to stderr before throwing, so the reason survives. Nothing better is possible without changing Verilator. nextest's process-per-test reports it. |

Sanitizers and leaks:
- **UBSan** (`run.sh ubsan`): 0 runtime errors over all cases, the held path (`hold`,
  `eval_held`, `release`, a hold of a poisoned model: `Vfat held`) included.
- **`leaks --atExit`** (macOS): 0 leaks after the fatal-unwind and destroy cases, plain and
  `--timing`.
- **ASan could not run on macOS.** The conda clang 21 ASan runtime deadlocks in
  `__asan::InitializeShadowMemory` on macOS 27 before `main`.
- On linux-aarch64 (gcc 15.3 runtimes), `run.sh asan` and `run.sh ubsan` both run to the
  end with no sanitizer report; `delay_internal` aborts as expected (exit 134). The ASan
  hang does not occur there.

## Later changes
- Offset 128 is now `cov_runs` (`simjig_cov_run` runs of counters) instead of
  `cov_counters`; the slot had no consumer, so `SIMJIG_ABI_VERSION` stayed 1. See
  tracecov.md, "Coverage".
- `ModelVtable::check(*const Self, ir_hash)` reads `abi_version` and `size` through field
  pointers before it creates a reference. Miri rejects the older `&self` form over a table
  shorter than the mirror ("constructing invalid value ... dangling reference").
- `Signal::decode` decodes storage and direction once and checks
  `storage == for_width(width)` and `words == ceil(width/32)`; the accessors take typed
  narrow pointers or a wide cell, and every match is exhaustive. Miri showed the older
  catch-all arm doing an 8-byte read of a 1-byte member for an unknown storage class.
  Run with `cargo +nightly miri test -p simjig --lib model::abi`.
- Narrow writes must mask: Verilator widens a `CData`/`SData`/`QData` input without
  masking it, so an unmasked write of 5-, 12- and 40-bit inputs reads back as `0xff`,
  `0xffff` and `u64::MAX` in the DUT (tested on `counter.sv`'s `tiny_in`, `mid_in`,
  `quad_in`).

## Measured costs (`cargo test --release -p simjig --test abi_counter -- --ignored --nocapture`)
Apple M6, release, three runs, 50 M iterations:

| Operation | Cost |
|---|---|
| raw read through the table (`read_narrow`, 32-bit `IData`) | 0.36 to 0.52 ns |
| raw write through the table (`write_narrow`, 16-bit `SData`, masked) | 0.46 to 0.51 ns |
| `eval` through the vtable on the counter, no input change (barrier, `Scope` and Verilator's empty eval) | 15.8 to 16.1 ns |

The read and write match on `storage` per access, and the branch predicts perfectly in the
loop. This is the floor under DECISION §4 Q2's 5 ns budget; the kernel lookup comes on top.

## Contradictions and open issues
1. `unsafe` lives in both `model::abi` (the raw read/write helpers, with `// SAFETY:`
   comments) and `model::raw`; `ARCHITECTURE.md` lists both.
2. **Fatal routing, as implemented:**
   - throw in plain models;
   - record and return for `$fatal`/`$stop` in `--timing` models;
   - throw (and so abort, after a stderr record) for internal fatals inside coroutines.

   `_exit` would not help, because the abort happens inside Verilator's promise.
3. `$stop` always returns STOP, unlike Verilator (see above).
4. `destroy` frees poisoned models too, rather than leaking them. The
   spike shows no leaks or UB after unwinding. A throw from a model destructor would still
   `terminate` (destructors are `noexcept`).
5. `--timing` models record `$fatal` and then return, so the generator must set
   `kTiming = true` exactly when it passes `--timing`. A wrong `false` turns `$fatal` in a
   coroutine into an abort.
6. **The test model build lives in `crates/simjig/build.rs`**, because Cargo has no test-only
   build scripts. It runs Verilator on every fresh build of `simjig` (about 10 s). If
   Verilator is missing, it degrades to a cargo warning plus a failing test. It should move
   into `simjig-build` once that crate has the Verilator driver.
7. `cc` (1.8.0, without the `parallel` feature) is a new workspace build-dependency. Why not
   something simpler: driving `c++`/`ar` by hand means reimplementing cc's
   compiler/archiver detection.
8. Signal reads and writes stay correct only while no eval runs. Nothing enforces this yet:
   it is the kernel's job (single thread, the write log applied before eval).

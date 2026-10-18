# n-linux notes: the gate on Linux

Scope: linux-aarch64 only (Ubuntu 26.04 LTS arm64 in a Lima `vz` VM on a macOS arm64
host, 4 vCPUs, 8 GiB). linux-64 (x86_64) is locked in `pixi.lock` but was not run.

## Setup, as a new Linux user
Fresh Ubuntu cloud image, then README's path: pixi installer, rustup
(`--default-toolchain none`; `rust-toolchain.toml` picks 1.99.1), `pixi install --locked`
(5 s; `pixi.lock` already had linux-aarch64), `pixi run cargo xtask setup` (13 min: the
four cargo tools build from source). Nothing failed. Notes:
- The README needs rustup and git first; the image has neither a C compiler nor
  `build-essential`. Under `pixi run`, conda's `cc` links, so the README path works
  without a system compiler; CONTRIBUTING's bare `cargo xtask setup` (outside pixi)
  would need one to link the xtask binary. The Lima provision installs
  `build-essential` so both work. Not verified without it.
- The pixi installer pings telemetry unless `PIXI_NO_TELEMETRY=1` is set before it.

## Failures found, and fixes
1. **Models did not link** (`undefined reference to symbol 'exp2@@GLIBC_2.17'`, then
   `__aarch64_ldadd4_acq_rel`). rustc links with `cc -nodefaultlibs`; std's `-lm`
   `-lpthread` and `compiler_builtins` come before the `rustc-link-arg` archives, where
   `--as-needed` and archive order drop them. `compile::system_libs` adds `-lm
   -lpthread` (Linux) and `-lgcc` (GNU) after the C++ stdlib, as g++ and
   `verilated.mk` do. Test: `compile::tests::linux_links_libm_libpthread_and_libgcc_after_the_archives`;
   the Linux gate itself is the end-to-end proof.
2. **Fresh builds were never cached** on Linux: every DUT warned `changed while
   verilating (or its time is in the future)`. The generated `.vlt` is stored just before
   the run and Linux's coarse file clock gave it the work directory's exact time, which
   `Manifest::record` must treat as changed. `Snapshot::start` now waits for the
   filesystem clock to tick past the work directory (≤ 50 probes, 1 ms apart; immediate
   on macOS). Test: `manifest::tests::a_dependency_written_just_before_the_start_is_stable`,
   which fails on Linux without the fix (verified in the VM) and passes on macOS either
   way.
3. **`doctor` warned `CXX unset`** under pixi on Linux (2 tests failed): conda-forge's
   `gxx` has no activation script. Environment-only fix in `pixi.toml`
   (`[target.linux-*.activation.env] CXX`); the two existing doctor tests are the test.
4. **Four `simjig-build::it` cache tests timed out** at 2 min. Not a hang: each takes 7 s
   alone and 26 s four at once (CPU-bound, vmstat 0% idle); in the full gate on 4 vCPUs
   several took 60-120 s. Environment-only: `.config/nextest.toml` gives the binary the
   6-minute timeout the other cold-cache tests have.

Checked and fine on Linux: `fork()` checkpoints and `threads(1)` detection
(`fork_checkpoints` tests pass), `-MD` dependency paths, path canonicalization, glibc
`strtol` (the `--threads` tests), the C-level `cpp-tests`.

## Repeatable
`ci/lima/simjig.yaml` and `ci/lima/run.sh`; see CONTRIBUTING.md ("Linux"). The script
syncs the host checkout's committed HEAD by `git fetch` into the guest's `~/simjig`.

## Results (VM, commit 027b064)
- `pixi run cargo xtask ci`: all checks passed. nextest 991 passed, 18 skipped (306 s);
  doctests 46 passed, 7 ignored; cpp-tests 0 failures. Wall time 5 min 18 s warm.
- `pixi run -e conformance conformance-regen --check`: 33 goldens reproduced exactly.
- `cargo xtask miri` (nightly 1.101.0-nightly 2026-10-17, unpinned): 93 + 115 passed,
  1 + 1 ignored.
- ASan and UBSan (`cpp/spike/run.sh asan|ubsan`, gcc 15.3 runtimes): both run to the
  end with no sanitizer report; `delay_internal` aborts as expected (exit 134). The
  macOS hang in `__asan::InitializeShadowMemory` (abi.md) does not occur on Linux.
- macOS host gate on the same commits: 991 passed, 18 skipped.

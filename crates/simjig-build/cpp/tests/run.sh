#!/usr/bin/env bash
# C-level tests of FST tracing, coverage and per-model isolation (cpp/tests/main.cpp).
# Run from the repository root: pixi run cpp-tests
# Builds four models into one executable the way the abi spike does (cpp/spike/run.sh): the
# other models are verilated first and compiled in as plain --exe sources, and the main model
# (--trace-fst --coverage) brings libverilated with the FST writer and coverage runtime.
set -euo pipefail
VERILATOR_ROOT="${VERILATOR_ROOT:-$(verilator --getenv VERILATOR_ROOT)}"
here="$(cd "$(dirname "$0")" && pwd)"
cpp="$(dirname "$here")"
out="${CARGO_TARGET_DIR:-$PWD/target}/tracecov-tests"
rm -rf "$out"
mkdir -p "$out/scratch"
vl() { verilator --cc --threads 1 "$@" >/dev/null; }
vl --no-timing "$here/tc.sv" --top-module tc --Mdir "$out/plain" --prefix Vtc_plain
vl --no-timing --coverage-line "$here/tc_hier.sv" --Mdir "$out/hier" --prefix Vtc_hier
vl --timing --trace-fst "$here/tc_timing.sv" --Mdir "$out/timing" --prefix Vtc_timing
others=$(ls "$out"/plain/*.cpp "$out"/hier/*.cpp "$out"/timing/*.cpp | grep -v '__ALL')
cflags="-include $cpp/simjig_vl_config.h -I$cpp -std=gnu++20 -I$out/plain -I$out/hier -I$out/timing"
verilator --cc --exe --build -j 4 --threads 1 --no-timing --trace-fst --coverage \
  "$here/tc.sv" --top-module tc --Mdir "$out/full" --prefix Vtc_full -o tracecov_tests \
  -CFLAGS "$cflags" \
  "$here/main.cpp" "$here/traits.cpp" "$cpp/simjig_rt.cpp" $others \
  "$VERILATOR_ROOT/include/verilated_timing.cpp" >/dev/null
"$out/full/tracecov_tests" "$out/scratch"

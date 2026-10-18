#!/usr/bin/env bash
# Spike: does throwing through Verilator-generated code unwind cleanly?
# Historical (docs/notes/abi.md): it prints its results and asserts nothing, so no gate runs
# it; crates/simjig/tests/abi_counter.rs tests the behaviours it showed.
# Run from the repository root: pixi run bash crates/simjig-build/cpp/spike/run.sh [asan|ubsan]
# Builds each model with `verilator --cc --exe --build`, adding the simjig runtime and the
# forced-include config to *every* compile (VM_USER_CFLAGS reaches libverilated too).
set -euo pipefail
VERILATOR_ROOT="${VERILATOR_ROOT:-$(verilator --getenv VERILATOR_ROOT)}"
here="$(cd "$(dirname "$0")" && pwd)"
cpp="$(dirname "$here")"
out="${CARGO_TARGET_DIR:-$PWD/target}/abi-spike${1:+-$1}"
mkdir -p "$out"
extra=""
ldflags=()
case "${1:-}" in
  asan) extra="-fsanitize=address,undefined -fno-omit-frame-pointer -g" ;;
  ubsan) extra="-fsanitize=undefined -fno-sanitize-recover=undefined -g" ;;
esac
if [[ -n "$extra" ]]; then ldflags=(-LDFLAGS "$extra"); fi
cflags="-include $cpp/simjig_vl_config.h -I$cpp -std=gnu++20 $extra"
# One Verilator run per model, but one executable: verilate the two other models first, then
# build the executable from the third, compiling the others' sources in as plain --exe files.
for m in delay delay_internal; do
  verilator --cc --timing "$here/$m.sv" --Mdir "$out/$m" --prefix "V$m" >/dev/null
done
verilator --cc --exe --build -j 4 "$here/fat.sv" --Mdir "$out/fat" --prefix Vfat \
  -CFLAGS "$cflags -I$out/delay -I$out/delay_internal" ${ldflags[@]+"${ldflags[@]}"} \
  "$here/driver.cpp" "$here/shims.cpp" "$cpp/simjig_rt.cpp" \
  $(ls "$out"/delay/*.cpp "$out"/delay_internal/*.cpp | grep -v '__ALL') \
  "$VERILATOR_ROOT/include/verilated_timing.cpp" >/dev/null
cd "$here"  # bad.hex is read relative to the working directory
"$out/fat/Vfat" fat
"$out/fat/Vfat" delay
echo "== delay_internal (internal fatal inside a coroutine; expected to abort) =="
set +e
"$out/fat/Vfat" delay_internal
echo "exit status: $?"

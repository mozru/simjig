#!/usr/bin/env bash
# Reproduces every number in bench/README.md. Not part of `cargo xtask ci`.
#
#   bench/run.sh            # env, build, bug check, then every measurement (building
#                           # nothing); writes bench/results/{env,bug}.txt, raw.jsonl, table.md
#   bench/run.sh --design mid
#                           # the same for the mid-size design; writes bench/results/mid/
#   bench/run.sh run --sizes 10000 --runs 1   # any subcommand of bench/bench.py
#   bench/run.sh --design mid run --sizes 10000 --runs 1   # the same, for the mid design
#   BENCH_RUN_ARGS="--sizes 1000000 --flows simjig,cocotb,marlin,cpp" bench/run.sh
#                           # the whole sequence, with these `run` options
#
# Designs: fifo (the default) and mid. Flows: simjig (each design's plain and `_pub`
# models), cocotb, Marlin and raw C++ (bench/README.md).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.pixi/bin:$HOME/.cargo/bin:$PATH" PIXI_NO_TELEMETRY=1
py() { pixi run --manifest-path "$root/pixi.toml" -e conformance python "$root/bench/bench.py" "$@"; }
design=fifo
if [ "${1:-}" = "--design" ]; then
    design="${2:?--design needs fifo or mid}"
    shift 2
fi
if [ $# -gt 0 ]; then
    py "$1" --design "$design" "${@:2}"
    exit
fi
results="$root/bench/results"
[ "$design" = fifo ] || results="$results/$design"
mkdir -p "$results"
py env | tee "$results/env.txt"
py build --design "$design"
py bug --design "$design" | tee "$results/bug.txt"
raw="$results/raw.jsonl"
rm -f "$raw"
# BENCH_RUN_ARGS: extra `run` options, such as "--sizes 10000 --runs 1" (word-split on purpose).
# shellcheck disable=SC2086
py run --design "$design" --out "$raw" ${BENCH_RUN_ARGS:-}
py table "$raw" | tee "$results/table.md"

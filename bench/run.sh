#!/usr/bin/env bash
# Reproduces every number in bench/README.md. Not part of `cargo xtask ci`.
#
#   bench/run.sh            # env, build, bug check, then every measurement (building
#                           # nothing); writes bench/results/{env,bug}.txt, raw.jsonl, table.md
#   bench/run.sh run --sizes 10000 --runs 1   # any subcommand of bench/bench.py
#   BENCH_RUN_ARGS="--sizes 1000000 --flows simjig,cocotb,marlin,cpp" bench/run.sh
#                           # the whole sequence, with these `run` options
#
# Flows: simjig (models fifo and fifo_pub), cocotb, Marlin and raw C++ (bench/README.md).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.pixi/bin:$HOME/.cargo/bin:$PATH" PIXI_NO_TELEMETRY=1
py() { pixi run --manifest-path "$root/pixi.toml" -e conformance python "$root/bench/bench.py" "$@"; }
if [ $# -gt 0 ]; then
    py "$@"
    exit
fi
mkdir -p "$root/bench/results"
py env | tee "$root/bench/results/env.txt"
py build
py bug | tee "$root/bench/results/bug.txt"
raw="$root/bench/results/raw.jsonl"
rm -f "$raw"
# BENCH_RUN_ARGS: extra `run` options, such as "--sizes 10000 --runs 1" (word-split on purpose).
# shellcheck disable=SC2086
py run --out "$raw" ${BENCH_RUN_ARGS:-}
py table "$raw" | tee "$root/bench/results/table.md"

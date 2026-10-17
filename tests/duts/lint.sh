#!/usr/bin/env bash
# Lint every shared DUT with the pinned Verilator: `pixi run duts-lint`.
# Each line is: <dir> <top> <extra flags...> -- <sources in compile order>. Designs with a
# parameter are linted at more than one value. Any warning fails the run (-Wall, and
# Verilator exits non-zero on warnings unless -Wno-fatal).
set -euo pipefail
cd "$(dirname "$0")"

lint() {
    local dir=$1 top=$2; shift 2
    local flags=() srcs=()
    while [[ $1 != -- ]]; do flags+=("$1"); shift; done
    shift
    for s in "$@"; do srcs+=("$dir/$s"); done
    echo "lint $dir ${flags[*]-}"
    verilator --lint-only -Wall --top-module "$top" ${flags[@]+"${flags[@]}"} "${srcs[@]}"
}

lint inv inv -- inv.sv
lint flop flop -- flop.sv
lint counter counter -- counter.sv
lint counter counter -GWIDTH=33 -- counter.sv
lint comb_chain comb_chain -- comb_chain.sv
lint fifo fifo -GDEPTH=2 -- fifo.sv
lint fifo fifo -GDEPTH=4 -- fifo.sv
lint fifo fifo -GDEPTH=16 -GWIDTH=33 -- fifo.sv
lint struct_pipe struct_pipe -- struct_pipe_pkg.sv struct_pipe.sv
lint wide wide -- wide.sv
lint async_fifo async_fifo -GDEPTH=4 -- async_fifo.sv
lint async_fifo async_fifo -GDEPTH=8 -- async_fifo.sv
lint async_fifo async_fifo -GDEPTH=32 -GWIDTH=65 -- async_fifo.sv
lint timing_dut timing_dut --timing -- timing_dut.sv
lint fatal fatal -- fatal.sv
for k in 0 1 2 3; do lint fatal fatal -GAT_START=$k -- fatal.sv; done
lint iface_top iface_top -- iface_pkg.sv bus_if.sv iface_top.sv
lint iface_top iface_top_wrap -- iface_pkg.sv bus_if.sv iface_top.sv iface_top_wrap.sv
lint dpi dpi -- dpi.sv
lint derived_clk derived_clk -- derived_clk.sv
echo "duts-lint: all designs clean"

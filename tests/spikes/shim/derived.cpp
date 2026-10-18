// derived_clk (tests/duts/derived_clk) with rtl/derived_clk.vlt (div_clk public_flat_rd).
// internal_op 0: the public_flat_rd copy derived_clk__DOT__div_clk (the port is id 2).
#define SPK_PORTS(X) X(0, clk) X(1, rst) X(2, div_clk) X(3, fast_count) X(4, slow_count)
#define SPK_INTERNAL_OP                                                                  \
    if (op == 0) return top->rootp->derived_clk__DOT__div_clk;
#include "model.inc"

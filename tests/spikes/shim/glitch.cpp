// glitch (spike rtl) with rtl/glitch.vlt (pulse public_flat_rd).
// internal_op 0: the internal derived clock glitch__DOT__pulse.
#define SPK_PORTS(X) X(0, clk) X(1, pulses)
#define SPK_INTERNAL_OP                                                                  \
    if (op == 0) return top->rootp->glitch__DOT__pulse;
#include "model.inc"

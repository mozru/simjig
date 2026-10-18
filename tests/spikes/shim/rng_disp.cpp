// rng_disp (spike rtl): compiled twice, as Vrng_a and Vrng_b, to put the same design in
// one binary under two prefixes.
#define SPK_PORTS(X) X(0, clk) X(1, rnd)
#include "model.inc"

// counter (tests/duts/counter), --trace-fst. Used by the fork and multi-model spikes.
#define SPK_PORTS(X) X(0, clk) X(1, rst) X(2, en) X(3, count)
#include "model.inc"

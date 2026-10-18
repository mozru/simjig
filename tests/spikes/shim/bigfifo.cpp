// fifo (tests/duts/fifo) at DEPTH=2**23, WIDTH=64: a 64 MiB memory for fork/CoW timing.
// internal_op 0: size of the memory in bytes.
// internal_op 1: write `arg` into one element per 4 KiB (dirties every page); returns the
//                number of elements written.
// internal_op 2: sum of one element per 4 KiB (reads every page).
#define SPK_PORTS(X)                                                                     \
    X(0, clk) X(1, rst) X(2, in_valid) X(3, in_ready) X(4, in_data) X(5, out_valid)      \
        X(6, out_ready) X(7, out_data) X(8, level)
#define SPK_INTERNAL_OP                                                                  \
    auto& mem = top->rootp->fifo__DOT__mem;                                              \
    constexpr uint64_t kStride = 4096 / sizeof(QData);                                   \
    constexpr uint64_t kDepth = sizeof(mem) / sizeof(QData);                             \
    switch (op) {                                                                        \
    case 0: return sizeof(mem);                                                          \
    case 1: {                                                                            \
        uint64_t n = 0;                                                                  \
        for (uint64_t i = 0; i < kDepth; i += kStride, ++n) mem[i] = arg;                \
        return n;                                                                        \
    }                                                                                    \
    case 2: {                                                                            \
        uint64_t sum = 0;                                                                \
        for (uint64_t i = 0; i < kDepth; i += kStride) sum += mem[i];                    \
        return sum;                                                                      \
    }                                                                                    \
    default: break;                                                                      \
    }
#include "model.inc"

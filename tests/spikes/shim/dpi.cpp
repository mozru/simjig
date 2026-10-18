// dpi (tests/duts/dpi) WITHOUT dpi_impl.c: dpi_add and dpi_tick come from Rust
// (tests/dpi.rs). internal_op 0 calls dpi_add `arg` times through a volatile function
// pointer (Rust side, catch_unwind inside); internal_op 1 does the same with a C++
// function of the same body; internal_op 2 with a plain Rust extern "C" fn (no guard).
// They isolate the cost of one C++ -> Rust call.
#include <cstdint>
extern "C" int32_t dpi_add(int32_t x, int32_t y);
extern "C" int32_t spk_rust_add_plain(int32_t x, int32_t y);
namespace {
__attribute__((noinline)) int32_t cxx_add(int32_t x, int32_t y) {
    return static_cast<int32_t>(static_cast<uint32_t>(x) + static_cast<uint32_t>(y));
}
uint64_t call_loop(int32_t (*volatile fn)(int32_t, int32_t), uint64_t n) {
    int32_t acc = 0;
    for (uint64_t i = 0; i < n; ++i) acc = fn(acc, 1);
    return static_cast<uint64_t>(acc);
}
}  // namespace
#define SPK_PORTS(X) X(0, clk) X(1, a) X(2, b) X(3, sum) X(4, calls)
#define SPK_INTERNAL_OP                                                                  \
    switch (op) {                                                                        \
    case 0: return call_loop(&dpi_add, arg);                                             \
    case 1: return call_loop(&cxx_add, arg);                                             \
    case 2: return call_loop(&spk_rust_add_plain, arg);                                  \
    default: break;                                                                      \
    }
#include "model.inc"

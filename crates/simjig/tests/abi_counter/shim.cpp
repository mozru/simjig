// Hand-instantiated simjig shim for the test models: exactly what simjig-build will generate.
#include "Vcounter.h"
#include "Vticker.h"
#include "simjig_model.h"

namespace {

constexpr uint8_t kRead = SIMJIG_ACCESS_READ;
constexpr uint8_t kReadWrite = SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE;

struct CounterTraits {
    using Model = Vcounter;
    static constexpr const char* kPrefix = "Vcounter";
    static constexpr uint64_t kIrHash = 0x5eed'c0de'0000'0001ull;
    static constexpr bool kTiming = false;
    static void bind(Vcounter& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "counter.clk", 1, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.rst, "counter.rst", 1, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.cmd, "counter.cmd", 8, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.half_in, "counter.half_in", 16, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.wide_in, "counter.wide_in", 100, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.count, "counter.count", 32, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.half_out, "counter.half_out", 16, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.quad_out, "counter.quad_out", 40, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.wide_out, "counter.wide_out", 100, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.tiny_in, "counter.tiny_in", 5, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.mid_in, "counter.mid_in", 12, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.quad_in, "counter.quad_in", 40, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.tiny_ext, "counter.tiny_ext", 8, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.mid_ext, "counter.mid_ext", 16, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.quad_ext, "counter.quad_ext", 64, SIMJIG_DIR_OUT, kRead);
        SIMJIG_SIGNAL(t, m.all_ones, "counter.all_ones", 1, SIMJIG_DIR_OUT, kRead);
    }
};

struct TickerTraits {
    using Model = Vticker;
    static constexpr const char* kPrefix = "Vticker";
    static constexpr uint64_t kIrHash = 0x5eed'c0de'0000'0002ull;
    static constexpr bool kTiming = true;
    static void bind(Vticker& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.arm, "ticker.arm", 1, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.ticks, "ticker.ticks", 8, SIMJIG_DIR_OUT, kRead);
    }
};

}  // namespace

SIMJIG_EXPORT_VTABLE(Vcounter, CounterTraits)
SIMJIG_EXPORT_VTABLE(Vticker, TickerTraits)

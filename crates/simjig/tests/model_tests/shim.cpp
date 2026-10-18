// Hand-instantiated simjig shims for the model tests: what simjig-build will generate per
// model. Slot order, names, widths and access must match the `SIGNALS` tables in `duts.rs`.
#include "Vdut_comb_chain.h"
#include "Vdut_comb_chain___024root.h"
#include "Vdut_counter.h"
#include "Vdut_fatal.h"
#include "Vdut_flop.h"
#include "Vdut_inv.h"
#include "Vdut_timing.h"
#include "Vdut_wide.h"
#include "Vhello.h"
#include "simjig_model.h"

namespace {

constexpr uint8_t kRead = SIMJIG_ACCESS_READ;
constexpr uint8_t kReadWrite = SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE;
constexpr uint8_t kIn = SIMJIG_DIR_IN;
constexpr uint8_t kOut = SIMJIG_DIR_OUT;

struct InvTraits {
    using Model = Vdut_inv;
    static constexpr const char* kPrefix = "Vdut_inv";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0001ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.a, "inv.a", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.y, "inv.y", 1, kOut, kRead);
    }
};

struct FlopTraits {
    using Model = Vdut_flop;
    static constexpr const char* kPrefix = "Vdut_flop";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0002ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "flop.clk", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.rst, "flop.rst", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.d, "flop.d", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.q, "flop.q", 1, kOut, kRead);
    }
};

struct CounterTraits {
    using Model = Vdut_counter;
    static constexpr const char* kPrefix = "Vdut_counter";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0003ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "counter.clk", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.rst, "counter.rst", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.en, "counter.en", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.count, "counter.count", 8, kOut, kRead);
    }
};

struct CombChainTraits {
    using Model = Vdut_comb_chain;
    static constexpr const char* kPrefix = "Vdut_comb_chain";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0004ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.a, "comb_chain.a", 8, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.b, "comb_chain.b", 8, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.y, "comb_chain.y", 8, kOut, kRead);
        SIMJIG_SIGNAL(t, m.y_eq, "comb_chain.y_eq", 1, kOut, kRead);
        SIMJIG_SIGNAL(t, m.rootp->comb_chain__DOT__s1, "comb_chain.s1", 8, SIMJIG_DIR_INTERNAL,
                      kRead);
        SIMJIG_SIGNAL(t, m.rootp->comb_chain__DOT__s2, "comb_chain.s2", 8, SIMJIG_DIR_INTERNAL,
                      kRead);
        SIMJIG_SIGNAL(t, m.rootp->comb_chain__DOT__s3, "comb_chain.s3", 8, SIMJIG_DIR_INTERNAL,
                      kRead);
    }
};

struct FatalTraits {
    using Model = Vdut_fatal;
    static constexpr const char* kPrefix = "Vdut_fatal";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0005ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "fatal.clk", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.rst, "fatal.rst", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.fire, "fatal.fire", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.kind, "fatal.kind", 2, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.alive, "fatal.alive", 8, kOut, kRead);
    }
};

struct WideTraits {
    using Model = Vdut_wide;
    static constexpr const char* kPrefix = "Vdut_wide";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0006ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "wide.clk", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i1, "wide.i1", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i8, "wide.i8", 8, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i33, "wide.i33", 33, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i64, "wide.i64", 64, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i65, "wide.i65", 65, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.i200, "wide.i200", 200, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.o1, "wide.o1", 1, kOut, kRead);
        SIMJIG_SIGNAL(t, m.o8, "wide.o8", 8, kOut, kRead);
        SIMJIG_SIGNAL(t, m.o33, "wide.o33", 33, kOut, kRead);
        SIMJIG_SIGNAL(t, m.o64, "wide.o64", 64, kOut, kRead);
        SIMJIG_SIGNAL(t, m.o65, "wide.o65", 65, kOut, kRead);
        SIMJIG_SIGNAL(t, m.o200, "wide.o200", 200, kOut, kRead);
        SIMJIG_SIGNAL(t, m.q200, "wide.q200", 200, kOut, kRead);
    }
};

struct TimingTraits {
    using Model = Vdut_timing;
    static constexpr const char* kPrefix = "Vdut_timing";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0007ull;
    static constexpr bool kTiming = true;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.start, "timing_dut.start", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.iclk_out, "timing_dut.iclk_out", 1, kOut, kRead);
        SIMJIG_SIGNAL(t, m.ticks, "timing_dut.ticks", 8, kOut, kRead);
        SIMJIG_SIGNAL(t, m.done, "timing_dut.done", 1, kOut, kRead);
    }
};

struct HelloTraits {
    using Model = Vhello;
    static constexpr const char* kPrefix = "Vhello";
    static constexpr uint64_t kIrHash = 0x5eed'0000'0000'0008ull;
    static constexpr bool kTiming = false;
    static void bind(Model& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "hello.clk", 1, kIn, kReadWrite);
        SIMJIG_SIGNAL(t, m.n, "hello.n", 8, kOut, kRead);
    }
};

}  // namespace

SIMJIG_EXPORT_VTABLE(Vdut_inv, InvTraits)
SIMJIG_EXPORT_VTABLE(Vdut_flop, FlopTraits)
SIMJIG_EXPORT_VTABLE(Vdut_counter, CounterTraits)
SIMJIG_EXPORT_VTABLE(Vdut_comb_chain, CombChainTraits)
SIMJIG_EXPORT_VTABLE(Vdut_fatal, FatalTraits)
SIMJIG_EXPORT_VTABLE(Vdut_wide, WideTraits)
SIMJIG_EXPORT_VTABLE(Vdut_timing, TimingTraits)
SIMJIG_EXPORT_VTABLE(Vhello, HelloTraits)

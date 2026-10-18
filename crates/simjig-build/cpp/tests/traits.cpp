// Traits for the test models, written exactly as simjig-build's shim emitter writes them
// (crates/simjig-build/src/codegen/shim.rs; compare its snapshots).
#include "Vtc_full.h"
#include "Vtc_full__Syms.h"
#include "Vtc_full___024root.h"
#include "Vtc_hier.h"
#include "Vtc_hier__Syms.h"
#include "Vtc_hier___024root.h"
#include "Vtc_plain.h"
#include "Vtc_timing.h"
#include "simjig_model.h"

namespace {

constexpr uint8_t kRead = SIMJIG_ACCESS_READ;
constexpr uint8_t kReadWrite = SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE;

template <class M>
void bind_tc(M& m, simjig::SignalTable& t) {
    SIMJIG_SIGNAL(t, m.clk, "tc.clk", 1, SIMJIG_DIR_IN, kReadWrite);
    SIMJIG_SIGNAL(t, m.rst, "tc.rst", 1, SIMJIG_DIR_IN, kReadWrite);
    SIMJIG_SIGNAL(t, m.cnt, "tc.cnt", 8, SIMJIG_DIR_OUT, kRead);
    SIMJIG_SIGNAL(t, m.rnd, "tc.rnd", 32, SIMJIG_DIR_OUT, kRead);
    SIMJIG_SIGNAL(t, m.odd, "tc.odd", 1, SIMJIG_DIR_OUT, kRead);
}

struct FullTraits {
    using Model = Vtc_full;
    static constexpr const char* kPrefix = "Vtc_full";
    static constexpr uint64_t kIrHash = 0x7c00000000000001ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = true;
    static constexpr bool kCoverage = true;
    static void bind(Vtc_full& m, simjig::SignalTable& t) { bind_tc(m, t); }
};

struct PlainTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000002ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) { bind_tc(m, t); }
};

struct HierTraits {
    using Model = Vtc_hier;
    static constexpr const char* kPrefix = "Vtc_hier";
    static constexpr uint64_t kIrHash = 0x7c00000000000003ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = true;
    static void bind(Vtc_hier& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "tc_hier.clk", 1, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.a, "tc_hier.a", 1, SIMJIG_DIR_IN, kReadWrite);
    }
};

struct TimingTraits {
    using Model = Vtc_timing;
    static constexpr const char* kPrefix = "Vtc_timing";
    static constexpr uint64_t kIrHash = 0x7c00000000000004ull;
    static constexpr bool kTiming = true;
    static constexpr bool kTraceFst = true;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_timing& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.ticks, "tc_timing.ticks", 8, SIMJIG_DIR_OUT, kRead);
    }
};

}  // namespace

SIMJIG_EXPORT_VTABLE(Vtc_full, FullTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain, PlainTraits)
SIMJIG_EXPORT_VTABLE(Vtc_hier, HierTraits)
SIMJIG_EXPORT_VTABLE(Vtc_timing, TimingTraits)

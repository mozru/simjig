// Traits for the test models, written exactly as simjig-build's shim emitter writes them
// (crates/simjig-build/src/codegen/shim.rs; compare its snapshots).
#include "Vtc_cg.h"
#include "Vtc_cg__Syms.h"
#include "Vtc_cg___024root.h"
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
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "r", "tc.u.r", 1, kRead);
    }
};

// The same model with its probe bound 5 bits wide: the storage class (CData) still matches, so
// only the create-time check against Verilator's metadata can catch it.
struct BadProbeTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000005ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "r", "tc.u.r", 5, kRead);
    }
};

// The localparam tc_sub.LP, read-only (allowed: the value is readable) and read-write (refused
// at create: the variable is constexpr storage, and a write through it is undefined).
struct ParamReadTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000006ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "LP", "tc.u.LP", 8, kRead);
    }
};
struct ParamWriteTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000007ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "LP", "tc.u.LP", 8, kReadWrite);
    }
};
// A probe of a scope the model has no public variable in, and of a variable the scope lacks:
// create says which one is missing.
struct NoScopeTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000009ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.v", "r", "tc.v.r", 1, kRead);
    }
};
struct NoVarTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c0000000000000aull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "q", "tc.u.q", 1, kRead);
    }
};
// tc_sub.r is public_flat_rd only: a read-write probe of it is refused too.
struct ReadOnlyWriteTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c00000000000008ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "r", "tc.u.r", 1, kReadWrite);
    }
};

// tc_sub.rv is a `real`: Verilator records it as 1 bit wide, so only its type refuses it.
struct RealProbeTraits {
    using Model = Vtc_plain;
    static constexpr const char* kPrefix = "Vtc_plain";
    static constexpr uint64_t kIrHash = 0x7c0000000000000bull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = false;
    static void bind(Vtc_plain& m, simjig::SignalTable& t) {
        bind_tc(m, t);
        SIMJIG_PROBE(t, m, "tc.u", "rv", "tc.u.rv", 1, kRead);
    }
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

struct CgTraits {
    using Model = Vtc_cg;
    static constexpr const char* kPrefix = "Vtc_cg";
    static constexpr uint64_t kIrHash = 0x7c00000000000009ull;
    static constexpr bool kTiming = false;
    static constexpr bool kTraceFst = false;
    static constexpr bool kCoverage = true;
    static void bind(Vtc_cg& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "tc_cg.clk", 1, SIMJIG_DIR_IN, kReadWrite);
        SIMJIG_SIGNAL(t, m.a, "tc_cg.a", 2, SIMJIG_DIR_IN, kReadWrite);
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
SIMJIG_EXPORT_VTABLE(Vtc_plain_badprobe, BadProbeTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_param_rd, ParamReadTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_param_rw, ParamWriteTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_rd_as_rw, ReadOnlyWriteTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_no_scope, NoScopeTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_no_var, NoVarTraits)
SIMJIG_EXPORT_VTABLE(Vtc_plain_real, RealProbeTraits)
SIMJIG_EXPORT_VTABLE(Vtc_hier, HierTraits)
SIMJIG_EXPORT_VTABLE(Vtc_cg, CgTraits)
SIMJIG_EXPORT_VTABLE(Vtc_timing, TimingTraits)

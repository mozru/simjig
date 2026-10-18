// Hand-instantiated traits for the spike models (what simjig-build will generate).
#include "Vfat.h"
#include "Vdelay.h"
#include "Vdelay_internal.h"
#include "simjig_model.h"

namespace {
constexpr uint8_t kRw = SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE;
struct FatTraits {
    using Model = Vfat;
    static constexpr const char* kPrefix = "Vfat";
    static constexpr uint64_t kIrHash = 1;
    static constexpr bool kTiming = false;
    static void bind(Vfat& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.clk, "clk", 1, SIMJIG_DIR_IN, kRw);
        SIMJIG_SIGNAL(t, m.sel, "sel", 8, SIMJIG_DIR_IN, kRw);
        SIMJIG_SIGNAL(t, m.cnt, "cnt", 32, SIMJIG_DIR_OUT, SIMJIG_ACCESS_READ);
    }
};
struct DelayTraits {
    using Model = Vdelay;
    static constexpr const char* kPrefix = "Vdelay";
    static constexpr uint64_t kIrHash = 2;
    static constexpr bool kTiming = true;
    static void bind(Vdelay& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.go, "go", 1, SIMJIG_DIR_IN, kRw);
        SIMJIG_SIGNAL(t, m.stage, "stage", 8, SIMJIG_DIR_OUT, SIMJIG_ACCESS_READ);
    }
};
struct DelayInternalTraits {
    using Model = Vdelay_internal;
    static constexpr const char* kPrefix = "Vdelay_internal";
    static constexpr uint64_t kIrHash = 3;
    static constexpr bool kTiming = true;
    static void bind(Vdelay_internal& m, simjig::SignalTable& t) {
        SIMJIG_SIGNAL(t, m.stage, "stage", 8, SIMJIG_DIR_OUT, SIMJIG_ACCESS_READ);
    }
};
}  // namespace

SIMJIG_EXPORT_VTABLE(Vfat, FatTraits)
SIMJIG_EXPORT_VTABLE(Vdelay, DelayTraits)
SIMJIG_EXPORT_VTABLE(Vdelay_internal, DelayInternalTraits)

// Spike driver: exercises each failure path through the C ABI only (no Verilator headers).
#include "simjig_abi.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

extern "C" const simjig_model_vtable* simjig_vtable_Vfat(void);
extern "C" const simjig_model_vtable* simjig_vtable_Vdelay(void);
extern "C" const simjig_model_vtable* simjig_vtable_Vdelay_internal(void);

static const char* kStatus[] = {"OK", "FINISH", "STOP", "FATAL", "EXCEPTION", "POISONED", "UNSUPPORTED"};
static const char* kKind[] = {"DISPLAY", "WARNING", "ERROR", "STOP", "FATAL", "FINISH", "EXCEPTION", "DROPPED"};

static void drain(const simjig_model_vtable* vt, simjig_handle* h) {
    simjig_msg msgs[16];
    size_t n = vt->drain_msgs(h, msgs, 16);
    for (size_t i = 0; i < n; ++i) {
        std::printf("    msg %-9s t=%llu %s:%u hier='%s' '%s'\n", kKind[msgs[i].kind],
                    (unsigned long long)msgs[i].time, msgs[i].file, msgs[i].line, msgs[i].hier,
                    msgs[i].msg);
    }
}

static void* sig(const simjig_model_vtable* vt, simjig_handle* h, const char* name) {
    uint32_t n = 0;
    const simjig_signal* s = vt->signals(h, &n);
    for (uint32_t i = 0; i < n; ++i)
        if (std::strcmp(s[i].name, name) == 0) return s[i].ptr;
    std::abort();
}

// One clock cycle of fat with `sel`; prints each eval status.
static void fat_case(int sel, uint32_t error_limit) {
    const simjig_model_vtable* vt = simjig_vtable_Vfat();
    simjig_create_opts opts{sizeof(opts), 0, "TOP", error_limit, 0, nullptr};
    simjig_handle* h = nullptr;
    simjig_status st = vt->create(&opts, &h);
    std::printf("fat sel=%d limit=%u: create=%s\n", sel, error_limit, kStatus[st]);
    auto* clk = static_cast<uint8_t*>(sig(vt, h, "clk"));
    auto* s = static_cast<uint8_t*>(sig(vt, h, "sel"));
    auto* cnt = static_cast<uint32_t*>(sig(vt, h, "cnt"));
    *s = static_cast<uint8_t>(sel);
    for (int cycle = 0; cycle < 3; ++cycle) {
        *clk = 0;
        vt->set_time(h, 10 * cycle);
        st = vt->eval(h);
        *clk = 1;
        vt->set_time(h, 10 * cycle + 5);
        st = vt->eval(h);
        std::printf("  cycle %d: eval=%s cnt=%u\n", cycle, kStatus[st], *cnt);
        drain(vt, h);
    }
    st = vt->final(h);
    std::printf("  final=%s\n", kStatus[st]);
    drain(vt, h);
    vt->destroy(h);
    std::printf("  destroyed\n");
}

// fat_case through the held path a synchronous run takes (hold, eval_held, release): the
// same failures behind eval_held's barrier, a release that ends the hold, a second release
// and a NULL one, and a hold of a model the run poisoned.
static void fat_held_case(int sel) {
    const simjig_model_vtable* vt = simjig_vtable_Vfat();
    simjig_create_opts opts{sizeof(opts), 0, "TOP", 0, 0, nullptr};
    simjig_handle* h = nullptr;
    simjig_status st = vt->create(&opts, &h);
    std::printf("fat held sel=%d: create=%s hold=%s\n", sel, kStatus[st], kStatus[vt->hold(h)]);
    auto* clk = static_cast<uint8_t*>(sig(vt, h, "clk"));
    auto* s = static_cast<uint8_t*>(sig(vt, h, "sel"));
    auto* cnt = static_cast<uint32_t*>(sig(vt, h, "cnt"));
    *s = static_cast<uint8_t>(sel);
    for (int cycle = 0; cycle < 3; ++cycle) {
        *clk = 0;
        st = vt->eval_held(h, 10 * cycle);
        *clk = 1;
        st = vt->eval_held(h, 10 * cycle + 5);
        const bool msgs = (st & SIMJIG_MSGS_PENDING) != 0;
        st &= ~SIMJIG_MSGS_PENDING;
        std::printf("  cycle %d: eval_held=%s msgs=%d cnt=%u\n", cycle, kStatus[st], msgs, *cnt);
        drain(vt, h);
    }
    vt->release(h);
    vt->release(h);
    vt->release(nullptr);
    std::printf("  rehold=%s\n", kStatus[vt->hold(h)]);
    vt->release(h);
    st = vt->final(h);
    std::printf("  final=%s\n", kStatus[st]);
    drain(vt, h);
    vt->destroy(h);
    std::printf("  destroyed\n");
}

static void delay_case(const simjig_model_vtable* vt, bool go) {
    simjig_handle* h = nullptr;
    simjig_status st = vt->create(nullptr, &h);
    std::printf("%s go=%d: create=%s\n", vt->prefix, go, kStatus[st]);
    uint32_t n = 0;
    const simjig_signal* sigs = vt->signals(h, &n);
    if (std::strcmp(vt->prefix, "Vdelay") == 0) *static_cast<uint8_t*>(sigs[0].ptr) = go;
    uint64_t t = 0;
    for (int step = 0; step < 6; ++step) {
        vt->set_time(h, t);
        st = vt->eval(h);
        uint64_t next = vt->next_time_slot(h);
        std::printf("  t=%llu eval=%s stage=%u pending=%u next=%lld\n", (unsigned long long)t,
                    kStatus[st], *static_cast<uint8_t*>(sigs[n - 1].ptr), vt->events_pending(h),
                    next == SIMJIG_TIME_NONE ? -1LL : (long long)next);
        drain(vt, h);
        if (st != SIMJIG_OK || next == SIMJIG_TIME_NONE) break;
        t = next;
    }
    vt->destroy(h);
    std::printf("  destroyed\n");
}

int main(int argc, char** argv) {
    std::string only = argc > 1 ? argv[1] : "";
    const simjig_model_vtable* vt = simjig_vtable_Vfat();
    std::printf("vtable abi=%u size=%u prefix=%s verilator=%s\n", vt->abi_version, vt->size,
                vt->prefix, vt->verilator_version);
    if (only.empty() || only == "fat") {
        for (int sel : {1, 2, 3, 4, 5, 6, 7, 8}) fat_case(sel, 0);
        fat_case(3, 3);  // $error under the limit for two cycles, STOP on the third
    }
    if (only.empty() || only == "held") {
        for (int sel : {1, 2, 3, 4, 5, 6, 7, 8}) fat_held_case(sel);
    }
    if (only.empty() || only == "delay") {
        delay_case(simjig_vtable_Vdelay(), false);
        delay_case(simjig_vtable_Vdelay(), true);
    }
    if (only == "delay_internal") delay_case(simjig_vtable_Vdelay_internal(), false);
    std::printf("driver done\n");
    return 0;
}

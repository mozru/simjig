// Loads the generated shim of `cases/wide` through its vtable and exercises it.
// Usage: driver <ir_hash in hex>. Prints the signal table, one line per entry, then "ok".
// Any failed check prints "FAIL: ..." and exits 1.
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "simjig_abi.h"

extern "C" const simjig_model_vtable* simjig_vtable_Vwide_top(void);

#define CHECK(cond)                                         \
    do {                                                    \
        if (!(cond)) {                                      \
            std::printf("FAIL: %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            std::exit(1);                                   \
        }                                                   \
    } while (0)

static const simjig_signal* find(const simjig_signal* sigs, uint32_t n, const char* name) {
    for (uint32_t i = 0; i < n; ++i) {
        if (std::strcmp(sigs[i].name, name) == 0) return &sigs[i];
    }
    std::printf("FAIL: no signal %s\n", name);
    std::exit(1);
}

static uint8_t* u8(const simjig_signal* s) { return static_cast<uint8_t*>(s->ptr); }

int main(int argc, char** argv) {
    CHECK(argc == 2);
    const simjig_model_vtable* vt = simjig_vtable_Vwide_top();
    CHECK(vt->abi_version == SIMJIG_ABI_VERSION);
    CHECK(vt->size == sizeof(simjig_model_vtable));
    CHECK(std::strcmp(vt->prefix, "Vwide_top") == 0);
    CHECK(vt->ir_hash == std::strtoull(argv[1], nullptr, 16));

    simjig_create_opts opts{};
    opts.size = sizeof opts;
    simjig_handle* h = nullptr;
    CHECK(vt->create(&opts, &h) == SIMJIG_OK);
    uint32_t n = 0;
    const simjig_signal* sigs = vt->signals(h, &n);
    for (uint32_t i = 0; i < n; ++i) {
        const simjig_signal& s = sigs[i];
        CHECK(s.id == i && s.ptr != nullptr);
        std::printf("%u %s %u %u %u %u\n", s.id, s.name, s.width, s.storage, s.dir, s.access);
    }

    // A port: out_nib = {match.lo, match.hi}.
    *u8(find(sigs, n, "wide_top.match")) = 0x12;
    CHECK(vt->eval(h) == SIMJIG_OK);
    CHECK(*u8(find(sigs, n, "wide_top.out_nib")) == 0x21);

    // A public_flat_rw probe drives the accumulator (reset values are 0, Verilator's
    // default without +verilator+rand+reset); a public_flat_rd probe reads it.
    uint8_t* clk = u8(find(sigs, n, "wide_top.clk"));
    *u8(find(sigs, n, "wide_top.knob")) = 3;
    for (int edge = 0; edge < 4; ++edge) {
        *clk = static_cast<uint8_t>(edge % 2 == 0);
        CHECK(vt->eval(h) == SIMJIG_OK);
    }
    CHECK(*u8(find(sigs, n, "wide_top.u_acc.sum")) == 6);
    CHECK(*u8(find(sigs, n, "wide_top.total")) == 6);

    vt->destroy(h);
    std::printf("ok\n");
    return 0;
}

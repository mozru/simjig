// Drives any generated shim through its vtable from a script on the command line.
// Built with -DSIMJIG_VT=simjig_vtable_<prefix>. Usage: driver [name=<instance>] op...
//   w:<signal>:<hex>   write a signal of at most 64 bits
//   r:<signal>:<hex>   read it and check the value
//   e                  eval
// A failed create prints its messages and exits 2; a failed check prints "FAIL: ..." and
// exits 1. Prints "ok" at the end.
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include "simjig_abi.h"

extern "C" const simjig_model_vtable* SIMJIG_VT(void);

namespace {

const simjig_signal* find(const simjig_signal* sigs, uint32_t n, const std::string& name) {
    for (uint32_t i = 0; i < n; ++i) {
        if (name == sigs[i].name) return &sigs[i];
    }
    std::printf("FAIL: no signal %s\n", name.c_str());
    std::exit(1);
}

size_t bytes(const simjig_signal* s) {
    switch (s->storage) {
        case SIMJIG_STORAGE_C: return 1;
        case SIMJIG_STORAGE_S: return 2;
        case SIMJIG_STORAGE_I: return 4;
        case SIMJIG_STORAGE_Q: return 8;
        default: std::printf("FAIL: %s is wider than 64 bits\n", s->name); std::exit(1);
    }
}

void drain(const simjig_model_vtable* vt, simjig_handle* h) {
    simjig_msg m[16];
    size_t got = 0;
    while ((got = vt->drain_msgs(h, m, 16)) > 0) {
        for (size_t i = 0; i < got; ++i) std::printf("msg %u: %s\n", m[i].kind, m[i].msg);
    }
}

}  // namespace

int main(int argc, char** argv) {
    const simjig_model_vtable* vt = SIMJIG_VT();
    int first = 1;
    simjig_create_opts opts{};
    opts.size = sizeof opts;
    if (argc > 1 && std::strncmp(argv[1], "name=", 5) == 0) {
        opts.name = argv[1] + 5;
        first = 2;
    }
    simjig_handle* h = nullptr;
    if (vt->create(&opts, &h) != SIMJIG_OK) {
        std::printf("create failed\n");
        drain(vt, h);
        return 2;
    }
    uint32_t n = 0;
    const simjig_signal* sigs = vt->signals(h, &n);
    for (int i = first; i < argc; ++i) {
        const std::string op = argv[i];
        if (op == "e") {
            if (vt->eval(h) != SIMJIG_OK) {
                std::printf("FAIL: eval\n");
                return 1;
            }
            continue;
        }
        const size_t c1 = op.find(':');
        const size_t c2 = op.rfind(':');
        if (c1 == std::string::npos || c2 == c1) {
            std::printf("FAIL: bad op %s\n", op.c_str());
            return 1;
        }
        const simjig_signal* s = find(sigs, n, op.substr(c1 + 1, c2 - c1 - 1));
        const uint64_t value = std::strtoull(op.c_str() + c2 + 1, nullptr, 16);
        if (op[0] == 'w') {
            std::memcpy(s->ptr, &value, bytes(s));  // little-endian host
        } else {
            uint64_t got = 0;
            std::memcpy(&got, s->ptr, bytes(s));
            if (got != value) {
                std::printf("FAIL: %s is %" PRIx64 ", expected %" PRIx64 "\n", s->name, got,
                            value);
                return 1;
            }
        }
    }
    vt->destroy(h);
    std::printf("ok\n");
    return 0;
}

// Generic model shim, included by every per-model shim after it defines:
//   SPK_PORTS(X)     X(id, member) for each port the spike pokes or peeks;
//   SPK_INTERNAL_OP  optional body of `uint64_t internal_op(SPK_TOP* top, op, arg)`.
// build.rs passes -DSPK_TOP=<prefix>, -DSPK_HEADER and -DSPK_ROOT_HEADER, plus the VM_* defines
// from <prefix>_classes.mk, so one shim source can serve several prefixes.
#include "spk_runtime.h"

#include "verilated.h"
#include SPK_HEADER
#include SPK_ROOT_HEADER
#if VM_TRACE_FST
#include "verilated_fst_c.h"
#endif

#include <cstdio>
#include <cstdlib>

#define SPK_CAT2(a, b) a##b
#define SPK_CAT(a, b) SPK_CAT2(a, b)
#define SPK_STR2(a) #a
#define SPK_STR(a) SPK_STR2(a)

namespace {

struct Inst {
    SPK_TOP* top;
#if VM_TRACE_FST
    VerilatedFstC* tfp = nullptr;
#endif
};

Inst* inst(void* p) { return static_cast<Inst*>(p); }

void* create(VerilatedContext* contextp) {
    // Constructing a model also sets Verilated::threadContextp() to contextp.
    return new Inst{new SPK_TOP{contextp, "top"}};
}

void trace_close(void* p);

void destroy(void* p) {
    trace_close(p);
    inst(p)->top->final();
    delete inst(p)->top;
    delete inst(p);
}

// Exceptions must not cross into Rust (`extern "C"` boundary). The spikes never expect one,
// so any exception is a bug in the spike: abort loudly.
void eval(void* p) {
    try {
        inst(p)->top->eval();
    } catch (...) {
        std::fprintf(stderr, "spike: C++ exception escaped eval()\n");
        std::abort();
    }
}

void poke(void* p, uint32_t port, uint64_t value) {
    SPK_TOP* const t = inst(p)->top;
    switch (port) {
#define SPK_POKE(id, member) \
    case id: t->member = value; break;
        SPK_PORTS(SPK_POKE)
#undef SPK_POKE
    default: std::abort();
    }
}

uint64_t peek(void* p, uint32_t port) {
    SPK_TOP* const t = inst(p)->top;
    switch (port) {
#define SPK_PEEK(id, member) \
    case id: return static_cast<uint64_t>(t->member);
        SPK_PORTS(SPK_PEEK)
#undef SPK_PEEK
    default: std::abort();
    }
}

bool events_pending(void* p) { return inst(p)->top->eventsPending(); }
uint64_t next_time_slot(void* p) { return inst(p)->top->nextTimeSlot(); }

#if VM_TRACE_FST
void trace_open(void* p, const char* path) {
    Inst* const i = inst(p);
    if (!i->tfp) {
        Verilated::traceEverOn(true);
        i->tfp = new VerilatedFstC;
        i->top->trace(i->tfp, 99);
    }
    i->tfp->open(path);
}
void trace_dump(void* p, uint64_t time) {
    if (inst(p)->tfp) inst(p)->tfp->dump(time);
}
void trace_flush(void* p) {
    if (inst(p)->tfp) inst(p)->tfp->flush();
}
void trace_close(void* p) {
    if (inst(p)->tfp) inst(p)->tfp->close();
}
#else
void trace_open(void*, const char*) {}
void trace_dump(void*, uint64_t) {}
void trace_flush(void*) {}
void trace_close(void*) {}
#endif

uint64_t internal_op(void* p, uint32_t op, uint64_t arg) {
    SPK_TOP* const top = inst(p)->top;
    (void)top;
    (void)op;
    (void)arg;
#ifdef SPK_INTERNAL_OP
    SPK_INTERNAL_OP
#endif
    std::abort();
}

}  // namespace

extern "C" const SpkVtable* SPK_CAT(spk_vtable_, SPK_TOP)() {
    static const SpkVtable vt{
        SPK_STR(SPK_TOP), create,     destroy,     eval,        poke,
        peek,             events_pending, next_time_slot, trace_open, trace_dump,
        trace_flush,      trace_close, internal_op,
    };
    return &vt;
}

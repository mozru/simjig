// Shared declarations for the spike shims (C++ side). See ../src/lib.rs for the Rust side.
//
// Every model shim (model.inc) exports one function, spk_vtable_<prefix>(), returning a
// table of plain C function pointers. This mirrors DECISION.md's simjig_vtable_<prefix>():
// the Rust side never sees a C++ type, only `void*` instances and this table.
#pragma once
#include <cstddef>
#include <cstdint>

class VerilatedContext;

// $display and Verilator's own messages land here instead of printf: libverilated is
// compiled with -DVL_PRINTF=spk_printf (see build.rs). Output is appended to a buffer
// keyed by Verilated::threadContextp(), so a test can see which context it was routed to.
void spk_printf(const char* formatp, ...);

extern "C" {

struct SpkVtable {
    const char* name;
    void* (*create)(VerilatedContext* contextp);
    void (*destroy)(void* inst);
    void (*eval)(void* inst);
    void (*poke)(void* inst, uint32_t port, uint64_t value);
    uint64_t (*peek)(void* inst, uint32_t port);
    bool (*events_pending)(void* inst);
    uint64_t (*next_time_slot)(void* inst);
    // Tracing; no-ops unless the model was verilated with --trace-fst.
    void (*trace_open)(void* inst, const char* path);
    void (*trace_dump)(void* inst, uint64_t time);
    void (*trace_flush)(void* inst);
    void (*trace_close)(void* inst);
    // Model-specific escape hatch into internals (op codes documented per shim).
    uint64_t (*internal_op)(void* inst, uint32_t op, uint64_t arg);
};

}  // extern "C"

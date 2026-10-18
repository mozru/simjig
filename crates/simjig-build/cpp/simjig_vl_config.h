// simjig_vl_config.h: the Verilator configuration simjig compiles every translation unit with.
//
// Force-included (`-include simjig_vl_config.h`) into *every* C++ translation unit of a simjig
// build: libverilated (verilated.cpp, verilated_threads.cpp, verilated_timing.cpp), the
// Verilator-generated model sources, the generated shim and simjig_rt.cpp. One header instead
// of loose -D flags keeps the set in one place and lets the build cache key on its hash.
//
// Why each define (checked against Verilator 5.052 include/verilated.cpp and verilated.h):
// - VL_USER_FINISH, VL_USER_STOP, VL_USER_STOP_MAYBE, VL_USER_FATAL, VL_USER_WARN: each one
//   removes Verilator's default definition of vl_finish / vl_stop / vl_stop_maybe / vl_fatal /
//   vl_warn from verilated.cpp (`#ifndef VL_USER_*`). simjig_rt.cpp defines them instead, so
//   `$fatal`, `$stop`, `$error` and internal fatals never call exit() or abort().
//   They only change verilated.cpp, but are defined everywhere so every object agrees.
// - VL_PRINTF / VL_VPRINTF: verilated.h uses them only `#ifndef`. VL_PRINTF_MT (which every
//   $display/$write/$fatal text goes through) ends in VL_PRINTF, so redirecting it captures all
//   model output into the per-model message ring (simjig_rt.cpp).
#ifndef SIMJIG_VL_CONFIG_H_
#define SIMJIG_VL_CONFIG_H_

#include <cstdarg>

// Time comes only from each model's VerilatedContext, which the testbench sets (verilated_funcs.h
// VerilatedContext::time()). Without it, a context time of 0 falls back to the legacy weak
// `sc_time_stamp()`, which is an undefined symbol at link time on macOS.
#define VL_TIME_CONTEXT 1

#define VL_USER_FINISH 1
#define VL_USER_STOP 1
#define VL_USER_STOP_MAYBE 1
#define VL_USER_FATAL 1
#define VL_USER_WARN 1

// Verilator 5.052 declares vl_finish/vl_stop/vl_fatal/vl_warn in verilated_funcs.h, but
// vl_stop_maybe only at its own (now removed) definition in verilated.cpp, which VL_STOP_MT
// then calls. Without this declaration verilated.cpp does not compile with VL_USER_STOP_MAYBE.
void vl_stop_maybe(const char* filename, int linenum, const char* hier, bool maybe);

// Defined in simjig_rt.cpp. Same contract as printf/vprintf; the return value is ignored by
// every Verilator caller.
int simjig_vl_printf(const char* format, ...) __attribute__((format(printf, 1, 2)));
int simjig_vl_vprintf(const char* format, va_list args);
#define VL_PRINTF simjig_vl_printf
#define VL_VPRINTF simjig_vl_vprintf

#endif  // SIMJIG_VL_CONFIG_H_

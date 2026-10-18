/*
 * simjig_abi.h: the C ABI between a generated Verilator model and the simjig Rust runtime.
 *
 * Every model exports exactly one symbol, `const simjig_model_vtable* simjig_vtable_<prefix>(void)`.
 * Everything else the runtime does with the model goes through that table. The Rust mirror
 * of this file is `crates/simjig/src/model/abi.rs`; the two must change together.
 *
 * Versioning rules (append-only):
 *   - `abi_version` changes only when an existing field changes meaning or layout. The Rust
 *     side rejects any other version.
 *   - New entries are appended to the end of a struct. `size` says how many bytes the model
 *     side knows about, so a newer runtime can tell that an older model lacks an entry.
 *   - Status codes, message kinds and storage classes are append-only too: a value never
 *     changes meaning once released.
 *
 * Threading: a handle belongs to one thread. No entry point may run concurrently with another
 * on the same handle. Distinct handles are independent (each owns its VerilatedContext).
 *
 * Exceptions: no entry point ever lets a C++ exception escape. Every entry point that can run
 * model code returns a status (or a sentinel documented on the entry).
 *
 * This header is C (C99) and C++ compatible and depends only on <stdint.h> and <stddef.h>.
 */
#ifndef SIMJIG_ABI_H_
#define SIMJIG_ABI_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped only on an incompatible change; appending entries does not bump it. */
#define SIMJIG_ABI_VERSION 1u

/* ------------------------------------------------------------------------------------------ */
/* Status codes, returned by entry points that run model code. A plain integer, not a C enum,
 * so its size is fixed across compilers. */
typedef int32_t simjig_status;

/* The call completed; the model may continue. */
#define SIMJIG_OK 0
/* `$finish` has run (now or earlier: sticky). The model is still usable (e.g. for `final`). */
#define SIMJIG_FINISH 1
/* `$stop`, or an `$error` over the error limit, has run (sticky). The model is still usable. */
#define SIMJIG_STOP 2
/* `$fatal` or an internal Verilator fatal. A FATAL message was recorded; the model is now
 * poisoned. */
#define SIMJIG_FATAL 3
/* A C++ exception other than a fatal escaped model code. An EXCEPTION message was recorded;
 * the model is now poisoned. */
#define SIMJIG_EXCEPTION 4
/* The model was already poisoned by an earlier FATAL or EXCEPTION; nothing ran. */
#define SIMJIG_POISONED 5
/* The entry point is reserved but not implemented by this model build (e.g. tracing without
 * --trace-fst). Nothing ran; the model state is unchanged. */
#define SIMJIG_UNSUPPORTED 6

/* `next_time_slot` returns this when the model has no pending timed events. */
#define SIMJIG_TIME_NONE UINT64_MAX

/* ------------------------------------------------------------------------------------------ */
/* Signal descriptors. Verilator stores a W-bit signal in one of five C++ types; `storage`
 * names which, and the Rust side reads exactly that many bytes. */
#define SIMJIG_STORAGE_C 1 /* CData, uint8_t,  width 1..=8 */
#define SIMJIG_STORAGE_S 2 /* SData, uint16_t, width 9..=16 */
#define SIMJIG_STORAGE_I 3 /* IData, uint32_t, width 17..=32 */
#define SIMJIG_STORAGE_Q 4 /* QData, uint64_t, width 33..=64 */
#define SIMJIG_STORAGE_W 5 /* VlWide<words>, uint32_t[words] least significant first, width > 64 */

#define SIMJIG_DIR_INTERNAL 0 /* not a port: a public_flat_rd/_rw member */
#define SIMJIG_DIR_IN 1
#define SIMJIG_DIR_OUT 2
#define SIMJIG_DIR_INOUT 3

/* Access bits. A write to a member without WRITE is undefined (Verilator may have folded it). */
#define SIMJIG_ACCESS_READ 1u
#define SIMJIG_ACCESS_WRITE 2u

typedef struct simjig_signal {
    uint32_t id;      /* index in the table; equals the IR slot id */
    uint32_t width;   /* declared width in bits, >= 1 */
    uint32_t words;   /* 32-bit words needed for `width`: (width + 31) / 32 */
    uint8_t storage;  /* SIMJIG_STORAGE_* */
    uint8_t dir;      /* SIMJIG_DIR_* */
    uint8_t access;   /* SIMJIG_ACCESS_* bits */
    uint8_t reserved; /* zero */
    const char* name; /* hierarchical name, NUL-terminated, static lifetime */
    void* ptr;        /* the member; valid from create until destroy; never moves */
} simjig_signal;

/* ------------------------------------------------------------------------------------------ */
/* Messages: everything the model printed or reported, in order. */
#define SIMJIG_MSG_DISPLAY 0 /* $display/$write/$warning/$info text, or other Verilator output */
#define SIMJIG_MSG_WARNING 1 /* internal Verilator warning (vl_warn) */
#define SIMJIG_MSG_ERROR 2   /* $error or a failed assertion */
#define SIMJIG_MSG_STOP 3    /* $stop */
#define SIMJIG_MSG_FATAL 4   /* $fatal or an internal Verilator fatal */
#define SIMJIG_MSG_FINISH 5  /* $finish */
#define SIMJIG_MSG_EXCEPTION 6 /* a C++ exception caught by the barrier; `msg` is what() */
#define SIMJIG_MSG_DROPPED 7 /* the ring overflowed; `line` holds the number of lost records */

typedef struct simjig_msg {
    uint32_t kind;    /* SIMJIG_MSG_* */
    uint32_t line;    /* source line, 0 if unknown */
    uint64_t time;    /* context time (ticks of the time precision) when recorded */
    const char* file; /* source file, "" if unknown */
    const char* hier; /* hierarchical scope, "" if unknown */
    const char* msg;  /* DISPLAY: the text as printed (with its newline); others: no newline */
    size_t msg_len;   /* bytes in `msg`, excluding the NUL terminator */
} simjig_msg;

/* ------------------------------------------------------------------------------------------ */
/* Model handle: opaque to C and Rust. */
typedef struct simjig_handle simjig_handle;

/* Create-time options. `size` is sizeof(simjig_create_opts) as the caller compiled it. */
#define SIMJIG_CREATE_ECHO 1u /* also print every captured message to stdout */

typedef struct simjig_create_opts {
    uint32_t size;
    uint32_t flags;          /* SIMJIG_CREATE_* bits */
    const char* name;        /* instance name passed to the Verilated constructor; NULL = "TOP" */
    uint32_t error_limit;    /* $error count that turns into STOP; 0 = Verilator's (default 1) */
    uint32_t argc;           /* plusargs for VerilatedContext::commandArgs */
    const char* const* argv; /* argc entries; may be NULL when argc is 0 */
} simjig_create_opts;

typedef struct simjig_model_vtable {
    uint32_t abi_version;          /* SIMJIG_ABI_VERSION the model was built against */
    uint32_t size;                 /* sizeof(simjig_model_vtable) as the model was built */
    const char* prefix;            /* Verilator --prefix, e.g. "Vcounter" */
    const char* verilator_version; /* Verilated::productVersion() at build time */
    uint64_t ir_hash;              /* hash of the svtypes IR the generated Rust was made from */

    /* Allocates a context and the model. Sets *out (NULL only if allocation failed, with
     * EXCEPTION). On FATAL/EXCEPTION, *out is a poisoned handle whose messages can be drained;
     * it must still be destroyed. */
    simjig_status (*create)(const simjig_create_opts* opts, simjig_handle** out);
    /* Frees the model and its context. Does not run final blocks. Accepts NULL. */
    void (*destroy)(simjig_handle* h);
    /* One eval() of the model at the current context time. */
    simjig_status (*eval)(simjig_handle* h);
    /* Runs final blocks. */
    simjig_status (*final)(simjig_handle* h);
    /* Sets the context time, in ticks of the design's time precision. */
    void (*set_time)(simjig_handle* h, uint64_t time);
    /* 1 if the model has timed events pending (--timing designs), else 0. 0 when poisoned. */
    uint32_t (*events_pending)(simjig_handle* h);
    /* Time of the next timed event, or SIMJIG_TIME_NONE. Never fatal (unlike Verilator's). */
    uint64_t (*next_time_slot)(simjig_handle* h);
    /* The signal table: *count entries, valid until destroy. Never NULL for a live handle. */
    const simjig_signal* (*signals)(simjig_handle* h, uint32_t* count);
    /* Moves up to `cap` oldest messages into `out` and returns how many were written. With
     * cap == 0, returns the number pending without draining. String pointers in drained
     * records stay valid until the next call of any entry point on `h`. */
    size_t (*drain_msgs)(simjig_handle* h, simjig_msg* out, size_t cap);

    /* FST tracing. UNSUPPORTED unless the model was built with tracing. */
    simjig_status (*trace_open)(simjig_handle* h, const char* path);
    simjig_status (*trace_dump)(simjig_handle* h, uint64_t time);
    simjig_status (*trace_close)(simjig_handle* h);
    /* Coverage counters (`--coverage`): pointer to *count uint32_t counters, or NULL. */
    uint32_t* (*cov_counters)(simjig_handle* h, size_t* count);
    /* Append new entries here. */
} simjig_model_vtable;

#ifdef __cplusplus
}
#endif

#endif /* SIMJIG_ABI_H_ */

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
 * on the same handle. Distinct handles are independent: each owns its VerilatedContext (with
 * threads(1), so no Verilator thread pool) and its own `$urandom` state, and every entry point
 * that runs model code first makes them the thread's current ones.
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
/* A file named by the call could not be opened (or would clobber the trace being detached).
 * A WARNING message names the file and the reason. Nothing ran; the model state is unchanged. */
#define SIMJIG_IO_ERROR 7

/* Or'ed into `eval_held`'s status when the model has messages to drain (`drain_msgs`). */
#define SIMJIG_MSGS_PENDING 0x100

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

    /* ---- Appended after size 32 (SIMJIG_ABI_VERSION 1). A caller compiled before them
     * passes a smaller `size`, and create reads them as SIMJIG_INIT_ZERO. ---- */

    /* SIMJIG_INIT_*: the initial values of the model's variables (docs/notes/n-xinit.md). */
    uint32_t init;
    /* With SIMJIG_INIT_RANDOM: the seed the initial values are drawn from, in 1..=INT32_MAX
     * (VerilatedContext::randSeed while the model is constructed). Ignored otherwise. */
    uint32_t init_seed;
} simjig_create_opts;

/* Values of `simjig_create_opts.init`, numbered as Verilator's randReset. */
/* Whatever the plusargs say (+verilator+rand+reset+, default 0: every variable without an
 * initializer starts at 0). */
#define SIMJIG_INIT_ZERO 0u
/* Each variable without an initializer starts at a value drawn from `init_seed`, the variable's
 * scope and its name (Verilator's VL_SCOPED_RAND_RESET_*, randReset 2), and only while the
 * model is constructed: afterwards the context's randReset and randSeed are what the plusargs
 * set, so `$urandom`, class objects and `$readmem` draw exactly as with SIMJIG_INIT_ZERO.
 * Needs SIMJIG_FEATURE_RANDOM_INIT; without it the values are whatever Verilator compiled in. */
#define SIMJIG_INIT_RANDOM 2u

/* One contiguous array of coverage counters (see `cov_runs`). */
typedef struct simjig_cov_run {
    uint32_t* counters; /* `len` live counters, valid until destroy */
    size_t len;
} simjig_cov_run;

/* Bits of `simjig_model_vtable.features`: how the model was verilated. */
#define SIMJIG_FEATURE_TIMING 1u    /* --timing */
#define SIMJIG_FEATURE_TRACE_FST 2u /* --trace-fst: the trace_* entries work */
#define SIMJIG_FEATURE_COVERAGE 4u  /* --coverage (any of its kinds): the cov_* entries work */
/* Verilated with --x-initial unique (Verilator's default) by a shim that reads
 * `simjig_create_opts.init`: SIMJIG_INIT_RANDOM randomizes the initial values. */
#define SIMJIG_FEATURE_RANDOM_INIT 8u
/* The model has a variable C++ may write (Verilator's VLVF_PUB_RW: `public_flat_rw`, `public`
 * or `public_flat` on a variable, `--public-flat-rw`, a read-write probe). Verilator 5.052
 * schedules the logic such a variable feeds under the 'ico' region's 'first iteration'
 * trigger, so every eval() runs it again, written or not: an eval with nothing written is
 * not a no-op. Set when the shim is compiled with SIMJIG_PUBLIC_RW=1. */
#define SIMJIG_FEATURE_PUBLIC_RW 16u

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

    /* FST tracing (docs/notes/tracecov.md). Every trace_* entry returns UNSUPPORTED, and does
     * nothing, unless the model was verilated with --trace-fst (SIMJIG_FEATURE_TRACE_FST).
     * At most one trace is open per handle; destroy closes it. */
    /* trace_open_ex(h, path, 0, 0). */
    simjig_status (*trace_open)(simjig_handle* h, const char* path);
    /* Writes one sample at `time` (ticks of the time precision; non-decreasing across calls).
     * No-op (OK) when no trace is open or `time` is before the trace's start time. */
    simjig_status (*trace_dump)(simjig_handle* h, uint64_t time);
    /* Writes the trace's header and closes it. No-op (OK) when no trace is open. */
    simjig_status (*trace_close)(simjig_handle* h);
    /* Coverage counters (--coverage): *count runs that together hold every counter of the
     * model's code coverage (line, branch, toggle, expression, FSM and user points) exactly
     * once, valid until destroy; NULL with *count = 0 without --coverage or without points.
     * Verilator keeps one counter array per module object (the root, each module instance it
     * did not inline, each interface instance) plus one in the symbol table; a run is a
     * maximal contiguous range of them, and the runs come in symbol-table order (the order of
     * the objects in V<prefix>__Syms). The run layout and an index's coverage point are fixed
     * for one build (docs/notes/s1c2fix.md has the mapping), and the runs are fixed at create.
     * Covergroup bins are NOT in any run: Verilator allocates them on the heap when the
     * covergroup's `new` runs (during an eval), outside the symbol table. cov_write and
     * cov_zero do include them. (Before the first release this slot was `cov_counters`,
     * one array or NULL.) */
    const simjig_cov_run* (*cov_runs)(simjig_handle* h, size_t* count);

    /* ---- Appended after size 136 (SIMJIG_ABI_VERSION 1). ---- */

    /* SIMJIG_FEATURE_* bits. */
    uint64_t features;
    /* Opens an FST trace at `path` (closing an open one first). `depth` limits the trace to
     * signals at most that many scope levels below the model's top scope (1 = the top's own
     * signals); 0 traces everything. Samples are written only for trace_dump times >=
     * `start_time`, so a replay can run untraced up to the interesting time. IO_ERROR if the
     * file cannot be created. */
    simjig_status (*trace_open_ex)(simjig_handle* h, const char* path, uint32_t depth,
                                   uint64_t start_time);
    /* Writes buffered samples to the file. Call before fork() so no sample is pending in both
     * processes. No-op (OK) when no trace is open. */
    simjig_status (*trace_flush)(simjig_handle* h);
    /* Detach and reopen, for a fork() child that inherited an open trace: points every file
     * descriptor on the inherited trace file at /dev/null, closes the inherited writer (its
     * output goes to /dev/null, so the parent's file is untouched), then opens `path` with the
     * same depth and start time. With no trace open it is trace_open_ex(h, path, 0, 0).
     * The inherited writer is detached before anything can fail: IO_ERROR (if `path` is the
     * inherited file itself or cannot be created, or the inherited descriptor cannot be
     * redirected, in which case the writer is abandoned unclosed) leaves no trace open, so a
     * later close or destroy cannot reach the parent's file. The child must never close,
     * flush or destroy an inherited trace before this call. */
    simjig_status (*trace_reopen)(simjig_handle* h, const char* path);
    /* Sets every coverage counter of the model to 0, covergroup bins included. UNSUPPORTED
     * without --coverage. */
    simjig_status (*cov_zero)(simjig_handle* h);
    /* Writes Verilator's coverage.dat for this model (VerilatedCovContext::write), every
     * point including covergroup bins, to `path`;
     * NULL means Verilator's default (`coverage.dat`, or +verilator+coverage+file+). IO_ERROR
     * if the file cannot be created. UNSUPPORTED without --coverage. */
    simjig_status (*cov_write)(simjig_handle* h, const char* path);
    /* Replaces the model's private `$urandom`/`$random` state with one seeded from `seed`
     * (VlRNG(seed)). Each handle starts with VlRNG(seed from +verilator+seed+, default 1),
     * which is Verilator's own single-model stream. */
    simjig_status (*rng_seed)(simjig_handle* h, uint64_t seed);

    /* ---- Appended after size 192 (SIMJIG_ABI_VERSION 1): held evaluation. ---- */

    /* Installs the model as this thread's current model (its context, its `$urandom`
     * state) until `release`, for a run of `eval_held` calls with no other model's entry
     * point between them (a synchronous testbench). OK, or POISONED. Entry points called
     * while held work as before. */
    simjig_status (*hold)(simjig_handle* h);
    /* Ends `hold`. Accepts a handle that is not held (and NULL). */
    void (*release)(simjig_handle* h);
    /* Sets the context time to `time` and runs one eval(), as set_time and eval do, behind
     * the same exception barrier; while held, without installing the model again. The status
     * has SIMJIG_MSGS_PENDING or'ed in when messages wait to be drained. */
    simjig_status (*eval_held)(simjig_handle* h, uint64_t time);
    /* Append new entries here. */
} simjig_model_vtable;

#ifdef __cplusplus
}
#endif

#endif /* SIMJIG_ABI_H_ */

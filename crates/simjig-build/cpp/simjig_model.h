// simjig_model.h: builds one model's simjig_model_vtable and signal table from a traits struct.
//
// The generator (simjig-build) emits, per model, one shim .cpp that includes the Verilated
// headers and this file, defines a traits struct and exports the vtable:
//
//   #include "Vcounter.h"
//   #include "Vcounter___024root.h"   // only with kCoverage (with __Syms.h: the symbol table type)
//   #include "Vcounter__Syms.h"        // only with kCoverage
//   #include "simjig_model.h"
//   struct CounterTraits {
//       using Model = Vcounter;
//       static constexpr const char* kPrefix = "Vcounter";
//       static constexpr uint64_t kIrHash = 0x0123456789abcdefull;
//       static constexpr bool kTiming = false;   // verilated with --timing (coroutines)?
//       static constexpr bool kTraceFst = false; // verilated with --trace-fst? (default false)
//       static constexpr bool kCoverage = false; // verilated with --coverage*? (default false)
//       // Only for --x-initial 0 or fast (default true: Verilator's own --x-initial unique).
//       static constexpr bool kXInitialUnique = false;
//       static void bind(Vcounter& m, simjig::SignalTable& t) {
//           SIMJIG_SIGNAL(t, m.clk, "clk", 1, SIMJIG_DIR_IN, SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE);
//           SIMJIG_PROBE(t, m, "counter.u_core", "state", "counter.u_core.state", 4,
//                        SIMJIG_ACCESS_READ);
//       }
//   };
//   SIMJIG_EXPORT_VTABLE(Vcounter, CounterTraits)
//
// Invariants:
// - Member addresses are taken once, right after construction, in IR slot order; slot i of the
//   table has id i. Verilator members never move while the model lives.
// - Each SIMJIG_SIGNAL (a port) statically checks that the member's C++ type is exactly the
//   storage type Verilator uses for that width (so a width/type drift fails the C++ build, not
//   the simulation).
// - Each SIMJIG_PROBE (an internal public_flat variable) is looked up in Verilator's scope
//   table, which knows its address whether or not its module was inlined, and is checked
//   there for storage class, exact width, packing and access (find_probe: a writable probe
//   must be public_flat_rw and not a parameter); a mismatch fails create.
// - No model code runs outside simjig::guard().
// - Trace and coverage code is compiled only for kTraceFst / kCoverage models (`if constexpr`
//   in templates), so a model without them needs neither verilated_fst_c.cpp nor
//   verilated_cov.cpp at link time; their entries then return SIMJIG_UNSUPPORTED.
// - A trace is closed before its model is destroyed (ModelHandle::teardown).
#ifndef SIMJIG_MODEL_H_
#define SIMJIG_MODEL_H_

#include "simjig_abi.h"
#include "simjig_rt.h"
#include "verilated_cov.h"
#include "verilated_fst_c.h"

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

namespace simjig {

// The storage class of a Verilator member type; undefined for anything else, so binding an
// unexpected type is a compile error.
template <class T>
struct storage_of;
template <>
struct storage_of<CData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_C> {};
template <>
struct storage_of<SData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_S> {};
template <>
struct storage_of<IData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_I> {};
template <>
struct storage_of<QData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_Q> {};
template <std::size_t N>
struct storage_of<VlWide<N>> : std::integral_constant<uint8_t, SIMJIG_STORAGE_W> {};

class SignalTable final {
public:
    void add(uint32_t width, uint8_t dir, uint8_t access, const char* name, void* ptr) {
        const auto id = static_cast<uint32_t>(m_signals.size());
        m_signals.push_back(simjig_signal{id, width, words_for_width(width),
                                          storage_for_width(width), dir, access, 0, name, ptr});
    }
    const std::vector<simjig_signal>& signals() const { return m_signals; }

private:
    std::vector<simjig_signal> m_signals;
};

// Optional traits members, so shims written before they existed keep compiling.
template <class Traits>
constexpr bool trace_fst_of() {
    if constexpr (requires { Traits::kTraceFst; }) {
        return Traits::kTraceFst;
    } else {
        return false;
    }
}
template <class Traits>
constexpr bool coverage_of() {
    if constexpr (requires { Traits::kCoverage; }) {
        return Traits::kCoverage;
    } else {
        return false;
    }
}
// Verilator's default is --x-initial unique, so a shim says so only when the model was built
// with another mode (`--x-initial 0` or `fast`), whose initial values are compiled in.
template <class Traits>
constexpr bool x_initial_unique_of() {
    if constexpr (requires { Traits::kXInitialUnique; }) {
        return Traits::kXInitialUnique;
    } else {
        return true;
    }
}

// simjig_create_opts.init and .init_seed, or SIMJIG_INIT_ZERO from a caller compiled before
// they were appended.
inline bool random_init(const simjig_create_opts* opts) {
    return opts != nullptr
           && opts->size >= offsetof(simjig_create_opts, init_seed) + sizeof(opts->init_seed)
           && opts->init == SIMJIG_INIT_RANDOM;
}

// An open FST trace. A template only so that nothing names VerilatedFstC's out-of-line members
// unless a kTraceFst model instantiates it.
template <class Model>
struct FstTrace final {
    std::unique_ptr<VerilatedFstC> writer;  // null: no trace open
    FileId file;                             // the file `writer` writes, for detach
    uint32_t depth = 0;
    uint64_t start_time = 0;
    void reset() { writer.reset(); }
};
struct NoTrace final {
    void reset() {}
};

template <class Traits>
struct ModelHandle final : simjig_handle {
    explicit ModelHandle(const simjig_create_opts* opts) : simjig_handle{opts, Traits::kTiming} {}
    void teardown() noexcept override {
        try {
            trace.reset();  // its close writes the header and needs the model alive
        } catch (...) {
            // A failing close loses the trace, never the process.
        }
        model.reset();
    }
    std::unique_ptr<typename Traits::Model> model;
    SignalTable table;
    std::vector<simjig_cov_run> cov;  // cov_runs; empty without kCoverage
    std::conditional_t<trace_fst_of<Traits>(), FstTrace<typename Traits::Model>, NoTrace> trace;
};

// The model's symbol table object (V<prefix>__Syms), which holds the root, every module object
// Verilator did not inline and the table's own counter array as members: the range in which
// Runtime::cov_runs keeps counters. The model's own vlSymsp is private; its root's copy is
// public, and a kCoverage shim includes the headers that define both types.
template <class Model>
std::pair<const void*, size_t> syms_object(Model& m) {
    using Root = std::remove_cv_t<std::remove_pointer_t<std::remove_cv_t<decltype(Model::rootp)>>>;
    using Syms = std::remove_pointer_t<std::remove_cv_t<decltype(Root::vlSymsp)>>;
    static_assert(sizeof(Root) > 0 && sizeof(Syms) > 0,
                  "simjig: a kCoverage shim must include <prefix>___024root.h and <prefix>__Syms.h");
    return {m.rootp->vlSymsp, sizeof(Syms)};
}

template <class Traits>
struct Vtable final {
    using Handle = ModelHandle<Traits>;
    using Model = typename Traits::Model;
    static constexpr bool kTrace = trace_fst_of<Traits>();
    static constexpr bool kCoverage = coverage_of<Traits>();
    // Verilator writes `traceCapable` into every model header: a shim that disagrees with the
    // Verilator command line fails to compile instead of tracing nothing.
    static_assert(kTrace == Model::traceCapable,
                  "simjig: kTraceFst must be true exactly when verilated with --trace-fst");

    static Handle* handle(simjig_handle* h) { return static_cast<Handle*>(h); }
    static Model& model(simjig_handle* h) { return *handle(h)->model; }

    // guard() for calls that only observe the model (trace, coverage): the sticky FINISH and
    // STOP describe the simulation, not this call, so they read as OK here.
    template <class F>
    static simjig_status observe(simjig_handle* h, F&& body) noexcept {
        const simjig_status s = guard(h, std::forward<F>(body));
        return s == SIMJIG_FINISH || s == SIMJIG_STOP ? SIMJIG_OK : s;
    }

    static simjig_status create(const simjig_create_opts* opts, simjig_handle** out) noexcept {
        if (out == nullptr) return SIMJIG_EXCEPTION;
        *out = nullptr;
        Handle* h = nullptr;
        try {
            h = new Handle{opts};
        } catch (...) {
            return SIMJIG_EXCEPTION;
        }
        *out = h;
        return guard(h, [h, opts] {
            VerilatedContext& ctx = h->rt.context();
            if (opts != nullptr && opts->argc > 0 && opts->argv != nullptr) {
                // commandArgs copies the strings; its non-const parameter is historical.
                ctx.commandArgs(static_cast<int>(opts->argc), const_cast<const char**>(opts->argv));
            }
            if (opts != nullptr && opts->error_limit > 0) {
                ctx.errorLimit(static_cast<int>(opts->error_limit));
            }
            // A traced model's trace_init fatals unless this was set before time 0; it makes
            // Verilator compute signals only waves read, so only traced builds pay for it.
            if constexpr (kTrace) ctx.traceEverOn(true);
            // After commandArgs, so +verilator+seed+ applies; before the model, whose
            // constructor may already draw (class objects seed from the current stream).
            h->rt.reseed(h->rt.context_seed());
            const char* name = opts != nullptr && opts->name != nullptr ? opts->name : "TOP";
            if (random_init(opts)) {
                construct_randomized(ctx, h, name, opts->init_seed);
            } else {
                h->model = std::make_unique<Model>(&ctx, name);
            }
            // The constructor registered every coverage point (note_cov_counter).
            if constexpr (kCoverage) {
                const auto [syms, size] = syms_object(*h->model);
                h->cov = h->rt.cov_runs(syms, size);
            } else {
                h->rt.cov_runs(nullptr, 0);
            }
            Traits::bind(*h->model, h->table);
        });
    }

    // The constructor's _ctor_var_reset draws every initial value with VL_SCOPED_RAND_RESET_*
    // from the context's randReset and randSeed, so both are set for the construction alone
    // and put back after it: `$urandom` (seeded above from the plusargs' seed) and every later
    // draw stay as without SIMJIG_INIT_RANDOM. Each randSeed() bumps Verilator's global seed
    // epoch; thread_rng() catches the thread up without losing the model's stream. The two
    // thread_rng() calls are defensive: they matter only to a constructor that draws from
    // the thread's stream (none of the test designs does, and removing them passes every
    // test; docs/notes/n-xinit.md), and keep such a draw on the model's stream.
    static void construct_randomized(VerilatedContext& ctx, Handle* h, const char* name,
                                     uint32_t seed) {
        const int reset = ctx.randReset();
        const int plusarg_seed = ctx.randSeed();
        ctx.randReset(2);
        ctx.randSeed(static_cast<int>(seed));
        (void)thread_rng();
        struct Restore {
            VerilatedContext& ctx;
            int reset;
            int seed;
            ~Restore() {
                ctx.randReset(reset);
                ctx.randSeed(seed);
                (void)thread_rng();
            }
        } restore{ctx, reset, plusarg_seed};
        h->model = std::make_unique<Model>(&ctx, name);
    }

    static simjig_status eval(simjig_handle* h) noexcept {
        return guard(h, [h] { model(h).eval(); });
    }

    static simjig_status eval_held(simjig_handle* h, uint64_t time) noexcept {
        if (h == nullptr) return SIMJIG_EXCEPTION;
        return h->guard_held(time, [](simjig_handle& hh, void*) { model(&hh).eval(); },
                             nullptr);
    }

    static simjig_status final_blocks(simjig_handle* h) noexcept {
        return guard(h, [h] { model(h).final(); });
    }

    static uint32_t events_pending(simjig_handle* h) noexcept {
        uint32_t pending = 0;
        if constexpr (Traits::kTiming) {
            guard(h, [h, &pending] { pending = model(h).eventsPending() ? 1 : 0; });
        }
        return pending;
    }

    // Verilator's nextTimeSlot() is a fatal when nothing is pending (and always without
    // --timing); the ABI returns SIMJIG_TIME_NONE instead.
    static uint64_t next_time_slot(simjig_handle* h) noexcept {
        uint64_t next = SIMJIG_TIME_NONE;
        if constexpr (Traits::kTiming) {
            guard(h, [h, &next] {
                if (model(h).eventsPending()) next = model(h).nextTimeSlot();
            });
        }
        return next;
    }

    static const simjig_signal* signals(simjig_handle* h, uint32_t* count) noexcept {
        if (h == nullptr) {
            if (count != nullptr) *count = 0;
            return nullptr;
        }
        const auto& sigs = handle(h)->table.signals();
        if (count != nullptr) *count = static_cast<uint32_t>(sigs.size());
        return sigs.data();
    }

    // ---- FST tracing (kTraceFst only) --------------------------------------------------

    // Precondition for every trace_* body below: kTrace, h live and not poisoned.
    static simjig_status open_trace(simjig_handle* h, const char* path, uint32_t depth,
                                    uint64_t start_time) {
        if (!probe_writable(h->rt, path, "trace file")) return SIMJIG_IO_ERROR;
        auto& t = handle(h)->trace;
        return observe(h, [h, &t, path, depth, start_time] {
            t.reset();
            auto writer = std::make_unique<VerilatedFstC>();
            // `levels` is ignored by 5.052's generated traceBaseModel; depth is applied with
            // $dumpvars semantics instead, relative to the model's top scope (its name).
            model(h).trace(writer.get(), 0);
            if (depth > 0) writer->dumpvars(static_cast<int>(depth), model(h).name());
            writer->open(path);
            t.writer = std::move(writer);
            t.file = file_id(path);
            t.depth = depth;
            t.start_time = start_time;
        });
    }

    static simjig_status trace_open_ex(simjig_handle* h, const char* path, uint32_t depth,
                                       uint64_t start_time) noexcept {
        if constexpr (!kTrace) {
            return SIMJIG_UNSUPPORTED;
        } else {
            if (h == nullptr) return SIMJIG_EXCEPTION;
            if (h->rt.poisoned()) return SIMJIG_POISONED;
            return open_trace(h, path, depth, start_time);
        }
    }

    static simjig_status trace_open(simjig_handle* h, const char* path) noexcept {
        return trace_open_ex(h, path, 0, 0);
    }

    static simjig_status trace_dump(simjig_handle* h, uint64_t time) noexcept {
        if constexpr (!kTrace) {
            return SIMJIG_UNSUPPORTED;
        } else {
            if (h == nullptr) return SIMJIG_EXCEPTION;
            if (h->rt.poisoned()) return SIMJIG_POISONED;
            auto& t = handle(h)->trace;
            if (!t.writer || time < t.start_time) return SIMJIG_OK;
            return observe(h, [&t, time] { t.writer->dump(time); });
        }
    }

    static simjig_status trace_flush(simjig_handle* h) noexcept {
        if constexpr (!kTrace) {
            return SIMJIG_UNSUPPORTED;
        } else {
            if (h == nullptr) return SIMJIG_EXCEPTION;
            if (h->rt.poisoned()) return SIMJIG_POISONED;
            auto& t = handle(h)->trace;
            if (!t.writer) return SIMJIG_OK;
            return observe(h, [&t] { t.writer->flush(); });
        }
    }

    static simjig_status trace_close(simjig_handle* h) noexcept {
        if constexpr (!kTrace) {
            return SIMJIG_UNSUPPORTED;
        } else {
            if (h == nullptr) return SIMJIG_EXCEPTION;
            if (h->rt.poisoned()) return SIMJIG_POISONED;
            auto& t = handle(h)->trace;
            if (!t.writer) return SIMJIG_OK;
            return observe(h, [&t] { t.reset(); });
        }
    }

    // docs/notes/spikes.md §1 surprise 3: a fork child shares the trace file's descriptor and
    // offset with its parent, so any write or close through the inherited writer corrupts the
    // parent's file. Redirecting the descriptor to /dev/null first makes the close harmless.
    // The detach comes before every check of the new path: a reopen that fails must not leave
    // the inherited writer attached, where a later close or destroy would write through it.
    static simjig_status trace_reopen(simjig_handle* h, const char* path) noexcept {
        if constexpr (!kTrace) {
            return SIMJIG_UNSUPPORTED;
        } else {
            if (h == nullptr) return SIMJIG_EXCEPTION;
            if (h->rt.poisoned()) return SIMJIG_POISONED;
            auto& t = handle(h)->trace;
            if (!t.writer) return open_trace(h, path, 0, 0);
            if (!t.file.valid || redirect_to_devnull(t.file) < 0) {
                // The descriptor cannot be moved, so the writer is never closed: it is
                // abandoned (its destructor would flush and write the header to the parent's
                // file). The process is a fork child, so this leaks only its own copy.
                static_cast<void>(t.writer.release());
                h->rt.on_warn("", 0, "",
                              t.file.valid ? "trace_reopen: cannot open /dev/null"
                                           : "trace_reopen: the inherited trace file is unknown");
                return SIMJIG_IO_ERROR;
            }
            const simjig_status closed = observe(h, [&t] { t.reset(); });
            if (closed != SIMJIG_OK) return closed;
            const FileId target = file_id(path);
            if (target.valid && target.dev == t.file.dev && target.ino == t.file.ino) {
                h->rt.on_warn("", 0, "", "trace_reopen: the new trace path is the inherited file");
                return SIMJIG_IO_ERROR;
            }
            return open_trace(h, path, t.depth, t.start_time);
        }
    }

    // ---- Coverage (kCoverage only) ------------------------------------------------------

    static const simjig_cov_run* cov_runs(simjig_handle* h, size_t* count) noexcept {
        const std::vector<simjig_cov_run>* runs = h != nullptr ? &handle(h)->cov : nullptr;
        const bool any = runs != nullptr && !runs->empty();
        if (count != nullptr) *count = any ? runs->size() : 0;
        return any ? runs->data() : nullptr;
    }

    static simjig_status cov_zero(simjig_handle* h) noexcept {
        if constexpr (!kCoverage) {
            return SIMJIG_UNSUPPORTED;
        } else {
            return observe(h, [h] { h->rt.context().coveragep()->zero(); });
        }
    }

    static simjig_status cov_write(simjig_handle* h, const char* path) noexcept {
        if constexpr (!kCoverage) {
            return SIMJIG_UNSUPPORTED;
        } else {
            std::string file;
            const simjig_status named = observe(h, [h, path, &file] {
                file = path != nullptr ? path : h->rt.context().coveragep()->defaultFilename();
            });
            if (named != SIMJIG_OK) return named;
            // VerilatedCovContext::write reports an unwritable file with a fatal, which would
            // poison a model whose simulation is fine.
            if (!probe_writable(h->rt, file.c_str(), "coverage file")) return SIMJIG_IO_ERROR;
            return observe(h, [h, &file] { h->rt.context().coveragep()->write(file); });
        }
    }

    // simjig-build defines SIMJIG_PUBLIC_RW=1 for a model whose scope table has a writable
    // variable (`syms::writable_public`), known only once Verilator has run, after the shim
    // was emitted.
#if defined(SIMJIG_PUBLIC_RW) && SIMJIG_PUBLIC_RW
    static constexpr uint64_t kPublicRw = SIMJIG_FEATURE_PUBLIC_RW;
#else
    static constexpr uint64_t kPublicRw = 0;
#endif
    static constexpr uint64_t kFeatures = kPublicRw | (Traits::kTiming ? SIMJIG_FEATURE_TIMING : 0)
                                          | (kTrace ? SIMJIG_FEATURE_TRACE_FST : 0)
                                          | (kCoverage ? SIMJIG_FEATURE_COVERAGE : 0)
                                          | (x_initial_unique_of<Traits>()
                                                 ? SIMJIG_FEATURE_RANDOM_INIT
                                                 : 0);

    static constexpr simjig_model_vtable table{
        SIMJIG_ABI_VERSION,
        sizeof(simjig_model_vtable),
        Traits::kPrefix,
        VERILATOR_VERSION,
        Traits::kIrHash,
        &create,
        &simjig_rt_destroy,
        &eval,
        &final_blocks,
        &simjig_rt_set_time,
        &events_pending,
        &next_time_slot,
        &signals,
        &simjig_rt_drain_msgs,
        &trace_open,
        &trace_dump,
        &trace_close,
        &cov_runs,
        kFeatures,
        &trace_open_ex,
        &trace_flush,
        &trace_reopen,
        &cov_zero,
        &cov_write,
        &simjig_rt_rng_seed,
        &simjig_rt_hold,
        &simjig_rt_release,
        &eval_held,
    };
};

}  // namespace simjig

// Binds one member into the signal table, after checking its C++ type against `width`.
#define SIMJIG_SIGNAL(table_, member_, name_, width_, dir_, access_)                            \
    do {                                                                                     \
        using simjig_member_t_ = std::remove_cv_t<std::remove_reference_t<decltype(member_)>>; \
        static_assert(::simjig::storage_of<simjig_member_t_>::value                         \
                          == ::simjig::storage_for_width(width_),                            \
                      "simjig: storage type of " name_ " does not match its width");         \
        static_assert(sizeof(simjig_member_t_) == ::simjig::storage_bytes(width_),          \
                      "simjig: size of " name_ " does not match its width");                 \
        (table_).add((width_), (dir_), (access_), (name_), static_cast<void*>(&(member_)));  \
    } while (0)

// Binds one probe: the public_flat variable `var_` of scope `<model name>.<scope_>`, found and
// checked against `width_` and `access_` at create (simjig::find_probe throws on a mismatch).
#define SIMJIG_PROBE(table_, model_, scope_, var_, name_, width_, access_)                   \
    (table_).add((width_), SIMJIG_DIR_INTERNAL, (access_), (name_),                          \
                 ::simjig::find_probe(*(model_).contextp(), (model_).name(), (scope_), (var_),  \
                                      (name_), (width_), (access_)))

// Exports the model's single C symbol, `simjig_vtable_<PREFIX>`.
#define SIMJIG_EXPORT_VTABLE(PREFIX, TRAITS)                                     \
    extern "C" const simjig_model_vtable* simjig_vtable_##PREFIX(void) {         \
        return &::simjig::Vtable<TRAITS>::table;                                 \
    }

#endif  // SIMJIG_MODEL_H_

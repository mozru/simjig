// simjig_rt.h: the hand-written C++ runtime shared by every simjig model.
//
// Concepts:
// - `simjig::Runtime` is the per-handle state: the model's own VerilatedContext, the message
//   ring, and the sticky outcome flags (finished, stopped, fatal, poisoned).
// - The *active* runtime is a thread-local pointer set for the duration of each entry point
//   (`Scope`). Verilator's hooks (vl_fatal, vl_stop_maybe, VL_PRINTF, ...) take no context
//   argument, so they find their model through it.
// - Per-model isolation (docs/notes/spikes.md §2, docs/notes/tracecov.md): a Scope also makes
//   the model's context Verilator's thread context (`$time`, `$finish` flags, `$display`
//   attribution) and swaps the model's private VlRNG into Verilator's thread-local `$urandom`
//   state, so each model's stream depends only on its own seed and its own draws.
// - `guard()` is the exception barrier. Every `extern "C"` entry point that runs model code
//   runs it inside guard(), so no C++ exception ever reaches Rust (that would be UB).
//
// Invariants:
// - Once poisoned, a handle never runs model code again: guard() returns POISONED.
// - A FatalUnwind is only ever thrown by a hook while a Scope is active, i.e. inside guard().
#ifndef SIMJIG_RT_H_
#define SIMJIG_RT_H_

#include "simjig_abi.h"
#include "verilated.h"

#include <cstddef>
#include <cstdint>
#include <deque>
#include <exception>
#include <memory>
#include <string>
#include <type_traits>
#include <vector>

namespace simjig {

// Thrown by the fatal hooks to abandon the current eval. Deliberately not a std::exception, so
// that no `catch (const std::exception&)` other than the barrier's can swallow it.
struct FatalUnwind final {};

// One recorded message; simjig_msg is its borrowed C view.
struct Record {
    uint32_t kind;
    uint32_t line;
    uint64_t time;
    std::string file;
    std::string hier;
    std::string msg;
};

class Runtime final {
public:
    // `coroutines`: the model was verilated with --timing. A C++ exception that leaves a
    // Verilator coroutine calls VlPromise::unhandled_exception(), which is std::abort() in
    // 5.052, so `$fatal`/`$stop` must not throw in such models (see on_stop_maybe).
    Runtime(const simjig_create_opts* opts, bool coroutines);

    VerilatedContext& context() { return *m_context; }
    // The model's private `$urandom` state while no Scope of this runtime is active (inside
    // one it is swapped into VlRNG::vl_thread_rng(), and this holds the outer state).
    VlRNG& rng() { return m_rng; }
    // Replaces the model's private `$urandom` state with VlRNG(seed), wherever it currently
    // lives (the thread slot while this runtime's Scope is active, else m_rng).
    void reseed(uint64_t seed) noexcept;
    // The seed Verilator gives the thread stream of this context: VerilatedContextImp::
    // randSeedDefault64() (verilated.cpp) for a nonzero randSeed (+verilator+seed+, default 1).
    uint64_t context_seed() const;
    bool poisoned() const { return m_poisoned; }
    void poison() { m_poisoned = true; }

    // Verilator hooks (called through the active runtime).
    void on_print(const char* text, size_t len) noexcept;
    void on_finish(const char* file, int line, const char* hier) noexcept;
    void on_stop_maybe(const char* file, int line, const char* hier, bool maybe);
    [[noreturn]] void on_fatal(const char* file, int line, const char* hier, const char* msg);
    void on_warn(const char* file, int line, const char* hier, const char* msg) noexcept;
    // Barrier: a non-fatal C++ exception escaped model code.
    void on_exception(const char* what) noexcept;

    // Coverage: every counter a point registers (VL_COVER_INSERT, simjig_vl_config.h) is noted
    // while the model is constructed. cov_runs() then keeps those inside the model's symbol
    // table object [begin, begin + size), which holds every module object and the table's own
    // array as members, and drops the rest (Verilator's shared `fake_zero_count` for later
    // instances of a module, which is a function-local static). The counters are sorted by
    // address, which is symbol-table member order, deduplicated (an object-local and a
    // symbol-table counter are each registered once per point) and merged into maximal
    // contiguous runs. Consumes the noted counters.
    void note_cov_counter(uint32_t* countp) noexcept;
    std::vector<simjig_cov_run> cov_runs(const void* begin, size_t size);

    // Status after a completed (non-throwing) call; poisons on a deferred fatal.
    simjig_status outcome();
    size_t drain(simjig_msg* out, size_t cap) noexcept;

private:
    void push(Record record) noexcept;
    // If the newest record is the text Verilator printed just before calling the hook (e.g.
    // "[0] %Error: t.sv:3: Assertion failed in TOP.t: boom"), turn it into the hook's record
    // instead of adding a duplicate. Returns false if there was no such text.
    bool adopt_print(uint32_t kind, const char* marker, const char* file, int line,
                     const char* hier) noexcept;

    std::unique_ptr<VerilatedContext> m_context;
    VlRNG m_rng{0};  // reseeded by create (see Vtable::create); VlRNG(0) draws nothing
    std::deque<Record> m_ring;
    std::vector<Record> m_drained;  // backs the string pointers handed out by drain()
    std::vector<uint32_t*> m_cov_counters;
    bool m_cov_lost = false;
    size_t m_dropped = 0;
    size_t m_hook_mark = 0;  // m_ring.size() right after the last hook record
    bool m_echo = false;
    bool m_coroutines = false;
    bool m_stopped = false;
    bool m_fatal = false;  // a fatal was recorded without unwinding (coroutine models)
    bool m_poisoned = false;
};

// The runtime whose entry point is running on this thread, or nullptr.
Runtime* active() noexcept;

// Makes `rt` the active runtime, its context Verilator's thread context and its VlRNG the
// thread's `$urandom` state; undoes all three on exit (scopes nest, e.g. a DPI call that
// evaluates another model).
class Scope final {
public:
    explicit Scope(Runtime& rt) noexcept;
    ~Scope();
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;

private:
    Runtime& m_rt;
    Runtime* m_previous;
};

// Verilator's thread-local `$urandom` state (VlRNG::vl_thread_rng()) with its reseed epoch
// brought up to date *without* losing the state. vl_thread_rng() overwrites the state from the
// thread context's seed whenever any context's seed changed since its last call on this thread
// (verilated.cpp, VlRNG::vl_thread_rng), including when another model's context is created.
// Swapping through this keeps such a reseed from landing on a model's private state.
VlRNG& thread_rng() noexcept;

// The identity of a file, to find descriptors open on it after it was renamed or unlinked.
struct FileId {
    uint64_t dev = 0;
    uint64_t ino = 0;
    bool valid = false;
};
FileId file_id(const char* path) noexcept;
// Points every descriptor of this process open on `id` at /dev/null (dup2). The descriptors
// come from the kernel's list (/proc/self/fd, /dev/fd), not a scan up to RLIMIT_NOFILE, and the
// soft limit is raised for the call if one lies above it. Returns how many were redirected, or
// -1 if /dev/null could not be opened.
int redirect_to_devnull(const FileId& id) noexcept;
// Creates (or truncates) `path` for writing and closes it again, so that a writer that cannot
// report an open failure (fstcpp's std::ofstream) or reports it with a fatal
// (VerilatedCovContext::write) is never given a path it cannot create. On failure records a
// WARNING naming `what` and the reason, and returns false.
bool probe_writable(Runtime& rt, const char* path, const char* what) noexcept;

// The address of a probe: the public_flat_rd/_rw variable `var` of scope `<model_name>.<scope>`
// in the model's scope table (V<prefix>__Syms__Slow.cpp). Verilator registers it there with
// its real address whether its module was inlined into the root or kept as its own object, so
// this is the one way to find a probe that does not depend on inlining (V3Inline.cpp in 5.052
// keeps large multi-use modules, modules with classes and no_inline_module ones apart). The
// variable must have the storage class Verilator uses for `width`, exactly `width` bits and no
// unpacked dimension. A probe with SIMJIG_ACCESS_WRITE in `access` must be public_flat_rw and
// not a parameter: Verilator stores parameters as `static constexpr` members, so a writable
// pointer to one would let safe Rust write read-only memory (SIGBUS on macOS, undefined
// behaviour anywhere); a read-only probe of a parameter is fine. Throws std::runtime_error
// naming the probe (`display`) otherwise.
void* find_probe(VerilatedContext& ctx, const char* model_name, const char* scope,
                 const char* var, const char* display, uint32_t width, uint8_t access);

}  // namespace simjig

// The C-opaque handle. The generated per-model handle derives from it and owns the model as a
// member, so the model is destroyed before `rt` (and with it the VerilatedContext), which is
// the order Verilator requires.
struct simjig_handle {
    simjig_handle(const simjig_create_opts* opts, bool coroutines) : rt{opts, coroutines} {}
    virtual ~simjig_handle() = default;
    // Destroys the model and everything attached to it (trace first). simjig_rt_destroy calls
    // it inside a Scope, while `rt` and its context are still alive.
    virtual void teardown() noexcept {}
    simjig_handle(const simjig_handle&) = delete;
    simjig_handle& operator=(const simjig_handle&) = delete;
    // Runs `fn(*this)` behind the barrier. Not a template on purpose: keeps the barrier in
    // one audited place (simjig_rt.cpp).
    simjig_status guard(void (*fn)(simjig_handle&, void*), void* arg) noexcept;

    simjig::Runtime rt;
};

namespace simjig {

// Runs `body()` behind the exception barrier on `h`; see the file comment.
template <class F>
simjig_status guard(simjig_handle* h, F&& body) noexcept {
    if (h == nullptr) return SIMJIG_EXCEPTION;
    using Body = std::remove_reference_t<F>;
    return h->guard([](simjig_handle&, void* p) { (*static_cast<Body*>(p))(); },
                    static_cast<void*>(&body));
}

}  // namespace simjig

// Model-independent vtable entries, defined in simjig_rt.cpp.
extern "C" {
void simjig_rt_destroy(simjig_handle* h);
void simjig_rt_set_time(simjig_handle* h, uint64_t time);
size_t simjig_rt_drain_msgs(simjig_handle* h, simjig_msg* out, size_t cap);
simjig_status simjig_rt_rng_seed(simjig_handle* h, uint64_t seed);
// The ABI structs' layout as this compiler sees it, for the Rust mirror's tests: writes up to
// `cap` of {sizeof, alignof} for simjig_model_vtable, simjig_signal, simjig_msg,
// simjig_create_opts and simjig_cov_run, in that order. Returns the number of values written.
size_t simjig_rt_abi_layout(uint32_t* out, size_t cap);

// One field of a struct in simjig_abi.h, as this compiler lays it out.
typedef struct simjig_abi_field {
    const char* type;  // the struct's C name, e.g. "simjig_model_vtable"
    const char* name;  // the field's name, e.g. "trace_open_ex"
    uint32_t offset;   // offsetof(type, name)
    uint32_t size;     // sizeof the field
} simjig_abi_field;
// Every field of simjig_model_vtable, simjig_signal, simjig_msg, simjig_create_opts and
// simjig_cov_run, in that struct order and in declaration order within each; *count entries, static storage.
// The Rust mirror's offset_of! asserts are compared against it (docs/notes/tracecov.md).
const simjig_abi_field* simjig_rt_abi_fields(size_t* count);
}

#endif  // SIMJIG_RT_H_

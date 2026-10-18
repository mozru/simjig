// simjig_rt.h: the hand-written C++ runtime shared by every simjig model.
//
// Concepts:
// - `simjig::Runtime` is the per-handle state: the model's own VerilatedContext, the message
//   ring, and the sticky outcome flags (finished, stopped, fatal, poisoned).
// - The *active* runtime is a thread-local pointer set for the duration of each entry point
//   (`Scope`). Verilator's hooks (vl_fatal, vl_stop_maybe, VL_PRINTF, ...) take no context
//   argument, so they find their model through it.
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
    std::deque<Record> m_ring;
    std::vector<Record> m_drained;  // backs the string pointers handed out by drain()
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

// Makes `rt` the active runtime and its context Verilator's thread context.
class Scope final {
public:
    explicit Scope(Runtime& rt) noexcept;
    ~Scope();
    Scope(const Scope&) = delete;
    Scope& operator=(const Scope&) = delete;

private:
    Runtime* m_previous;
};

}  // namespace simjig

// The C-opaque handle. The generated per-model handle derives from it and owns the model as a
// member, so the model is destroyed before `rt` (and with it the VerilatedContext), which is
// the order Verilator requires.
struct simjig_handle {
    simjig_handle(const simjig_create_opts* opts, bool coroutines) : rt{opts, coroutines} {}
    virtual ~simjig_handle() = default;
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
simjig_status simjig_rt_trace_open_unsupported(simjig_handle* h, const char* path);
simjig_status simjig_rt_trace_dump_unsupported(simjig_handle* h, uint64_t time);
simjig_status simjig_rt_trace_close_unsupported(simjig_handle* h);
uint32_t* simjig_rt_cov_counters_none(simjig_handle* h, size_t* count);
// The ABI structs' layout as this compiler sees it, for the Rust mirror's tests: writes up to
// `cap` of {sizeof, alignof} for simjig_model_vtable, simjig_signal, simjig_msg and
// simjig_create_opts, in that order. Returns the number of values written.
size_t simjig_rt_abi_layout(uint32_t* out, size_t cap);
}

#endif  // SIMJIG_RT_H_

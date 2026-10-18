// simjig_rt.cpp: Verilator user hooks, output capture and the exception barrier.
//
// Compiled once per (Verilator version, compiler, simjig_vl_config.h) into the same archive as
// libverilated, with `-include simjig_vl_config.h` (see docs/notes/abi.md for the recipe).
//
// How SystemVerilog failures reach these hooks in Verilator 5.052 (V3EmitCFunc.h, verilated.cpp):
//   $fatal  -> VL_WRITEF_NX("[t] %Fatal: f.sv:N: ...")  then VL_STOP_MT(f, N, "", false)
//   $error  -> VL_WRITEF_NX("[t] %Error: f.sv:N: ...")  then VL_STOP_MT(f, N, "")  (maybe=true)
//   $stop   ->                                              VL_STOP_MT(f, N, "")  (maybe=true)
//   $finish -> VL_FINISH_MT(f, N, "")
//   internal errors ($readmem, convergence, null deref) -> VL_FATAL_MT -> vl_fatal
// With one simulation thread the *_MT wrappers call vl_stop_maybe/vl_finish/vl_fatal at once.
// The hooks cannot tell $stop from $error by their arguments; only the "%Error:" text that
// precedes an $error tells them apart (Runtime::adopt_print).
#include "simjig_rt.h"

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <utility>

namespace simjig {
namespace {

// Bounded so a chatty model that is never drained cannot grow without limit. When full, the
// oldest record is dropped: the newest records (the failure) are the ones a report needs.
constexpr size_t kRingCapacity = 4096;

thread_local Runtime* t_active = nullptr;

std::string trim_newline(std::string text) {
    while (!text.empty() && (text.back() == '\n' || text.back() == '\r')) text.pop_back();
    return text;
}

const char* or_empty(const char* s) { return s != nullptr ? s : ""; }

// No runtime is active: a hook ran outside every entry point (it cannot be caught). Report and
// end the process the way Verilator's default would, but say why.
[[noreturn]] void orphan_fatal(const char* file, int line, const char* msg) {
    std::fprintf(stderr, "simjig: fatal outside any simjig entry point: %s:%d: %s\n",
                 or_empty(file), line, or_empty(msg));
    std::fflush(stderr);
    std::abort();
}

}  // namespace

Runtime* active() noexcept { return t_active; }

Scope::Scope(Runtime& rt) noexcept : m_previous{t_active} {
    t_active = &rt;
    // Verilator's hooks and VL_*_MT wrappers use the thread context, not the model's: point it
    // at this model's context so several models on one thread stay separate.
    Verilated::threadContextp(&rt.context());
}

Scope::~Scope() { t_active = m_previous; }

Runtime::Runtime(const simjig_create_opts* opts, bool coroutines)
    : m_context{std::make_unique<VerilatedContext>()}, m_coroutines{coroutines} {
    // Only Verilator's default vl_stop reads this; set it anyway so nothing else exits.
    m_context->fatalOnError(false);
    if (opts != nullptr) m_echo = (opts->flags & SIMJIG_CREATE_ECHO) != 0;
}

void Runtime::push(Record record) noexcept {
    try {
        if (m_echo) {
            std::fwrite(record.msg.data(), 1, record.msg.size(), stdout);
            if (record.kind != SIMJIG_MSG_DISPLAY) std::fputc('\n', stdout);
        }
        if (m_ring.size() == kRingCapacity) {
            m_ring.pop_front();
            ++m_dropped;
            if (m_hook_mark > 0) --m_hook_mark;
        }
        m_ring.push_back(std::move(record));
    } catch (...) {
        ++m_dropped;  // allocation failure: count the record as lost rather than throw
    }
}

void Runtime::on_print(const char* text, size_t len) noexcept {
    try {
        // A $write without a newline continues in the next print: keep it one record.
        if (!m_ring.empty() && m_ring.size() > m_hook_mark
            && m_ring.back().kind == SIMJIG_MSG_DISPLAY && !m_ring.back().msg.empty()
            && m_ring.back().msg.back() != '\n') {
            m_ring.back().msg.append(text, len);
            if (m_echo) std::fwrite(text, 1, len, stdout);
            return;
        }
        push(Record{SIMJIG_MSG_DISPLAY, 0, m_context->time(), "", "", std::string(text, len)});
    } catch (...) {
        ++m_dropped;
    }
}

bool Runtime::adopt_print(uint32_t kind, const char* marker, const char* file, int line,
                          const char* hier) noexcept {
    if (m_ring.empty() || m_ring.size() <= m_hook_mark) return false;
    Record& last = m_ring.back();
    if (last.kind != SIMJIG_MSG_DISPLAY || last.msg.find(marker) == std::string::npos) {
        return false;
    }
    try {
        last.kind = kind;
        last.line = static_cast<uint32_t>(line);
        last.file = or_empty(file);
        last.hier = or_empty(hier);
        last.msg = trim_newline(std::move(last.msg));
    } catch (...) {
        ++m_dropped;
    }
    m_hook_mark = m_ring.size();
    return true;
}

void Runtime::on_finish(const char* file, int line, const char* hier) noexcept {
    push(Record{SIMJIG_MSG_FINISH, static_cast<uint32_t>(line), m_context->time(),
                or_empty(file), or_empty(hier), "Verilog $finish"});
    m_hook_mark = m_ring.size();
    m_context->gotFinish(true);
}

void Runtime::on_stop_maybe(const char* file, int line, const char* hier, bool maybe) {
    m_context->errorCountInc();
    m_context->gotError(true);
    if (!maybe) {  // $fatal
        if (!adopt_print(SIMJIG_MSG_FATAL, "%Fatal: ", file, line, hier)) {
            push(Record{SIMJIG_MSG_FATAL, static_cast<uint32_t>(line), m_context->time(),
                        or_empty(file), or_empty(hier), "Verilog $fatal"});
            m_hook_mark = m_ring.size();
        }
        m_context->gotFinish(true);
        // Unwinding ends the process inside a coroutine (VlPromise::unhandled_exception aborts),
        // so --timing models record and return; outcome() then reports FATAL and poisons.
        if (m_coroutines) {
            m_fatal = true;
            return;
        }
        throw FatalUnwind{};
    }
    if (adopt_print(SIMJIG_MSG_ERROR, "%Error: ", file, line, hier)) {
        // $error: the simulation continues until the error limit (Verilator's own rule).
        if (m_context->errorCount() < m_context->errorLimit()) return;
    } else {
        // $stop. Verilator would ignore it below the error limit; IEEE 1800-2023 §20.2 says it
        // suspends the simulation, so simjig always reports STOP.
        push(Record{SIMJIG_MSG_STOP, static_cast<uint32_t>(line), m_context->time(),
                    or_empty(file), or_empty(hier), "Verilog $stop"});
        m_hook_mark = m_ring.size();
    }
    m_stopped = true;
    m_context->gotFinish(true);
}

void Runtime::on_fatal(const char* file, int line, const char* hier, const char* msg) {
    push(Record{SIMJIG_MSG_FATAL, static_cast<uint32_t>(line), m_context->time(),
                or_empty(file), or_empty(hier), or_empty(msg)});
    m_hook_mark = m_ring.size();
    m_context->gotError(true);
    m_context->gotFinish(true);
    if (m_coroutines) {
        // vl_fatal callers may be followed by VL_UNREACHABLE, so returning is not an option;
        // unwinding is, but aborts inside a coroutine. Leave a trace that survives an abort.
        std::fprintf(stderr, "simjig: fatal: %s:%d: %s\n", or_empty(file), line, or_empty(msg));
        std::fflush(stderr);
    }
    throw FatalUnwind{};
}

void Runtime::on_warn(const char* file, int line, const char* hier, const char* msg) noexcept {
    push(Record{SIMJIG_MSG_WARNING, static_cast<uint32_t>(line), m_context->time(),
                or_empty(file), or_empty(hier), or_empty(msg)});
    m_hook_mark = m_ring.size();
}

void Runtime::on_exception(const char* what) noexcept {
    push(Record{SIMJIG_MSG_EXCEPTION, 0, m_context->time(), "", "", or_empty(what)});
    m_hook_mark = m_ring.size();
    m_poisoned = true;
}

simjig_status Runtime::outcome() {
    if (m_fatal) {
        m_poisoned = true;
        return SIMJIG_FATAL;
    }
    if (m_stopped) return SIMJIG_STOP;
    if (m_context->gotFinish()) return SIMJIG_FINISH;
    return SIMJIG_OK;
}

size_t Runtime::drain(simjig_msg* out, size_t cap) noexcept {
    const size_t pending = m_ring.size() + (m_dropped > 0 ? 1 : 0);
    if (out == nullptr || cap == 0) return pending;
    m_drained.clear();
    try {
        m_drained.reserve(std::min(cap, pending));
        if (m_dropped > 0) {
            m_drained.push_back(Record{SIMJIG_MSG_DROPPED, static_cast<uint32_t>(m_dropped),
                                       m_context->time(), "", "", "messages dropped"});
            m_dropped = 0;
        }
        while (m_drained.size() < cap && !m_ring.empty()) {
            m_drained.push_back(std::move(m_ring.front()));
            m_ring.pop_front();
            if (m_hook_mark > 0) --m_hook_mark;
        }
    } catch (...) {
        // reserve() failed: hand out what was moved so far.
    }
    for (size_t i = 0; i < m_drained.size(); ++i) {
        const Record& r = m_drained[i];
        out[i] = simjig_msg{r.kind,         r.line,       r.time,      r.file.c_str(),
                            r.hier.c_str(), r.msg.c_str(), r.msg.size()};
    }
    return m_drained.size();
}

}  // namespace simjig

simjig_status simjig_handle::guard(void (*fn)(simjig_handle&, void*), void* arg) noexcept {
    if (rt.poisoned()) return SIMJIG_POISONED;
    try {
        const simjig::Scope scope{rt};
        fn(*this, arg);
        return rt.outcome();
    } catch (const simjig::FatalUnwind&) {
        rt.poison();  // the hook recorded the message before throwing
        return SIMJIG_FATAL;
    } catch (const std::exception& e) {
        rt.on_exception(e.what());
        return SIMJIG_EXCEPTION;
    } catch (...) {
        rt.on_exception("unknown C++ exception");
        return SIMJIG_EXCEPTION;
    }
}

//==============================================================================================
// Verilator user hooks (enabled by VL_USER_* in simjig_vl_config.h).

void vl_finish(const char* filename, int linenum, const char* hier) {
    if (simjig::Runtime* rt = simjig::active()) return rt->on_finish(filename, linenum, hier);
    Verilated::threadContextp()->gotFinish(true);
}

void vl_stop_maybe(const char* filename, int linenum, const char* hier, bool maybe) {
    if (simjig::Runtime* rt = simjig::active()) return rt->on_stop_maybe(filename, linenum, hier, maybe);
    simjig::orphan_fatal(filename, linenum, maybe ? "$stop/$error" : "$fatal");
}

// Verilator's own vl_stop_maybe calls vl_stop; nothing in 5.052 calls vl_stop directly, but it
// is a declared extern, so keep it defined with $stop meaning.
void vl_stop(const char* filename, int linenum, const char* hier) {
    vl_stop_maybe(filename, linenum, hier, true);
}

void vl_fatal(const char* filename, int linenum, const char* hier, const char* msg) {
    if (simjig::Runtime* rt = simjig::active()) rt->on_fatal(filename, linenum, hier, msg);
    simjig::orphan_fatal(filename, linenum, msg);
}

void vl_warn(const char* filename, int linenum, const char* hier, const char* msg) {
    if (simjig::Runtime* rt = simjig::active()) return rt->on_warn(filename, linenum, hier, msg);
    std::fprintf(stderr, "%%Warning: %s:%d: %s\n", simjig::or_empty(filename), linenum,
                 simjig::or_empty(msg));
}

int simjig_vl_vprintf(const char* format, va_list args) {
    va_list again;
    va_copy(again, args);
    const int len = std::vsnprintf(nullptr, 0, format, args);
    if (len <= 0) {
        va_end(again);
        return len;
    }
    std::string text(static_cast<size_t>(len), '\0');
    std::vsnprintf(text.data(), text.size() + 1, format, again);
    va_end(again);
    if (simjig::Runtime* rt = simjig::active()) {
        rt->on_print(text.data(), text.size());
    } else {
        std::fwrite(text.data(), 1, text.size(), stdout);
    }
    return len;
}

int simjig_vl_printf(const char* format, ...) {
    va_list args;
    va_start(args, format);
    const int len = simjig_vl_vprintf(format, args);
    va_end(args);
    return len;
}

//==============================================================================================
// Model-independent vtable entries.

extern "C" {

void simjig_rt_destroy(simjig_handle* h) {
    if (h == nullptr) return;
    // The model's destructor may consult the thread context, so make this model's current.
    const simjig::Scope scope{h->rt};
    try {
        delete h;
    } catch (...) {
        // Destructors are noexcept; nothing reaches here in practice. Never let it escape.
    }
}

void simjig_rt_set_time(simjig_handle* h, uint64_t time) {
    if (h != nullptr) h->rt.context().time(time);
}

size_t simjig_rt_drain_msgs(simjig_handle* h, simjig_msg* out, size_t cap) {
    return h != nullptr ? h->rt.drain(out, cap) : 0;
}

simjig_status simjig_rt_trace_open_unsupported(simjig_handle*, const char*) {
    return SIMJIG_UNSUPPORTED;
}
simjig_status simjig_rt_trace_dump_unsupported(simjig_handle*, uint64_t) {
    return SIMJIG_UNSUPPORTED;
}
simjig_status simjig_rt_trace_close_unsupported(simjig_handle*) { return SIMJIG_UNSUPPORTED; }

uint32_t* simjig_rt_cov_counters_none(simjig_handle*, size_t* count) {
    if (count != nullptr) *count = 0;
    return nullptr;
}

size_t simjig_rt_abi_layout(uint32_t* out, size_t cap) {
    const uint32_t layout[] = {
        sizeof(simjig_model_vtable), alignof(simjig_model_vtable),
        sizeof(simjig_signal),       alignof(simjig_signal),
        sizeof(simjig_msg),          alignof(simjig_msg),
        sizeof(simjig_create_opts),  alignof(simjig_create_opts),
    };
    const size_t n = std::min(cap, sizeof(layout) / sizeof(layout[0]));
    if (out != nullptr) std::copy(layout, layout + n, out);
    return n;
}

}  // extern "C"

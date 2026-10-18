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

#include "verilated_syms.h"

#include <algorithm>
#include <functional>
#include <cerrno>
#include <stdexcept>
#include <cstdio>
#include <cstring>
#include <utility>

#include <fcntl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <unistd.h>

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

void note_cov_counter(uint32_t* countp) noexcept {
    // Points register while the model is constructed, inside create's Scope. A model built
    // outside simjig has no active runtime and nobody to tell.
    if (t_active != nullptr) t_active->note_cov_counter(countp);
}

void Runtime::note_cov_counter(uint32_t* countp) noexcept {
    try {
        m_cov_counters.push_back(countp);
    } catch (...) {
        m_cov_lost = true;  // out of memory: cov_runs then reports nothing rather than a part
    }
}

std::vector<simjig_cov_run> Runtime::cov_runs(const void* begin, size_t size) {
    std::vector<uint32_t*> seen = std::move(m_cov_counters);
    m_cov_counters.clear();
    if (m_cov_lost) return {};
    const auto* lo = static_cast<const char*>(begin);
    const auto inside = [lo, size](uint32_t* p) {
        const auto* c = reinterpret_cast<const char*>(p);
        return std::less_equal<const char*>{}(lo, c) && std::less<const char*>{}(c, lo + size);
    };
    seen.erase(std::remove_if(seen.begin(), seen.end(), [&](uint32_t* p) { return !inside(p); }),
               seen.end());
    std::sort(seen.begin(), seen.end(), std::less<uint32_t*>{});
    seen.erase(std::unique(seen.begin(), seen.end()), seen.end());
    std::vector<simjig_cov_run> runs;
    for (uint32_t* p : seen) {
        if (!runs.empty() && runs.back().counters + runs.back().len == p) {
            ++runs.back().len;
        } else {
            runs.push_back(simjig_cov_run{p, 1});
        }
    }
    return runs;
}

VlRNG& thread_rng() noexcept {
    // The address of a thread_local is fixed for the thread's lifetime, so it is taken once.
    // The first call may reseed: the state then belongs to no model yet.
    static thread_local VlRNG* t_rng = nullptr;
    if (t_rng == nullptr) {
        t_rng = &VlRNG::vl_thread_rng();
        return *t_rng;
    }
    const VlRNG keep = *t_rng;
    (void)VlRNG::vl_thread_rng();  // syncs the thread's seed epoch; may overwrite the state
    *t_rng = keep;
    return *t_rng;
}

Scope::Scope(Runtime& rt) noexcept : m_rt{rt}, m_previous{t_active} {
    t_active = &rt;
    // Verilator's hooks and VL_*_MT wrappers use the thread context, not the model's: point it
    // at this model's context so several models on one thread stay separate. It must be set
    // before thread_rng(), whose reseed path reads it.
    Verilated::threadContextp(&rt.context());
    std::swap(thread_rng(), rt.rng());
}

Scope::~Scope() {
    std::swap(thread_rng(), m_rt.rng());
    t_active = m_previous;
    if (m_previous != nullptr) Verilated::threadContextp(&m_previous->context());
}

FileId file_id(const char* path) noexcept {
    struct stat st{};
    if (path == nullptr || ::stat(path, &st) != 0) return FileId{};
    return FileId{static_cast<uint64_t>(st.st_dev), static_cast<uint64_t>(st.st_ino), true};
}

int redirect_to_devnull(const FileId& id) noexcept {
    if (!id.valid) return 0;
    const int null_fd = ::open("/dev/null", O_WRONLY | O_CLOEXEC);
    if (null_fd < 0) return -1;
    // Descriptors are small integers below the soft limit; cap the scan for "unlimited".
    struct rlimit lim{};
    rlim_t max_fd = 1024;
    if (::getrlimit(RLIMIT_NOFILE, &lim) == 0 && lim.rlim_cur != RLIM_INFINITY) {
        max_fd = std::min<rlim_t>(lim.rlim_cur, 65536);
    }
    int moved = 0;
    for (int fd = 0; fd < static_cast<int>(max_fd); ++fd) {
        struct stat st{};
        if (fd == null_fd || ::fstat(fd, &st) != 0) continue;
        if (static_cast<uint64_t>(st.st_dev) == id.dev
            && static_cast<uint64_t>(st.st_ino) == id.ino && ::dup2(null_fd, fd) == fd) {
            ++moved;
        }
    }
    ::close(null_fd);
    return moved;
}

bool probe_writable(Runtime& rt, const char* path, const char* what) noexcept {
    if (path != nullptr) {
        if (std::FILE* f = std::fopen(path, "wb")) {
            std::fclose(f);
            return true;
        }
    }
    try {
        const std::string msg = std::string{"cannot create "} + what + " '" + or_empty(path)
                                + "': " + (path != nullptr ? std::strerror(errno) : "no path");
        rt.on_warn("", 0, "", msg.c_str());
    } catch (...) {
        rt.on_warn("", 0, "", "cannot create an output file");
    }
    return false;
}

void* find_probe(VerilatedContext& ctx, const char* model_name, const char* scope,
                 const char* var, const char* display, uint32_t width) {
    const std::string scope_name = std::string{or_empty(model_name)} + "." + or_empty(scope);
    const auto fail = [&](const std::string& why) {
        throw std::runtime_error("simjig: probe '" + std::string{or_empty(display)} + "': " + why);
    };
    const VerilatedScope* s = ctx.scopeFind(scope_name.c_str());
    if (s == nullptr) fail("no scope '" + scope_name + "' in the model (not public_flat?)");
    const VerilatedVar* v = s->varFind(or_empty(var));
    if (v == nullptr) {
        fail("no public variable '" + std::string{or_empty(var)} + "' in scope '" + scope_name
             + "' (a probe inside a hierarchical block, hier_block, is in another model)");
    }
    if (v->udims() != 0) fail("the model's variable is an unpacked array");
    if (v->entBits() != static_cast<int>(width)) {
        fail("the model's variable is " + std::to_string(v->entBits())
             + " bits wide, but it is bound as " + std::to_string(width) + " bits");
    }
    // verilated_sym_props.h: the storage class Verilator chose for the variable.
    const VerilatedVarType expected = width <= 8    ? VLVT_UINT8
                                      : width <= 16 ? VLVT_UINT16
                                      : width <= 32 ? VLVT_UINT32
                                      : width <= 64 ? VLVT_UINT64
                                                    : VLVT_WDATA;
    if (v->vltype() != expected) fail("the model's variable is not stored as a packed vector");
    if (v->datap() == nullptr) fail("the model's variable has no address");
    return v->datap();
}

Runtime::Runtime(const simjig_create_opts* opts, bool coroutines)
    : m_context{std::make_unique<VerilatedContext>()}, m_coroutines{coroutines} {
    // Only Verilator's default vl_stop reads this; set it anyway so nothing else exits.
    m_context->fatalOnError(false);
    // The default is the hardware concurrency, and the first model added to the context then
    // starts that many - 1 pool threads even for a --threads 1 model (docs/notes/spikes.md §1,
    // surprise 1). Threads break fork() and are never used by simjig's --threads 1 models.
    m_context->threads(1);
    if (opts != nullptr) m_echo = (opts->flags & SIMJIG_CREATE_ECHO) != 0;
}

void Runtime::reseed(uint64_t seed) noexcept {
    if (active() == this) {
        thread_rng() = VlRNG{seed};
    } else {
        m_rng = VlRNG{seed};
    }
}

uint64_t Runtime::context_seed() const {
    // Verilator's formula, including its sign extension of a negative seed. Verilator draws a
    // nondeterministic seed for randSeed 0; +verilator+seed+0 already replaces 0 with a drawn
    // seed at parse time, so 0 only comes from code and is kept as the (deterministic) 0.
    const auto s = static_cast<uint64_t>(m_context->randSeed());
    return (s << 32) ^ s;
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
    {
        // The model's destructor may consult the thread context, so make this model's current.
        const simjig::Scope scope{h->rt};
        h->teardown();
    }
    // Outside the Scope: it swaps the RNG back through `h->rt`, which delete frees.
    delete h;
    // ~VerilatedContext leaves the thread context null; an enclosing entry point (a DPI call
    // that destroyed another model) needs its own back.
    if (simjig::Runtime* outer = simjig::active()) Verilated::threadContextp(&outer->context());
}

void simjig_rt_set_time(simjig_handle* h, uint64_t time) {
    if (h != nullptr) h->rt.context().time(time);
}

size_t simjig_rt_drain_msgs(simjig_handle* h, simjig_msg* out, size_t cap) {
    return h != nullptr ? h->rt.drain(out, cap) : 0;
}

simjig_status simjig_rt_rng_seed(simjig_handle* h, uint64_t seed) {
    if (h == nullptr) return SIMJIG_EXCEPTION;
    if (h->rt.poisoned()) return SIMJIG_POISONED;
    h->rt.reseed(seed);
    return SIMJIG_OK;
}

#define SIMJIG_FIELD(T, F) \
    simjig_abi_field { #T, #F, offsetof(T, F), sizeof(static_cast<T*>(nullptr)->F) }
const simjig_abi_field* simjig_rt_abi_fields(size_t* count) {
    static const simjig_abi_field fields[] = {
        SIMJIG_FIELD(simjig_model_vtable, abi_version),
        SIMJIG_FIELD(simjig_model_vtable, size),
        SIMJIG_FIELD(simjig_model_vtable, prefix),
        SIMJIG_FIELD(simjig_model_vtable, verilator_version),
        SIMJIG_FIELD(simjig_model_vtable, ir_hash),
        SIMJIG_FIELD(simjig_model_vtable, create),
        SIMJIG_FIELD(simjig_model_vtable, destroy),
        SIMJIG_FIELD(simjig_model_vtable, eval),
        SIMJIG_FIELD(simjig_model_vtable, final),
        SIMJIG_FIELD(simjig_model_vtable, set_time),
        SIMJIG_FIELD(simjig_model_vtable, events_pending),
        SIMJIG_FIELD(simjig_model_vtable, next_time_slot),
        SIMJIG_FIELD(simjig_model_vtable, signals),
        SIMJIG_FIELD(simjig_model_vtable, drain_msgs),
        SIMJIG_FIELD(simjig_model_vtable, trace_open),
        SIMJIG_FIELD(simjig_model_vtable, trace_dump),
        SIMJIG_FIELD(simjig_model_vtable, trace_close),
        SIMJIG_FIELD(simjig_model_vtable, cov_runs),
        SIMJIG_FIELD(simjig_model_vtable, features),
        SIMJIG_FIELD(simjig_model_vtable, trace_open_ex),
        SIMJIG_FIELD(simjig_model_vtable, trace_flush),
        SIMJIG_FIELD(simjig_model_vtable, trace_reopen),
        SIMJIG_FIELD(simjig_model_vtable, cov_zero),
        SIMJIG_FIELD(simjig_model_vtable, cov_write),
        SIMJIG_FIELD(simjig_model_vtable, rng_seed),
        SIMJIG_FIELD(simjig_signal, id),
        SIMJIG_FIELD(simjig_signal, width),
        SIMJIG_FIELD(simjig_signal, words),
        SIMJIG_FIELD(simjig_signal, storage),
        SIMJIG_FIELD(simjig_signal, dir),
        SIMJIG_FIELD(simjig_signal, access),
        SIMJIG_FIELD(simjig_signal, reserved),
        SIMJIG_FIELD(simjig_signal, name),
        SIMJIG_FIELD(simjig_signal, ptr),
        SIMJIG_FIELD(simjig_msg, kind),
        SIMJIG_FIELD(simjig_msg, line),
        SIMJIG_FIELD(simjig_msg, time),
        SIMJIG_FIELD(simjig_msg, file),
        SIMJIG_FIELD(simjig_msg, hier),
        SIMJIG_FIELD(simjig_msg, msg),
        SIMJIG_FIELD(simjig_msg, msg_len),
        SIMJIG_FIELD(simjig_create_opts, size),
        SIMJIG_FIELD(simjig_create_opts, flags),
        SIMJIG_FIELD(simjig_create_opts, name),
        SIMJIG_FIELD(simjig_create_opts, error_limit),
        SIMJIG_FIELD(simjig_create_opts, argc),
        SIMJIG_FIELD(simjig_create_opts, argv),
        SIMJIG_FIELD(simjig_cov_run, counters),
        SIMJIG_FIELD(simjig_cov_run, len),
    };
    if (count != nullptr) *count = sizeof(fields) / sizeof(fields[0]);
    return fields;
}
#undef SIMJIG_FIELD

size_t simjig_rt_abi_layout(uint32_t* out, size_t cap) {
    const uint32_t layout[] = {
        sizeof(simjig_model_vtable), alignof(simjig_model_vtable),
        sizeof(simjig_signal),       alignof(simjig_signal),
        sizeof(simjig_msg),          alignof(simjig_msg),
        sizeof(simjig_create_opts),  alignof(simjig_create_opts),
        sizeof(simjig_cov_run),      alignof(simjig_cov_run),
    };
    const size_t n = std::min(cap, sizeof(layout) / sizeof(layout[0]));
    if (out != nullptr) std::copy(layout, layout + n, out);
    return n;
}

}  // extern "C"

// Hand-written runtime for the spikes: contexts, output routing, the thread-RNG swap and
// a few process probes. Compiled into the single shared libverilated archive (build.rs),
// because libverilated calls spk_printf.
#include "spk_runtime.h"

#include "verilated.h"

#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <utility>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#else
#include <dirent.h>
#endif

namespace {
// Single-threaded by construction (one model thread), so no lock.
std::map<const VerilatedContext*, std::string>& outputs() {
    static std::map<const VerilatedContext*, std::string> s;
    return s;
}
}  // namespace

void spk_printf(const char* formatp, ...) {
    va_list ap;
    va_start(ap, formatp);
    char buf[4096];
    const int n = std::vsnprintf(buf, sizeof buf, formatp, ap);
    va_end(ap);
    if (n > 0) outputs()[Verilated::threadContextp()].append(buf, std::min<size_t>(n, sizeof buf - 1));
}

extern "C" {

// threads == 0 keeps Verilator's default, which is the process's hardware parallelism:
// the context then starts a VlThreadPool of (threads - 1) workers when the first model is
// added, even for a --threads 1 model. threads == 1 creates no pool.
VerilatedContext* spk_ctx_new(uint32_t seed, uint32_t threads) {
    auto* const c = new VerilatedContext;
    if (threads != 0) c->threads(threads);
    c->randSeed(static_cast<int>(seed));
    // $fatal/$stop: record and keep running; the spikes poll gotFinish().
    c->fatalOnError(false);
    return c;
}
void spk_ctx_delete(VerilatedContext* c) {
    outputs().erase(c);
    delete c;
}
uint64_t spk_ctx_time(const VerilatedContext* c) { return c->time(); }
void spk_ctx_set_time(VerilatedContext* c, uint64_t t) { c->time(t); }
bool spk_ctx_got_finish(const VerilatedContext* c) { return c->gotFinish(); }
void spk_ctx_make_current(VerilatedContext* c) { Verilated::threadContextp(c); }
const VerilatedContext* spk_ctx_current() { return Verilated::threadContextp(); }

// Copies and clears the output routed to `c`. Returns the full length (may exceed cap).
size_t spk_ctx_take_output(const VerilatedContext* c, char* buf, size_t cap) {
    std::string s = std::move(outputs()[c]);
    outputs()[c].clear();
    std::memcpy(buf, s.data(), std::min(cap, s.size()));
    return s.size();
}

// A private $urandom stream. Verilator's $urandom draws from a thread_local VlRNG
// (VlRNG::vl_thread_rng), shared by every model evaluated on the thread. Swapping a
// per-model VlRNG in before eval and out after gives each model its own stream.
VlRNG* spk_rng_new(uint64_t seed) { return new VlRNG{seed}; }
void spk_rng_delete(VlRNG* r) { delete r; }
void spk_rng_swap(VlRNG* r) { std::swap(*r, VlRNG::vl_thread_rng()); }

// Number of OS threads in this process.
size_t spk_thread_count() {
#if defined(__APPLE__)
    thread_act_array_t threads;
    mach_msg_type_number_t count = 0;
    if (task_threads(mach_task_self(), &threads, &count) != KERN_SUCCESS) return 0;
    for (mach_msg_type_number_t i = 0; i < count; ++i) mach_port_deallocate(mach_task_self(), threads[i]);
    vm_deallocate(mach_task_self(), reinterpret_cast<vm_address_t>(threads), count * sizeof(thread_act_t));
    return count;
#else
    size_t n = 0;
    if (DIR* const d = opendir("/proc/self/task")) {
        while (const dirent* e = readdir(d)) n += e->d_name[0] != '.';
        closedir(d);
    }
    return n;
#endif
}

// Points every descriptor open on `path` at /dev/null. A forked child uses this to
// detach from a trace file it inherited, so closing the inherited writer cannot touch
// the parent's file. Returns how many descriptors were redirected.
int spk_detach_fds_on(const char* path) {
    struct stat target{};
    if (stat(path, &target) != 0) return -1;
    const int null_fd = open("/dev/null", O_WRONLY);
    int n = 0;
    for (int fd = 0; fd < 1024; ++fd) {
        struct stat st{};
        if (fd == null_fd || fstat(fd, &st) != 0) continue;
        if (st.st_dev == target.st_dev && st.st_ino == target.st_ino) {
            dup2(null_fd, fd);
            ++n;
        }
    }
    close(null_fd);
    return n;
}

}  // extern "C"

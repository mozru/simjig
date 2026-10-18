// C-level tests of tracing, coverage and per-model isolation, through the vtable only (the
// way the Rust runtime uses it). Run with `pixi run cpp-tests` (cpp/tests/run.sh).
//
// Usage: tracecov_tests <scratch dir>. Every test writes its files under the scratch dir.
// The process must stay single-threaded: the fork test relies on it (docs/notes/spikes.md §1).
#include "simjig_abi.h"
#include "simjig_rt.h"

#include <fcntl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <functional>
#include <iterator>
#include <string>
#include <vector>

#if defined(__APPLE__)
#include <mach/mach.h>
#else
#include <dirent.h>
#endif

extern "C" {
const simjig_model_vtable* simjig_vtable_Vtc_full(void);
const simjig_model_vtable* simjig_vtable_Vtc_plain(void);
const simjig_model_vtable* simjig_vtable_Vtc_plain_badprobe(void);
const simjig_model_vtable* simjig_vtable_Vtc_plain_param_rd(void);
const simjig_model_vtable* simjig_vtable_Vtc_plain_param_rw(void);
const simjig_model_vtable* simjig_vtable_Vtc_plain_rd_as_rw(void);
const simjig_model_vtable* simjig_vtable_Vtc_hier(void);
const simjig_model_vtable* simjig_vtable_Vtc_timing(void);
const simjig_model_vtable* simjig_vtable_Vtc_cg(void);
}

namespace {

std::string g_dir;
int g_failures = 0;
const char* g_test = "";

#define CHECK(cond)                                                                        \
    do {                                                                                   \
        if (!(cond)) {                                                                     \
            std::fprintf(stderr, "FAIL %s: %s:%d: %s\n", g_test, __FILE__, __LINE__, #cond); \
            ++g_failures;                                                                  \
        }                                                                                  \
    } while (0)

#define CHECK_EQ(a, b)                                                                       \
    do {                                                                                     \
        const auto va_ = (a);                                                                \
        const auto vb_ = (b);                                                                \
        if (!(va_ == vb_)) {                                                                 \
            std::fprintf(stderr, "FAIL %s: %s:%d: %s == %s (%llu vs %llu)\n", g_test, __FILE__, \
                         __LINE__, #a, #b, static_cast<unsigned long long>(va_),             \
                         static_cast<unsigned long long>(vb_));                              \
            ++g_failures;                                                                    \
        }                                                                                    \
    } while (0)

std::string path(const char* name) { return g_dir + "/" + name; }

// One model instance driven through its vtable: signal 0 is clk, 1 rst, 2 cnt, 3 rnd (tc).
class Model {
public:
    Model(const simjig_model_vtable* vt, const char* name, const char* const* argv = nullptr,
          uint32_t argc = 0)
        : m_vt{vt} {
        simjig_create_opts opts{sizeof(simjig_create_opts), 0, name, 0, argc, argv};
        m_status = vt->create(&opts, &m_h);
        uint32_t n = 0;
        m_sigs = vt->signals(m_h, &n);
    }
    ~Model() { m_vt->destroy(m_h); }
    Model(const Model&) = delete;
    Model& operator=(const Model&) = delete;

    const simjig_model_vtable* vt() const { return m_vt; }
    simjig_handle* h() const { return m_h; }
    simjig_status created() const { return m_status; }
    uint64_t time() const { return m_time; }

    void poke8(uint32_t id, uint8_t v) { *static_cast<uint8_t*>(m_sigs[id].ptr) = v; }
    uint32_t peek32(uint32_t id) const { return *static_cast<uint32_t*>(m_sigs[id].ptr); }
    uint8_t peek8(uint32_t id) const { return *static_cast<uint8_t*>(m_sigs[id].ptr); }

    simjig_status at(uint64_t t) {
        m_time = t;
        m_vt->set_time(m_h, t);
        return m_vt->eval(m_h);
    }
    // One clock cycle (period 10) with a trace sample after each eval. Returns rnd after the
    // rising edge.
    uint32_t cycle(bool dump = false) {
        poke8(0, 1);
        CHECK_EQ(at(m_time + 5), SIMJIG_OK);
        if (dump) CHECK_EQ(m_vt->trace_dump(m_h, m_time), SIMJIG_OK);
        poke8(0, 0);
        CHECK_EQ(at(m_time + 5), SIMJIG_OK);
        if (dump) CHECK_EQ(m_vt->trace_dump(m_h, m_time), SIMJIG_OK);
        return peek32(3);
    }
    // Holds rst through one cycle, starting at the current time.
    void reset() {
        poke8(1, 1);
        poke8(0, 0);
        CHECK_EQ(at(m_time), SIMJIG_OK);
        cycle();
        poke8(1, 0);
    }
    std::vector<simjig_msg> drain() {
        std::vector<simjig_msg> out(m_vt->drain_msgs(m_h, nullptr, 0));
        out.resize(m_vt->drain_msgs(m_h, out.data(), out.size()));
        return out;
    }

private:
    const simjig_model_vtable* m_vt;
    simjig_handle* m_h = nullptr;
    simjig_status m_status = SIMJIG_EXCEPTION;
    const simjig_signal* m_sigs = nullptr;
    uint64_t m_time = 0;
};

//-------------------------------------------------------------------------------------------
// A structural FST reader (header and block chain only; no decompression), after fstcpp's
// writer (Verilator include/fstcpp/fstcpp_writer.cpp): every block is a type byte and a
// big-endian u64 length that counts itself; the header block comes first; value-change blocks
// begin with their start and end time.

uint64_t be64(const std::vector<uint8_t>& b, size_t at) {
    uint64_t v = 0;
    for (size_t i = 0; i < 8; ++i) v = (v << 8) | b[at + i];
    return v;
}

struct Fst {
    bool valid = false;
    std::string error;
    uint64_t start_time = 0;
    uint64_t end_time = 0;
    uint64_t num_scopes = 0;
    uint64_t num_vars = 0;
    std::vector<uint64_t> vc_start;  // start time of every value-change block
    int geometry_blocks = 0;
    int hierarchy_blocks = 0;  // a writer closes with one geometry and one hierarchy block
};

constexpr uint8_t kBlockHeader = 0;
constexpr uint64_t kHeaderLength = 329;  // 321 bytes of fields + the 8-byte length itself

bool is_vc_block(uint8_t type) { return type == 1 || type == 5 || type == 8; }
bool is_hierarchy_block(uint8_t type) { return type == 4 || type == 6 || type == 7; }

Fst read_fst(const std::string& file) {
    Fst f;
    std::ifstream in{file, std::ios::binary};
    const std::vector<uint8_t> b{std::istreambuf_iterator<char>{in}, {}};
    if (b.size() < 1 + kHeaderLength || b[0] != kBlockHeader || be64(b, 1) != kHeaderLength) {
        f.error = "no FST header block";
        return f;
    }
    f.start_time = be64(b, 9);
    f.end_time = be64(b, 17);
    double endian = 0;
    std::memcpy(&endian, &b[25], sizeof endian);  // native-endian e, the FST endianness probe
    if (std::fabs(endian - std::exp(1.0)) > 1e-12) {
        f.error = "bad endianness probe";
        return f;
    }
    f.num_scopes = be64(b, 41);
    f.num_vars = be64(b, 49);
    size_t at = 0;
    while (at < b.size()) {
        if (at + 9 > b.size()) {
            f.error = "truncated block header";
            return f;
        }
        const uint8_t type = b[at];
        const uint64_t len = be64(b, at + 1);
        if (len < 8 || at + 1 + len > b.size() || (type > 8 && type < 254)) {
            f.error = "bad block at offset " + std::to_string(at);
            return f;
        }
        if (is_vc_block(type)) f.vc_start.push_back(be64(b, at + 9));
        f.geometry_blocks += type == 3;
        f.hierarchy_blocks += is_hierarchy_block(type);
        at += 1 + len;
    }
    f.valid = at == b.size();
    return f;
}

bool exists(const std::string& file) { return std::ifstream{file}.good(); }

std::string slurp(const std::string& file) {
    std::ifstream in{file, std::ios::binary};
    return {std::istreambuf_iterator<char>{in}, {}};
}

size_t thread_count() {
#if defined(__APPLE__)
    thread_act_array_t threads;
    mach_msg_type_number_t count = 0;
    if (task_threads(mach_task_self(), &threads, &count) != KERN_SUCCESS) return 0;
    for (mach_msg_type_number_t i = 0; i < count; ++i) {
        mach_port_deallocate(mach_task_self(), threads[i]);
    }
    vm_deallocate(mach_task_self(), reinterpret_cast<vm_address_t>(threads),
                  count * sizeof(thread_act_t));
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

bool has_warning(Model& m, const char* needle) {
    for (const simjig_msg& msg : m.drain()) {
        if (msg.kind == SIMJIG_MSG_WARNING && std::strstr(msg.msg, needle) != nullptr) return true;
    }
    return false;
}

//-------------------------------------------------------------------------------------------
// Tests

void vtable_describes_the_build() {
    const simjig_model_vtable* full = simjig_vtable_Vtc_full();
    CHECK_EQ(full->abi_version, SIMJIG_ABI_VERSION);
    CHECK_EQ(full->size, sizeof(simjig_model_vtable));
    CHECK_EQ(full->features, SIMJIG_FEATURE_TRACE_FST | SIMJIG_FEATURE_COVERAGE);
    CHECK_EQ(simjig_vtable_Vtc_plain()->features, 0u);
    CHECK_EQ(simjig_vtable_Vtc_hier()->features, SIMJIG_FEATURE_COVERAGE);
    CHECK_EQ(simjig_vtable_Vtc_timing()->features,
             SIMJIG_FEATURE_TIMING | SIMJIG_FEATURE_TRACE_FST);
}

void probe_width_is_checked_at_create() {
    {
        Model ok{simjig_vtable_Vtc_plain(), "TOP"};
        CHECK_EQ(ok.created(), SIMJIG_OK);
    }
    Model bad{simjig_vtable_Vtc_plain_badprobe(), "TOP"};
    CHECK_EQ(bad.created(), SIMJIG_EXCEPTION);
    bool named = false;
    for (const simjig_msg& msg : bad.drain()) {
        if (msg.kind != SIMJIG_MSG_EXCEPTION) continue;
        named = std::strstr(msg.msg, "probe 'tc.u.r'") != nullptr
                && std::strstr(msg.msg, "is 1 bits wide, but it is bound as 5 bits") != nullptr;
        if (!named) std::fprintf(stderr, "  message: %s\n", msg.msg);
    }
    CHECK(named);
    CHECK_EQ(bad.vt()->eval(bad.h()), SIMJIG_POISONED);
}

// The message of a failed create, or "" when there is none.
std::string create_failure(Model& m) {
    for (const simjig_msg& msg : m.drain()) {
        if (msg.kind == SIMJIG_MSG_EXCEPTION) return msg.msg;
    }
    return "";
}

// A localparam is constexpr storage in the model: it may be probed for reading, but a
// read-write probe would hand out a writable pointer to read-only memory (a write is SIGBUS on
// macOS, undefined behaviour anywhere). A public_flat_rd variable is not writable either.
void probe_access_is_checked_at_create() {
    {
        Model rd{simjig_vtable_Vtc_plain_param_rd(), "TOP"};
        CHECK_EQ(rd.created(), SIMJIG_OK);
        uint32_t n = 0;
        const simjig_signal* s = rd.vt()->signals(rd.h(), &n);
        CHECK(n == 6 && *static_cast<const uint8_t*>(s[5].ptr) == 0x5a);
    }
    Model rw{simjig_vtable_Vtc_plain_param_rw(), "TOP"};
    CHECK_EQ(rw.created(), SIMJIG_EXCEPTION);
    const std::string why = create_failure(rw);
    if (why.find("probe 'tc.u.LP'") == std::string::npos
        || why.find("is a parameter") == std::string::npos) {
        std::fprintf(stderr, "  message: %s\n", why.c_str());
        CHECK(false);
    }
    Model ro{simjig_vtable_Vtc_plain_rd_as_rw(), "TOP"};
    CHECK_EQ(ro.created(), SIMJIG_EXCEPTION);
    const std::string ro_why = create_failure(ro);
    if (ro_why.find("probe 'tc.u.r'") == std::string::npos
        || ro_why.find("public_flat_rd") == std::string::npos) {
        std::fprintf(stderr, "  message: %s\n", ro_why.c_str());
        CHECK(false);
    }
}

// simjig_rt_abi_fields lists every field: within each struct the fields start at 0, follow
// each other without overlap, and end inside the struct.
void abi_field_table_covers_every_field() {
    size_t n = 0;
    const simjig_abi_field* f = simjig_rt_abi_fields(&n);
    CHECK_EQ(n, 49u);
    const std::pair<const char*, size_t> types[] = {
        {"simjig_model_vtable", sizeof(simjig_model_vtable)},
        {"simjig_signal", sizeof(simjig_signal)},
        {"simjig_msg", sizeof(simjig_msg)},
        {"simjig_create_opts", sizeof(simjig_create_opts)},
        {"simjig_cov_run", sizeof(simjig_cov_run)},
    };
    size_t i = 0;
    for (const auto& [type, size] : types) {
        CHECK(i < n && std::strcmp(f[i].type, type) == 0 && f[i].offset == 0);
        uint32_t end = 0;
        for (; i < n && std::strcmp(f[i].type, type) == 0; ++i) {
            CHECK(f[i].offset >= end);
            end = f[i].offset + f[i].size;
        }
        CHECK(end <= size && size - end < 8);  // only tail padding is left
    }
    CHECK_EQ(i, n);
    CHECK(std::strcmp(f[24].name, "rng_seed") == 0 && f[24].offset == 184);
}

void plain_model_reports_unsupported() {
    Model m{simjig_vtable_Vtc_plain(), "TOP"};
    const simjig_model_vtable* vt = m.vt();
    const std::string fst = path("plain.fst");
    CHECK_EQ(vt->trace_open(m.h(), fst.c_str()), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->trace_open_ex(m.h(), fst.c_str(), 1, 5), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->trace_dump(m.h(), 0), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->trace_flush(m.h()), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->trace_reopen(m.h(), fst.c_str()), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->trace_close(m.h()), SIMJIG_UNSUPPORTED);
    CHECK(!exists(fst));
    size_t n = 99;
    CHECK(vt->cov_runs(m.h(), &n) == nullptr);
    CHECK_EQ(n, 0u);
    CHECK_EQ(vt->cov_zero(m.h()), SIMJIG_UNSUPPORTED);
    CHECK_EQ(vt->cov_write(m.h(), path("plain.dat").c_str()), SIMJIG_UNSUPPORTED);
    CHECK(!exists(path("plain.dat")));
}

// simjig::thread_rng() brings Verilator's reseed epoch up to date without losing the state, so
// a context created while a model's state is swapped in (a DPI call that creates a model)
// cannot overwrite that model's stream.
void thread_rng_survives_a_reseed_epoch() {
    VerilatedContext ctx;  // makes itself the thread context, which the reseed path reads
    simjig::thread_rng() = VlRNG{42};
    VlRNG expect{42};
    ctx.randSeed(5);  // bumps Verilator's global seed epoch
    CHECK_EQ(simjig::thread_rng().rand64(), expect.rand64());
    CHECK_EQ(VL_RANDOM_I(), static_cast<IData>(expect.rand64()));  // what $urandom draws
}

void fst_trace_is_written_and_parses() {
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    const std::string fst = path("full.fst");
    CHECK_EQ(m.vt()->trace_open(m.h(), fst.c_str()), SIMJIG_OK);
    m.reset();
    for (int i = 0; i < 20; ++i) m.cycle(true);
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);  // no trace open: no-op
    const Fst f = read_fst(fst);
    CHECK(f.valid);
    if (!f.error.empty()) std::fprintf(stderr, "  %s\n", f.error.c_str());
    CHECK_EQ(f.start_time, 15u);  // the first dump: reset() ran untraced up to t = 10
    CHECK_EQ(f.end_time, m.time());
    CHECK(!f.vc_start.empty());
    CHECK(f.num_scopes >= 3);  // TOP, TOP.tc, TOP.tc.u
    CHECK(f.num_vars > 5);
}

void trace_from_t_has_no_sample_before_t() {
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    const std::string fst = path("from_t.fst");
    constexpr uint64_t kStart = 100;
    CHECK_EQ(m.vt()->trace_open_ex(m.h(), fst.c_str(), 0, kStart), SIMJIG_OK);
    m.reset();
    for (int i = 0; i < 20; ++i) m.cycle(true);  // dumps at 15, 20, ..., 210
    m.vt()->trace_flush(m.h());
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
    const Fst f = read_fst(fst);
    CHECK(f.valid);
    CHECK_EQ(f.start_time, kStart);
    CHECK_EQ(f.end_time, m.time());
    CHECK(!f.vc_start.empty());
    for (uint64_t t : f.vc_start) CHECK(t >= kStart);
}

void trace_depth_limits_the_signals() {
    uint64_t vars[2] = {0, 0};
    for (uint32_t depth : {0u, 1u}) {
        Model m{simjig_vtable_Vtc_full(), "TOP"};
        const std::string fst = path(depth == 0 ? "depth0.fst" : "depth1.fst");
        CHECK_EQ(m.vt()->trace_open_ex(m.h(), fst.c_str(), depth, 0), SIMJIG_OK);
        m.reset();
        m.cycle(true);
        CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
        const Fst f = read_fst(fst);
        CHECK(f.valid);
        vars[depth] = f.num_vars;
    }
    CHECK_EQ(vars[1], 5u);  // TOP's own ports: clk, rst, cnt, rnd, odd
    CHECK(vars[0] > vars[1]);
}

void destroy_closes_an_open_trace() {
    const std::string fst = path("destroyed.fst");
    {
        Model m{simjig_vtable_Vtc_full(), "TOP"};
        CHECK_EQ(m.vt()->trace_open(m.h(), fst.c_str()), SIMJIG_OK);
        m.reset();
        m.cycle(true);
    }
    CHECK(read_fst(fst).valid);
}

void unwritable_paths_are_io_errors_not_fatals() {
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    const std::string bad = path("no/such/dir/x");
    CHECK_EQ(m.vt()->trace_open(m.h(), bad.c_str()), SIMJIG_IO_ERROR);
    CHECK(has_warning(m, "cannot create trace file"));
    CHECK_EQ(m.vt()->cov_write(m.h(), bad.c_str()), SIMJIG_IO_ERROR);
    CHECK(has_warning(m, "cannot create coverage file"));
    m.reset();  // still usable: nothing was poisoned
    CHECK_EQ(m.vt()->trace_dump(m.h(), m.time()), SIMJIG_OK);  // no trace open: no-op
}

// A forked child detaches from the inherited trace and writes its own; the parent's file
// stays valid (docs/notes/spikes.md §1 rule 4).
// redirect_to_devnull finds every descriptor open on the file, including one above the
// current soft RLIMIT_NOFILE (the limit can be lowered after the descriptor was opened), by
// listing the open descriptors instead of scanning up to the limit.
void devnull_redirect_finds_descriptors_above_the_limit() {
    const std::string file = path("redirect.txt");
    const int fd = ::open(file.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0);
    constexpr int kHigh = 3000;
    CHECK_EQ(::dup2(fd, kHigh), kHigh);
    struct rlimit old{};
    CHECK_EQ(::getrlimit(RLIMIT_NOFILE, &old), 0);
    struct rlimit low = old;
    low.rlim_cur = 256;
    CHECK_EQ(::setrlimit(RLIMIT_NOFILE, &low), 0);
    const int moved = simjig::redirect_to_devnull(simjig::file_id(file.c_str()));
    CHECK_EQ(::setrlimit(RLIMIT_NOFILE, &old), 0);
    CHECK_EQ(moved, 2);
    struct stat high{};
    struct stat null{};
    CHECK_EQ(::fstat(kHigh, &high), 0);
    CHECK_EQ(::stat("/dev/null", &null), 0);
    CHECK(high.st_rdev == null.st_rdev && high.st_ino == null.st_ino);
    ::close(kHigh);
    ::close(fd);
}

void fork_child_detaches_and_reopens() {
    CHECK_EQ(thread_count(), 1u);
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    const std::string parent = path("parent.fst");
    const std::string child = path("child.fst");
    CHECK_EQ(m.vt()->trace_open(m.h(), parent.c_str()), SIMJIG_OK);
    m.reset();
    for (int i = 0; i < 10; ++i) m.cycle(true);
    const uint64_t fork_time = m.time();
    CHECK_EQ(m.vt()->trace_flush(m.h()), SIMJIG_OK);
    const std::string before_fork = slurp(parent);
    std::fflush(nullptr);
    const pid_t pid = fork();
    if (pid == 0) {
        int bad = 0;
        // Reopening on the inherited file itself would truncate the parent's waves. The
        // failed call has already detached the inherited writer, so the retry opens afresh.
        bad |= m.vt()->trace_reopen(m.h(), parent.c_str()) != SIMJIG_IO_ERROR;
        bad |= m.vt()->trace_reopen(m.h(), child.c_str()) != SIMJIG_OK;
        for (int i = 0; i < 5; ++i) {
            m.poke8(0, 1);
            bad |= m.at(m.time() + 5) != SIMJIG_OK;
            bad |= m.vt()->trace_dump(m.h(), m.time()) != SIMJIG_OK;
            m.poke8(0, 0);
            bad |= m.at(m.time() + 5) != SIMJIG_OK;
            bad |= m.vt()->trace_dump(m.h(), m.time()) != SIMJIG_OK;
        }
        bad |= m.vt()->trace_close(m.h()) != SIMJIG_OK;
        _exit(bad);  // never return into the parent's stack; Model's destructor does not run
    }
    CHECK(pid > 0);
    int status = -1;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    // The child closed its copy of the writer; none of that reached the parent's file. (Had it,
    // the parent's later writes would mostly paper over it: they reuse the same offsets.)
    CHECK(slurp(parent) == before_fork);
    for (int i = 0; i < 10; ++i) m.cycle(true);
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
    const Fst p = read_fst(parent);
    const Fst c = read_fst(child);
    CHECK(p.valid);
    CHECK(c.valid);
    CHECK_EQ(p.geometry_blocks, 1);
    CHECK_EQ(p.hierarchy_blocks, 1);
    CHECK_EQ(c.hierarchy_blocks, 1);
    CHECK_EQ(p.end_time, m.time());
    CHECK_EQ(c.start_time, fork_time + 5);
    CHECK_EQ(c.end_time, fork_time + 50);
}

// A child whose branch file cannot be created: trace_reopen fails, and nothing the child does
// with the model afterwards (close, destroy) reaches the parent's file, because the inherited
// writer was detached before the reopen checked the new path.
void fork_child_with_an_uncreatable_path_leaves_the_parent_alone() {
    CHECK_EQ(thread_count(), 1u);
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    const std::string parent = path("parent_blocked.fst");
    const std::string blocked = path("no/such/dir/child.fst");
    CHECK_EQ(m.vt()->trace_open(m.h(), parent.c_str()), SIMJIG_OK);
    m.reset();
    for (int i = 0; i < 10; ++i) m.cycle(true);
    CHECK_EQ(m.vt()->trace_flush(m.h()), SIMJIG_OK);
    const std::string before_fork = slurp(parent);
    std::fflush(nullptr);
    const pid_t pid = fork();
    if (pid == 0) {
        int bad = 0;
        bad |= m.vt()->trace_reopen(m.h(), blocked.c_str()) != SIMJIG_IO_ERROR;
        bad |= m.vt()->trace_dump(m.h(), m.time() + 1) != SIMJIG_OK;  // no trace: a no-op
        bad |= m.vt()->trace_close(m.h()) != SIMJIG_OK;
        m.vt()->destroy(m.h());
        _exit(bad);
    }
    CHECK(pid > 0);
    int status = -1;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(slurp(parent) == before_fork);
    for (int i = 0; i < 10; ++i) m.cycle(true);
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
    const Fst p = read_fst(parent);
    CHECK(p.valid);
    CHECK_EQ(p.end_time, m.time());
}

// The sum of every counter of every run, and (in *counters) how many counters there are.
uint64_t runs_total(Model& m, size_t* counters = nullptr, size_t* runs = nullptr) {
    size_t n = 0;
    const simjig_cov_run* r = m.vt()->cov_runs(m.h(), &n);
    uint64_t sum = 0;
    size_t len = 0;
    for (size_t k = 0; r != nullptr && k < n; ++k) {
        len += r[k].len;
        for (size_t i = 0; i < r[k].len; ++i) sum += r[k].counters[i];
    }
    if (counters != nullptr) *counters = len;
    if (runs != nullptr) *runs = n;
    return sum;
}

void coverage_counters_count_and_zero() {
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    size_t n = 0;
    runs_total(m, &n);
    CHECK(n > 0);
    const auto sum = [&] { return runs_total(m); };
    m.reset();
    for (int i = 0; i < 10; ++i) m.cycle();
    const uint64_t first = sum();
    CHECK(first > 0);
    for (int i = 0; i < 10; ++i) m.cycle();
    CHECK(sum() > first);
    CHECK_EQ(m.vt()->cov_zero(m.h()), SIMJIG_OK);
    CHECK_EQ(sum(), 0u);
    m.cycle();
    CHECK(sum() > 0);
}

// coverage.dat: one `C '<keys>' <count>` line per point, after the format line.
uint64_t dat_total(const std::string& text, size_t* points) {
    uint64_t total = 0;
    *points = 0;
    size_t at = 0;
    while ((at = text.find("\nC '", at)) != std::string::npos) {
        const size_t end = text.find('\n', at + 1);
        const size_t quote = text.rfind('\'', end);
        total += std::strtoull(text.c_str() + quote + 1, nullptr, 10);
        ++*points;
        at = end;
    }
    return total;
}

void coverage_dat_is_written() {
    Model m{simjig_vtable_Vtc_full(), "TOP"};
    m.reset();
    for (int i = 0; i < 10; ++i) m.cycle();
    const std::string dat = path("coverage.dat");
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    const std::string text = slurp(dat);
    CHECK(text.rfind("# SystemC::Coverage-3\n", 0) == 0);
    size_t points = 0;
    const uint64_t total = dat_total(text, &points);
    size_t n = 0;
    const uint64_t live = runs_total(m, &n);
    // A counter shared by aliased signals (tc_sub.clk is tc.clk, tc_sub.q is tc.odd) is one
    // point per alias (docs/notes/tracecov.md), so coverage.dat counts more than the counters.
    CHECK(n > 0);
    CHECK(total > live);
    CHECK(live > 0);
    CHECK_EQ(m.vt()->cov_zero(m.h()), SIMJIG_OK);
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    CHECK_EQ(dat_total(slurp(dat), &points), 0u);
    // A NULL path writes Verilator's default file, named by +verilator+coverage+file+.
    const std::string named = "+verilator+coverage+file+" + path("named.dat");
    const char* const argv[] = {named.c_str()};
    Model p{simjig_vtable_Vtc_full(), "TOP", argv, 1};
    CHECK_EQ(p.vt()->cov_write(p.h(), nullptr), SIMJIG_OK);
    CHECK(slurp(path("named.dat")).rfind("# SystemC::Coverage-3\n", 0) == 0);
    CHECK(text.find("v_branch/tc") != std::string::npos);
    CHECK(text.find("v_toggle/tc") != std::string::npos);
}

// Each tc_leaf instance is its own object with its own 3 counters, and the symbol table has 3
// more (for static functions): three runs in symbol-table order, nothing in the root, and no
// counter twice, so the live counts sum to exactly the coverage.dat total. coverage.dat has 3
// points: without per_instance, both instances' points have the same keys and are summed.
void counters_of_every_module_object_are_runs() {
    Model m{simjig_vtable_Vtc_hier(), "TOP"};
    size_t n = 0;
    const simjig_cov_run* r = m.vt()->cov_runs(m.h(), &n);
    CHECK_EQ(n, 3u);
    for (size_t k = 0; r != nullptr && k < n; ++k) {
        CHECK_EQ(r[k].len, 3u);
        if (k > 0) CHECK(r[k - 1].counters < r[k].counters);
    }
    *static_cast<uint8_t*>(m.vt()->signals(m.h(), nullptr)[1].ptr) = 1;
    for (uint64_t t = 0; t < 100; t += 5) {
        *static_cast<uint8_t*>(m.vt()->signals(m.h(), nullptr)[0].ptr) = (t / 5) % 2;
        m.vt()->set_time(m.h(), t);
        CHECK_EQ(m.vt()->eval(m.h()), SIMJIG_OK);
    }
    const std::string dat = path("hier.dat");
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    size_t points = 0;
    const uint64_t total = dat_total(slurp(dat), &points);
    CHECK(total > 0);
    CHECK_EQ(points, 3u);
    CHECK_EQ(runs_total(m), total);
    CHECK_EQ(m.vt()->cov_zero(m.h()), SIMJIG_OK);
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    CHECK_EQ(dat_total(slurp(dat), &points), 0u);
}

// Counters are noted only while create constructs the model: a covergroup created by `new`
// (or any coverage point registered later) must not make the runtime keep collecting
// pointers it will never hand out.
void cov_noting_stops_after_create() {
    uint32_t counters[4] = {};
    simjig::Runtime rt{nullptr, false};
    {
        const simjig::Scope scope{rt};
        simjig::note_cov_counter(&counters[0]);
        simjig::note_cov_counter(&counters[1]);
    }
    const std::vector<simjig_cov_run> runs = rt.cov_runs(counters, sizeof counters);
    CHECK(runs.size() == 1 && runs[0].counters == counters && runs[0].len == 2);
    {
        const simjig::Scope scope{rt};
        simjig::note_cov_counter(&counters[2]);  // what an eval-time registration does
    }
    CHECK(rt.cov_runs(counters, sizeof counters).empty());
}

// A covergroup's bins are registered when `new` runs, in the first eval, in counters
// Verilator allocates on the heap: they are in no run (the runs are fixed at create), but
// cov_write writes them and cov_zero clears them.
void covergroup_bins_are_written_but_not_in_runs() {
    Model m{simjig_vtable_Vtc_cg(), "TOP"};
    CHECK_EQ(m.created(), SIMJIG_OK);
    size_t n = 0;
    const uint64_t before = runs_total(m, &n);
    CHECK(n > 0);
    CHECK_EQ(before, 0u);
    const simjig_signal* s = m.vt()->signals(m.h(), nullptr);
    for (uint64_t t = 0; t < 80; t += 5) {
        *static_cast<uint8_t*>(s[0].ptr) = (t / 5) % 2;
        *static_cast<uint8_t*>(s[1].ptr) = static_cast<uint8_t>((t / 10) % 4);
        m.vt()->set_time(m.h(), t);
        CHECK_EQ(m.vt()->eval(m.h()), SIMJIG_OK);
    }
    size_t after_n = 0;
    const uint64_t live = runs_total(m, &after_n);
    CHECK_EQ(after_n, n);  // the runs did not change
    const std::string dat = path("cg.dat");
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    const std::string text = slurp(dat);
    size_t points = 0;
    const uint64_t total = dat_total(text, &points);
    CHECK(text.find("covergroup") != std::string::npos);
    CHECK(live > 0);
    CHECK(total > live);  // the bins' hits are in coverage.dat, not in the runs
    if (!(total > live)) std::fprintf(stderr, "  live %llu total %llu\n%s\n",
        static_cast<unsigned long long>(live), static_cast<unsigned long long>(total), text.c_str());
    CHECK_EQ(m.vt()->cov_zero(m.h()), SIMJIG_OK);
    CHECK_EQ(m.vt()->cov_write(m.h(), dat.c_str()), SIMJIG_OK);
    CHECK_EQ(dat_total(slurp(dat), &points), 0u);
}

std::vector<uint32_t> draws(Model& m, int n) {
    std::vector<uint32_t> out;
    for (int i = 0; i < n; ++i) out.push_back(m.cycle());
    return out;
}

void urandom_streams_are_per_model_and_reproducible() {
    std::vector<uint32_t> alone;
    {
        Model a{simjig_vtable_Vtc_full(), "A"};
        a.reset();
        alone = draws(a, 8);
    }
    CHECK(alone[0] != alone[1]);
    // Interleaved with another model of the same design (and a third one created mid-run,
    // which bumps Verilator's global seed epoch), A sees exactly its solo stream.
    Model a{simjig_vtable_Vtc_full(), "A"};
    Model b{simjig_vtable_Vtc_full(), "B"};
    a.reset();
    b.reset();
    std::vector<uint32_t> got_a;
    std::vector<uint32_t> got_b;
    for (int i = 0; i < 8; ++i) {
        got_a.push_back(a.cycle());
        got_b.push_back(b.cycle());
        if (i == 3) {
            Model c{simjig_vtable_Vtc_plain(), "C"};
            c.reset();
        }
    }
    CHECK(got_a == alone);
    CHECK(got_b == alone);  // same default seed (+verilator+seed+1): the same stream
    // A different seed gives a different stream, reproducibly; through rng_seed or plusargs.
    std::vector<uint32_t> seeded[2];
    for (auto& s : seeded) {
        Model c{simjig_vtable_Vtc_full(), "C"};
        CHECK_EQ(c.vt()->rng_seed(c.h(), 0x5eed), SIMJIG_OK);
        c.reset();
        s = draws(c, 8);
    }
    CHECK(seeded[0] == seeded[1]);
    CHECK(seeded[0] != alone);
    const char* const argv[] = {"+verilator+seed+7"};
    Model p{simjig_vtable_Vtc_full(), "P", argv, 1};
    p.reset();
    CHECK(draws(p, 8) != alone);
}

void display_is_attributed_to_its_model() {
    Model a{simjig_vtable_Vtc_full(), "A"};
    Model b{simjig_vtable_Vtc_plain(), "B"};
    a.reset();
    b.at(1000);  // B runs 1000 ticks later, so a `$time` read from A's context would show
    b.reset();
    for (int i = 0; i < 6; ++i) {
        a.cycle();
        b.cycle();
    }
    const auto texts = [](Model& m) {
        std::string all;
        for (const simjig_msg& msg : m.drain()) {
            if (msg.kind == SIMJIG_MSG_DISPLAY) all.append(msg.msg, msg.msg_len);
        }
        return all;
    };
    const std::string from_a = texts(a);
    const std::string from_b = texts(b);
    // cnt == 3 is seen at the 4th rising edge after reset (t = 15, 25, 35, 45).
    CHECK(from_a == "tc A.tc cnt=3 t=45\n");
    CHECK(from_b == "tc B.tc cnt=3 t=1045\n");
    if (from_a.find("t=45") == std::string::npos) std::fprintf(stderr, "  A: %s", from_a.c_str());
    if (from_b.find("t=1045") == std::string::npos) std::fprintf(stderr, "  B: %s", from_b.c_str());
}

void contexts_start_no_threads() {
    Model a{simjig_vtable_Vtc_full(), "A"};
    Model b{simjig_vtable_Vtc_plain(), "B"};
    a.reset();
    b.reset();
    CHECK_EQ(thread_count(), 1u);
}

void timing_model_traces_its_own_slots() {
    Model m{simjig_vtable_Vtc_timing(), "TOP"};
    const std::string fst = path("timing.fst");
    CHECK_EQ(m.vt()->trace_open_ex(m.h(), fst.c_str(), 0, 20), SIMJIG_OK);
    CHECK_EQ(m.at(0), SIMJIG_OK);
    CHECK_EQ(m.vt()->trace_dump(m.h(), 0), SIMJIG_OK);
    while (m.vt()->events_pending(m.h()) != 0) {
        const uint64_t next = m.vt()->next_time_slot(m.h());
        CHECK_EQ(m.at(next), SIMJIG_OK);
        CHECK_EQ(m.vt()->trace_dump(m.h(), next), SIMJIG_OK);
    }
    CHECK_EQ(m.peek8(0), 6u);
    CHECK_EQ(m.vt()->trace_close(m.h()), SIMJIG_OK);
    const Fst f = read_fst(fst);
    CHECK(f.valid);
    CHECK_EQ(f.start_time, 20u);
    CHECK_EQ(f.end_time, 60u);
}

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <scratch dir>\n", argv[0]);
        return 2;
    }
    g_dir = argv[1];
    const std::pair<const char*, void (*)()> tests[] = {
        {"vtable_describes_the_build", vtable_describes_the_build},
        // Fork first, while the process certainly has a single thread.
        {"fork_child_detaches_and_reopens", fork_child_detaches_and_reopens},
        {"fork_child_with_an_uncreatable_path_leaves_the_parent_alone",
         fork_child_with_an_uncreatable_path_leaves_the_parent_alone},
        {"plain_model_reports_unsupported", plain_model_reports_unsupported},
        {"probe_width_is_checked_at_create", probe_width_is_checked_at_create},
        {"probe_access_is_checked_at_create", probe_access_is_checked_at_create},
        {"abi_field_table_covers_every_field", abi_field_table_covers_every_field},
        {"thread_rng_survives_a_reseed_epoch", thread_rng_survives_a_reseed_epoch},
        {"fst_trace_is_written_and_parses", fst_trace_is_written_and_parses},
        {"trace_from_t_has_no_sample_before_t", trace_from_t_has_no_sample_before_t},
        {"trace_depth_limits_the_signals", trace_depth_limits_the_signals},
        {"destroy_closes_an_open_trace", destroy_closes_an_open_trace},
        {"unwritable_paths_are_io_errors_not_fatals", unwritable_paths_are_io_errors_not_fatals},
        {"coverage_counters_count_and_zero", coverage_counters_count_and_zero},
        {"coverage_dat_is_written", coverage_dat_is_written},
        {"devnull_redirect_finds_descriptors_above_the_limit",
         devnull_redirect_finds_descriptors_above_the_limit},
        {"counters_of_every_module_object_are_runs", counters_of_every_module_object_are_runs},
        {"cov_noting_stops_after_create", cov_noting_stops_after_create},
        {"covergroup_bins_are_written_but_not_in_runs",
         covergroup_bins_are_written_but_not_in_runs},
        {"urandom_streams_are_per_model_and_reproducible",
         urandom_streams_are_per_model_and_reproducible},
        {"display_is_attributed_to_its_model", display_is_attributed_to_its_model},
        {"contexts_start_no_threads", contexts_start_no_threads},
        {"timing_model_traces_its_own_slots", timing_model_traces_its_own_slots},
    };
    for (const auto& [name, fn] : tests) {
        g_test = name;
        const int before = g_failures;
        fn();
        std::printf("%s %s\n", g_failures == before ? "ok  " : "FAIL", name);
    }
    std::printf("%d failure(s)\n", g_failures);
    return g_failures == 0 ? 0 : 1;
}

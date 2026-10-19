// The raw C++ flow of the sync_fifo benchmark (bench/README.md): the ceiling. A
// hand-written Verilator loop, with no framework between the testbench and the model, of
// the same three variants as bench/simjig/tests/bench.rs, bench/cocotb/test_bench.py and
// bench/marlin/tests/bench.rs, with the same stimulus and checks.
//
//     bench_cpp VARIANT      (clock_only | monitor_only | full)
//
// The loop is the one bench/marlin's testbench runs (see its module comment):
// - time 0: one eval with the initial inputs (as simjig's kernel does before the clock's
//   first edge), so the first sample sees settled outputs; the clock starts high;
// - each cycle starts at the rising edge: the monitor samples first (every signal still
//   holds its pre-edge value, as in the event-driven flows), then clk = 1 and eval;
// - at the falling edge the producer and consumer write their inputs, then clk = 0 and
//   eval.
// Each half period advances the context's time by 5 ns (the RTL's 1 ps precision: 5000),
// as a Verilator main does; the design has no delays, so nothing reads it.
//
// BENCH_CYCLES sets the cycles (10 000 by default). The body is timed (model construction
// excluded) and one JSON line is appended to the file BENCH_OUT (stderr when unset). A
// failed check prints what differed and exits 1.

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <memory>
#include <string>

#include "verilated.h"
#include "Vsync_fifo.h"

namespace {

constexpr std::size_t kDepth = 16;  // the -GDEPTH the build passes
constexpr uint64_t kPhase = 24;     // a driver redraws its rate every kPhase cycles
constexpr uint32_t kRates[3] = {10, 50, 90};
constexpr uint32_t kProducerSeed = 0x12345678;
constexpr uint32_t kConsumerSeed = 0x9E3779B9;
constexpr uint64_t kHalfPeriod = 5000;  // 5 ns at 1 ps

using Clock = std::chrono::steady_clock;

// Marsaglia's xorshift32 (13, 17, 5), the same stream as the other flows'.
struct XorShift32 {
    uint32_t x;
    uint32_t next() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        return x;
    }
    uint32_t rate() { return kRates[next() % 3]; }
};

// One driver's stream and the rate of its current phase.
struct Driver {
    XorShift32 rng;
    uint32_t rate = 0;
    // Whether to act in cycle i (the rate is redrawn at each phase's start).
    bool fire(uint64_t i) {
        if (i % kPhase == 0) rate = rng.rate();
        return rng.next() % 100 < rate;
    }
};

struct Bench {
    VerilatedContext& ctx;
    Vsync_fifo& dut;

    void eval() { dut.eval(); }
    void rise() {
        dut.clk = 1;
        dut.eval();
        ctx.timeInc(kHalfPeriod);
    }
    void fall() {
        dut.clk = 0;
        dut.eval();
        ctx.timeInc(kHalfPeriod);
    }
};

[[noreturn]] void check_failed(const char* what, uint64_t got, uint64_t want, uint64_t cycle) {
    std::fprintf(stderr, "check failed: %s: dut %llu, model %llu, at cycle %llu\n", what,
                 static_cast<unsigned long long>(got), static_cast<unsigned long long>(want),
                 static_cast<unsigned long long>(cycle));
    std::exit(1);
}

void check(const char* what, uint64_t got, uint64_t want, uint64_t cycle) {
    if (got != want) check_failed(what, got, want, cycle);
}

// The full testbench's reference model and what it counted.
struct Model {
    std::deque<uint32_t> entries;
    uint32_t shown = 0;  // what the registered rd_data shows: the last pop's entry
    uint64_t pushes = 0, pops = 0;
    uint32_t hash = 0;  // of every popped byte, in order

    // The monitor at one rising edge: checks the pre-edge outputs, then applies the
    // edge's push and pop.
    void sample(const Vsync_fifo& dut, uint64_t cycle) {
        bool p = dut.push, q = dut.pop;
        uint32_t w = dut.wr_data;
        std::size_t len = entries.size();
        check("count", dut.count, len, cycle);
        check("full", dut.full, len == kDepth, cycle);
        check("empty", dut.empty, len == 0, cycle);
        check("rd_data", dut.rd_data, shown, cycle);
        bool do_pop = q && len > 0;
        bool do_push = p && (len < kDepth || do_pop);
        if (do_pop) {
            shown = entries.front();
            entries.pop_front();
            ++pops;
            hash = hash * 31 + shown;
        }
        if (do_push) {
            entries.push_back(w);
            ++pushes;
        }
    }
};

// (A) The clock alone: n rising edges and the falling edges between them.
std::string clock_only(Bench b, uint64_t n) {
    b.eval();  // time 0's first eval, before the clock's first edge
    for (uint64_t i = 0; i < n; ++i) {
        if (i > 0) b.fall();
        b.rise();
    }
    return "\"hash\":0";
}

// (B) A monitor reading six signals per rising edge.
std::string monitor_only(Bench b, uint64_t n) {
    uint32_t hash = 0;
    b.eval();  // time 0's first eval, before the clock's first edge
    for (uint64_t i = 0; i < n; ++i) {
        if (i > 0) b.fall();
        const Vsync_fifo& d = b.dut;
        uint32_t s = d.push + 2u * d.pop + 4u * d.full + 8u * d.empty + 16u * d.count +
                     1024u * d.rd_data;
        hash = hash * 31 + s;
        b.rise();
    }
    return "\"hash\":" + std::to_string(hash);
}

// (C) Reset, then each cycle: the monitor (with its reference model) at the rising
// edge, the producer and the consumer at the falling edge.
std::string full(Bench b, uint64_t n) {
    Vsync_fifo& d = b.dut;
    // Reset: two edges with every input idle, released at a falling edge.
    d.rst = 1;
    d.push = 0;
    d.pop = 0;
    b.eval();  // time 0's first eval, before the clock's first edge
    b.rise();
    b.fall();
    b.rise();
    b.fall();
    d.rst = 0;

    Driver producer{{kProducerSeed}}, consumer{{kConsumerSeed}};
    Model model;
    for (uint64_t i = 0; i < n; ++i) {
        model.sample(d, i);
        b.rise();
        d.push = producer.fire(i);
        d.wr_data = producer.rng.next() & 0xFF;
        d.pop = consumer.fire(i);
        b.fall();
    }
    return "\"pushes\":" + std::to_string(model.pushes) +
           ",\"pops\":" + std::to_string(model.pops) + ",\"hash\":" + std::to_string(model.hash);
}

void report(const char* variant, uint64_t n, double secs, const std::string& result) {
    char head[256];
    std::snprintf(head, sizeof head,
                  "{\"flow\":\"cpp\",\"dut\":\"fifo\",\"variant\":\"%s\",\"cycles\":%llu,"
                  "\"seconds\":%.6f,",
                  variant, static_cast<unsigned long long>(n), secs);
    std::string line = head + result + "}\n";
    const char* path = std::getenv("BENCH_OUT");
    std::FILE* f = path ? std::fopen(path, "a") : stderr;
    if (!f) {
        std::perror("BENCH_OUT");
        std::exit(2);
    }
    std::fputs(line.c_str(), f);
    if (path) std::fclose(f);
}

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s clock_only|monitor_only|full\n", argv[0]);
        return 2;
    }
    const char* variant = argv[1];
    const char* env = std::getenv("BENCH_CYCLES");
    uint64_t n = env ? std::strtoull(env, nullptr, 10) : 10000;
    auto ctx = std::make_unique<VerilatedContext>();
    auto dut = std::make_unique<Vsync_fifo>(ctx.get());
    Bench b{*ctx, *dut};

    auto start = Clock::now();
    std::string result;
    if (std::strcmp(variant, "clock_only") == 0) {
        result = clock_only(b, n);
    } else if (std::strcmp(variant, "monitor_only") == 0) {
        result = monitor_only(b, n);
    } else if (std::strcmp(variant, "full") == 0) {
        result = full(b, n);
    } else {
        std::fprintf(stderr, "unknown variant %s\n", variant);
        return 2;
    }
    double secs = std::chrono::duration<double>(Clock::now() - start).count();
    report(variant, n, secs, result);
    dut->final();
    return 0;
}

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

#include <cstring>
#include <deque>
#include <memory>

#include "Vsync_fifo.h"
#include "common.h"
#include "verilated.h"

namespace {

using namespace bench;

constexpr std::size_t kDepth = 16;  // the -GDEPTH the build passes

using Sim = bench::Bench<Vsync_fifo>;

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
std::string clock_only(Sim b, uint64_t n) {
    b.eval();  // time 0's first eval, before the clock's first edge
    for (uint64_t i = 0; i < n; ++i) {
        if (i > 0) b.fall();
        b.rise();
    }
    return "\"hash\":0";
}

// (B) A monitor reading six signals per rising edge.
std::string monitor_only(Sim b, uint64_t n) {
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
std::string full(Sim b, uint64_t n) {
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

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s clock_only|monitor_only|full\n", argv[0]);
        return 2;
    }
    const char* variant = argv[1];
    uint64_t n = cycles();
    auto ctx = std::make_unique<VerilatedContext>();
    auto dut = std::make_unique<Vsync_fifo>(ctx.get());
    Sim b{*ctx, *dut};

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
    report("fifo", variant, n, secs, result);
    dut->final();
    return 0;
}

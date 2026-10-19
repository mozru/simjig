// The raw C++ flow of the mid-size benchmark design (bench/rtl/mid_pipe.sv; bench/README.md,
// "Designs"): the same three variants as bench/simjig/tests/mid.rs,
// bench/cocotb/test_mid.py and bench/marlin/tests/mid.rs, with the same stimulus and
// checks, and the same loop as main.cpp (see its comment).
//
//     bench_cpp VARIANT      (clock_only | monitor_only | full)
//
// BENCH_CYCLES sets the cycles (10 000 by default). The body is timed (model construction
// excluded) and one JSON line is appended to the file BENCH_OUT (stderr when unset). A
// failed check prints what differed and exits 1.

#include <cstring>
#include <deque>
#include <memory>
#include <utility>

#include "Vmid_pipe.h"
#include "common.h"
#include "verilated.h"

namespace {

using namespace bench;

constexpr std::size_t kDepth = 256;  // mid_pipe's DEPTH
constexpr uint64_t kLatency = 128 + 1;  // samples from a pop to its out_valid: STAGES + 1

using Sim = bench::Bench<Vmid_pipe>;

uint32_t fold64(uint64_t x) { return static_cast<uint32_t>(x) ^ static_cast<uint32_t>(x >> 32); }

// The full testbench's scoreboard and what it counted.
struct Model {
    std::deque<uint64_t> entries;
    // Entries popped and inside the pipeline: (the sample they are due at, the entry).
    std::deque<std::pair<uint64_t, uint64_t>> pipe;
    uint64_t pushes = 0, pops = 0;
    uint32_t hash = 0;  // of every out_crc, in order

    // The monitor at sample i (just before rising edge i): checks the pre-edge outputs,
    // then applies the edge's push and pop.
    void sample(const Vmid_pipe& dut, uint64_t i) {
        bool p = dut.push, q = dut.pop;
        uint64_t w = dut.wr_data;
        std::size_t len = entries.size();
        check("count", dut.count, len, i);
        check("full", dut.full, len == kDepth, i);
        check("empty", dut.empty, len == 0, i);
        bool due = !pipe.empty() && pipe.front().first == i;
        check("out_valid", dut.out_valid, due, i);
        if (due) {
            check("out_data", dut.out_data, pipe.front().second, i);
            pipe.pop_front();
            hash = hash * 31 + dut.out_crc;
        }
        bool do_pop = q && len > 0;
        bool do_push = p && (len < kDepth || do_pop);
        if (do_pop) {
            pipe.emplace_back(i + kLatency, entries.front());
            entries.pop_front();
            ++pops;
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

// (B) A monitor reading seven signals per rising edge.
std::string monitor_only(Sim b, uint64_t n) {
    uint32_t hash = 0;
    b.eval();  // time 0's first eval, before the clock's first edge
    for (uint64_t i = 0; i < n; ++i) {
        if (i > 0) b.fall();
        const Vmid_pipe& d = b.dut;
        uint32_t s = d.push + 2u * d.pop + 4u * d.full + 8u * d.empty + 16u * d.count +
                     16384u * d.out_valid;
        hash = (hash * 31 + s) * 31 + fold64(d.out_data);
        b.rise();
    }
    return "\"hash\":" + std::to_string(hash);
}

// (C) Reset, then each cycle: the monitor (with its scoreboard) at the rising edge, the
// producer and the consumer at the falling edge.
std::string full(Sim b, uint64_t n) {
    Vmid_pipe& d = b.dut;
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
        d.wr_data = producer.rng.next64();
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
    auto dut = std::make_unique<Vmid_pipe>(ctx.get());
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
    report("mid", variant, n, secs, result);
    dut->final();
    return 0;
}

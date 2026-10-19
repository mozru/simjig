// What the raw C++ testbenches of the benchmark (main.cpp: the sync_fifo; mid.cpp: the
// mid-size design) share: the stimulus streams, the checks, and the result line. Each
// is the same as in the other flows (bench/README.md, "Testbench").
#pragma once

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>

#include "verilated.h"

namespace bench {

constexpr uint64_t kPhase = 24;  // a driver redraws its rate every kPhase cycles
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
    // A 64-bit value from two draws, the high half first.
    uint64_t next64() {
        uint64_t hi = next();
        return hi << 32 | next();
    }
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

[[noreturn]] inline void check_failed(const char* what, uint64_t got, uint64_t want,
                                      uint64_t cycle) {
    std::fprintf(stderr, "check failed: %s: dut %llu, model %llu, at cycle %llu\n", what,
                 static_cast<unsigned long long>(got), static_cast<unsigned long long>(want),
                 static_cast<unsigned long long>(cycle));
    std::exit(1);
}

inline void check(const char* what, uint64_t got, uint64_t want, uint64_t cycle) {
    if (got != want) check_failed(what, got, want, cycle);
}

// Cycles to run: BENCH_CYCLES, else 10 000.
inline uint64_t cycles() {
    const char* env = std::getenv("BENCH_CYCLES");
    return env ? std::strtoull(env, nullptr, 10) : 10000;
}

// Appends one result line to the file BENCH_OUT (stderr when unset).
inline void report(const char* dut, const char* variant, uint64_t n, double secs,
                   const std::string& result) {
    char head[256];
    std::snprintf(head, sizeof head,
                  "{\"flow\":\"cpp\",\"dut\":\"%s\",\"variant\":\"%s\",\"cycles\":%llu,"
                  "\"seconds\":%.6f,",
                  dut, variant, static_cast<unsigned long long>(n), secs);
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

// The model and its context, and the three ways a testbench steps it: time 0's first
// eval, and the two edges (each advances time by a half period, as a Verilator main does;
// neither design has delays, so nothing reads it).
template <typename Model>
struct Bench {
    VerilatedContext& ctx;
    Model& dut;

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

}  // namespace bench

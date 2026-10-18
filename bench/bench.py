"""Driver of the simjig-vs-cocotb benchmark (bench/README.md). Run it through bench/run.sh.

    bench/run.sh build        # build both testbenches (never during measurement)
    bench/run.sh run [--runs 5] [--sizes 10000,100000,1000000,10000000] [--out FILE]
    bench/run.sh bug          # an injected RTL bug must fail both testbenches
    bench/run.sh table FILE   # the results table (Markdown) of a raw results file

`build` verilates and compiles both testbenches. `run` builds nothing: for each size and variant it runs
simjig's test binary and cocotb's simulator, each in a fresh process, sequentially, and
appends one JSON line per run to FILE: the testbench's own timing of its body
(`seconds`), the process's wall time (`process_seconds`), the 1-minute load average
before the batch (`load`), and what the testbench counted (`pushes`, `pops`, `hash`),
which must agree between the two flows.

Before each batch it waits until the 1-minute load average is below --max-load (1.0) and
no `cargo`, `rustc`, `clang`, `verilator` or `nextest` process is running (the machine
runs other agents; this driver builds nothing while it measures). With --max-wait set, a
batch that gave up waiting is marked `quiet: false` and flagged in the table.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE / "cocotb"))
import run as cocotb_run  # noqa: E402  (bench/cocotb/run.py)

SIMJIG_DIR = HERE / "simjig"
TARGET = ROOT / "target"
VARIANTS = ["clock_only", "monitor_only", "full"]
SIMJIG_TESTS = {"clock_only": "a_clock_only", "monitor_only": "b_monitor_only", "full": "c_full"}
SIMJIG_DUTS = ["fifo", "fifo_pub"]
# The injected bug: a full FIFO drops a push that comes with a pop (the `|| do_pop` that
# the RTL's handshake comment requires is gone).
BUG = ("wire do_push = push && (!full || do_pop);", "wire do_push = push && !full;")


def build_simjig(target: Path, rtl: Path | None = None) -> Path:
    """Builds the simjig test binary (profile `bench`) and returns its path."""
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    if rtl is not None:
        env["BENCH_RTL"] = str(rtl)
    cmd = ["pixi", "run", "--manifest-path", str(ROOT / "pixi.toml"), "cargo", "test",
           "--profile", "bench", "--no-run", "--message-format=json"]  # fmt: skip
    out = subprocess.run(cmd, cwd=SIMJIG_DIR, env=env, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"simjig build failed:\n{out.stderr}")
    for line in out.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
            if msg["target"]["name"] == "bench":
                return Path(msg["executable"])
    sys.exit("simjig build: no test executable named `bench`")


def run_simjig(exe: Path, dut: str, variant: str, n: int, out: Path) -> float:
    """One simjig run; returns the process's wall time."""
    env = dict(os.environ, BENCH_CYCLES=str(n), BENCH_OUT=str(out))
    test = f"{SIMJIG_TESTS[variant]}::{dut}"
    t0 = time.perf_counter()
    r = subprocess.run([str(exe), "--exact", test, "--test-threads", "1", "-q"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    wall = time.perf_counter() - t0
    if r.returncode != 0:
        sys.exit(f"simjig {test} failed:\n{r.stdout}\n{r.stderr}")
    return wall


def run_cocotb(variant: str, n: int, out: Path, build_dir: Path) -> float:
    t0 = time.perf_counter()
    cocotb_run.test(variant, n, out, build_dir)
    return time.perf_counter() - t0


BUSY = ("cargo", "rustc", "clang", "verilator", "nextest")


def busy_processes() -> list[str]:
    """Compiler and build processes running now (none of them is this driver's)."""
    ps = subprocess.run(["ps", "-Ao", "pid=,comm="], capture_output=True, text=True).stdout
    found = []
    for line in ps.splitlines():
        pid, _, comm = line.strip().partition(" ")
        if any(b in Path(comm).name for b in BUSY):
            found.append(f"{pid} {Path(comm).name}")
    return found


def wait_for_quiet(max_load: float, max_wait: float | None) -> tuple[float, bool]:
    """Waits until the 1-minute load average is below `max_load` and no build process
    runs, for at most `max_wait` seconds (`None`: forever). Returns the load and whether
    the machine was quiet."""
    deadline = None if max_wait is None else time.monotonic() + max_wait
    while True:
        load, busy = os.getloadavg()[0], busy_processes()
        if load < max_load and not busy:
            return load, True
        if deadline is not None and time.monotonic() >= deadline:
            return load, False
        print(f"  load {load:.2f} (want < {max_load}), busy: {busy[:4]}; waiting",
              file=sys.stderr)  # fmt: skip
        time.sleep(15)


def tag_last(out: Path, **extra) -> None:
    """Adds `extra` to the last JSON line of `out` (the run just made)."""
    lines = out.read_text().splitlines()
    last = json.loads(lines[-1])
    last.update(extra)
    lines[-1] = json.dumps(last)
    out.write_text("\n".join(lines) + "\n")


BUILT = TARGET / "bench-built.json"
COCOTB_DIR = TARGET / "bench-cocotb"


def cmd_build(_: argparse.Namespace) -> None:
    print("building simjig", file=sys.stderr)
    exe = build_simjig(TARGET / "bench-simjig")
    print("building cocotb", file=sys.stderr)
    cocotb_run.build(cocotb_run.RTL, COCOTB_DIR)
    BUILT.write_text(json.dumps({"simjig": str(exe)}))


def cmd_run(a: argparse.Namespace) -> None:
    out = a.out.resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    if not BUILT.exists():
        sys.exit("nothing built: run `bench/run.sh build` first")
    exe = Path(json.loads(BUILT.read_text())["simjig"])
    cocotb_dir = COCOTB_DIR
    for n in a.sizes:
        for variant in a.variants:
            flows = [("simjig", d) for d in SIMJIG_DUTS] + [("cocotb", "fifo")]
            if n > a.cocotb_max:
                flows = flows[:-1]
            for flow, dut in flows:
                load, quiet = wait_for_quiet(a.max_load, a.max_wait)
                print(f"{flow}/{dut} {variant} {n}: {a.runs} runs (load {load:.2f})",
                      file=sys.stderr)  # fmt: skip
                for _ in range(a.runs):
                    if flow == "simjig":
                        wall = run_simjig(exe, dut, variant, n, out)
                    else:
                        wall = run_cocotb(variant, n, out, cocotb_dir)
                    tag_last(out, process_seconds=round(wall, 6), load=round(load, 2),
                             quiet=quiet)  # fmt: skip


def cmd_bug(_: argparse.Namespace) -> None:
    """Both testbenches must fail on the RTL with BUG applied."""
    src = cocotb_run.RTL.read_text()
    if BUG[0] not in src:
        sys.exit(f"the RTL no longer has {BUG[0]!r}; update BUG")
    bug_dir = TARGET / "bench-bug"
    bug_dir.mkdir(parents=True, exist_ok=True)
    rtl = bug_dir / "sync_fifo.sv"
    rtl.write_text(src.replace(BUG[0], BUG[1]))
    print(f"injected: {BUG[0]!r} -> {BUG[1]!r}", file=sys.stderr)
    exe = build_simjig(bug_dir / "simjig", rtl)
    env = dict(os.environ, BENCH_CYCLES="10000", BENCH_OUT=str(bug_dir / "simjig.jsonl"))
    r = subprocess.run([str(exe), "--exact", "c_full::fifo", "--test-threads", "1"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    simjig_failed = r.returncode != 0
    report = re.findall(r"^.*(?:panicked|assertion|left|right|at time|time:).*$",
                        r.stdout + r.stderr, re.M)  # fmt: skip
    print("simjig:", "FAILED (caught)" if simjig_failed else "passed (MISSED)")
    print("\n".join(report[:8]))
    cocotb_dir = bug_dir / "cocotb"
    cocotb_run.build(rtl, cocotb_dir)
    try:
        cocotb_run.test("full", 10000, bug_dir / "cocotb.jsonl", cocotb_dir)
        cocotb_failed = False
    except SystemExit:
        cocotb_failed = True
    log = (cocotb_dir / "full.log").read_text()
    print("cocotb:", "FAILED (caught)" if cocotb_failed else "passed (MISSED)")
    print("\n".join(re.findall(r"^.*(?:AssertionError|assert |FAIL).*$", log, re.M)[:8]))
    if not (simjig_failed and cocotb_failed):
        sys.exit("an injected bug went unnoticed")


def cmd_table(a: argparse.Namespace) -> None:
    rows = [json.loads(line) for line in a.file.read_text().splitlines() if line.strip()]
    groups: dict[tuple, list[dict]] = {}
    for r in rows:
        groups.setdefault((r["variant"], r["cycles"], r["flow"], r["dut"]), []).append(r)
    order = {v: i for i, v in enumerate(VARIANTS)}
    med = {k: statistics.median(r["seconds"] for r in g) for k, g in groups.items()}
    print("| variant | cycles | flow | runs | median s | min s | max s | cycles/s (median) "
          "| process s (median) | speedup vs cocotb | load |")
    print("|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for k in sorted(groups, key=lambda k: (order[k[0]], k[1], k[2] != "simjig", k[3])):
        g = groups[k]
        variant, n, flow, dut = k
        secs = [r["seconds"] for r in g]
        proc = statistics.median(r["process_seconds"] for r in g)
        ref = med.get((variant, n, "cocotb", "fifo"))
        speed = f"{ref / med[k]:.1f}x" if ref and flow == "simjig" else ""
        loads = [r["load"] for r in g]
        load = f"{max(loads):.2f}" + ("" if all(r["quiet"] for r in g) else " (!)")
        name = flow if dut == "fifo" else f"{flow} ({dut})"
        print(f"| {variant} | {n:,} | {name} | {len(g)} | {med[k]:.4f} | {min(secs):.4f} "
              f"| {max(secs):.4f} | {n / med[k]:,.0f} | {proc:.3f} | {speed} | {load} |")
    # The two flows must have simulated the same thing.
    for (variant, n, flow, dut), g in groups.items():
        sig = {(r.get("pushes"), r.get("pops"), r["hash"]) for r in g}
        ref = groups.get((variant, n, "cocotb", "fifo"), g)
        if len(sig) != 1 or sig != {(r.get("pushes"), r.get("pops"), r["hash"]) for r in ref}:
            print(f"\nMISMATCH: {flow}/{dut} {variant} {n}: {sig}", file=sys.stderr)


def cmd_env(_: argparse.Namespace) -> None:
    """The machine and tool versions, for the README."""
    def sh(*cmd):
        return subprocess.run(cmd, capture_output=True, text=True).stdout.strip()

    import cocotb

    print("cpu:", sh("sysctl", "-n", "machdep.cpu.brand_string"))
    print("memory GiB:", int(sh("sysctl", "-n", "hw.memsize")) // 2**30)
    print("macOS:", platform.mac_ver()[0])
    print("python:", platform.python_version(), "cocotb:", cocotb.__version__)
    print("verilator:", sh("verilator", "--version"))
    print("rustc:", sh("pixi", "run", "--manifest-path", str(ROOT / "pixi.toml"),
                       "rustc", "--version"))  # fmt: skip
    print("c++:", sh(os.environ.get("CXX", "c++"), "--version").splitlines()[0])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--runs", type=int, default=5)
    r.add_argument("--sizes", type=lambda s: [int(x) for x in s.split(",")],
                   default=[10_000, 100_000, 1_000_000, 10_000_000])  # fmt: skip
    r.add_argument("--variants", type=lambda s: s.split(","), default=VARIANTS)
    r.add_argument("--cocotb-max", type=int, default=10_000_000,
                   help="largest size cocotb runs (extrapolate beyond it)")  # fmt: skip
    r.add_argument("--max-load", type=float, default=1.0)
    r.add_argument("--max-wait", type=float, default=None, help="seconds; default forever")
    r.add_argument("--out", type=Path, default=TARGET / "bench-results.jsonl")
    sub.add_parser("build")
    sub.add_parser("bug")
    t = sub.add_parser("table")
    t.add_argument("file", type=Path)
    sub.add_parser("env")
    a = ap.parse_args()
    if shutil.which("pixi") is None:
        sys.exit("pixi not on PATH")
    {"build": cmd_build, "run": cmd_run, "bug": cmd_bug, "table": cmd_table, "env": cmd_env}[a.cmd](a)


if __name__ == "__main__":
    main()

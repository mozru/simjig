"""Driver of the benchmark (bench/README.md). Run it through bench/run.sh.

    bench/run.sh build [--design D]   # build every testbench (never during measurement)
    bench/run.sh run [--design D] [--runs 5] [--sizes 10000,100000,1000000,10000000]
                     [--flows LIST] [--out FILE]
    bench/run.sh bug [--design D]     # an injected RTL bug must fail every testbench
    bench/run.sh table FILE           # the results table (Markdown) of a raw results file

The designs (DESIGNS): `fifo`, the tiny sync_fifo (the default), and `mid`, the mid-size
bench/rtl/mid_pipe.sv. The flows (FLOWS): simjig (twice: the design's plain model and its
`_pub` model), cocotb, Marlin and a raw C++ Verilator loop. `build` verilates and compiles every testbench. `run` builds nothing:
for each size and variant it runs each flow's testbench, each run in a fresh process,
sequentially, and appends one JSON line per run to FILE: the testbench's own timing of
its body (`seconds`), the process's wall time (`process_seconds`), the 1-minute load
average before the batch (`load`), and what the testbench counted (`pushes`, `pops`,
`hash`), which must agree between all flows.

Before each batch it waits until the 1-minute load average is below --max-load (2.5; see bench/README.md, "Idle machine") and
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
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE / "cocotb"))
import run as cocotb_run  # noqa: E402  (bench/cocotb/run.py)

SIMJIG_DIR = HERE / "simjig"
MARLIN_DIR = HERE / "marlin"
TARGET = ROOT / "target"
VARIANTS = ["clock_only", "monitor_only", "full"]
# The test of each variant, in simjig's and Marlin's test binaries.
RUST_TESTS = {"clock_only": "a_clock_only", "monitor_only": "b_monitor_only", "full": "c_full"}
# Every flow `run` knows, as (flow, the suffix of its model's name: the result's `dut` is
# the design's name plus it), and the names `--flows` selects them by.
FLOWS = {"simjig": ("simjig", ""), "simjig_pub": ("simjig", "_pub"),
         "cocotb": ("cocotb", ""), "marlin": ("marlin", ""),
         "cpp": ("cpp", "")}  # fmt: skip


@dataclass(frozen=True)
class Design:
    """A design of the benchmark and where each flow's testbench of it is."""

    name: str
    hdl: cocotb_run.Design  # its sources, top and parameters, the same in every flow
    rust_test: str  # the test target in bench/simjig and bench/marlin
    rtl_env: str  # the variable that replaces `hdl.rtl` in the Rust flows
    cpp_main: Path
    # The injected bug (`bug`): a replacement in `hdl.rtl` that every testbench must catch.
    bug: tuple[str, str]
    bug_what: str

    def target(self, base: str) -> Path:
        """A build directory: target/<base> for the FIFO (as before `mid` existed),
        target/<base>-<design> for the others."""
        return TARGET / (base if self.name == "fifo" else f"{base}-{self.name}")


DESIGNS = {
    "fifo": Design(
        "fifo", cocotb_run.DESIGNS["fifo"], "bench", "BENCH_RTL", HERE / "cpp" / "main.cpp",
        ("wire do_push = push && (!full || do_pop);", "wire do_push = push && !full;"),
        "a full FIFO drops a push that comes with a pop (the `|| do_pop` that the RTL's "
        "handshake comment requires is gone)",
    ),
    "mid": Design(
        "mid", cocotb_run.DESIGNS["mid"], "mid", "BENCH_MID_RTL", HERE / "cpp" / "mid.cpp",
        ("popped <= !rst && pop && !empty;", "popped <= !rst && pop;"),
        "a pop of an empty FIFO sends a stale entry down the pipeline (the `&& !empty` "
        "of stage 0's valid is gone)",
    ),
}  # fmt: skip


def build_rust(flow: str, crate: Path, target: Path, test: str,
               rtl_env: dict[str, str] | None = None) -> Path:  # fmt: skip
    """Builds a Rust flow's test binaries (profile `bench`) and returns the path of the
    one named `test`. `rtl_env` sets RTL replacements (`BENCH_RTL`, `BENCH_MID_RTL`)."""
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), **(rtl_env or {}))
    cmd = ["pixi", "run", "--manifest-path", str(ROOT / "pixi.toml"), "cargo", "test",
           "--profile", "bench", "--no-run", "--message-format=json"]  # fmt: skip
    out = subprocess.run(cmd, cwd=crate, env=env, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"{flow} build failed:\n{out.stderr}")
    for line in out.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
            if msg["target"]["name"] == test:
                return Path(msg["executable"])
    sys.exit(f"{flow} build: no test executable named `{test}`")


def rtl_env(design: Design, rtl: Path | None) -> dict[str, str]:
    """The Rust flows' environment that replaces the design's RTL file with `rtl`."""
    return {} if rtl is None else {design.rtl_env: str(rtl)}


def build_simjig(design: Design, target: Path, rtl: Path | None = None) -> Path:
    return build_rust("simjig", SIMJIG_DIR, target, design.rust_test, rtl_env(design, rtl))


def run_simjig(exe: Path, dut: str, variant: str, n: int, out: Path) -> float:
    """One simjig run; returns the process's wall time."""
    env = dict(os.environ, BENCH_CYCLES=str(n), BENCH_OUT=str(out))
    test = f"{RUST_TESTS[variant]}::{dut}"
    t0 = time.perf_counter()
    r = subprocess.run([str(exe), "--exact", test, "--test-threads", "1", "-q"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    wall = time.perf_counter() - t0
    if r.returncode != 0:
        sys.exit(f"simjig {test} failed:\n{r.stdout}\n{r.stderr}")
    return wall


def run_cocotb(design: Design, variant: str, n: int, out: Path) -> float:
    t0 = time.perf_counter()
    cocotb_run.test(design.hdl, variant, n, out, design.target("bench-cocotb"))
    return time.perf_counter() - t0


def marlin_env(artifacts: Path, rtl: dict[str, str], **extra: str) -> dict[str, str]:
    """The environment of a Marlin test run. Marlin verilates at run time, on a model's
    first use, with `make` reading MAKEFLAGS: the same -O2 as the other flows."""
    return dict(os.environ, BENCH_ARTIFACTS=str(artifacts), MAKEFLAGS=cocotb_run.OPT,
                **rtl, **extra)  # fmt: skip


def marlin_artifacts(design: Design) -> Path:
    """Where Marlin builds a design's model: one directory per design."""
    return design.target("bench-marlin/artifacts")


def build_marlin(design: Design, artifacts: Path, rtl: Path | None = None) -> Path:
    """Builds Marlin's test binary, then runs it once (10 cycles) so that Marlin builds
    its model into `artifacts`: measured runs then find it built."""
    exe = build_rust("marlin", MARLIN_DIR, MARLIN_TARGET, design.rust_test)
    if artifacts.exists():
        shutil.rmtree(artifacts)
    env = marlin_env(artifacts, rtl_env(design, rtl), BENCH_CYCLES="10", BENCH_OUT=os.devnull)
    r = subprocess.run([str(exe), "--exact", "a_clock_only", "--test-threads", "1"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    if r.returncode != 0:
        sys.exit(f"marlin model build failed:\n{r.stdout}\n{r.stderr}")
    return exe


def run_marlin(exe: Path, artifacts: Path, variant: str, n: int, out: Path) -> float:
    """One Marlin run; returns the process's wall time."""
    env = marlin_env(artifacts, {}, BENCH_CYCLES=str(n), BENCH_OUT=str(out))
    test = RUST_TESTS[variant]
    t0 = time.perf_counter()
    r = subprocess.run([str(exe), "--exact", test, "--test-threads", "1", "-q"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    wall = time.perf_counter() - t0
    if r.returncode != 0:
        sys.exit(f"marlin {test} failed:\n{r.stdout}\n{r.stderr}")
    if "Compiling" in r.stderr:
        sys.exit("marlin rebuilt its model in a measured run: run `bench/run.sh build`")
    return wall


def build_cpp(design: Design, build_dir: Path, rtl: Path | None = None) -> Path:
    """Verilates the design with its raw C++ testbench (bench/cpp/main.cpp or mid.cpp) as
    its `main`: simjig's Verilator flags (no `.vlt`: C++ reaches the ports anyway), the
    C++ at -O2."""
    if build_dir.exists():
        shutil.rmtree(build_dir)
    build_dir.mkdir(parents=True)
    hdl = design.hdl
    params = [f"-G{k}={v}" for k, v in hdl.params.items()]
    cmd = ["verilator", "--cc", "--exe", "--build", "-j", "0", "--no-timing",
           "--top-module", hdl.top, *params, "-Mdir", str(build_dir), "-o", "bench_cpp",
           *map(str, hdl.sources(rtl)), str(design.cpp_main)]  # fmt: skip
    env = dict(os.environ, MAKEFLAGS=cocotb_run.OPT)
    r = subprocess.run(cmd, env=env, capture_output=True, text=True)
    (build_dir / "build.log").write_text(r.stdout + r.stderr)
    if r.returncode != 0:
        sys.exit(f"cpp build failed:\n{r.stdout}\n{r.stderr}")
    return build_dir / "bench_cpp"


def run_cpp(exe: Path, variant: str, n: int, out: Path) -> float:
    """One C++ run; returns the process's wall time."""
    env = dict(os.environ, BENCH_CYCLES=str(n), BENCH_OUT=str(out))
    t0 = time.perf_counter()
    r = subprocess.run([str(exe), variant], env=env, capture_output=True, text=True)
    wall = time.perf_counter() - t0
    if r.returncode != 0:
        sys.exit(f"cpp {variant} failed:\n{r.stdout}\n{r.stderr}")
    return wall


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


MARLIN_TARGET = TARGET / "bench-marlin"


def built_file(design: Design) -> Path:
    """What `build` built for a design: the paths of the executables `run` runs."""
    return design.target("bench-built").with_suffix(".json")


def cmd_build(a: argparse.Namespace) -> None:
    design = DESIGNS[a.design]
    print(f"building simjig ({design.name})", file=sys.stderr)
    simjig = build_simjig(design, TARGET / "bench-simjig")
    print("building cocotb", file=sys.stderr)
    cocotb_run.build(design.hdl, design.target("bench-cocotb"))
    print("building marlin (and its model)", file=sys.stderr)
    marlin = build_marlin(design, marlin_artifacts(design))
    print("building cpp", file=sys.stderr)
    cpp = build_cpp(design, design.target("bench-cpp"))
    built = {"simjig": str(simjig), "marlin": str(marlin), "cpp": str(cpp)}
    built_file(design).write_text(json.dumps(built))


def run_one(built: dict, design: Design, flow: str, dut: str, variant: str, n: int,
            out: Path) -> float:  # fmt: skip
    """One run of one flow; returns the process's wall time."""
    if flow == "simjig":
        return run_simjig(Path(built["simjig"]), dut, variant, n, out)
    if flow == "marlin":
        return run_marlin(Path(built["marlin"]), marlin_artifacts(design), variant, n, out)
    if flow == "cpp":
        return run_cpp(Path(built["cpp"]), variant, n, out)
    return run_cocotb(design, variant, n, out)


def cmd_run(a: argparse.Namespace) -> None:
    design = DESIGNS[a.design]
    out = a.out.resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    if not built_file(design).exists():
        sys.exit(f"nothing built: run `bench/run.sh build --design {design.name}` first")
    if unknown := [f for f in a.flows if f not in FLOWS]:
        sys.exit(f"unknown flows {unknown}; known: {list(FLOWS)}")
    built = json.loads(built_file(design).read_text())
    if missing := [f for f in ("simjig", "marlin", "cpp") if f not in built]:
        sys.exit(f"built before {missing} existed: run `bench/run.sh build` again")
    for n in a.sizes:
        for variant in a.variants:
            flows = [FLOWS[f] for f in a.flows]
            if n > a.cocotb_max:
                flows = [f for f in flows if f[0] != "cocotb"]
            for flow, suffix in flows:
                dut = design.name + suffix
                load, quiet = wait_for_quiet(a.max_load, a.max_wait)
                print(f"{flow}/{dut} {variant} {n}: {a.runs} runs (load {load:.2f})",
                      file=sys.stderr)  # fmt: skip
                for _ in range(a.runs):
                    wall = run_one(built, design, flow, dut, variant, n, out)
                    tag_last(out, process_seconds=round(wall, 6), load=round(load, 2),
                             quiet=quiet)  # fmt: skip


def cmd_bug(a: argparse.Namespace) -> None:
    """Every testbench must fail on the design's RTL with its bug applied."""
    design = DESIGNS[a.design]
    old, new = design.bug
    src = design.hdl.rtl.read_text()
    if src.count(old) != 1:
        sys.exit(f"{design.hdl.rtl.name} no longer has {old!r} once; update DESIGNS")
    bug_dir = design.target("bench-bug")
    bug_dir.mkdir(parents=True, exist_ok=True)
    rtl = bug_dir / design.hdl.rtl.name
    rtl.write_text(src.replace(old, new))
    print(f"design {design.name}: {design.bug_what}")
    print(f"injected: {old!r} -> {new!r}")
    caught = [bug_simjig(design, rtl, bug_dir), bug_cocotb(design, rtl, bug_dir),
              bug_marlin(design, rtl, bug_dir), bug_cpp(design, rtl, bug_dir)]  # fmt: skip
    if not all(caught):
        sys.exit("an injected bug went unnoticed")


def bug_simjig(design: Design, rtl: Path, bug_dir: Path) -> bool:
    """simjig's C on the buggy RTL; whether it failed."""
    exe = build_simjig(design, bug_dir / "simjig", rtl)
    env = dict(os.environ, BENCH_CYCLES="10000", BENCH_OUT=str(bug_dir / "simjig.jsonl"))
    test = f"c_full::{design.name}"
    r = subprocess.run([str(exe), "--exact", test, "--test-threads", "1"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    failed = r.returncode != 0
    report = re.findall(r"^.*(?:panicked|assertion|left|right|at time|time:).*$",
                        r.stdout + r.stderr, re.M)  # fmt: skip
    print("simjig:", "FAILED (caught)" if failed else "passed (MISSED)")
    print("\n".join(report[:8]))
    return failed


def bug_cocotb(design: Design, rtl: Path, bug_dir: Path) -> bool:
    """cocotb's C on the buggy RTL; whether it failed."""
    cocotb_dir = bug_dir / "cocotb"
    cocotb_run.build(design.hdl, cocotb_dir, rtl)
    try:
        cocotb_run.test(design.hdl, "full", 10000, bug_dir / "cocotb.jsonl", cocotb_dir)
        failed = False
    except SystemExit:
        failed = True
    log = (cocotb_dir / "full.log").read_text()
    print("cocotb:", "FAILED (caught)" if failed else "passed (MISSED)")
    print("\n".join(re.findall(r"^.*(?:AssertionError|assert |FAIL).*$", log, re.M)[:8]))
    return failed


def bug_marlin(design: Design, rtl: Path, bug_dir: Path) -> bool:
    """Marlin's C on the buggy RTL (a model of its own, in `bug_dir`); whether it failed."""
    artifacts = bug_dir / "marlin-artifacts"
    if artifacts.exists():
        shutil.rmtree(artifacts)
    exe = build_rust("marlin", MARLIN_DIR, MARLIN_TARGET, design.rust_test)
    env = marlin_env(artifacts, rtl_env(design, rtl), BENCH_CYCLES="10000",
                     BENCH_OUT=str(bug_dir / "marlin.jsonl"))  # fmt: skip
    r = subprocess.run([str(exe), "--exact", "c_full", "--test-threads", "1"],
                       env=env, capture_output=True, text=True)  # fmt: skip
    failed = r.returncode != 0 and "assertion `left == right` failed" in r.stdout + r.stderr
    report = re.findall(r"^.*(?:panicked|assertion|left|right).*$", r.stdout + r.stderr, re.M)
    print("marlin:", "FAILED (caught)" if failed else "passed (MISSED)")
    print("\n".join(report[:8]))
    return failed


def bug_cpp(design: Design, rtl: Path, bug_dir: Path) -> bool:
    """The C++ testbench's C on the buggy RTL; whether it failed."""
    exe = build_cpp(design, bug_dir / "cpp", rtl)
    env = dict(os.environ, BENCH_CYCLES="10000", BENCH_OUT=str(bug_dir / "cpp.jsonl"))
    r = subprocess.run([str(exe), "full"], env=env, capture_output=True, text=True)
    failed = r.returncode == 1 and "check failed" in r.stderr
    print("cpp:", "FAILED (caught)" if failed else "passed (MISSED)")
    print(r.stderr.strip())
    return failed


TABLE_ORDER = ["simjig", "marlin", "cpp", "cocotb"]


def ratio(ref: float | None, x: float) -> str:
    return f"{ref / x:.1f}x" if ref else ""


def design_of(dut: str) -> str:
    """The design a result's `dut` is a model of (`fifo_pub` is the FIFO's)."""
    return dut.removesuffix("_pub")


def cmd_table(a: argparse.Namespace) -> None:
    rows = [json.loads(line) for line in a.file.read_text().splitlines() if line.strip()]
    groups: dict[tuple, list[dict]] = {}
    for r in rows:
        key = (design_of(r["dut"]), r["variant"], r["cycles"], r["flow"], r["dut"])
        groups.setdefault(key, []).append(r)
    order = {v: i for i, v in enumerate(VARIANTS)}
    med = {k: statistics.median(r["seconds"] for r in g) for k, g in groups.items()}
    several = len({k[0] for k in groups}) > 1
    print(("| design " if several else "") + "| variant | cycles | flow | runs | median s "
          "| min s | max s | cycles/s (median) | ns/cycle | process s (median) "
          "| speedup vs cocotb | time vs cpp | load |")
    print(("|---" if several else "") + "|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    key = lambda k: (k[0], order[k[1]], k[2], TABLE_ORDER.index(k[3]), k[4])  # noqa: E731
    for k in sorted(groups, key=key):
        g = groups[k]
        design, variant, n, flow, dut = k
        secs = [r["seconds"] for r in g]
        proc = statistics.median(r["process_seconds"] for r in g)
        cocotb = med.get((design, variant, n, "cocotb", design))
        speed = ratio(cocotb, med[k]) if flow != "cocotb" else ""
        cpp = med.get((design, variant, n, "cpp", design))
        vs_cpp = ratio(med[k], cpp) if cpp and flow != "cpp" else ""
        loads = [r["load"] for r in g]
        load = f"{max(loads):.2f}" + ("" if all(r["quiet"] for r in g) else " (!)")
        name = flow if dut == design else f"{flow} ({dut})"
        print((f"| {design} " if several else "")
              + f"| {variant} | {n:,} | {name} | {len(g)} | {med[k]:.6f} | {min(secs):.6f} "
              f"| {max(secs):.6f} | {n / med[k]:,.0f} | {1e9 * med[k] / n:,.1f} "
              f"| {proc:.3f} | {speed} | {vs_cpp} | {load} |")  # fmt: skip
    # Every flow must have simulated the same thing: one (pushes, pops, hash) per point.
    points: dict[tuple, dict[tuple, set]] = {}
    for (design, variant, n, flow, dut), g in groups.items():
        sigs = points.setdefault((design, variant, n), {})
        sigs[(flow, dut)] = {(r.get("pushes"), r.get("pops"), r["hash"]) for r in g}
    for (design, variant, n), sigs in points.items():
        if len(set().union(*sigs.values())) != 1:
            print(f"\nMISMATCH: {design} {variant} {n}: {sigs}", file=sys.stderr)


def cmd_env(_: argparse.Namespace) -> None:
    """The machine and tool versions, for the README."""
    def sh(*cmd):
        return subprocess.run(cmd, capture_output=True, text=True).stdout.strip()

    import cocotb

    print("cpu:", sh("sysctl", "-n", "machdep.cpu.brand_string"))
    print("memory GiB:", int(sh("sysctl", "-n", "hw.memsize")) // 2**30)
    print("macOS:", platform.mac_ver()[0])
    print("python:", platform.python_version(), "cocotb:", cocotb.__version__)
    lock = (MARLIN_DIR / "Cargo.lock").read_text()
    print("marlin:", re.search(r'name = "marlin"\nversion = "([^"]+)"', lock).group(1))
    print("verilator:", sh("verilator", "--version"))
    print("rustc:", sh("pixi", "run", "--manifest-path", str(ROOT / "pixi.toml"),
                       "rustc", "--version"))  # fmt: skip
    print("c++:", sh(os.environ.get("CXX", "c++"), "--version").splitlines()[0])


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    designs = argparse.ArgumentParser(add_help=False)
    designs.add_argument("--design", choices=list(DESIGNS), default="fifo")
    r = sub.add_parser("run", parents=[designs])
    r.add_argument("--runs", type=int, default=5)
    r.add_argument("--sizes", type=lambda s: [int(x) for x in s.split(",")],
                   default=[10_000, 100_000, 1_000_000, 10_000_000])  # fmt: skip
    r.add_argument("--variants", type=lambda s: s.split(","), default=VARIANTS)
    r.add_argument("--flows", type=lambda s: s.split(","), default=list(FLOWS),
                   help=f"comma-separated, of {','.join(FLOWS)} (default: all)")  # fmt: skip
    r.add_argument("--cocotb-max", type=int, default=10_000_000,
                   help="largest size cocotb runs (extrapolate beyond it)")  # fmt: skip
    r.add_argument("--max-load", type=float, default=2.5)
    r.add_argument("--max-wait", type=float, default=None, help="seconds; default forever")
    r.add_argument("--out", type=Path, default=TARGET / "bench-results.jsonl")
    sub.add_parser("build", parents=[designs])
    sub.add_parser("bug", parents=[designs])
    # `table` and `env` take --design too (and ignore it), so that run.sh can pass it on.
    t = sub.add_parser("table", parents=[designs])
    t.add_argument("file", type=Path)
    sub.add_parser("env", parents=[designs])
    a = ap.parse_args()
    if shutil.which("pixi") is None:
        sys.exit("pixi not on PATH")
    {"build": cmd_build, "run": cmd_run, "bug": cmd_bug, "table": cmd_table, "env": cmd_env}[a.cmd](a)


if __name__ == "__main__":
    main()

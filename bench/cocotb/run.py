"""Build and run the cocotb benchmark testbench (bench/README.md) with cocotb's runner.

    python bench/cocotb/run.py build [--design D] [--rtl FILE] [--build-dir DIR]
    python bench/cocotb/run.py test VARIANT CYCLES OUT [--design D] [--build-dir DIR]

`build` verilates and compiles once (not timed); `test` runs one variant for CYCLES
cycles in a fresh simulator process and appends its JSON line to OUT. The design (DESIGNS)
is `fifo` (the default) or `mid`; `--rtl` replaces its own RTL file (the first source). Run under
`pixi run -e conformance`, which pins cocotb 2.1.0 and Verilator 5.052.

The C++ is compiled at -O2, the level simjig-build uses (`BuildEnv::DEFAULT_CXX_OPT`),
instead of verilated.mk's -Os: OPT_FAST, OPT_SLOW and OPT_GLOBAL are passed to the
runner's `make` through MAKEFLAGS, where GNU make reads them as command-line variables.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
RTL = ROOT / "examples" / "lib" / "rtl" / "sync_fifo.sv"
OPT = "OPT_FAST=-O2 OPT_SLOW=-O2 OPT_GLOBAL=-O2"


@dataclass(frozen=True)
class Design:
    """One of the benchmark's designs, as every flow verilates it (bench/README.md)."""

    rtl: Path  # its own RTL file: the one `bench.py bug` injects a bug into
    others: tuple[Path, ...]  # the other sources it needs
    top: str
    params: dict  # -G overrides (mid_pipe: none, its defaults are the benchmark's)
    test_module: str  # its cocotb testbench, in this directory

    def sources(self, rtl: Path | None = None) -> list[Path]:
        return [rtl or self.rtl, *self.others]


DESIGNS = {
    "fifo": Design(RTL, (), "sync_fifo", {"WIDTH": 8, "DEPTH": 16, "FWFT": 0}, "test_bench"),
    "mid": Design(HERE.parent / "rtl" / "mid_pipe.sv", (RTL,), "mid_pipe", {}, "test_mid"),
}


def build_dir_of(design: str) -> Path:
    """Where a design's cocotb build goes: target/bench-cocotb for the FIFO (as before
    the mid-size design existed), target/bench-cocotb-<design> for the others."""
    return ROOT / "target" / ("bench-cocotb" if design == "fifo" else f"bench-cocotb-{design}")


def runner():
    from cocotb_tools.runner import get_runner

    return get_runner("verilator")


def build(design: Design, build_dir: Path, rtl: Path | None = None) -> None:
    if build_dir.exists():
        shutil.rmtree(build_dir)
    build_dir.mkdir(parents=True)
    os.environ["MAKEFLAGS"] = OPT
    runner().build(
        sources=design.sources(rtl),
        hdl_toplevel=design.top,
        parameters=design.params,
        build_dir=build_dir,
        timescale=("1ns", "1ps"),
        always=True,
        log_file=build_dir / "build.log",
    )


def test(design: Design, variant: str, n: int, out: Path, build_dir: Path) -> None:
    from cocotb_tools.check_results import get_results

    sys.path.insert(0, str(HERE))  # the runner passes sys.path on as PYTHONPATH
    for var in ("COCOTB_TEST_FILTER", "COCOTB_TESTCASE", "COCOTB_SCHEDULER_DEBUG"):
        os.environ.pop(var, None)
    log = build_dir / f"{variant}.log"
    t0 = time.perf_counter()
    results = runner().test(
        test_module=design.test_module,
        hdl_toplevel=design.top,
        hdl_toplevel_lang="verilog",  # a fresh runner has no sources to infer it from
        testcase=variant,
        build_dir=build_dir,
        test_dir=build_dir,
        results_xml=f"{variant}.xml",
        extra_env={
            "BENCH_CYCLES": str(n),
            "BENCH_OUT": str(out),
            "COCOTB_TRUST_INERTIAL_WRITES": "1",
            "COCOTB_LOG_LEVEL": "WARNING",
        },
        log_file=log,
    )
    wall = time.perf_counter() - t0
    total, failed = get_results(results)
    if failed or total != 1:
        sys.exit(f"cocotb {variant} failed ({failed} of {total}); see {log}")
    print(f"cocotb {variant} {n}: process {wall:.3f} s", file=sys.stderr)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cmd", choices=["build", "test"])
    ap.add_argument("args", nargs="*")
    ap.add_argument("--design", choices=list(DESIGNS), default="fifo")
    ap.add_argument("--rtl", type=Path, default=None)
    ap.add_argument("--build-dir", type=Path, default=None)
    a = ap.parse_args()
    design = DESIGNS[a.design]
    build_dir = (a.build_dir or build_dir_of(a.design)).resolve()
    if a.cmd == "build":
        build(design, build_dir, a.rtl and a.rtl.resolve())
    else:
        variant, n, out = a.args
        test(design, variant, int(n), Path(out).resolve(), build_dir)


if __name__ == "__main__":
    main()

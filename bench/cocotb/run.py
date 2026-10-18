"""Build and run the cocotb benchmark testbench (bench/README.md) with cocotb's runner.

    python bench/cocotb/run.py build [--rtl FILE] [--build-dir DIR]
    python bench/cocotb/run.py test VARIANT CYCLES OUT [--build-dir DIR]

`build` verilates and compiles once (not timed); `test` runs one variant for CYCLES
cycles in a fresh simulator process and appends its JSON line to OUT. Run under
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
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
RTL = ROOT / "examples" / "lib" / "rtl" / "sync_fifo.sv"
BUILD = ROOT / "target" / "bench-cocotb"
PARAMS = {"WIDTH": 8, "DEPTH": 16, "FWFT": 0}
OPT = "OPT_FAST=-O2 OPT_SLOW=-O2 OPT_GLOBAL=-O2"


def runner():
    from cocotb_tools.runner import get_runner

    return get_runner("verilator")


def build(rtl: Path, build_dir: Path) -> None:
    if build_dir.exists():
        shutil.rmtree(build_dir)
    build_dir.mkdir(parents=True)
    os.environ["MAKEFLAGS"] = OPT
    runner().build(
        sources=[rtl],
        hdl_toplevel="sync_fifo",
        parameters=PARAMS,
        build_dir=build_dir,
        timescale=("1ns", "1ps"),
        always=True,
        log_file=build_dir / "build.log",
    )


def test(variant: str, n: int, out: Path, build_dir: Path) -> None:
    from cocotb_tools.check_results import get_results

    sys.path.insert(0, str(HERE))  # the runner passes sys.path on as PYTHONPATH
    for var in ("COCOTB_TEST_FILTER", "COCOTB_TESTCASE", "COCOTB_SCHEDULER_DEBUG"):
        os.environ.pop(var, None)
    log = build_dir / f"{variant}.log"
    t0 = time.perf_counter()
    results = runner().test(
        test_module="test_bench",
        hdl_toplevel="sync_fifo",
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
    ap.add_argument("--rtl", type=Path, default=RTL)
    ap.add_argument("--build-dir", type=Path, default=BUILD)
    a = ap.parse_args()
    if a.cmd == "build":
        build(a.rtl.resolve(), a.build_dir.resolve())
    else:
        variant, n, out = a.args
        test(variant, int(n), Path(out).resolve(), a.build_dir.resolve())


if __name__ == "__main__":
    main()

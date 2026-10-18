"""Build the conformance DUTs with cocotb's Verilator runner and regenerate the goldens.

    pixi run -e conformance conformance-regen            # rewrite tests/conformance/golden
    pixi run -e conformance conformance-regen --check    # regenerate into target/ and diff

The check mode fails if any golden differs, so it proves the goldens are reproducible
from the pinned cocotb + Verilator and that nobody edited them by hand.
"""

from __future__ import annotations

import argparse
import filecmp
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
DUTS = ROOT / "tests" / "duts"
TB = HERE / "tb"
GOLDEN = HERE / "golden"
BUILD = ROOT / "target" / "conformance"

VERILATOR_VERSION = "5.052"
COCOTB_VERSION = "2.1.0"

# (DUT directory, top module, sources, parameters, test module, scenarios). Each scenario
# is one @cocotb.test in the test module and runs in its own simulator process, so every
# golden starts at time 0 from a freshly constructed model.
SUITES = [
    ("flop", "flop", ["flop.sv"], {}, "test_flop",
     ["edge_read", "readonly_read", "write_after_edge", "timeout"]),
    ("flop", "flop", ["flop.sv"], {}, "test_timeout_ties", ["timeout_ties"]),
    ("counter", "counter", ["counter.sv"], {"WIDTH": 8}, "test_counter",
     ["timer", "clock_cycles", "value_change", "first_combine"]),
    ("comb_chain", "comb_chain", ["comb_chain.sv"], {}, "test_comb_chain", ["comb_settle"]),
    ("fifo", "fifo", ["fifo.sv"], {"WIDTH": 8, "DEPTH": 4}, "test_fifo", ["fifo_stream"]),
    ("async_fifo", "async_fifo", ["async_fifo.sv"], {"WIDTH": 8, "DEPTH": 8},
     "test_async_fifo", ["two_clocks", "cdc_stream"]),
    ("derived_clk", "derived_clk", ["derived_clk.sv"], {}, "test_derived_clk", ["derived_edge"]),
    ("async_fifo", "async_fifo", ["async_fifo.sv"], {"WIDTH": 8, "DEPTH": 8},
     "test_wake_order", ["coincident_join"]),
    ("comb_chain", "comb_chain", ["comb_chain.sv"], {}, "test_wake_order",
     ["changed_join", "written_join"]),
    ("counter", "counter", ["counter.sv"], {"WIDTH": 8}, "test_wake_order",
     ["edge_kinds", "combinator_hop"]),
]  # fmt: skip


def check_toolchain() -> None:
    """Refuse to run against anything but the pinned Verilator from this pixi env."""
    import cocotb

    if cocotb.__version__ != COCOTB_VERSION:
        sys.exit(f"cocotb {cocotb.__version__} found, {COCOTB_VERSION} required")
    exe = shutil.which("verilator")
    if exe is None or not Path(exe).resolve().is_relative_to(Path(sys.prefix).resolve()):
        sys.exit(f"verilator must come from the pixi env {sys.prefix}, found {exe}")
    out = subprocess.run([exe, "--version"], capture_output=True, text=True, check=True)
    if not out.stdout.startswith(f"Verilator {VERILATOR_VERSION} "):
        sys.exit(f"Verilator {VERILATOR_VERSION} required, found: {out.stdout.strip()}")


def run_suite(
    dut: str, top: str, sources: list[str], params: dict, module: str, scenarios: list[str],
    out: Path,
) -> None:
    from cocotb_tools.check_results import get_results
    from cocotb_tools.runner import get_runner

    runner = get_runner("verilator")
    build_dir = BUILD / dut
    build_dir.mkdir(parents=True, exist_ok=True)
    runner.build(
        sources=[DUTS / dut / s for s in sources],
        hdl_toplevel=top,
        parameters=params,
        build_dir=build_dir,
        timescale=("1ns", "1ps"),
        build_args=["-Wall"],
        always=True,
        log_file=build_dir / "build.log",
    )
    for scenario in scenarios:
        log_file = build_dir / f"{scenario}.log"
        results = runner.test(
            test_module=module,
            hdl_toplevel=top,
            testcase=scenario,
            build_dir=build_dir,
            test_dir=build_dir,
            results_xml=f"{scenario}.xml",
            extra_env={"CONFORMANCE_OUT": str(out), "COCOTB_RANDOM_SEED": "1"},
            log_file=log_file,
        )
        total, failed = get_results(results)
        if failed or total != 1 or not (out / f"{scenario}.jsonl").exists():
            sys.exit(f"{module}.{scenario} failed ({failed} of {total}); see {log_file}")
        print(f"conformance: {module}.{scenario} ok")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="diff against the goldens instead")
    ap.add_argument(
        "--inertial",
        choices=["0", "1"],
        default="1",
        help="COCOTB_TRUST_INERTIAL_WRITES; the goldens use 1, cocotb's Verilator default",
    )
    ap.add_argument("--out", type=Path, help="write logs here instead (for experiments)")
    args = ap.parse_args()
    if args.inertial != "1" and not args.out:
        sys.exit("--inertial 0 changes the semantics; it needs --out, not the goldens")

    check_toolchain()
    sys.path.insert(0, str(TB))  # the runner passes sys.path to the simulator as PYTHONPATH
    out = args.out.resolve() if args.out else (BUILD / "golden-check" if args.check else GOLDEN)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    # cocotb's Python inherits these; make the run independent of the caller's settings.
    for var in ("COCOTB_TEST_FILTER", "COCOTB_TESTCASE", "COCOTB_SCHEDULER_DEBUG"):
        os.environ.pop(var, None)
    # cocotb's Verilator runner and Makefile both default this to 1; pin it so the goldens
    # never depend on the caller's environment. See README.md, "Write semantics".
    os.environ["COCOTB_TRUST_INERTIAL_WRITES"] = args.inertial
    for suite in SUITES:
        run_suite(*suite, out=out)

    if args.check:
        want = sorted(p.name for p in GOLDEN.glob("*.jsonl"))
        got = sorted(p.name for p in out.glob("*.jsonl"))
        if want != got:
            sys.exit(f"golden set differs: committed {want}, regenerated {got}")
        bad = [n for n in want if not filecmp.cmp(GOLDEN / n, out / n, shallow=False)]
        if bad:
            sys.exit(f"goldens differ from a fresh run: {bad} (diff against {out})")
        print(f"conformance: {len(want)} goldens reproduced exactly")
    else:
        print(f"conformance: wrote {len(list(out.glob('*.jsonl')))} goldens to {out}")


if __name__ == "__main__":
    main()

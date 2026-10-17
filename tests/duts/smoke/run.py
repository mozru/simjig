"""Build each shared DUT and run its smoke test: `pixi run -e conformance duts-smoke`.

The fatal DUT is run once per kind; each run must end the simulation the way README.md
in tests/duts/fatal records (the process exit code and the message in the log).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
DUTS = HERE.parent
BUILD = DUTS.parents[1] / "target" / "duts-smoke"

# (test name = top module, DUT directory, sources, extra build args)
SMOKES = [
    ("inv", "inv", ["inv.sv"], []),
    ("wide", "wide", ["wide.sv"], []),
    ("struct_pipe", "struct_pipe", ["struct_pipe_pkg.sv", "struct_pipe.sv"], []),
    ("timing_dut", "timing_dut", ["timing_dut.sv"], ["--timing"]),
    ("dpi", "dpi", ["dpi.sv"], [str(DUTS / "dpi" / "dpi_impl.c")]),
    ("derived_clk", "derived_clk", ["derived_clk.sv"], []),
    ("iface_top_wrap", "iface_top",
     ["iface_pkg.sv", "bus_if.sv", "iface_top.sv", "iface_top_wrap.sv"], []),
]  # fmt: skip

# kind -> (simulator exit code, text the log must contain)
FATAL = {
    0: (1, "%Fatal: fatal.sv:"),
    1: (1, "Verilog $stop"),
    2: (1, "%Error: fatal.sv:"),
    3: (0, "fatal.sv:34: Verilog $finish"),
}


def build(runner, top: str, dut: str, sources: list[str], args: list[str]) -> Path:
    build_dir = BUILD / top
    build_dir.mkdir(parents=True, exist_ok=True)
    runner.build(
        sources=[DUTS / dut / s for s in sources],
        hdl_toplevel=top,
        build_dir=build_dir,
        timescale=("1ns", "1ps"),
        build_args=args,
        always=True,
        log_file=build_dir / "build.log",
    )
    return build_dir


def main() -> None:
    from cocotb_tools.check_results import get_results
    from cocotb_tools.runner import get_runner

    sys.path.insert(0, str(HERE))
    runner = get_runner("verilator")
    for top, dut, sources, args in SMOKES:
        build_dir = build(runner, top, dut, sources, args)
        results = runner.test(
            test_module="test_smoke", hdl_toplevel=top, testcase=top,
            build_dir=build_dir, test_dir=build_dir, log_file=build_dir / "test.log",
        )  # fmt: skip
        total, failed = get_results(results)
        if failed or total != 1:
            sys.exit(f"smoke {top} failed; see {build_dir / 'test.log'}")
        print(f"duts-smoke: {top} ok")

    build_dir = build(runner, "fatal", "fatal", ["fatal.sv"], [])
    exe = build_dir / "fatal"
    for kind, (code, text) in FATAL.items():
        # Run the simulator directly: the runner raises on any non-zero exit code.
        env = dict(runner.env)
        env.update(
            COCOTB_TEST_MODULES="test_fatal", COCOTB_TOPLEVEL="fatal", FATAL_KIND=str(kind),
            COCOTB_TEST_FILTER="fire", COCOTB_RANDOM_SEED="1",
            COCOTB_RESULTS_FILE=str(build_dir / f"kind{kind}.xml"),
        )  # fmt: skip
        proc = subprocess.run([str(exe)], cwd=build_dir, env=env, capture_output=True, text=True)
        out = proc.stdout + proc.stderr
        (build_dir / f"kind{kind}.log").write_text(out)
        if proc.returncode != code or text not in out:
            sys.exit(f"fatal kind={kind}: exit {proc.returncode}, want {code} and {text!r}")
        print(f"duts-smoke: fatal kind={kind} ok (exit {code})")


if __name__ == "__main__":
    main()

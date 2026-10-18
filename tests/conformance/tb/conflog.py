"""Normalised event log shared by every conformance scenario.

Each scenario owns one `Log`. Tasks record through a `Probe`, which remembers the trigger
that last resumed the task, so every line says where in the scheduler the value was seen.
The line format is specified in tests/conformance/README.md; simjig emits the same format
and diffs it against the goldens byte for byte.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from cocotb.simtime import get_sim_time
from cocotb.triggers import ReadOnly, ReadWrite

# Time is logged in integer picoseconds; regen.py builds every DUT with timescale 1ns/1ps,
# so one simulator step is one picosecond.
TIME_UNIT = "ps"


def now() -> int:
    return int(get_sim_time(TIME_UNIT))


class Log:
    def __init__(self, scenario: str) -> None:
        self.scenario = scenario
        self.lines: list[dict[str, Any]] = []

    def probe(self, task: str) -> Probe:
        return Probe(self, task)

    def save(self) -> None:
        out = Path(os.environ["CONFORMANCE_OUT"]) / f"{self.scenario}.jsonl"
        with out.open("w") as f:
            for line in self.lines:
                f.write(json.dumps(line, separators=(",", ":")) + "\n")


class Probe:
    """One task's view: the label of the trigger that last woke it, and its phase."""

    def __init__(self, log: Log, task: str) -> None:
        self.log = log
        self.task = task
        self.trigger = "start"
        self.phase = "start"

    async def wait(self, trigger: Any, label: str) -> Any:
        """Await `trigger` and remember `label` as the trigger for later lines."""
        result = await trigger
        self.woke(label, trigger)
        return result

    def woke(self, label: str, trigger: Any = None) -> None:
        self.trigger = label
        if isinstance(trigger, ReadOnly):
            self.phase = "readonly"
        elif isinstance(trigger, ReadWrite):
            self.phase = "readwrite"
        else:
            self.phase = "active"

    def _line(self, op: str, signal: str, value: Any) -> None:
        self.log.lines.append(
            {
                "time": now(),
                "phase": self.phase,
                "trigger": self.trigger,
                "task": self.task,
                "op": op,
                "signal": signal,
                "value": value,
            }
        )

    def read(self, dut: Any, *names: str) -> None:
        for name in names:
            handle = dut
            for part in name.split("."):
                handle = getattr(handle, part)
            self._line("read", name, int(handle.value))

    def write(self, dut: Any, name: str, value: int) -> None:
        getattr(dut, name).value = value
        self._line("write", name, value)

    def note(self, what: str, value: str) -> None:
        """A non-signal outcome, such as which trigger of a First fired."""
        self._line("note", what, value)

# counter

Up-counter with synchronous reset and enable; wraps at `2**WIDTH`.

- Top: `counter`. Sources: `counter.sv`.
- Parameters: `WIDTH` (default 8; linted at 8 and 33).
- Conformance: `tests/conformance/tb/test_counter.py` (`timer`, `clock_cycles`,
  `value_change`, `first_combine`).

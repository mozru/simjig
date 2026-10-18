# s3c6-fix notes

## Generated wrapper and `timeunit` (Verilator 5.052, checked by hand)
A wrapper module `wrap` around an interface-ported `dut`, `--lint-only`:

| user units          | wrapper                 | result                              |
|---------------------|-------------------------|-------------------------------------|
| all `timeunit 1ns`  | none                    | `TIMESCALEMOD` on the wrapper, stop |
| all `timeunit 1ns`  | `timeunit 1s; 1s`       | passes                              |
| none                | `timeunit 1s; 1s`       | `TIMESCALEMOD` on the user's DUT    |
| none                | none                    | passes                              |
| none + `--timescale`| `timeunit 1s; 1s`       | passes                              |
| all `timeunit 1ns`  | `lint_off TIMESCALEMOD` | passes; JSON: wrapper unit `1ns`, precision `1ps` |
| mixed (user's)      | `lint_off TIMESCALEMOD` | `TIMESCALEMOD` on the user's unit   |

So a fixed `timeunit` in the wrapper breaks users without one, and emitting one only
when the user's units have one would need the per-module unit in the IR (a schema
change). The waiver (`// verilator lint_off TIMESCALEMOD` before the module, `lint_on`
after) gives the wrapper the unit of the highest timed module
(`V3LinkLevel::timescaling`) and no precision, and stays local to the wrapper's file.

## `Dut::build` on `Err`
Reruns come from the configuration, not from what Verilator read: the failure may come
before any Verilator run. `-v`/`-y` library paths inside `verilator_args` are not tracked
(only `-f`/`-F` option files, nested ones included).

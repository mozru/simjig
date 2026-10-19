# simjig

Instructions for coding agents. The project's rules live in `CONTRIBUTING.md` (the
constitution, the tools and the gate) and `ARCHITECTURE.md` (the code map and the single
home of each concept); follow both.

- `cargo xtask ci` (through `pixi run`) is the full gate, the same one CI runs. It must
  pass before you hand off.
- `cargo nextest run -p <crate>` while iterating.

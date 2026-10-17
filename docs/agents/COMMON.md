# Rules for implementation agents

You are one of several agents building simjig in parallel. The director (the main
session) merges branches. Follow these rules exactly.

## Read first
1. `CLAUDE.md`: the constitution. It is binding.
2. `ARCHITECTURE.md` and `docs/architecture/DECISION.md`: the design. It is binding unless
   you find a fact that contradicts it. If you do, record it in your notes and report it;
   don't silently deviate.
3. `~/.claude/skills/agentic-rust/SKILL.md` (and its `reference/` files when relevant):
   Rust rules.
4. Your task brief, in the prompt that launched you.

## Workspace
- Work **only** in your worktree, whose path is given in your brief, on its branch. Never
  touch `~/repos/simjig` (main) or another agent's worktree.
- Environment, once per shell:
  `export PATH="$HOME/.pixi/bin:$HOME/.cargo/bin:$PATH" PIXI_NO_TELEMETRY=1`.
  Then run `pixi install --locked` once in your worktree.
- Run every cargo or Verilator command through pixi: `pixi run cargo ...`,
  `pixi run verilator ...`. Verilator 5.052 is pinned. Its include directory is
  `$CONDA_PREFIX/share/verilator/include` inside `pixi run`.
- Verilator's source (5.053-devel) is available to read, never to modify, at
  `/private/tmp/claude-501/-Users-moshe-Library-Application-Support-Claude-scratch-workspaces-bd0ffb49-bc90-4a67-91d7-5b22f6cdcfaf-7e63edda-7ad4-40b0-9d3b-99091b22a94a-scratch-2026-10-17-f60ab0/0d0b5713-fb3c-4fc3-93bc-72e42bd6bd2d/scratchpad/review-verilator/src/verilator`.
  Confirm behaviour against the installed 5.052.
- Paths must not contain spaces. Keep build outputs in your worktree's `target/`.

## Ownership
- Edit only the files and directories your brief assigns to you.
- **Shared files** (the root `Cargo.toml`, `Cargo.lock`, a crate's `lib.rs` module list,
  `pixi.toml`, `pixi.lock`): keep each change minimal and additive. The director resolves
  conflicts.
- Add dependencies with `cargo add` (as workspace dependencies), and check each API
  against the locked version. Prefer the standard library.
- Write your design notes, measurements and surprises in `docs/notes/<your-name>.md`.

## Done means
- `pixi run cargo xtask ci` passes in your worktree.
- Commits are small and logical, and every commit builds. **No AI attribution** in commit
  messages (no `Co-Authored-By: Claude` trailer, no "Generated with" line).
  Commit to your branch only. Never push, rebase main or merge.
- Constitution rule 1: every behaviour has a test that would fail without it.
- **Final reply** (≤ 400 words):
  - what is done;
  - the exact commands you ran and their results;
  - what you did *not* verify;
  - the public API that other agents will integrate against;
  - open issues or contradictions with the design.

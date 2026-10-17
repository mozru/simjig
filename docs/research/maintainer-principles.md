# Maintainer principles -> a constitution for `simjig`

Research date: 2026-10-17. Purpose: pick 4-5 "constitution" rules for `simjig`'s CLAUDE.md. simjig is a Rust framework in which Rust testbenches drive Verilator RTL models. It has C++ FFI, codegen from Verilator JSON, a custom async executor and a verification library. AI agents do most of the development and one maintainer supervises.

**Scope.** This covers software-side maintainers only: systems, low-level and Rust. Hardware-community maintainers were dropped at the user's request.

**Sourcing.** Everything is paraphrased, and no quote exceeds 15 words. Sources marked (P) are primary: the maintainer's own essay, talk or doc, or the project's docs. Sources marked (S) are secondary and were used only where the primary text could not be fetched.

---

## Part 1 - Principles by person and project

### Linus Torvalds / Linux kernel
1. **No regressions ("we don't break userspace").** If a change breaks what users relied on, it is a regression. It gets fixed or reverted, and users are never told to adapt. A bug fix that breaks users is still a regression, and a revert is a legitimate first response. (P) https://docs.kernel.org/process/handling-regressions.html
2. **One logical change per patch, and every patch is bisectable.** Explain the problem before the solution. Every patch in a series must build and run. `Fixes:` names the culprit commit. (P) https://docs.kernel.org/process/submitting-patches.html
3. **"Good taste" means restructuring so special cases vanish.** His linked-list example in the TED talk shows this. He also says to worry about data structures more than code. (P) https://www.ted.com/talks/linus_torvalds_the_mind_behind_linux ; https://lwn.net/Articles/193245/
4. **AI policy.** Only a human may add `Signed-off-by` and certify the DCO. The human reviews the code and takes full responsibility. AI help is disclosed via `Assisted-by:`. The agent bug-fix procedure:
   - read the full process docs
   - build a reproducer, and stop if the bug is not real
   - build and verify the fix, with no new warnings
   - state explicitly anything that could not be built or tested
   - never send anything itself

   (P) https://docs.kernel.org/next/process/coding-assistants.html

### Richard Hipp / SQLite
1. **Test rigor far beyond the code.** Tests are about 590 times the library's size. There are four independent harnesses, 100% branch and MC/DC coverage, and continuous fuzzing. (P) https://www.sqlite.org/testing.html
2. **No fix without a regression test.** A bug counts as fixed only once a test reproduces it. (P) same page.
3. **Dense assertions.** About 6,750 `assert()`s cover pre- and postconditions and invariants. `ALWAYS()` and `NEVER()` mark defensive branches. (P) same page.
4. **Long-term support and conservatism.** SQLite commits to support through 2050 with a backward-compatible API and file format. More than 35% of the source is comments. The team avoids fashion and keeps the library self-contained. (P) https://www.sqlite.org/lts.html ; https://www.sqlite.org/selfcontained.html

### Salvatore Sanfilippo (antirez) / Redis
1. **Fight complexity.** Add complexity only when it clearly pays off, and get maximum user benefit for minimal code ("opportunistic programming"). The codebase should stay understandable by one dedicated programmer. (P) Redis Manifesto, https://github.com/redis/redis/blob/unstable/MANIFESTO
2. **Prefer self-contained components to large external dependencies** (Manifesto, "code as craft"). (P) same.
3. **Comments carry the "why".** He describes nine comment types. Design, why, teacher, checklist and guide comments are good. Trivial comments, TODO-debt and commented-out "backup" code are bad. (P) http://antirez.com/news/124
4. **LLMs amplify, they don't autopilot.** Do not vibe-code nontrivial systems. Feed rich context, including goals, invariants and approaches to avoid. Humans own the design decisions. (P) https://antirez.com/news/154

### Rob Pike / Go proverbs; Ken Thompson
1. **Clear is better than clever.** A little copying is better than a little dependency. (P) https://go-proverbs.github.io/
2. **The FFI and unsafe boundary is a different language with no guarantees.** The proverbs say "Cgo is not Go" and that `unsafe` gives no guarantees. (P) same.
3. **Measure before optimizing, and data dominates.** These are Pike's "Rules of Programming". (P) https://www.lysator.liu.se/c/pikestyle.html
4. **Thompson: when in doubt, use brute force.** Deleting code is productive work. (S, via ESR, *Art of Unix Programming*) http://www.catb.org/esr/writings/taoup/html/ch01s06.html

### Daniel Stenberg / curl
1. **API and ABI stability.** libcurl has been API- and ABI-compatible since 2006. Upgrades must not change documented behavior. (P) https://curl.se/libcurl/abi.html ; https://daniel.haxx.se/blog/?p=25714
2. **Small, focused commits** with explanatory messages. Big mixed patches are discouraged as hard to review and bisect. (P) https://curl.se/dev/contribute.html
3. **Tests, or a detailed account of how you verified.** Every new feature needs at least one test case. (P) same page.
4. **AI policy.** Disclose AI use and verify findings yourself. Never paste AI-written reports. A fabricated security report gets an instant ban. AI-assisted code must meet the normal bar, and if reviewers can tell AI wrote it, it needs more work. (P) https://curl.se/dev/contribute.html ; https://daniel.haxx.se/blog/2025/07/14/death-by-a-thousand-slops/ ; https://daniel.haxx.se/blog/2024/01/02/the-i-in-llm-stands-for-intelligence/

### Mitchell Hashimoto / HashiCorp, Ghostty
1. **Tao of HashiCorp.**
   - simple, modular, composable pieces
   - codify processes as versioned, executable code
   - immutability and versioning so changes can be rolled back
   - pragmatism over dogma

   (P) https://www.hashicorp.com/en/tao-of-hashicorp
2. **Harness engineering for agents.** When an agent errs, encode the fix in AGENTS.md or in a tool so the mistake does not recur. Give agents a way to verify their own work. Use small tasks, and keep planning separate from execution. (P) https://mitchellh.com/writing/my-ai-adoption-journey
3. **Discuss before coding.** PRs implement an accepted issue, and design is not debated in PRs. New contributors must be vouched for. (P) https://github.com/ghostty-org/ghostty/blob/main/CONTRIBUTING.md
4. **Ghostty AI policy.** Disclose the tool and how much it did. You must be able to explain the change and its system impact without AI. AI prose is human-edited and trimmed. No AI media. Low-quality AI submitters are publicly denounced. Maintainers are exempt. (P) https://github.com/ghostty-org/ghostty/blob/main/AI_POLICY.md

### Andrew Kelley / Zig
1. **The Zen of Zig.**
   - Communicate intent precisely.
   - Edge cases matter.
   - Favor reading code over writing it.
   - Compile errors beat runtime crashes, and runtime crashes beat bugs.
   - Reduce what one must remember.

   (P) `zig zen`; https://ziglang.org/documentation/master/#Zen (introduced 2017, commit f248ef5)
2. **Incremental improvements, and avoid local maximums.** (P) same.

### Andrew Gallant (BurntSushi) / ripgrep, regex
1. **A panic is a bug.** `unwrap`/`expect` are fine where a panic would reveal a broken internal assumption. Errors that are expected in normal use are returned as values. (P) https://burntsushi.net/unwrap/
2. **Documented guarantees drive design.** regex guarantees linear time, so it deliberately omits lookaround and backreferences, and the FAQ explains why. (P) https://github.com/BurntSushi/ripgrep/blob/master/FAQ.md
3. **Ongoing maintenance cost is reason enough to say no.** (P) https://burntsushi.net/foss/

### David Tolnay / serde, syn, anyhow, thiserror, trybuild
1. **Stability as a feature.** serde has stayed at 1.x since 2017. (P) https://github.com/serde-rs/serde
2. **Generated output and diagnostics are API and get snapshot tests.** trybuild's expected `.stderr` files are generated, then reviewed, never hand-written. Test only the diagnostics you care about. (P) https://github.com/dtolnay/trybuild
3. **Libraries expose typed errors and applications use opaque ones** (thiserror vs anyhow). (P) https://github.com/dtolnay/thiserror ; https://github.com/dtolnay/anyhow

### Aleksey Kladov (matklad) / rust-analyzer, TigerBeetle
1. **ARCHITECTURE.md.** Keep a short codemap, plus architectural invariants (often phrased as an absence, such as "X never depends on Y") and boundaries. Keep only stable facts. (P) https://matklad.github.io/2021/02/06/ARCHITECTURE.md.html
2. **Test features, not code.** Test at boundaries so tests survive refactors. Route calls through a single `check` helper. Use data-driven and expect (snapshot) tests. Keep tests fast and deterministic. Unawaitable background concurrency is untestable, so prefer structured concurrency. (P) https://matklad.github.io/2021/05/31/how-to-test.html
3. **The "not rocket science rule".** Main always passes, and a bot tests the merge result before main advances. The rule is Graydon Hoare's. (P) https://graydon2.dreamwidth.org/1597.html
4. **Constant-size build and CI surface.** Adding a test should not touch CI config. (P) https://matklad.github.io/2023/12/31/O(1)-build-file.html

### Joran Dirk Greef / TigerBeetle (TigerStyle)
1. **Ordered design goals: safety, then performance, then developer experience.** Style choices are justified against these. (P) https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md
2. **Assertions are a force multiplier.** Use at least 2 per function on average. Assert arguments, returns and invariants, and assert both the positive and negative space. Pair assertions across paths, such as before a write and after a read. Crash on corruption rather than continuing. (P) same.
3. **Put a limit on everything.** Every loop and queue gets a bound, and memory is statically allocated at startup. Recursion is avoided and control flow kept simple. Functions stay within 70 lines. (P) same.
4. **Zero technical debt.** Fix problems at design time. Handle every error and test error paths. Sketch performance first, covering network, disk, memory and CPU. Zero dependencies. (P) same.

### Carl Lerche & the tokio team
1. **Concurrency is tested exhaustively, not by luck.** loom permutes thread interleavings under the C11 memory model, with stated gaps such as SeqCst being modeled as AcqRel. (P) https://github.com/tokio-rs/loom
2. **Explicit stability contract.** Semver, LTS releases with at least a year of backports, an MSRV policy, and patch releases that do not substantially change runtime behavior. (P) https://github.com/tokio-rs/tokio/blob/master/CONTRIBUTING.md
3. **Comment on an issue before starting work on it.** (P) https://github.com/tokio-rs/tokio/blob/master/docs/contributing/README.md

### Niko Matsakis / Rust language design
1. **Ordered design axioms resolve tensions.** Reliability comes first. Then performant composable abstractions, low-level control and transparency, extensibility, and accessibility. When axioms conflict, the earlier one wins. (P) https://nikomatsakis.github.io/rust-design-axioms/
2. **Design for the long term,** and do the right thing even when it is hard (rules of thumb). (P) same.
3. **"If it compiles, it works" as an aspiration.** Encode guarantees in types. (P) https://github.com/nikomatsakis/rustacean-principles (deprecated in favor of the axioms)

### Jon Gjengset
1. **Interfaces should be unsurprising, flexible, obvious and constrained.** Rust for Rustaceans, ch. 3 "Designing Interfaces": follow conventions, make misuse hard through types, and keep the commitments a public type makes small and deliberate. (P) https://rust-for-rustaceans.com/
2. **Testing beyond unit tests.** The book covers test harnesses, doctests, fuzzing, property testing and Miri for unsafe code. (P) same, ch. 6.

### Bryan Cantrill / Oxide
1. **Rigor means correctness above all.** Fix root causes, not symptoms. Candor and transparency mean saying plainly what you know and don't know. (P) https://oxide.computer/principles
2. **Write it down before building it (the RFD process).** Significant design, API, process and testing decisions get a written doc covering the options, the trade-offs, the determination and the reasoning, so future readers know why. (P) https://rfd.shared.oxide.computer/rfd/0001
3. **Values are in tension, and a project must pick its core set explicitly.** Talk: "Platform as a Reflection of Values". (P) https://speakerdeck.com/bcantrill/platform-as-reflection-of-values-joyent-node-dot-js-and-beyond

### Steve Klabnik
1. **The most important thing with LLMs.** Give the model a fast, objective way to tell whether it is right, with failure output that says where and why, as rustc does, and quiet success output. (P) https://steveklabnik.com/writing/the-most-important-thing-when-working-with-llms/
2. **Small, verifiable steps.** Plan before executing larger tasks. Treat context compaction as a sign the task was too big. Discard and retry bad output rather than patching it. (P) https://steveklabnik.com/writing/agentic-development-basics/

### Casey Muratori (actionable point only)
1. **Semantic compression.** Write the concrete code first, and extract an abstraction only once the pattern has appeared at least twice. Make code usable before making it reusable. (P) https://caseymuratori.com/blog_0015

Jonathan Blow was considered and skipped. His views, such as dislike of dependencies and of big-abstraction culture, are already covered more actionably by antirez, Pike, TigerStyle and Muratori.

### Also cited
- **Rich Hickey.** Simple (not intertwined) is not the same as easy (near at hand); see https://www.infoq.com/presentations/Simple-Made-Easy/. Grow by accretion, never breakage; see "Spec-ulation", https://www.youtube.com/watch?v=oyLBGkS5ICk.
- **Kent Beck.** Make it work, make it right, make it fast (https://wiki.c2.com/?MakeItWorkMakeItRightMakeItFast). Tidy First: separate structural changes from behavioral changes, and make the change easy, then make the easy change (https://tidyfirst.substack.com/).
- **John Ousterhout.** Deep modules over shallow layers. Define errors out of existence. Strategic over tactical programming. *A Philosophy of Software Design*, https://web.stanford.edu/~ouster/cgi-bin/book.php.

### AI-contribution policies: summary
- **Permissive with accountability:** Linux, curl and Ghostty.
  - A named human is accountable and must be able to explain every line.
  - AI use is disclosed, via an `Assisted-by` trailer or a note in the PR.
  - Claims are verified with a reproducer, a build and tests, and whatever is unverified is stated.
  - The quality bar is the same or higher. Fabrications get bans or denouncement.
- **Bans:**
  - Gentoo (Council, April 2024) bans AI-generated contributions on copyright, quality and ethics grounds. (S) https://lwn.net/Articles/970072/
  - NetBSD (commit guidelines, May 2024) presumes LLM code tainted, and committing it needs written core approval. (P) https://www.netbsd.org/developers/commit-guidelines.html ; (S) https://www.theregister.com/2024/05/18/distros_ai_code/
- **Practitioner guidance:** Hashimoto, Klabnik and antirez agree on four points:
  - verification loops the agent can run itself
  - small tasks
  - plan before execution
  - the human owns the design

---

## Part 2 - Candidate constitution rules (ranked)

Selection criteria:
- (a) The rule recurs across at least 3 maintainers.
- (b) It fits an agent-heavy Rust/FFI/async project.
- (c) It is checkable in review or CI.
- (d) It is not already enforced by rustfmt, clippy or CI gating.

Items marked [TOP 5] are the recommendation.

### 1. [TOP 5] Prove it with a failing test
**Rule:** Every behavior change or bug fix adds a test that fails without the change and passes with it, at the public or RTL-observable boundary.
- **Sources:** SQLite (no fix without a regression test), curl (tests, or a detailed verification note), kernel AI doc (reproducer first), matklad (test features, not code), Tolnay (trybuild snapshots), TigerStyle (test the error paths too).
- **Why for simjig:** Agents produce plausible code quickly. Red-to-green is the cheapest proof that a change does what it claims. Testing at the boundary means a Verilated model, the executor's public API, or generated bindings, and such tests survive the refactors agents do often. Mock-only tests can pass while the FFI or codegen is wrong.
- **How to check:**
  - The PR adds or changes a test.
  - The PR body pastes the failing output from before the fix.
  - The reviewer can revert the non-test hunks and watch the test fail.
  - CI can warn on `fix:` commits that touch no test file.

### 2. [TOP 5] Report evidence, not confidence
**Rule:** Claim only what you ran: list the exact commands and results, and state explicitly what was not built, tested, or verified.
- **Sources:** kernel coding-assistants doc (state what could not be done), curl (verify findings yourself, explain how you tested), Ghostty (explain the change without AI), Cantrill/Oxide (candor and rigor), Klabnik and Hashimoto (objective verification loops).
- **Why for simjig:** One supervisor has to trust or distrust each agent's "done". A false completion claim is the typical agent failure. Verilator builds, C++ toolchains and long simulations are easy to skip silently.
- **How to check:**
  - The PR or commit body has a `Verified:` section listing commands and outcomes, and a `Not verified:` section.
  - The reviewer re-runs one claimed command.
  - Any false claim blocks the PR.

### 3. [TOP 5] One logical change per commit, every commit green
**Rule:** Keep each commit to one logical change, separate refactors from behavior changes, and make every commit build and pass tests.
- **Sources:** Linux (separate logical changes, bisectability), curl (small focused commits), Beck (Tidy First), Graydon/matklad (main always green), Klabnik (small verifiable steps).
- **Why for simjig:** Agents bundle a feature, an opportunistic rename and a reformat into one diff. A pure-refactor commit with untouched tests and snapshots reviews in seconds. Bisectability matters when a regression shows up only in a slow RTL regression run.
- **How to check:**
  - Read the commit list in review. Each message names one change, and structural commits are prefixed `refactor:`.
  - Refactor commits do not change tests or snapshots.
  - Pre-merge, run `git rebase --exec 'cargo test --workspace' origin/main` so every commit is tested.

### 4. [TOP 5] Never break users silently
**Rule:** Do not change public API, generated-code shape, or observable simulation semantics without an explicit decision, a reviewed snapshot or semver diff, and a CHANGELOG entry.
- **Sources:** Torvalds (no regressions), Stenberg (ABI stable since 2006), Hipp (compatible through 2050), tokio (semver, LTS, no behavior change in patch releases), Tolnay (serde 1.x; reviewed snapshots), Matsakis (design for the long term), Hickey (accretion, not breakage).
- **Why for simjig:** Users' testbenches depend on several things:
  - the shape of the generated bindings (how Verilator JSON maps to Rust)
  - the executor's scheduling semantics: ordering within a time step, when writes become visible, wake order
  - the verification-library APIs

  A silent semantic change can make passing testbenches wrong without a compile error, which is worse than a crash. Agents do not feel downstream breakage.
- **How to check:**
  - insta/expect-test snapshots of codegen for fixture JSON. Diffs must be reviewed and accepted, never auto-blessed by the agent.
  - `cargo semver-checks` on published crates.
  - Golden event-order traces for the executor.
  - A snapshot, semver or trace diff without a CHANGELOG line is blocked.

### 5. [TOP 5] Justify complexity in writing before it lands
**Rule:** New dependencies, public abstractions, unsafe/FFI surface, or executor-semantics changes need a written rationale naming the simpler alternatives rejected, approved before merge.
- **Sources:**
  - antirez (fight complexity, self-contained code)
  - Pike ("a little copying..."; cgo and unsafe are special)
  - Hipp and TigerStyle (zero or minimal dependencies)
  - Ousterhout (deep modules)
  - Muratori (abstract after two uses)
  - BurntSushi (maintenance cost justifies no)
  - Cantrill (RFDs record options and why)
  - Ghostty and tokio (discuss before coding)
- **Why for simjig:** Agents add crates and wrapper layers by default, and each adds ongoing cost. The C++/Verilator FFI boundary is simjig's highest-risk surface, because memory bugs there show up as heisenbug simulation mismatches. The executor is the semantic core. Growth in these areas should be deliberate and small, behind deep, safe interfaces, with the reasoning kept for future agents to read.
- **How to check:**
  - CI flags any diff that adds `[dependencies]` entries, new `pub` traits, modules or crates, a higher count of `unsafe`/`extern`/cxx-bridge items, or changes under the executor core.
  - Each flagged diff must link a short design note (`docs/design/NNN-*.md`, RFD-style: options, decision, why) or include a "Why not simpler" section in the PR.
  - Per-block `// SAFETY:` comments are left to clippy (`undocumented_unsafe_blocks`).

### 6. Ordered priorities as the tie-breaker
**Rule:** When goals conflict, choose correctness, then determinism and debuggability, then performance, then ergonomics, and state the trade-off in the PR.
- **Sources:** TigerStyle (safety > performance > DX), Matsakis (ordered axioms, reliability first), Zig Zen (bugs are worse than crashes), Cantrill (pick core values explicitly), Beck (work, right, fast).
- **Why not top 5:** It works best as a one-line preamble to the constitution rather than a rule, because it is a lens for judging trade-offs. Checks are weaker: a reviewer asks whether the PR named the trade-off. It is strongly recommended as the preamble.

### 7. Assert invariants; a panic means a bug
**Rule:** Return typed errors for anything a user or RTL model can cause. Assert internal invariants densely, especially across FFI and executor state, and panic only on violated invariants.
- **Sources:** TigerStyle (at least 2 assertions per function; crash on corruption), SQLite (about 6,750 asserts), BurntSushi (a panic is a bug), Tolnay (thiserror for libraries), Zig (crashes are better than bugs).
- **Why not top 5:** It is partly lintable (`clippy::unwrap_used`/`expect_used` in library crates). Assertion density is a style guideline. It fits the Rust-style section, with a specific note on asserting FFI handle validity and executor state transitions.

### 8. Deterministic, reproducible tests; flaky means bug
**Rule:** Randomized and concurrent tests log and accept their seed and are deterministic. Never mask flakiness with retries or sleeps, and fix the root cause.
- **Sources:** matklad (fast, deterministic tests; structured concurrency), tokio/loom (exhaustive interleavings), SQLite (replayable fuzz cases), TigerBeetle (deterministic simulation), Cantrill (root causes).
- **Why for simjig:** This is directly relevant to a custom async executor and constrained-random verification. It is ranked below the top 5 only for scope. Candidate home: the testing section, or fold it into rule 1 as a clause. Consider loom for any lock-free executor internals.
- **How to check:**
  - grep for unseeded RNG constructors and for `sleep`/retry in tests.
  - CI runs the suite twice with the same seed and diffs the output.
  - No `#[ignore]` for flakiness.

### 9. Human accountability and disclosure
**Rule:** Agents propose and the maintainer merges. Merge only code the maintainer can explain, and mark agent-authored commits with an `Assisted-by:` trailer.
- **Sources:** Linux, Ghostty, curl; NetBSD and Gentoo as the strict end.
- **Why not top 5:** In simjig nearly every commit is agent-assisted and there is a single maintainer, so this is enforced by branch protection and a commit-msg hook (tooling) rather than conduct. Put it in the process section.

### 10. Record invariants and the "why" where they live
**Rule:** When adding or moving a module boundary or invariant, update ARCHITECTURE.md. Comments explain why, never restate what, and commented-out code is forbidden.
- **Sources:** matklad (ARCHITECTURE.md), antirez (comment taxonomy), Pike ("document the details"), Hipp (35% comments), Hashimoto (encode lessons in AGENTS.md).
- **Why not top 5:** It is valuable for agent context but harder to gate, and partly overlaps rule 5's design notes. Check: a PR adding a crate, a top-level module or a `pub` boundary must touch ARCHITECTURE.md.

(Considered and folded into rules above or left as reviewer heuristics: Torvalds' "eliminate special cases", Ousterhout's "define errors out of existence", and Gjengset's "make misuse hard through types". These are good taste, but not objectively checkable.)

---

## Part 3 - Why these five form a coherent constitution

The five rules cover one change's lifecycle, each at a different step, so they do not overlap:

| Stage | Rule | Review question |
|---|---|---|
| Before writing | #5 Justify complexity in writing | Should this exist, and is it the simplest shape? |
| Slicing | #3 One logical change, every commit green | Can I review and bisect this? |
| Proving | #1 Failing test at the boundary | Does it do what it claims? |
| Reporting | #2 Evidence, not confidence | Can I trust the agent's "done"? |
| Protecting users | #4 Never break users silently | Did any API, codegen or semantics change shape or meaning? |

- **Each rule is a consensus, not one person's taste.** Every rule appears in at least five independent traditions:
  - kernel, curl, SQLite and tokio on tests, small commits and stability
  - antirez, Pike, TigerStyle, Ousterhout and Cantrill on simplicity and written design
  - the permissive AI policies and Klabnik/Hashimoto on evidence and verification loops
- **They target agent failure modes:**
  - bundled diffs (#3)
  - plausible but untested code (#1)
  - false "done" claims (#2)
  - dependency and abstraction sprawl, and unsafe growth (#5)
  - indifference to downstream breakage (#4)
- **They complement automation instead of duplicating it.** rustfmt, clippy (including `undocumented_unsafe_blocks` and `unwrap_used`) and CI gating already cover formatting, lint hygiene and "tests pass". The constitution covers what tools cannot judge alone, yet each rule still leaves a concrete artifact, so review is a yes/no check:
  - a red-before test output
  - a `Verified:` / `Not verified:` block
  - a clean commit list
  - a reviewed snapshot or semver diff with a CHANGELOG line
  - a "why not simpler" design note
- **They map onto simjig's risk surfaces:**
  - codegen and executor semantics (#4)
  - the C++/Verilator FFI and the executor core (#5)
  - slow RTL regressions, where bisectability matters (#3)
  - verification-library correctness, where a wrong-but-passing testbench is the worst outcome (#1, #2)

**Suggested CLAUDE.md text.** Optional preamble, from rule 6: *When goals conflict: correctness, then determinism and debuggability, then performance, then ergonomics.*

1. Every behavior change or fix adds a test that fails without it and passes with it, at the public or RTL-observable boundary.
2. Claim only what you ran: list commands and results, and state explicitly what was not built, tested, or verified.
3. One logical change per commit; separate refactors from behavior changes; every commit builds and passes tests.
4. Never change public API, generated code, or simulation semantics silently: require a reviewed snapshot or semver diff plus a CHANGELOG entry.
5. New dependencies, public abstractions, unsafe/FFI surface, or executor-semantics changes need a written "why not simpler" rationale approved before merge.

**Runner-ups and where they go:**
- #8 (seeded, deterministic tests; loom) goes in testing.
- #7 (asserts; a panic is a bug) goes in Rust style.
- #9 (`Assisted-by`, branch protection) goes in process.
- #10 (ARCHITECTURE.md) goes in docs.

---

## Source caveats
- The Ghostty AI_POLICY.md and CONTRIBUTING.md were fetched from main. The policy changed in Aug 2025 and again in Jan 2026.
- The Linux `coding-assistants` doc differs by version. "next" uses `Assisted-by: LLM [TOOL...]`, and release docs use `AGENT_NAME:MODEL_VERSION`.
- lkml.org (Torvalds' 2012 "WE DO NOT BREAK USERSPACE" mail) was behind bot protection, so the kernel's `handling-regressions.rst` is cited instead. It quotes Linus at length.
- The Gentoo and NetBSD positions are from 2024 and were not re-checked.
- The Thompson attribution is via ESR (secondary), and Beck's "work/right/fast" is via the c2 wiki.
- The Gjengset points are from his book's chapter structure (the book is not freely fetchable). Cantrill's talk content is from the slide listing, not a transcript.
- The tokio testing and commit specifics live in `docs/contributing/pull-requests.md`, which was not fetched.

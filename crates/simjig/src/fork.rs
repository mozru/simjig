//! `fork()` checkpoints: run several branches of a test from one simulation state.
//!
//! [`run_from_checkpoint`] is called from the test's main task at a quiet point. It forks
//! the test process once per branch, one branch at a time. Each child is an exact copy of
//! the checkpoint: the models' memory, the kernel's time, clocks and write log, and the
//! testbench's own variables. The child runs the branch body with its own [`Branch`] (index
//! and seed), sends the result to the parent through a socket, and exits. The parent
//! waits for the child, records its result and its cost, and forks the next branch. It is
//! never touched by a branch, so the test continues from the checkpoint once every branch
//! has reported; [`Branches`] holds the results.
//!
//! ```ignore
//! #[simjig::test]
//! async fn after_reset(ctx: &TestCtx, dut: counter::Dut) {
//!     reset(dut).await;                       // the checkpoint
//!     let branches = fork::run_from_checkpoint(ctx.seed(), 8, |branch| async move {
//!         let mut rng = branch.rng("stim");
//!         drive(dut, &mut rng).await?;        // different stimulus per branch
//!         Ok(dut.count.get())
//!     })
//!     .await
//!     .unwrap();
//!     let counts = branches.into_values();    // fails the test if any branch failed
//! }
//! ```
//!
//! # Rules, and how they are enforced
//! (docs/notes/spikes.md §1 has the experiments behind each.)
//! - **Unix only.** Elsewhere every call returns [`ForkError::Unsupported`].
//! - **At most two threads.** A forked child keeps only the calling thread; a lock another
//!   thread held at the fork stays locked forever in the child (the spike's child hung in
//!   `localtime`). Under `cargo nextest` a test process has exactly two threads, libtest's
//!   main thread (blocked waiting for the test) and the test's. Under multi-threaded
//!   `cargo test` there are more, and the call returns [`ForkError::Threads`], naming
//!   nextest. The same check catches a Verilator thread pool: simjig's runtime builds every
//!   `VerilatedContext` with `threads(1)`, and a model built otherwise would show up here.
//! - **A quiet point.** The caller is the main task, outside every `join!`, `first!` and
//!   `with_timeout` ([`ForkError::InCombinator`]: their operands are helpers polled inside
//!   `main`, not tasks), and no other task is unfinished ([`ForkError::OtherTasks`]), so a
//!   branch is exactly "the checkpoint, then this body".
//! - **The child never returns into the harness.** It ends with `_exit` after writing its
//!   report: on success from inside the branch's poll, and on any failure (a panic, a
//!   `TimeLimit`, a model `$fatal`, ...) from the `Drop` of a sentinel task, which the
//!   kernel runs when it ends the child's run. No destructor, atexit handler or
//!   copied stdio buffer runs in the child, so nothing outside memory may rely on `Drop`
//!   there.
//! - **Stdio is flushed before each fork** (Rust's and C's), so buffered output is not
//!   printed twice.
//! - **Open files** that a child must not share with its parent (a trace writer shares its
//!   descriptor and file offset with the parent) are detached by a [`ForkHook`]: flushed
//!   before the fork, and pointed at their own file in the child (for a Verilator FST, the
//!   ABI's `trace_reopen`). A child finishes (`at_child_exit`) only the hooks whose child
//!   half succeeded; when one fails, the later ones never run, and every resource not yet
//!   detached is left exactly as inherited.
//!
//! # Cost
//! Each [`BranchOutcome`] carries its [`BranchCost`]: the parent's `fork()` latency, the
//! wall time until the child was reaped, the child's CPU time and its minor page faults
//! (which count the copy-on-write copies). The spike measured 0.36 ms per fork on macOS,
//! independent of the model's size, plus about 0.1 ms per MiB the branch writes.
//!
//! # Limits
//! Unix only, nextest only (one test per process), branches run one at a time, and a
//! branch cannot hand a [`Failure`] back as data: the parent sees a [`RemoteFailure`]
//! (kind, time, task, location, message and the full text). Checkpoints cannot be nested:
//! inside a branch, `run_from_checkpoint` returns [`ForkError::Nested`] (fork a branch's
//! variations from the test's own checkpoint instead, with more branches).

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "fork, _exit, wait4 and proc_pidinfo are libc calls (ARCHITECTURE: unsafe lives in simjig::fork)"
)]
mod sys;
mod wire;

use std::cell::{Cell, RefCell};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

pub use wire::BranchValue;

use crate::kernel::state::{self, Phase};
use crate::{Failure, Precision, Rng, Seed, SimTime};

/// One branch of a checkpoint: which one, and its seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Branch {
    index: usize,
    count: usize,
    seed: Seed,
}

impl Branch {
    fn new(root: Seed, index: usize, count: usize) -> Self {
        let value = root.rng(&format!("fork.branch.{index}")).next_u64();
        Self {
            index,
            count,
            seed: Seed::new(value, root.source()),
        }
    }

    /// The branch's index, from 0.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// How many branches the checkpoint runs.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The branch's seed, derived from the root seed and the index (so the root seed in a
    /// failure report reproduces every branch).
    #[must_use]
    pub fn seed(&self) -> Seed {
        self.seed
    }

    /// The random stream `path` of this branch.
    #[must_use]
    pub fn rng(&self, path: &str) -> Rng {
        self.seed.rng(path)
    }
}

impl fmt::Display for Branch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fork branch {} of {} (seed {})",
            self.index, self.count, self.seed
        )
    }
}

/// Why [`run_from_checkpoint`] did not fork.
///
/// Its `Debug` is its `Display`, so `.unwrap()` and `.expect(..)` print the advice.
#[derive(Clone, PartialEq, Eq)]
pub enum ForkError {
    /// Not a platform fork checkpoints work on.
    Unsupported {
        /// Why.
        reason: String,
    },
    /// Called outside a running simulation, or while it was ending.
    NoSimulation,
    /// Called inside a fork branch: checkpoints cannot be nested.
    Nested,
    /// Called from a task other than `main`.
    NotMainTask {
        /// The calling task.
        task: String,
    },
    /// Called inside an operand of a `join!`, `first!` or `with_timeout` of the main task:
    /// the other operands run beside the call, so the checkpoint is not a quiet point.
    InCombinator {
        /// How many operands the main task's unfinished combinators have, the caller's
        /// own included.
        operands: usize,
    },
    /// Other tasks are still running, so the checkpoint is not a quiet point.
    OtherTasks {
        /// Their hierarchical names.
        tasks: Vec<String>,
    },
    /// The process has more threads than a fork can survive.
    Threads {
        /// How many it has.
        count: usize,
    },
    /// A [`ForkHook`] failed to prepare for the fork.
    Hook {
        /// The hook's message.
        message: String,
    },
    /// A system call failed.
    System {
        /// What was being done.
        call: &'static str,
        /// The error.
        error: String,
    },
}

impl fmt::Display for ForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { reason } => {
                write!(f, "fork checkpoints are not supported: {reason}")
            }
            Self::NoSimulation => f.write_str(
                "run_from_checkpoint needs a running simulation; call it from the main task \
                 inside Sim::run or a #[simjig::test]",
            ),
            Self::Nested => f.write_str("checkpoints cannot be nested inside a fork branch"),
            Self::NotMainTask { task } => write!(
                f,
                "run_from_checkpoint was called from task `{task}`; call it from the main task"
            ),
            Self::InCombinator { operands } => write!(
                f,
                "run_from_checkpoint was called inside a `join!`, `first!` or `with_timeout` \
                 of the main task ({operands} combinator operands are unfinished), so the \
                 other operands would run on inside every branch, and one could end it; \
                 call it from the main task directly, outside every combinator"
            ),
            Self::OtherTasks { tasks } => write!(
                f,
                "run_from_checkpoint needs a quiet point, but these tasks are still running: \
                 {}; join or cancel them first",
                tasks.join(", ")
            ),
            Self::Threads { count } => write!(
                f,
                "the test process has {count} threads, and fork() is safe only with at most 2 \
                 (libtest's main thread and the test's): a lock another thread holds stays \
                 locked forever in the child. Run the test under `cargo nextest run` (one test \
                 per process), not multi-threaded `cargo test`, and start no threads of your own"
            ),
            Self::Hook { message } => write!(f, "a fork hook failed before the fork: {message}"),
            Self::System { call, error } => write!(f, "{call} failed: {error}"),
        }
    }
}

impl fmt::Debug for ForkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ForkError {}

/// A branch's failure as its child reported it: the fields of its [`Failure`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFailure {
    /// The [`FailureKind`](crate::FailureKind) name (`TaskPanic`, `ModelFatal`, ...).
    pub kind: String,
    /// Simulation time of the failure.
    pub time: SimTime,
    /// The task that caused it, if one did.
    pub task: Option<String>,
    /// The source location, if known.
    pub location: Option<String>,
    /// The one-line message.
    pub message: String,
    /// The failure's full `Display`.
    pub text: String,
}

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildExit {
    /// It exited with this code.
    Exited(i32),
    /// A signal killed it.
    Signaled(i32),
}

/// `exited with code <c>` or `killed by signal <s>`.
impl fmt::Display for ChildExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exited with code {code}"),
            Self::Signaled(signal) => write!(f, "killed by signal {signal}"),
        }
    }
}

/// Why a branch has no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchError {
    /// The branch's simulation failed, or its body returned an error.
    Failed(RemoteFailure),
    /// The child ended without a report (killed by a signal, or a crash of the model).
    Died(ChildExit),
}

/// A branch that failed, and which one it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchFailure {
    /// The branch.
    pub branch: Branch,
    /// What went wrong.
    pub error: BranchError,
}

impl fmt::Display for BranchFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.error {
            BranchError::Failed(failure) => write!(f, "{} failed: {}", self.branch, failure.text),
            BranchError::Died(exit) => {
                write!(f, "{} died without a report: {exit}", self.branch)
            }
        }
    }
}

impl std::error::Error for BranchFailure {}

/// What one branch cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BranchCost {
    /// Wall time of the `fork()` call, as the parent saw it.
    pub fork: Duration,
    /// Wall time from the fork until the parent had reaped the child.
    pub wall: Duration,
    /// The child's user and system CPU time.
    pub cpu: Duration,
    /// The child's minor page faults: each copy-on-write copy of a page is one.
    pub minor_faults: u64,
    /// The child's peak resident set, in bytes (pages shared with the parent included).
    pub max_rss_bytes: u64,
}

/// What `wait4` reports about a child.
#[derive(Clone, Copy, Debug)]
struct ChildUsage {
    minor_faults: u64,
    max_rss_bytes: u64,
    cpu: Duration,
}

/// One branch's result and cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchOutcome<T> {
    /// The branch.
    pub branch: Branch,
    /// Its value, or why it has none.
    pub result: Result<T, BranchError>,
    /// What it cost.
    pub cost: BranchCost,
}

/// The outcomes of every branch of a checkpoint, in branch order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branches<T> {
    outcomes: Vec<BranchOutcome<T>>,
}

impl<T> Branches<T> {
    /// Every branch's outcome, in branch order.
    #[must_use]
    pub fn outcomes(&self) -> &[BranchOutcome<T>] {
        &self.outcomes
    }

    /// The failed branches, in branch order.
    pub fn failures(&self) -> impl Iterator<Item = BranchFailure> + '_ {
        self.outcomes.iter().filter_map(|o| match &o.result {
            Ok(_) => None,
            Err(error) => Some(BranchFailure {
                branch: o.branch,
                error: error.clone(),
            }),
        })
    }

    /// The sum of every branch's cost (the `fork`, `wall` and `cpu` times, the faults), with
    /// the largest peak RSS.
    #[must_use]
    pub fn total_cost(&self) -> BranchCost {
        self.outcomes
            .iter()
            .fold(BranchCost::default(), |total, o| BranchCost {
                fork: total.fork + o.cost.fork,
                wall: total.wall + o.cost.wall,
                cpu: total.cpu + o.cost.cpu,
                minor_faults: total.minor_faults + o.cost.minor_faults,
                max_rss_bytes: total.max_rss_bytes.max(o.cost.max_rss_bytes),
            })
    }

    /// Every branch's value, in branch order.
    ///
    /// # Panics
    /// If any branch failed; the message lists every failed branch with its index and seed
    /// (and so fails the test, as a `TaskPanic` at the caller's line).
    #[track_caller]
    pub fn into_values(self) -> Vec<T> {
        if let Some(first) = self.failures().next() {
            let end = match &first.error {
                BranchError::Failed(failure) => BranchEnd::Failed(failure.time),
                BranchError::Died(exit) => BranchEnd::Died(*exit),
            };
            FAILED_BRANCH.set(Some(FailedBranch {
                index: first.branch.index,
                end,
            }));
        }
        let failures: Vec<String> = self.failures().map(|f| f.to_string()).collect();
        assert!(
            failures.is_empty(),
            "{} of {} fork branches failed:\n{}",
            failures.len(),
            self.outcomes.len(),
            failures.join("\n")
        );
        self.outcomes
            .into_iter()
            .filter_map(|o| o.result.ok())
            .collect()
    }
}

/// The first failed branch of a checkpoint whose [`Branches::into_values`] failed the test,
/// for the harness's waves on failure: the failure time of the parent is the checkpoint's,
/// while the branch's waves go on to its own failure (`harness::replay`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FailedBranch {
    /// The branch's index.
    pub(crate) index: usize,
    /// How it ended.
    pub(crate) end: BranchEnd,
}

/// How the failed branch of a [`FailedBranch`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BranchEnd {
    /// Its child reported a failure at this time.
    Failed(SimTime),
    /// Its child died without a report: there is no failure time, and its trace was never
    /// finished (the child's `at_child_exit` hooks did not run).
    Died(ChildExit),
}

thread_local! {
    static FAILED_BRANCH: Cell<Option<FailedBranch>> = const { Cell::new(None) };
}

/// The branch recorded by the last failing [`Branches::into_values`] on this thread, which
/// is then forgotten (the harness takes it once, into the test's failure report).
pub(crate) fn take_failed_branch() -> Option<FailedBranch> {
    FAILED_BRANCH.take()
}

/// Forgets the recorded branch (at the start of a run).
pub(crate) fn forget_failed_branch() {
    FAILED_BRANCH.set(None);
}

/// Code that keeps a resource a forked child must not share with its parent: an open
/// trace file, a log, a socket. Registered with [`register_hook`].
pub struct ForkHook {
    before_fork: Box<dyn Fn() -> Result<(), String>>,
    in_child: ChildHook,
    at_child_exit: Option<Box<dyn Fn()>>,
}

/// A [`ForkHook`]'s child half.
type ChildHook = Box<dyn Fn(&Branch) -> Result<(), String>>;

impl ForkHook {
    /// A hook of two halves.
    ///
    /// - `before_fork` runs in the parent before each fork (flush buffered output). An error
    ///   cancels the checkpoint with [`ForkError::Hook`].
    /// - `in_child` runs in the child right after the fork, before the branch body (detach
    ///   the inherited resource, open the branch's own). An error fails the branch.
    pub fn new(
        before_fork: impl Fn() -> Result<(), String> + 'static,
        in_child: impl Fn(&Branch) -> Result<(), String> + 'static,
    ) -> Self {
        Self {
            before_fork: Box::new(before_fork),
            in_child: Box::new(in_child),
            at_child_exit: None,
        }
    }

    /// Adds a third half, run in a branch child just before it exits (whatever the
    /// branch's ending, unless a signal kills the child): the child `_exit`s without
    /// running destructors, so this is where a resource is finished (a trace's last sample
    /// dumped and its file closed).
    ///
    /// It runs only if this hook's `in_child` succeeded in that child: after a failed
    /// `in_child`, or when an earlier hook's failed first, the resource is still the
    /// parent's, and is left untouched.
    #[must_use]
    pub fn at_child_exit(mut self, at_exit: impl Fn() + 'static) -> Self {
        self.at_child_exit = Some(Box::new(at_exit));
        self
    }
}

impl fmt::Debug for ForkHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForkHook").finish_non_exhaustive()
    }
}

thread_local! {
    static HOOKS: RefCell<Vec<(u64, Rc<ForkHook>)>> = const { RefCell::new(Vec::new()) };
    static NEXT_HOOK: Cell<u64> = const { Cell::new(0) };
}

/// Registers `hook` on this thread until the returned guard is dropped. Hooks run in
/// registration order.
#[must_use = "the hook is removed when the guard is dropped"]
pub fn register_hook(hook: ForkHook) -> HookGuard {
    let id = NEXT_HOOK.replace(NEXT_HOOK.get() + 1);
    HOOKS.with_borrow_mut(|hooks| hooks.push((id, Rc::new(hook))));
    HookGuard { id }
}

/// Keeps a [`ForkHook`] registered; dropping it removes the hook.
#[derive(Debug)]
pub struct HookGuard {
    id: u64,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        // `try_with`: a guard dropped during thread teardown has nothing left to remove.
        match HOOKS.try_with(|hooks| hooks.borrow_mut().retain(|(id, _)| *id != self.id)) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// The hooks, cloned out so none is borrowed while one runs (a hook may register another).
fn hooks() -> Vec<Rc<ForkHook>> {
    HOOKS.with_borrow(|hooks| hooks.iter().map(|(_, h)| Rc::clone(h)).collect())
}

thread_local! {
    /// In a branch's child: the hooks whose `in_child` succeeded, in registration order.
    static ARMED: RefCell<Vec<Rc<ForkHook>>> = const { RefCell::new(Vec::new()) };
}

/// Records that `hook`'s `in_child` succeeded in this child, so its `at_child_exit` runs.
fn arm(hook: Rc<ForkHook>) {
    ARMED.with_borrow_mut(|armed| armed.push(hook));
}

/// Runs the `at_child_exit` of every armed hook, once. A hook whose `in_child` failed or
/// never ran (an earlier hook failed) is skipped: it still holds what the child inherited
/// (a trace writer sharing the parent's descriptor and offset), and finishing that would
/// write into the parent's resource.
fn run_exit_hooks() {
    for hook in ARMED.take() {
        if let Some(at_exit) = &hook.at_child_exit {
            // A panic here must not unwind past the branch's end; the report is sent.
            drop(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                at_exit,
            )));
        }
    }
}

/// Runs the child half of a fork without forking or exiting, for unit tests of hooks: the
/// `in_child` of every hook until one fails, then the `at_child_exit` of those that
/// succeeded, as a branch's child does. Returns the first failure.
#[cfg(test)]
pub(crate) fn run_child_hooks_in_place(index: usize) -> Result<(), String> {
    let branch = Branch::new(Seed::new(1, crate::SeedSource::Env), index, index + 1);
    let mut result = Ok(());
    for hook in hooks() {
        if let Err(message) = (hook.in_child)(&branch) {
            result = Err(message);
            break;
        }
        arm(hook);
    }
    run_exit_hooks();
    result
}

/// Checks that the caller is the main task at a quiet point; returns the precision.
fn check_quiet() -> Result<Precision, ForkError> {
    state::try_current("fork::run_from_checkpoint", |k| {
        if k.phase.get() == Phase::Ended {
            return Err(ForkError::NoSimulation);
        }
        let main = k.main.get();
        let current = k.current.get();
        if current != main {
            let task = current.map_or_else(|| "<kernel>".to_owned(), |key| k.task_name(key));
            return Err(ForkError::NotMainTask { task });
        }
        // A combinator operand of `main` runs as a helper inside `main`'s poll (`current`
        // is `main`), and helpers are not tasks: they are checked here, not below.
        let operands = main.map_or(0, |m| k.tasks.borrow().helpers_of(m));
        if operands > 0 || k.context.get() != main {
            return Err(ForkError::InCombinator { operands });
        }
        let others: Vec<String> = k
            .tasks
            .borrow()
            .unfinished()
            .into_iter()
            .filter(|(key, _)| Some(*key) != main)
            .map(|(_, name)| name.to_string())
            .collect();
        if others.is_empty() {
            Ok(k.precision)
        } else {
            Err(ForkError::OtherTasks { tasks: others })
        }
    })
    .unwrap_or(Err(ForkError::NoSimulation))
}

/// Runs `body` once per branch, each in a forked copy of the process at this point, and
/// returns their outcomes; the caller's own state is never changed by a branch.
///
/// `root` seeds the branches ([`Branch::seed`]); pass the test's
/// [`TestCtx::seed`](crate::TestCtx::seed). See the module docs for the rules.
///
/// # Errors
/// A [`ForkError`] if this is not a point the process can fork at (no fork happened then,
/// or the earlier branches' processes were already reaped and only a system call failed).
///
/// # Cancellation
/// Completes in its first poll in the parent; it never suspends there.
pub async fn run_from_checkpoint<T, F, Fut>(
    root: Seed,
    count: usize,
    mut body: F,
) -> Result<Branches<T>, ForkError>
where
    T: BranchValue,
    F: FnMut(Branch) -> Fut,
    Fut: Future<Output = crate::Result<T>>,
{
    if imp::in_branch() {
        return Err(ForkError::Nested);
    }
    check_quiet()?;
    let threads = imp::thread_count()?;
    if threads > 2 {
        return Err(ForkError::Threads { count: threads });
    }
    let mut outcomes = Vec::with_capacity(count);
    for index in 0..count {
        let branch = Branch::new(root, index, count);
        match imp::fork_branch(&branch)? {
            imp::Forked::Parent(child) => outcomes.push(child.collect(branch)?),
            imp::Forked::Child => {
                crate::spawn("fork_sentinel", Sentinel).detach();
                let result = body(branch).await;
                imp::finish_child(&match &result {
                    Ok(value) => wire::value_report(value),
                    Err(failure) => wire::failure_report(failure),
                });
            }
        }
    }
    Ok(Branches { outcomes })
}

/// A task that never completes, in a branch's child: the kernel drops it when it ends the
/// child's run, which can only be a failure, and its `Drop` sends that failure and exits.
struct Sentinel;

impl Future for Sentinel {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        let failure = state::try_current("a fork branch", |k| k.failure.borrow().clone())
            .flatten()
            .unwrap_or_else(ended_without_result);
        imp::finish_child(&wire::failure_report(&failure));
    }
}

/// The failure of a branch whose run ended with neither a result nor a kernel failure.
fn ended_without_result() -> Failure {
    let message = "the branch's run ended before the branch body returned".to_owned();
    Failure {
        kind: crate::FailureKind::TaskPanic {
            message: message.clone(),
        },
        time: state::try_current("a fork branch", |k| k.now.get()).unwrap_or_default(),
        precision: state::try_current("a fork branch", |k| k.precision).unwrap_or(Precision::PS),
        task: None,
        location: None,
        message,
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    use super::wire::{self, BranchValue, Report};
    use super::{Branch, BranchCost, BranchError, BranchOutcome, ForkError, sys};
    use crate::{Failure, FailureKind};

    thread_local! {
        /// In a branch's child: the socket to the parent, until the report is written.
        static CHANNEL: std::cell::RefCell<Option<UnixStream>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Whether this process is a branch's child: it holds the socket to its parent from
    /// the fork until it exits.
    pub(super) fn in_branch() -> bool {
        CHANNEL.with_borrow(Option::is_some)
    }

    pub(super) fn thread_count() -> Result<usize, ForkError> {
        sys::thread_count().map_err(|reason| ForkError::Unsupported { reason })
    }

    pub(super) enum Forked {
        Parent(Child),
        /// This process is the branch's child; the socket is in `CHANNEL`.
        Child,
    }

    pub(super) struct Child {
        pid: libc::pid_t,
        socket: UnixStream,
        forked_at: Instant,
        fork: std::time::Duration,
    }

    fn system(call: &'static str) -> impl FnOnce(io::Error) -> ForkError {
        move |error| ForkError::System {
            call,
            error: error.to_string(),
        }
    }

    /// Forks one branch: hooks, stdio flush, socket, `fork`; in the child, the child hooks.
    #[expect(
        clippy::disallowed_methods,
        reason = "the fork's cost is wall time; it is reported, never used by the simulation"
    )]
    pub(super) fn fork_branch(branch: &Branch) -> Result<Forked, ForkError> {
        let hooks = super::hooks();
        for hook in &hooks {
            (hook.before_fork)().map_err(|message| ForkError::Hook { message })?;
        }
        // An unflushed buffer would be written by both processes. A failed flush loses
        // output, not correctness, so it does not stop the checkpoint.
        drop(io::stdout().flush());
        drop(io::stderr().flush());
        sys::flush_c_stdio();
        let (parent_end, child_end) = UnixStream::pair().map_err(system("socketpair"))?;
        let forked_at = Instant::now();
        let pid = sys::fork().map_err(system("fork"))?;
        if pid == 0 {
            drop(parent_end);
            CHANNEL.set(Some(child_end));
            for hook in hooks {
                if let Err(message) = (hook.in_child)(branch) {
                    finish_child(&wire::failure_report(&hook_failure(&message)));
                }
                super::arm(hook);
            }
            return Ok(Forked::Child);
        }
        let fork = forked_at.elapsed();
        drop(child_end);
        Ok(Forked::Parent(Child {
            pid,
            socket: parent_end,
            forked_at,
            fork,
        }))
    }

    fn hook_failure(message: &str) -> Failure {
        let message = format!("a fork hook failed in the child: {message}");
        let (time, precision) =
            crate::kernel::state::try_current("a fork branch", |k| (k.now.get(), k.precision))
                .unwrap_or((crate::SimTime::ZERO, crate::Precision::PS));
        Failure {
            kind: FailureKind::TaskPanic {
                message: message.clone(),
            },
            time,
            precision,
            task: None,
            location: None,
            message,
        }
    }

    impl Child {
        /// Reads the child's report until it closes the socket, then reaps it.
        pub(super) fn collect<T: BranchValue>(
            mut self,
            branch: Branch,
        ) -> Result<BranchOutcome<T>, ForkError> {
            let mut bytes = Vec::new();
            // A read error leaves the report incomplete; it then decodes as no report.
            drop(self.socket.read_to_end(&mut bytes));
            let (exit, usage) = sys::wait(self.pid).map_err(system("wait4"))?;
            let cost = BranchCost {
                fork: self.fork,
                wall: self.forked_at.elapsed(),
                cpu: usage.cpu,
                minor_faults: usage.minor_faults,
                max_rss_bytes: usage.max_rss_bytes,
            };
            let result = match wire::decode_report::<T>(&bytes) {
                Some(Report::Value(value)) => Ok(value),
                Some(Report::Failed(failure)) => Err(BranchError::Failed(failure)),
                None => Err(BranchError::Died(exit)),
            };
            Ok(BranchOutcome {
                branch,
                result,
                cost,
            })
        }
    }

    /// Ends a branch's child: writes `report` to the parent and exits (code 0 for a value,
    /// 1 for a failure) without running anything else of this process.
    pub(super) fn finish_child(report: &[u8]) -> ! {
        let code = i32::from(report.first() != Some(&0));
        super::run_exit_hooks();
        if let Some(mut socket) = CHANNEL.take() {
            // If the parent cannot read the report, it sees the exit code instead.
            drop(socket.write_all(report));
        }
        drop(io::stdout().flush());
        drop(io::stderr().flush());
        sys::exit_now(code)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{Branch, BranchOutcome, BranchValue, ForkError};

    pub(super) fn in_branch() -> bool {
        false
    }

    pub(super) fn thread_count() -> Result<usize, ForkError> {
        Err(ForkError::Unsupported {
            reason: "fork() exists only on Unix".to_owned(),
        })
    }

    pub(super) enum Forked {
        Parent(Child),
        Child,
    }

    pub(super) enum Child {}

    impl Child {
        pub(super) fn collect<T: BranchValue>(
            self,
            _branch: Branch,
        ) -> Result<BranchOutcome<T>, ForkError> {
            match self {}
        }
    }

    pub(super) fn fork_branch(_branch: &Branch) -> Result<Forked, ForkError> {
        thread_count().map(|_| Forked::Child)
    }

    pub(super) fn finish_child(_report: &[u8]) -> ! {
        unreachable!("invariant: no branch child exists without fork()")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hook that logs its halves as `<name> in` and `<name> exit`; its child half fails
    /// if `fails`.
    fn logging(log: &Rc<RefCell<Vec<String>>>, name: &'static str, fails: bool) -> ForkHook {
        let (child, exit) = (Rc::clone(log), Rc::clone(log));
        ForkHook::new(
            || Ok(()),
            move |_| {
                child.borrow_mut().push(format!("{name} in"));
                if fails {
                    Err(format!("{name} fails"))
                } else {
                    Ok(())
                }
            },
        )
        .at_child_exit(move || exit.borrow_mut().push(format!("{name} exit")))
    }

    /// The second of three hooks fails in the child: the third never runs, and only the
    /// first, which succeeded, is finished at exit.
    #[test]
    fn a_child_finishes_only_the_hooks_whose_child_half_succeeded() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let _a = register_hook(logging(&log, "a", false));
        let _b = register_hook(logging(&log, "b", true));
        let _c = register_hook(logging(&log, "c", false));
        assert_eq!(run_child_hooks_in_place(0), Err("b fails".to_owned()));
        assert_eq!(*log.borrow(), ["a in", "b in", "a exit"]);
    }

    /// `run_from_checkpoint(..).await.unwrap()` (the docs' example) prints the error's
    /// `Debug`: it must read as the advice, not as a struct dump.
    #[test]
    fn a_fork_error_debugs_as_its_message() {
        for error in [
            ForkError::NoSimulation,
            ForkError::Threads { count: 9 },
            ForkError::InCombinator { operands: 2 },
            ForkError::OtherTasks {
                tasks: vec!["main.driver".to_owned()],
            },
        ] {
            assert_eq!(format!("{error:?}"), error.to_string());
        }
    }
}

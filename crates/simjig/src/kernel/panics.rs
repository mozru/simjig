//! Source locations of task panics.
//!
//! `catch_unwind` hands the kernel only the panic's payload; the location exists only in
//! the panic hook. So the kernel chains a hook in front of the existing one: while a
//! [`Sim::run`](crate::Sim::run) is on this thread's stack, the hook stores the location
//! of the latest panic in a thread-local, and the task poller takes it right after
//! `catch_unwind` returns. The previous hook still runs, so panics print as before.
//!
//! A trigger misused by the testbench (say `ReadOnly` awaited in the `ReadOnly` phase)
//! detects it in `poll`, far from the user's line. [`panic_at`] panics there with the
//! location the trigger captured when it was made, so the failure points at the user.
//!
//! The hook is installed once per process and never removed. Removing it at the end of a
//! run would be wrong when runs overlap on several threads (`cargo test`), or when someone
//! else installed a hook after ours: `take_hook` would remove theirs. Outside a run the
//! hook only forwards to the previous one.

use std::cell::Cell;
use std::fmt;
use std::panic::{self, Location};
use std::sync::Once;

use super::failure::SourceLocation;

thread_local! {
    /// Whether a simulation is running on this thread.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    /// The location of the latest panic on this thread during the current run.
    static LAST: Cell<Option<&'static SourceLocation>> = const { Cell::new(None) };
    /// The location the next panic is blamed on instead of its own, set by [`panic_at`].
    static BLAME: Cell<Option<&'static Location<'static>>> = const { Cell::new(None) };
}

static HOOK: Once = Once::new();

fn install_hook() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            // `try_with`: a panic during thread-local teardown must still reach `previous`.
            // Taken even outside a run, so a blame never outlives its panic.
            let blame = BLAME.try_with(Cell::take).ok().flatten();
            if CAPTURING.try_with(Cell::get).unwrap_or(false)
                && let Some(location) = blame.or_else(|| info.location())
            {
                let interned = SourceLocation::intern(location);
                // An error only means LAST is already destroyed: nothing to record into.
                LAST.try_with(|l| l.set(Some(interned))).ok();
            }
            previous(info);
        }));
    });
}

/// Captures panic locations on this thread until dropped.
pub(crate) struct CaptureGuard(());

/// Starts capturing panic locations on this thread, installing the hook if needed.
pub(crate) fn capture() -> CaptureGuard {
    install_hook();
    CAPTURING.set(true);
    LAST.set(None);
    CaptureGuard(())
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURING.set(false);
        LAST.set(None);
    }
}

/// Forgets any earlier panic, before running user code under `catch_unwind`.
pub(crate) fn forget() {
    LAST.set(None);
}

/// Puts back a location taken with [`take`], for a panic still unwinding.
pub(crate) fn restore(location: Option<&'static SourceLocation>) {
    LAST.set(location);
}

/// The location of the panic that `catch_unwind` just caught, if the hook saw one (a
/// `resume_unwind` skips the hook).
pub(crate) fn take() -> Option<&'static SourceLocation> {
    LAST.take()
}

/// Panics with `message`, recording `location` (the user's line) as the panic's location.
/// The message names `location` too, since the default hook prints the panic's own.
pub(crate) fn panic_at(location: &'static Location<'static>, message: fmt::Arguments<'_>) -> ! {
    install_hook();
    BLAME.set(Some(location));
    panic!("{message} (at {location})")
}

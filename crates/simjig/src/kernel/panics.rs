//! Source locations of task panics.
//!
//! `catch_unwind` hands the kernel only the panic's payload; the location exists only in
//! the panic hook. So the kernel chains a hook in front of the existing one: while a
//! [`Sim::run`](crate::Sim::run) is on this thread's stack, the hook stores the location
//! of the latest panic in a thread-local, and the task poller takes it right after
//! `catch_unwind` returns. The previous hook still runs, so panics print as before.
//!
//! The hook is installed once per process and never removed. Removing it at the end of a
//! run would be wrong when runs overlap on several threads (`cargo test`), or when someone
//! else installed a hook after ours: `take_hook` would remove theirs. Outside a run the
//! hook only forwards to the previous one.

use std::cell::Cell;
use std::panic;
use std::sync::Once;

use super::failure::SourceLocation;

thread_local! {
    /// Whether a simulation is running on this thread.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    /// The location of the latest panic on this thread during the current run.
    static LAST: Cell<Option<&'static SourceLocation>> = const { Cell::new(None) };
}

static HOOK: Once = Once::new();

fn install_hook() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            // `try_with`: a panic during thread-local teardown must still reach `previous`.
            if CAPTURING.try_with(Cell::get).unwrap_or(false)
                && let Some(location) = info.location()
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

/// The location of the panic that `catch_unwind` just caught, if the hook saw one (a
/// `resume_unwind` skips the hook).
pub(crate) fn take() -> Option<&'static SourceLocation> {
    LAST.take()
}

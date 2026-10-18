//! `fork()` checkpoint helper (experiment 1).
//!
//! The parent is the checkpoint: it stays frozen at the fork point while a child process
//! explores one branch with its own stimulus, then reports back over a socket and exits.
//!
//! Rules this helper enforces (see `docs/notes/spikes.md` for the evidence):
//! - Only the calling thread survives in the child. libtest's main thread is gone, so the
//!   child must never return into the test harness: it ends with `_exit`, never `exit`
//!   (which would run atexit handlers and flush stdio buffers copied from the parent).
//! - A panic in the child is caught and becomes exit code 101.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn fork() -> i32;
    fn _exit(code: i32) -> !;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEnd {
    Exited(i32),
    Signaled(i32),
}

/// What a child produced.
#[derive(Debug)]
pub struct ChildRun {
    pub end: ChildEnd,
    /// The string the child body returned (empty if it panicked or died).
    pub report: String,
    /// Wall time of `fork()` as seen by the parent.
    pub fork_latency: Duration,
}

/// Runs `body` in a forked child and waits for it.
pub fn in_child(body: impl FnOnce() -> String) -> ChildRun {
    let (mut parent_end, mut child_end) = UnixStream::pair().expect("socketpair");
    let start = Instant::now();
    // SAFETY: the child only runs `body` and then `_exit`s; see the module rules.
    let pid = unsafe { fork() };
    let fork_latency = start.elapsed();
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        drop(parent_end);
        let code = match panic::catch_unwind(AssertUnwindSafe(body)) {
            Ok(report) => {
                let _ = child_end.write_all(report.as_bytes());
                0
            }
            Err(_) => 101,
        };
        drop(child_end);
        // SAFETY: always sound; skips atexit handlers and destructors on purpose.
        unsafe { _exit(code) }
    }
    drop(child_end);
    let mut report = String::new();
    let _ = parent_end.read_to_string(&mut report);
    ChildRun {
        end: wait(pid),
        report,
        fork_latency,
    }
}

fn wait(pid: i32) -> ChildEnd {
    let mut status = 0;
    // SAFETY: `pid` is our child; `status` is a valid out-pointer.
    let r = unsafe { waitpid(pid, &raw mut status, 0) };
    assert_eq!(r, pid, "waitpid");
    // POSIX status layout, the same on Linux and macOS.
    if status & 0x7f == 0 {
        ChildEnd::Exited((status >> 8) & 0xff)
    } else {
        ChildEnd::Signaled(status & 0x7f)
    }
}

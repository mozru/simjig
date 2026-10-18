//! The operating-system calls behind `fork` checkpoints: `fork`, `_exit`, `wait4`, the
//! thread count, and flushing C stdio. Unix only; the only `unsafe` of `simjig::fork`.

use std::io;
use std::ptr;
use std::time::Duration;

use super::{ChildExit, ChildUsage};

/// Forks the process; returns the child's pid in the parent and 0 in the child.
///
/// The child is a copy of the whole address space with only the calling thread, which is
/// why the caller checks [`thread_count`] first (docs/notes/spikes.md §1, surprise 2).
pub(super) fn fork() -> io::Result<libc::pid_t> {
    // SAFETY: `fork` has no memory-safety preconditions. What makes the child sound is the
    // caller's contract (`fork` module docs): no other thread exists that could hold a lock,
    // and the child never returns into the test harness but ends in `exit_now`.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(pid)
    }
}

/// Ends the process at once with `code`: no atexit handlers, no destructors, no flush of
/// stdio buffers copied from the parent.
pub(super) fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` is always sound to call; skipping the process's teardown is the point.
    unsafe { libc::_exit(code) }
}

/// Flushes every C stdio stream, so buffered output is written once, before the fork, not
/// once by each process.
pub(super) fn flush_c_stdio() {
    // SAFETY: `fflush(NULL)` flushes all open output streams; it takes no pointer we own.
    unsafe { libc::fflush(ptr::null_mut()) };
}

/// Waits for the child `pid` to end; returns how it ended and what it used.
pub(super) fn wait(pid: libc::pid_t) -> io::Result<(ChildExit, ChildUsage)> {
    let mut status: libc::c_int = 0;
    // SAFETY: `rusage` is plain old data; all zeroes is a valid value.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is a child of this process; `status` and `usage` are valid,
        // writable and live for the call.
        let reaped = unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) };
        if reaped == pid {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    let exit = if libc::WIFSIGNALED(status) {
        ChildExit::Signaled(libc::WTERMSIG(status))
    } else {
        ChildExit::Exited(libc::WEXITSTATUS(status))
    };
    Ok((exit, usage_of(&usage)))
}

fn duration(tv: libc::timeval) -> Duration {
    let secs = u64::try_from(tv.tv_sec).unwrap_or(0);
    let micros = u32::try_from(tv.tv_usec).unwrap_or(0);
    Duration::from_secs(secs) + Duration::from_micros(u64::from(micros))
}

fn usage_of(usage: &libc::rusage) -> ChildUsage {
    // `ru_maxrss` is in bytes on macOS and in KiB on Linux (getrusage(2) on each).
    let rss_unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
    ChildUsage {
        minor_faults: u64::try_from(usage.ru_minflt).unwrap_or(0),
        max_rss_bytes: u64::try_from(usage.ru_maxrss).unwrap_or(0) * rss_unit,
        cpu: duration(usage.ru_utime) + duration(usage.ru_stime),
    }
}

/// The number of threads in this process.
///
/// # Errors
/// A description of why the count is unknown (always, on a Unix other than macOS and
/// Linux).
#[cfg(target_os = "macos")]
pub(super) fn thread_count() -> Result<usize, String> {
    // SAFETY: `proc_taskinfo` is plain old data; all zeroes is a valid value.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(size_of::<libc::proc_taskinfo>())
        .map_err(|e| format!("proc_taskinfo size: {e}"))?;
    // SAFETY: `getpid` has no preconditions.
    let pid = unsafe { libc::getpid() };
    // SAFETY: `info` is a writable `proc_taskinfo` of exactly `size` bytes, which is what
    // PROC_PIDTASKINFO fills (sys/proc_info.h).
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size) };
    if written != size {
        return Err(format!(
            "proc_pidinfo(PROC_PIDTASKINFO) failed: {}",
            io::Error::last_os_error()
        ));
    }
    usize::try_from(info.pti_threadnum).map_err(|e| format!("thread count: {e}"))
}

/// The number of threads in this process.
///
/// # Errors
/// A description of why the count is unknown.
#[cfg(target_os = "linux")]
pub(super) fn thread_count() -> Result<usize, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("cannot read /proc/self/status: {e}"))?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| "/proc/self/status has no Threads: line".to_owned())
}

/// The number of threads in this process.
///
/// # Errors
/// Always: only macOS and Linux are supported.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn thread_count() -> Result<usize, String> {
    Err("counting threads is implemented only on macOS and Linux".to_owned())
}

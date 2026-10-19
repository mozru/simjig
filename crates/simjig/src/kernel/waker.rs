//! Task wakers without allocation: the waker's data pointer *is* the packed task identity.
//!
//! Layout of the data word (64-bit targets only): bits 63..32 hold the simulation id
//! (`sim_gen`), bits 31..16 the task's slot index and bits 15..0 the low 16 bits of the
//! task's generation. The pointer is never dereferenced; it carries no provenance
//! (`ptr::without_provenance`), so cloning and dropping are free and Miri-clean.
//!
//! A wake goes to [`state::wake_task`], which queues the task on the current kernel if it
//! is that simulation, ignores wakes for simulations that have ended or tasks that no
//! longer exist, and panics if the simulation is alive on another thread. A generation
//! that wrapped around in 16 bits can at worst cause a spurious poll, which every future
//! must tolerate anyway.
//!
//! This is one of the few modules allowed to use `unsafe` (DECISION §2 row 12).
//!
//! Why not `std::task::Wake` on an `Arc`: it allocates per task and needs an
//! `Arc<Mutex<_>>` run queue for `Send + Sync`; the packed integer is `Send + Sync`
//! trivially, and the thread check makes cross-thread misuse loud instead of racy.
#![expect(
    unsafe_code,
    reason = "the only way to build a Waker from a custom vtable"
)]

use std::ptr;
use std::task::{RawWaker, RawWakerVTable, Waker};

use super::state;

const _: () = assert!(
    size_of::<usize>() >= 8,
    "the packed waker needs a 64-bit pointer"
);

/// Highest task slot index a waker can carry.
pub(crate) const MAX_TASK_INDEX: u32 = 0xffff;

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_waker);

const fn pack(sim_gen: u32, index: u32, gen16: u16) -> usize {
    ((sim_gen as usize) << 32) | ((index as usize) << 16) | gen16 as usize
}

fn unpack(data: usize) -> (u32, u32, u16) {
    let sim_gen = u32::try_from(data >> 32).unwrap_or(0);
    let index = u32::try_from((data >> 16) & 0xffff).unwrap_or(0);
    let gen16 = u16::try_from(data & 0xffff).unwrap_or(0);
    (sim_gen, index, gen16)
}

fn raw(data: usize) -> RawWaker {
    RawWaker::new(ptr::without_provenance(data), &VTABLE)
}

// The four vtable functions only read the pointer's address; none dereferences it, so they
// have no safety preconditions beyond the `unsafe fn` signature the vtable requires.

unsafe fn clone(data: *const ()) -> RawWaker {
    raw(data.addr())
}

unsafe fn wake(data: *const ()) {
    let (sim_gen, index, gen16) = unpack(data.addr());
    state::wake_task(sim_gen, index, gen16);
}

unsafe fn wake_by_ref(data: *const ()) {
    let (sim_gen, index, gen16) = unpack(data.addr());
    state::wake_task(sim_gen, index, gen16);
}

unsafe fn drop_waker(_data: *const ()) {}

/// The `(sim_gen, index, gen16)` a waker made by [`task_waker`] carries, or `None` for any
/// other waker: the kernel queues its own tasks without a round trip through the waker.
pub(crate) fn own_task(waker: &Waker) -> Option<(u32, u32, u16)> {
    ptr::eq(waker.vtable(), &raw const VTABLE).then(|| unpack(waker.data().addr()))
}

/// A waker that queues task (`index`, `gen16`) of simulation `sim_gen` when woken.
///
/// # Panics
/// If `index` exceeds [`MAX_TASK_INDEX`].
pub(crate) fn task_waker(sim_gen: u32, index: u32, gen16: u16) -> Waker {
    assert!(
        index <= MAX_TASK_INDEX,
        "invariant: task index fits the waker"
    );
    // SAFETY: `RawWaker` contract (std::task::RawWakerVTable): the data is a plain integer
    // that is never dereferenced, so it is valid for any lifetime and on any thread;
    // `clone` returns a waker with the same data and vtable; `wake` and `wake_by_ref` have
    // the same effect and own no resources; `drop_waker` has nothing to release. Calling
    // them from another thread is memory-safe and is reported by `wake_task`.
    unsafe { Waker::from_raw(raw(pack(sim_gen, index, gen16))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_round_trips() {
        let data = pack(0xdead_beef, 0x1234, 0xabcd);
        assert_eq!(unpack(data), (0xdead_beef, 0x1234, 0xabcd));
    }

    #[test]
    fn wakers_clone_wake_and_drop_without_a_running_sim() {
        // No kernel is installed: every operation must be a harmless no-op.
        let waker = task_waker(u32::MAX, 3, 7);
        let other = waker.clone();
        assert!(waker.will_wake(&other));
        other.wake_by_ref();
        other.wake();
        drop(waker);
    }
}

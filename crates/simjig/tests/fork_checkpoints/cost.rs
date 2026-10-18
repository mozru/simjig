//! What a branch costs, as `BranchCost` reports it. Timings are printed (run with
//! `--no-capture`), not asserted; the copy-on-write fault count is.

use std::cell::RefCell;
use std::rc::Rc;

use simjig::TestCtx;
use simjig::fork;

use crate::counter;

/// Bytes the parent touches before forking, which one branch then rewrites.
const BALLAST: usize = 32 << 20;

/// The smallest page size of the supported systems (Linux x86-64; macOS arm64 uses 16 KiB).
const MIN_PAGE: usize = 4096;

/// The largest page size of the supported systems.
const MAX_PAGE: usize = 16384;

#[simjig::test(timeout = "1us")]
async fn copy_on_write_shows_in_the_branch_cost(ctx: &TestCtx, _dut: counter::Dut) {
    let ballast = Rc::new(RefCell::new(vec![1u8; BALLAST]));
    let shared = Rc::clone(&ballast);
    let branches = fork::run_from_checkpoint(ctx.seed(), 3, move |branch| {
        let ballast = Rc::clone(&shared);
        async move {
            if branch.index() == 1 {
                // One write per page copies every page.
                let mut ballast = ballast.borrow_mut();
                for byte in ballast.iter_mut().step_by(MIN_PAGE) {
                    *byte = 2;
                }
            }
            Ok(())
        }
    })
    .await
    .unwrap();
    let costs: Vec<_> = branches.outcomes().iter().map(|o| o.cost).collect();
    for (i, cost) in costs.iter().enumerate() {
        eprintln!(
            "SIMJIG_FORK_COST branch={i} fork_us={} wall_us={} cpu_us={} minor_faults={} \
             max_rss_mib={}",
            cost.fork.as_micros(),
            cost.wall.as_micros(),
            cost.cpu.as_micros(),
            cost.minor_faults,
            cost.max_rss_bytes >> 20
        );
        assert!(cost.wall >= cost.fork, "{cost:?}");
    }
    let idle = costs[0].minor_faults.max(costs[2].minor_faults);
    let copied = costs[1].minor_faults.saturating_sub(idle);
    assert!(
        copied >= (BALLAST / MAX_PAGE) as u64,
        "rewriting {BALLAST} bytes copies at least one fault per page: {costs:?}"
    );
    assert!(
        ballast.borrow().iter().all(|&b| b == 1),
        "the parent's copy is untouched"
    );
}

//! A scratch directory outlives the run that made it, unless somebody sweeps.
//!
//! `Drop` is not a guarantee: it does not run on SIGKILL, and on this machine a `cargo test` gets
//! killed from outside routinely -- a deadline, the OOM killer, Ctrl-C. Two of those directories
//! were sitting in tmpfs from runs killed earlier today, which is the same shape as the leak that
//! reached 6.947 directories and 610 MB.
//!
//! So the cleanup cannot live only in the `Drop` of the run that made the mess: the next run
//! clears what dead ones left. The pid is in the name, and a pid with no process behind it says
//! the run is over.

use sondc::scratch::Scratch;

/// A pid with no process behind it.
fn dead_pid() -> u32 {
    for p in (40000u32..4_000_000).step_by(7919) {
        if !std::path::Path::new(&format!("/proc/{p}")).exists() {
            return p;
        }
    }
    panic!("every pid on the machine is taken");
}

#[test]
fn the_next_run_clears_the_scratch_of_runs_that_are_gone() {
    // Shaped exactly like the real thing, and named after a run that is over. `with_name` does
    // not sweep, so what gets measured below is what `new` does.
    let dead = Scratch::with_name(&format!("sondtmp-leftovers-{}-1a2b3c", dead_pid()));
    std::fs::write(dead.join("filler"), vec![0u8; 4096]).unwrap();
    let alive = Scratch::with_name(&format!("sondtmp-leftovers-{}-4d5e6f", std::process::id()));

    let dead_path = dead.to_path_buf();
    let live_path = alive.to_path_buf();
    let _ = Scratch::new("sondtmp-trigger");

    assert!(!dead_path.exists(), "the scratch of a dead run is still there: {dead_path:?}");
    assert!(
        live_path.exists(),
        "and the scratch of a run still going was taken with it: {live_path:?}"
    );
}

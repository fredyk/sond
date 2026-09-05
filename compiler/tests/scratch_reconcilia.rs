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

use sondc::scratch::Temporal;

/// A pid with no process behind it.
fn pid_muerto() -> u32 {
    for p in (40000u32..4_000_000).step_by(7919) {
        if !std::path::Path::new(&format!("/proc/{p}")).exists() {
            return p;
        }
    }
    panic!("every pid on the machine is taken");
}

#[test]
fn the_next_run_clears_the_scratch_of_runs_that_are_gone() {
    // Shaped exactly like the real thing, and named after a run that is over. `con_nombre` does
    // not sweep, so what gets measured below is what `nuevo` does.
    let muerto = Temporal::con_nombre(&format!("sondtmp-restos-{}-1a2b3c", pid_muerto()));
    std::fs::write(muerto.join("relleno"), vec![0u8; 4096]).unwrap();
    let vivo = Temporal::con_nombre(&format!("sondtmp-restos-{}-4d5e6f", std::process::id()));

    let ruta_muerta = muerto.to_path_buf();
    let ruta_viva = vivo.to_path_buf();
    let _ = Temporal::nuevo("sondtmp-disparo");

    assert!(!ruta_muerta.exists(), "the scratch of a dead run is still there: {ruta_muerta:?}");
    assert!(
        ruta_viva.exists(),
        "and the scratch of a run still going was taken with it: {ruta_viva:?}"
    );
}

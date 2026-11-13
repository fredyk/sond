//! Two `sond` runs at the same time must not read each other's compiled test binary.
//!
//! `sond test` compiles the test harness to a scratch binary and then runs it. If the name of
//! that scratch file is derived from the source file alone, two files that happen to share a
//! stem -- `mod.snd` in two directories, two projects with a `main.snd` -- name the SAME path,
//! and the machine has many `sond` processes on it at once (a fleet of agents, a Makefile with
//! -j, two terminals). Then one run executes the other's binary, or deletes it mid-run.
//!
//! The failure that matters is not the noisy one. A run whose binary was swapped for a passing
//! one exits 0: a test suite that is failing reports success, which is the reading nobody
//! double-checks.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// A scratch project holding its own `lib/`, so the run resolves `include` by walking up to it
/// and never depends on what this machine happens to have in ~/.local/lib/sond.
fn proyecto() -> sondc::scratch::Scratch {
    let base = sondc::scratch::Scratch::new("sond-conc");
    let output = Command::new("cp").arg("-R").arg(repo().join("lib")).arg(&base).output().unwrap();
    assert!(output.status.success(), "the scratch project needs a lib/ of its own");

    // Same stem, two directories, opposite verdicts: one passes, one fails on purpose.
    write(
        &base.join("green/mod.snd"),
        "include \"x86_64/linux/testing.snd\"\n\
         fn value() -> i64 { 111 }\n\
         test \"passes\" { assert_eq(value(), 111) }\n",
    );
    write(
        &base.join("red/mod.snd"),
        "include \"x86_64/linux/testing.snd\"\n\
         fn value() -> i64 { 222 }\n\
         test \"fails\" { assert_eq(value(), 999) }\n",
    );
    base
}

fn sond_test(file: &Path) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg(file)
        .output()
        .expect("sond runs")
        .status
        .code()
        .unwrap_or(-1)
}

#[test]
fn two_runs_of_files_with_the_same_name_each_get_their_own_verdict() {
    let base = proyecto();
    let green = base.join("green/mod.snd");
    let red = base.join("red/mod.snd");

    // On its own each file says what it is, which is why a sequential suite never sees this.
    assert_eq!(sond_test(&green), 0, "the passing file passes when it runs alone");
    assert_eq!(sond_test(&red), 1, "the failing file fails when it runs alone");

    let vueltas = 20;
    let (mut verde_mal, mut rojo_mal) = (0, 0);
    for _ in 0..vueltas {
        let v = green.clone();
        let hilo = std::thread::spawn(move || sond_test(&v));
        let r = sond_test(&red);
        let vr = hilo.join().unwrap();
        if vr != 0 {
            verde_mal += 1;
        }
        if r != 1 {
            rojo_mal += 1;
        }
    }
    assert_eq!(
        (verde_mal, rojo_mal),
        (0, 0),
        "in {vueltas} concurrent pairs the passing file was called failing {verde_mal} times and \
         -- the one that costs money -- the FAILING file was called passing {rojo_mal} times"
    );
}

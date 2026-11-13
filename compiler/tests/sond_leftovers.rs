//! What a run that is killed from outside leaves behind, and who clears it.
//!
//! `Drop` does not run on SIGKILL, and Ctrl-C at the terminal no longer reaches a gate that lives
//! in a process group of its own. So the deadline inside the tool cannot be the only thing that
//! cleans up: whoever makes the mess cannot be the only one who clears it. Every run starts by
//! sweeping the scratch of runs that are gone -- the processes first, then the directory.
//!
//! Found on this machine as two `.sond-<pid>-*` directories whose runs had ended, one of them
//! still holding a core at 100%.

use std::path::{Path, PathBuf};
use std::process::Command;
use sondc::scratch::Scratch;

/// A pid with no process behind it, high enough not to be handed out in the next second.
fn dead_pid() -> u32 {
    for p in (40000u32..4_000_000).step_by(7919) {
        if !Path::new(&format!("/proc/{p}")).exists() {
            return p;
        }
    }
    panic!("every pid on the machine is taken");
}

/// A scratch directory shaped exactly like `sond`'s, with a process running out of it. Built
/// INSIDE `tmp`, which is the `TMPDIR` the run under test is given: the sweep is global over the
/// temp directory by design, so a leftover left in the shared `/tmp` is fair game for any other
/// `sond` the suite starts in parallel -- and one of them swept this fixture out from under the
/// spawn, which came back as ENOENT.
fn leftover_of(pid: u32, tmp: &Path) -> (PathBuf, std::process::Child) {
    let dir = tmp.join(format!(".sond-{pid}-{:016x}", 0xfeedfaceu32));
    std::fs::create_dir(&dir).unwrap();
    let gate = dir.join("gate");
    std::fs::copy("/bin/sleep", &gate).unwrap();
    // ETXTBSY: another test in this binary forked while `gate` was still open for writing, and
    // `exec` refuses until that descriptor is gone. It is a race, not a failure.
    let mut child = None;
    for _ in 0..50 {
        match Command::new(&gate).arg("600").spawn() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            otro => {
                child = Some(otro.expect("the leftover runs"));
                break;
            }
        }
    }
    (dir, child.expect("the leftover runs"))
}

/// Any `sond` invocation, used here only for its side effect of starting up. `tmp` is the
/// temp directory it sweeps.
fn run_sond(dir: &Path, tmp: &Path) {
    let f = dir.join("t.snd");
    std::fs::write(&f, "fn main() -> i64 { 0 }\n").unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_sond"))
        .env("TMPDIR", tmp)
        .arg("build")
        .arg(&f)
        .output();
}

/// A tree with a `lib/` of its own and one gate that never finishes.
fn hung_tree(name: &str) -> Scratch {
    let base = Scratch::new(name);
    let out = Command::new("cp")
        .arg("-R")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("lib"))
        .arg(&base)
        .output()
        .unwrap();
    assert!(out.status.success(), "the tree needs a lib/ of its own");
    std::fs::create_dir_all(base.join("src")).unwrap();
    std::fs::write(base.join("src/slow.snd"), "fn slow() -> i64 { 1 }\n").unwrap();
    std::fs::write(
        base.join("src/lento_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"slow.snd\"\n\
         test \"spins\" { let mut i = 0\n for i >= 0 { i = i + 1 }\n assert_eq(slow(), 1) }\n",
    )
    .unwrap();
    base
}

#[test]
fn the_next_run_sweeps_what_a_killed_run_left_spinning() {
    let work = Scratch::new("sond-leftovers");
    let (leftover, mut process) = leftover_of(dead_pid(), &work);

    run_sond(&work, &work);

    // `try_wait`, not /proc: a killed child of this test stays a zombie -- with a live
    // /proc/<pid> -- until its parent reaps it, which reads exactly like "still running".
    let mut alive = true;
    for _ in 0..50 {
        alive = matches!(process.try_wait(), Ok(None));
        if !alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let left = leftover.exists();
    // Leave nothing behind whichever way the assert goes.
    let _ = process.kill();
    let _ = process.wait();

    assert!(!alive, "the leftover process of a dead run is still spinning: {}", process.id());
    assert!(!left, "and its scratch directory is still there: {}", leftover.display());
}

#[test]
fn a_run_that_is_still_going_is_left_alone() {
    // The bad way to sweep is to sweep everything: several `sond` run at once here -- a fleet of
    // agents, `make -j`, two terminals -- and a sweep that cannot tell a dead run from a live one
    // deletes the binary the run next door is about to execute.
    //
    // What is checked is the effect, not the directory: the live run must still reach its own
    // verdict. Swept, its gate is killed early and the run reports a failure it never had.
    let base = hung_tree("sond-vivo");
    let started = std::time::Instant::now();
    let alive = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg("--timeout=20")
        .arg(base.join("src"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("sond runs");

    // Long enough that the live run is past its own sweep and into the gate.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let otro = Scratch::new("sond-otro");
    run_sond(&otro, &otro);

    let output = alive.wait_with_output().expect("the live run ends on its own");
    let texto = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        texto.contains("did not finish within 20s"),
        "the live run reached its own verdict rather than being cut short by the sweep \
         (after {:?}):\n{texto}",
        started.elapsed()
    );
    assert!(texto.contains("HUNG"), "and reported it as a hang:\n{texto}");
}

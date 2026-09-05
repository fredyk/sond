//! What a compiled program actually DOES, measured by running it.
//!
//! The unit tests under `codegen` compile through a helper that goes straight from the parser to
//! codegen, skipping the checks and desugaring passes that `main.rs` runs in between. That helper
//! is fine for asserting on emitted assembly, but it cannot answer "what does `sondc` do with this
//! file": a shadowing fix once left four of those tests green while the compiler still returned
//! `10` where they asserted `4`, because the bug lived in a pass the helper never runs.
//!
//! Every case here goes through the real binary, the same way a user does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How long a compiled test program may run before it is treated as hung.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs `bin`, returning its exit code and whatever it printed, or `None` if it had to be killed
/// for running past `DEADLINE`.
///
/// The deadline lives HERE, in Rust, and not in a `timeout` binary looked up at a fixed path.
/// Three tests in this file used to do `if Path::new("/usr/bin/timeout").exists()` and, when it
/// did not, fall through to running with no limit at all — their own comment said so. A machine
/// that keeps `timeout` anywhere else silently lost the protection, and a test that hangs does
/// not fail: it holds the whole suite with nothing on screen, which reads as "slow today".
fn run_with_deadline(bin: &Path) -> Option<(i32, String)> {
    let salida = bin.with_extension("stdout");
    let fichero = std::fs::File::create(&salida).expect("the output file must be writable");
    let mut hijo = Command::new(bin)
        .stdout(std::process::Stdio::from(fichero))
        .spawn()
        .expect("the compiled program must be runnable");
    let inicio = std::time::Instant::now();
    let estado = loop {
        match hijo.try_wait().expect("waiting on the child must work") {
            Some(estado) => break estado,
            None => {
                if inicio.elapsed() >= DEADLINE {
                    let _ = hijo.kill();
                    let _ = hijo.wait();
                    let _ = std::fs::remove_file(&salida);
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    };
    let impreso = std::fs::read_to_string(&salida).unwrap_or_default();
    let _ = std::fs::remove_file(&salida);
    Some((
        estado
            .code()
            .expect("the program must exit normally, not by signal"),
        impreso,
    ))
}

/// Compiles `src` with the binary this `cargo test` just built and returns the program's exit
/// code. Files are named after the process and the case so two concurrent runs cannot truncate
/// each other's output.
fn compile_and_run(case: &str, src: &str) -> i32 {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("behaviour-{}-{}", std::process::id(), case);
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("the test source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "`{case}` must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (code, _) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("`{case}` ran for over {DEADLINE:?} without finishing: the compiled program never terminates")
    });
    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(Path::new(&bin));
    code
}

/// Compiles `src` and returns WHAT IT PRINTED. Same deadline as `compile_and_run`, which is the
/// point: a program that never ends fails this test instead of hanging the suite.
///
/// Printed output, not the exit status: the status is ONE BYTE, so a program answering 100000
/// reports 160 and the test passes or fails for a reason that has nothing to do with the bug.
fn compile_and_print(case: &str, src: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("behaviour-{}-{}", std::process::id(), case);
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("the test source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "`{case}` must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (_, printed) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("`{case}` ran for over {DEADLINE:?} without finishing: the compiled program never terminates")
    });
    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(Path::new(&bin));
    printed
}

/// Compiles `src` expecting sondc to REFUSE it, and returns what it said. A program that must not
/// compile is never run: the whole point is that no binary comes out of it.
fn compile_error(case: &str, src: &str) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("behaviour-{}-{}", std::process::id(), case);
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("the test source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    let dicho = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "`{case}` must NOT compile, but sondc accepted it and said: {dicho}"
    );
    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(Path::new(&bin));
    dicho
}

#[test]
fn a_string_shadowed_in_an_inner_block_does_not_steal_the_outer_length() {
    // `m.len` on the last line is the OUTER `m` ("hola"), so 4. The inner `m` is a different
    // variable that stops existing at the closing brace.
    assert_eq!(
        compile_and_run(
            "shadow-if",
            "fn main() -> i64 {\n\
             \x20let m: string = \"hola\"\n\
             \x20if 1 == 1 { let m: string = \"0123456789\" }\n\
             \x20m.len\n\
             }\n",
        ),
        4
    );
}

#[test]
fn a_length_read_before_the_shadowing_block_is_the_one_that_was_in_scope() {
    // `r` is read on line 3, where the only `a` in scope is the five-character one.
    assert_eq!(
        compile_and_run(
            "shadow-earlier",
            "fn main() -> i64 {\n\
             \x20let a: string = \"12345\"\n\
             \x20let r = a.len\n\
             \x20if 1 == 1 { let a: string = \"xy\" }\n\
             \x20r\n\
             }\n",
        ),
        5
    );
}

#[test]
fn a_call_the_prover_knows_nothing_about_does_not_erase_what_it_proved() {
    // Every bound here is provable: the first loop leaves `len <= 16`, so `i = len - 1` is at
    // most 15, and the loop condition keeps it at 0 or above. `print_int` is a builtin, and the
    // prover has no contract for it — which used to make it abandon the whole path and forget
    // both bounds, so `d[i]` was refused inside `arp-reply.snd` while the identical loop with a
    // user-defined function in place of the builtin compiled clean.
    assert_eq!(
        compile_and_run(
            "opaque-call",
            "fn main() -> i64 {\n\
             \x20let mut n = 1234\n\
             \x20let mut d[16]\n\
             \x20let mut len = 0\n\
             \x20for n > 0 && len < 16 invariant { len <= 16 } {\n\
             \x20 d[len] = n % 10\n\
             \x20 len = len + 1\n\
             \x20 n = n / 10\n\
             \x20}\n\
             \x20let mut i = len - 1\n\
             \x20for i >= 0 invariant { i < 16 } {\n\
             \x20 print_int(d[i])\n\
             \x20 i = i - 1\n\
             \x20}\n\
             \x20len\n\
             }\n",
        ),
        4
    );
}

#[test]
fn a_let_in_an_inner_block_does_not_write_the_outer_variable() {
    // The inner `let n = 33` is a different variable that stops existing at the closing
    // brace; the `n` on the last line is still the outer one, so 7, not 33.
    assert_eq!(
        compile_and_run(
            "shadow-let-outer",
            "fn main() -> i64 {\n\
             \x20let n = 7\n\
             \x20if 1 == 1 { let n = 33 }\n\
             \x20n\n\
             }\n",
        ),
        7
    );
}

#[test]
fn a_let_in_an_inner_block_does_not_write_the_parameter() {
    // `f`'s parameter `n` is 7; the block's `let n = 33` must not overwrite it.
    assert_eq!(
        compile_and_run(
            "shadow-param",
            "fn f(n: i64) -> i64 {\n\
             \x20if 1 == 1 { let n = 33 }\n\
             \x20n\n\
             }\n\
             fn main() -> i64 { f(7) }\n",
        ),
        7
    );
}

#[test]
fn the_outer_slot_is_readable_and_writable_after_the_block() {
    // `outer` reads the outer `n` (7, not the block's 33) BEFORE it is reassigned; the
    // later `n = 5` writes the outer slot. Result 7 + 5 = 12. If the inner `let n` had
    // written through the shared slot, `outer` would have been 33 and the sum 38.
    assert_eq!(
        compile_and_run(
            "shadow-outer-rw",
            "fn main() -> i64 {\n\
             \x20let n = 7\n\
             \x20if 1 == 1 { let n = 33 }\n\
             \x20let outer = n\n\
             \x20n = 5\n\
             \x20outer + n\n\
             }\n",
        ),
        12
    );
}

#[test]
fn shadowing_is_in_effect_inside_the_block() {
    // Inside the `if`, `n` refers to the block's own 33; the outer `n` is still 7
    // afterwards. `inner + n` = 33 + 7 = 40 (not 7 + 7 = 14 if the shadow were inert).
    assert_eq!(
        compile_and_run(
            "shadow-inside",
            "fn main() -> i64 {\n\
             \x20let n = 7\n\
             \x20let inner = if 1 == 1 { let n = 33  n } else { 0 }\n\
             \x20inner + n\n\
             }\n",
        ),
        40
    );
}

/// Compile the real selftest.snd and return the path to the binary.
fn compile_selftest() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let repo = PathBuf::from(manifest).parent().unwrap().to_path_buf();
    let snd = repo.join("examples/host/crypto/x25519-lib/selftest.snd");

    let out_bin = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("behaviour-x25519-selftest-{}.bin", std::process::id()));

    let compile_out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .args(["-o", out_bin.to_str().unwrap()])
        .output()
        .expect("sondc must be runnable");

    assert!(
        compile_out.status.success(),
        "selftest.snd must compile, but:\n{}",
        String::from_utf8_lossy(&compile_out.stderr)
    );
    out_bin
}

#[test]
fn selftest_x25519_rfc7748_vector1() {
    let bin = compile_selftest();

    let out = Command::new(&bin)
        .output()
        .expect("the compiled program must run");

    let _ = std::fs::remove_file(&bin);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "selftest program must exit 0, got {:?}\nstderr: {}",
        out.status.code(),
        stderr
    );
    assert!(
        stdout.contains("PASS: output matches RFC 7748 test vector 1"),
        "expected RFC vector PASS but got:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

/// A `return 7` inside a `lock` must return 7, not fall through to the code after the lock.
/// The lock is released, then control goes to the function's real return.
#[test]
fn lock_return_returns_the_early_value() {
    assert_eq!(
        compile_and_run(
            "lock-return",
            "let mut mtx[1]\n\
             fn f() -> i64 {\n\
                 lock (mtx) {\n\
                     return 7\n\
                 }\n\
                 42\n\
             }\n\
             fn main() -> i64 { f() }\n"
        ),
        7
    );
}

/// A `break` inside a `lock` must release the lock before jumping to the loop's end.
/// If the lock is not released, the second loop will deadlock trying to re-acquire it.
/// This test uses `cargo test --timeout` or a spawned child killed after 10s to prevent
/// infinite hangs if the bug reappears.
///
/// We run the binary directly and let the test harness's timeout handle it.
/// If we were to get a deadlock, `cargo test`'s default timeout (or a CI timeout)
/// would catch it. For local safety, we use `timeout` command if available.
#[test]
fn lock_break_releases_the_lock() {
    let src = "let mut mtx[1]\n\
               fn main() -> i64 {\n\
                   let mut i = 0\n\
                   for {\n\
                       lock (mtx) {\n\
                           i = i + 1\n\
                           break\n\
                       }\n\
                   }\n\
                   for i < 100000 {\n\
                       lock (mtx) { i = i + 1 }\n\
                   }\n\
                   print_int(i)\n\
                   0\n\
               }\n";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("behaviour-lock-break-{}", std::process::id());
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("the test source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "lock-break must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A deadlock in lock+break would hang here for ever, so the run carries a deadline.
    let (exit_code, stdout) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("the program ran past {DEADLINE:?}: a deadlock in lock+break would cause this")
    });

    // The count travels on stdout, NOT in the exit status: an exit status is a single BYTE, so
    // 100000 would arrive as 160 and the assertion could never hold whatever the compiler did.
    // Written against the status, this test was red from the day it was born and stayed red for
    // the right-looking reason -- "the lock was not released" -- while the lock was released fine.
    assert_eq!(
        stdout.trim(),
        "100000",
        "break released the lock and the second loop ran to the end (exit status was {exit_code})"
    );

    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(&bin);
}

/// Two threads fight over the same lock. One holds it while the other spins. When the first
/// releases, the second must be woken by the futex wake in the release path (SYS_futex 202 +
/// FUTEX_WAKE), not by a bogus syscall. Without the correct wake, the waiting thread would
/// sleep forever and the program deadlocks.
#[test]
fn lock_with_two_threads_fairness_futex_wake_path() {
    let src = "let mut mtx[1]\n\
               let mut result[1]\n\
               let mut done[1]\n\
               fn first() -> i64 {\n\
                   lock (mtx) {\n\
                       done[0] = 1\n\
                   }\n\
                   0\n\
               }\n\
               fn second() -> i64 {\n\
                   for peek64(done) < 1 { }\n\
                   lock (mtx) {\n\
                       result[0] = 42\n\
                   }\n\
                   0\n\
               }\n\
               fn main() -> i64 {\n\
                   poke64(done, 0)\n\
                   poke64(result, 0)\n\
                   spawn first()\n\
                   second()\n\
                   peek64(result)\n\
               }\n";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("behaviour-lock-contend-{}", std::process::id());
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("the test source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "lock-contend must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A missing futex wake deadlocks, so the run carries a deadline.
    let (exit_code, _stdout) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("the program ran past {DEADLINE:?}: a missing futex wake deadlocks like this")
    });
    assert_eq!(exit_code, 42, "second thread acquired the lock after first released it (futex wake works)");

    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(&bin);
}

/// A `for x in a` loop with `continue` MUST NOT hang. The counter must advance past the
/// continue path (Bug: continue inside foreach never increments the hidden counter).
#[test]
fn foreach_with_continue_compiles_and_runs() {
    let src = "fn main() -> i64 {\n\
               let a[4]\n\
               a[0] = 1\n\
               a[1] = 2\n\
               a[2] = 3\n\
               a[3] = 4\n\
               let mut t = 0\n\
               for x in a {\n\
                   if x == 2 { continue }\n\
                   t = t + x\n\
               }\n\
               print_int(t)\n\
               0\n\
               }\n";
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let stem = format!("continue-foreach-{}", std::process::id());
    let snd = dir.join(format!("{stem}.snd"));
    let bin = dir.join(format!("{stem}.bin"));
    std::fs::write(&snd, src).expect("write source");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd).arg("-o").arg(&bin)
        .output().expect("sondc must run");
    assert!(out.status.success(), "compile must succeed: {}",
        String::from_utf8_lossy(&out.stderr));

    let (_exit_code, stdout) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("the program ran past {DEADLINE:?}: continue inside foreach skips the counter increment")
    });
    assert_eq!(stdout.trim(), "8", "1+3+4=8, skipping 2");
    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(&bin);
}

/// `for i in 0..n` with `continue` must compile and produce correct results.
#[test]
fn range_for_with_continue_compiles_and_runs() {
    // Sum 0..4 = 0+1+2+3 = 6, skip 2 via continue -> 0+1+3 = 4
    let src = "fn main() -> i64 {\n\
               let mut t = 0\n\
               for i in 0..4 {\n\
                   if i == 2 { continue }\n\
                   t = t + i\n\
               }\n\
               t\n\
               }\n";
    assert_eq!(compile_and_run("range-continue", src), 4);
}

/// `for let mut i=0; i<n; i=i+1` with `continue` must compile and produce correct results.
#[test]
fn cstyle_for_with_continue_compiles_and_runs() {
    // Same sum: 0+1+3 = 4
    let src = "fn main() -> i64 {\n\
               let mut t = 0\n\
               for let mut i = 0; i < 4; i = i + 1 {\n\
                   if i == 2 { continue }\n\
                   t = t + i\n\
               }\n\
               t\n\
               }\n";
    assert_eq!(compile_and_run("cstyle-continue", src), 4);
}

/// A `continue` inside a nested for targets the INNER loop's counter only.
#[test]
fn nested_for_continue_advances_inner_not_outer() {
    // x=1, y=1: total += 1. x=1, y=2: continue (skip). x=3, y=1: total += 3. x=3, y=2: continue. total=4
    let src = "fn main() -> i64 {\n\
               let a[2]\n\
               a[0] = 1\n\
               a[1] = 3\n\
               let mut total = 0\n\
               for x in a {\n\
                   let b[2]\n\
                   b[0] = 1\n\
                   b[1] = 2\n\
                   for y in b {\n\
                       if y == 2 { continue }\n\
                       total = total + x * y\n\
                   }\n\
               }\n\
               total\n\
               }\n";
    assert_eq!(compile_and_run("nested-continue", src), 4);
}

/// A compiled program that never terminates must FAIL the test, not hang the suite.
///
/// This is the guard for the whole file: several tests here exist precisely to catch loops the
/// compiler leaves spinning, and a run with no deadline turns such a bug into `cargo test`
/// sitting there for ever — which reads as "the suite is slow today", not as a failure. It was
/// measured: with the foreach `continue` bug alive, one test in this file held the suite past
/// two minutes with nothing on screen.
#[test]
fn a_program_that_never_ends_fails_instead_of_hanging_the_suite() {
    let src = "fn main() -> i64 {\n\
               for {\n\
               }\n\
               0\n\
               }\n";
    let inicio = std::time::Instant::now();
    let r = std::panic::catch_unwind(|| compile_and_run("never-ends", src));
    assert!(
        r.is_err(),
        "a program stuck in `for {{}}` must make the test fail, but it returned {:?}",
        r.ok()
    );
    assert!(
        inicio.elapsed() < std::time::Duration::from_secs(60),
        "the deadline must cut the run short; it took {:?}",
        inicio.elapsed()
    );
}

/// The upper bound of `for i in a..b` is evaluated ONCE, on entry, however deep the body buries
/// the assignment that moves it.
///
/// The bound used to go straight into the loop condition, where it is re-read every turn: a body
/// that raised its own bound never finished, and the program compiled without a word. Burying
/// that same assignment one level in, inside an `if`, is the case a top-level-only check misses --
/// and the first fix here did miss it.
#[test]
fn the_bound_of_a_range_for_is_read_once_even_when_moved_inside_a_branch() {
    let src = "fn main() -> i64 {\n\
               let mut n = 4\n\
               let mut vueltas = 0\n\
               for i in 0..n {\n\
                   if vueltas >= 0 {\n\
                       n = n + 1\n\
                   }\n\
                   vueltas = vueltas + 1\n\
                   if vueltas > 50 { break }\n\
               }\n\
               vueltas\n\
               }\n";
    assert_eq!(
        compile_and_run("bound-moved-in-branch", src),
        4,
        "the loop must run 4 times: the bound is what `n` was on entry, not what it becomes"
    );
}

/// The same, with the assignment at the top level of the body.
#[test]
fn the_bound_of_a_range_for_is_read_once() {
    let src = "fn main() -> i64 {\n\
               let mut n = 4\n\
               let mut vueltas = 0\n\
               for i in 0..n {\n\
                   n = n + 1\n\
                   vueltas = vueltas + 1\n\
                   if vueltas > 50 { break }\n\
               }\n\
               vueltas\n\
               }\n";
    assert_eq!(compile_and_run("bound-moved", src), 4);
}

/// A bound that is a CALL is evaluated once, not once per turn.
#[test]
fn a_called_bound_is_evaluated_once_not_once_per_turn() {
    let src = "let mut llamadas = 0\n\
               fn tope() -> i64 { llamadas = llamadas + 1  3 }\n\
               fn main() -> i64 {\n\
                   let mut vueltas = 0\n\
                   for i in 0..tope() { vueltas = vueltas + 1 }\n\
                   llamadas * 10 + vueltas\n\
               }\n";
    assert_eq!(
        compile_and_run("called-bound", src),
        13,
        "one call (10) and three turns (3); re-reading the bound made it four calls"
    );
}

/// A `Slice` may not promise more elements than its backing array holds. Measured before the fix:
/// this program compiled without a word and printed 422181705433056 -- five words of somebody
/// else's stack -- where 100 was the only defensible answer.
#[test]
fn a_slice_may_not_promise_more_than_its_buffer_holds() {
    let dicho = compile_error(
        "slice-overlong",
        "fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
         let s = Slice { ptr: a, len: 9 }\n\
         let mut suma = 0\n\
         for i in 0..s.len { suma = suma + s[i] }\n\
         print_int(suma)\n\
         0\n\
         }\n",
    );
    assert!(
        dicho.contains("len") || dicho.contains("length") || dicho.contains("size"),
        "the refusal must say the promised length is the problem, but said: {dicho}"
    );
}

/// The control: the same program with the honest length compiles and adds up its own four words.
#[test]
fn a_slice_over_its_whole_buffer_still_compiles_and_adds_up() {
    let printed = compile_and_print(
        "slice-exact",
        "fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
         let s = Slice { ptr: a, len: 4 }\n\
         let mut suma = 0\n\
         for i in 0..s.len { suma = suma + s[i] }\n\
         print_int(suma)\n\
         0\n\
         }\n",
    );
    assert_eq!(printed.trim(), "100", "the honest slice must add up its own four words");
}

/// An `import ... as <alias>` must actually link the module's code, not just resolve names.
/// Regression: the alias rewrote names but the module body never reached codegen.
#[test]
fn an_import_alias_compiles_and_runs() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join(format!("behaviour-import-alias-{}", std::process::id()));
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/strings.snd"),
        "pub fn itoa(n: i64) -> i64 { n }",
    )
    .unwrap();
    std::fs::write(
        root.join("main.snd"),
        "import strings as st\nfn main() -> i64 { print_int(st::itoa(7))\n 0 }",
    )
    .unwrap();
    let bin = root.join("main.bin");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(root.join("main.snd"))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "import alias must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, printed) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("import alias program ran for over {DEADLINE:?} without finishing")
    });
    assert_eq!(code, 0, "the program must exit 0");
    assert_eq!(printed.trim(), "7", "st::itoa(7) must print 7");
    let _ = std::fs::remove_dir_all(&root);
}

/// A plain `import` must compile and run, not just resolve names in text.
#[test]
fn an_import_compiles_and_runs() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join(format!("behaviour-import-plain-{}", std::process::id()));
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/strings.snd"),
        "pub fn itoa(n: i64) -> i64 { n }",
    )
    .unwrap();
    std::fs::write(
        root.join("main.snd"),
        "import strings\nfn main() -> i64 { print_int(strings::itoa(7))\n 0 }",
    )
    .unwrap();
    let bin = root.join("main.bin");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(root.join("main.snd"))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "import must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, printed) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("import program ran for over {DEADLINE:?} without finishing")
    });
    assert_eq!(code, 0, "the program must exit 0");
    assert_eq!(printed.trim(), "7", "strings::itoa(7) must print 7");
    let _ = std::fs::remove_dir_all(&root);
}

/// Double-importing the same module must compile and run (no duplicate definitions).
#[test]
fn double_import_compiles_and_runs() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join(format!("behaviour-import-double-{}", std::process::id()));
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/strings.snd"),
        "pub fn itoa(n: i64) -> i64 { n }",
    )
    .unwrap();
    std::fs::write(
        root.join("main.snd"),
        "import strings\nimport strings\nfn main() -> i64 { print_int(strings::itoa(7))\n 0 }",
    )
    .unwrap();
    let bin = root.join("main.bin");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(root.join("main.snd"))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "double import must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, printed) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("double import program ran for over {DEADLINE:?} without finishing")
    });
    assert_eq!(code, 0, "the program must exit 0");
    assert_eq!(printed.trim(), "7", "strings::itoa(7) must print 7");
    let _ = std::fs::remove_dir_all(&root);
}

/// Import + include of the same file must compile and run (no duplicate definitions).
#[test]
fn import_and_include_compiles_and_runs() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let root = dir.join(format!("behaviour-import-include-{}", std::process::id()));
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(
        root.join("lib/strings.snd"),
        "pub fn itoa(n: i64) -> i64 { n }",
    )
    .unwrap();
    std::fs::write(
        root.join("main.snd"),
        "import strings\ninclude \"lib/strings.snd\"\nfn main() -> i64 { print_int(strings::itoa(7))\n 0 }",
    )
    .unwrap();
    let bin = root.join("main.bin");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(root.join("main.snd"))
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        out.status.success(),
        "import+include must compile, but sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (code, printed) = run_with_deadline(&bin).unwrap_or_else(|| {
        panic!("import+include program ran for over {DEADLINE:?} without finishing")
    });
    assert_eq!(code, 0, "the program must exit 0");
    assert_eq!(printed.trim(), "7", "strings::itoa(7) must print 7");
    let _ = std::fs::remove_dir_all(&root);
}

/// A local `Slice` with an out-of-bounds constant index must be rejected at compile time.
/// Before the fix: compiled, printed garbage from an unrelated stack location.
#[test]
fn local_slice_out_of_bounds_is_refused() {
    let dicho = compile_error(
        "local-slice-oob",
        "fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
         let s = Slice { ptr: a, len: 4 }\n\
         print_int(s[7])\n\
         0\n\
         }\n",
    );
    assert!(
        dicho.contains("out of bounds") || (dicho.contains("cannot prove") && dicho.contains("slice")),
        "out-of-bounds index into a local slice must be rejected, but said: {dicho}"
    );
}

/// An unbounded variable index into a local slice is rejected.
#[test]
fn local_slice_unbounded_variable_index_is_refused() {
    let dicho = compile_error(
        "local-slice-unbound",
        "fn f(s: Slice, i: i64) -> i64 { s[i] }\n\
         fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
         let s = Slice { ptr: a, len: 4 }\n\
         print_int(usa(s, 5))\n\
         0\n\
         }\n\
         fn usa(s: Slice, i: i64) -> i64 { let t = s\nt[i] }", // i is a param, unbound
    );
    assert!(
        dicho.contains("cannot prove") && dicho.contains("slice"),
        "unbounded index into local slice must be rejected, but said: {dicho}"
    );
}

/// The canonical loop over a local slice compiles and works correctly.
#[test]
fn local_slice_canonical_loop_works() {
    let printed = compile_and_print(
        "local-slice-loop",
        "fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
         let s = Slice { ptr: a, len: 4 }\n\
         let mut suma = 0\n\
         for i in 0..s.len { suma = suma + s[i] }\n\
         print_int(suma)\n\
         0\n\
         }\n",
    );
    assert_eq!(printed.trim(), "100", "the loop must add up the four slice elements");
}

/// A constant index within the slice's declared length compiles and works.
#[test]
fn local_slice_in_bounds_const_index_works() {
    let printed = compile_and_print(
        "local-slice-const-in",
        "fn main() -> i64 {\n\
         let a[4]\n\
         a[0] = 5  a[1] = 15  a[2] = 25  a[3] = 35\n\
         let s = Slice { ptr: a, len: 4 }\n\
         print_int(s[3])\n\
         0\n\
         }\n",
    );
    assert_eq!(printed.trim(), "35", "in-bounds constant index must return the right value");
}

/// An empty local slice is fine as long as it is not indexed.
#[test]
fn empty_local_slice_without_index_compiles() {
    let printed = compile_and_print(
        "local-slice-empty",
        "fn main() -> i64 {\n\
         let a[4]\n\
         let s = Slice { ptr: a, len: 0 }\n\
         print_int(s.len)\n\
         0\n\
         }\n",
    );
    assert_eq!(printed.trim(), "0", "empty slice with len 0 is fine");
}

/// A slice that arrives from a call is a slice: its indices are proven or refused, and where it
/// was built decides nothing. Before this, `s[3]` on a call-returned slice reached memory with
/// no check at all.
#[test]
fn an_index_into_a_slice_from_a_call_is_refused_without_a_bound() {
    let dicho = compile_error(
        "slice-from-call-oob",
        "fn make(a: i64) -> (s: Slice) { Slice { ptr: a, len: 4 } }\n\
         fn main() -> i64 {\n\
         let buf[8]\n\
         let s = make(buf)\n\
         print_int(s[3])\n\
         0\n\
         }\n",
    );
    assert!(
        dicho.contains("cannot prove") && dicho.contains("slice `s`"),
        "an unbounded index into a call-returned slice must be refused, got: {dicho}"
    );
}

/// And the way out is the one the language already teaches: the loop condition IS the bound, so
/// walking the slice needs no annotation. This pins that the refusal above has a remedy — a
/// check whose only answer is "you cannot write this" would be worse than no check.
#[test]
fn walking_a_slice_from_a_call_needs_no_annotation() {
    let printed = compile_and_print(
        "slice-from-call-loop",
        "fn make(a: i64) -> (s: Slice) { Slice { ptr: a, len: 4 } }\n\
         fn main() -> i64 {\n\
         let mut buf[4]\n\
         buf[0] = 1  buf[1] = 2  buf[2] = 3  buf[3] = 4\n\
         let s = make(buf)\n\
         let mut i = 0\n\
         let mut total = 0\n\
         for i < s.len { total = total + s[i]  i = i + 1 }\n\
         print_int(total)\n\
         0\n\
         }\n",
    );
    assert_eq!(printed.trim(), "10", "the four words must add up to 10");
}

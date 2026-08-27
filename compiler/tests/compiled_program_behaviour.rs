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

    let code = Command::new(&bin)
        .status()
        .expect("the compiled program must be runnable")
        .code()
        .expect("the program must exit normally, not by signal");
    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(Path::new(&bin));
    code
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

    // Run under `timeout` if available (Linux), otherwise run directly.
    // The `timeout` binary kills the process after 10 seconds, preventing
    // an infinite hang from a deadlock bug.
    let run = if Path::new("/usr/bin/timeout").exists() {
        format!("timeout 10 {}", bin.display())
    } else {
        // Fallback: just run directly. If it hangs, the test will be killed
        // by the CI/CI-like timeout.
        format!("{}", bin.display())
    };

    let code = std::process::Command::new("sh")
        .arg("-c")
        .arg(&run)
        .output()
        .expect("the compiled program must be runnable");

    let exit_code_str = code.status.code().map_or("unknown".to_string(), |c| c.to_string());

    if code.status.code() == Some(124) {
        // timeout returned 124 = process was killed by timeout
        panic!("the program hung for 10s — a deadlock in lock+break would cause this\n  stderr: {}", String::from_utf8_lossy(&code.stderr));
    }

    // The count travels on stdout, NOT in the exit status: an exit status is a single BYTE, so
    // 100000 would arrive as 160 and the assertion could never hold whatever the compiler did.
    // Written against the status, this test was red from the day it was born and stayed red for
    // the right-looking reason -- "the lock was not released" -- while the lock was released fine.
    let salida = String::from_utf8_lossy(&code.stdout).trim().to_string();
    assert_eq!(
        salida, "100000",
        "break released the lock and the second loop ran to the end (exit status was {})",
        exit_code_str
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

    // Run under timeout to catch deadlocks from missing futex wake.
    let run = if Path::new("/usr/bin/timeout").exists() {
        format!("timeout 10 {}", bin.display())
    } else {
        format!("{}", bin.display())
    };

    let code = std::process::Command::new("sh")
        .arg("-c")
        .arg(&run)
        .output()
        .expect("the compiled program must be runnable");

    if code.status.code() == Some(124) {
        panic!("the program hung for 10s — missing futex wake causes deadlock\n  stderr: {}", String::from_utf8_lossy(&code.stderr));
    }

    let exit_code = code.status.code().expect("the program must exit normally");
    assert_eq!(exit_code, 42, "second thread acquired the lock after first released it (futex wake works)");

    let _ = std::fs::remove_file(&snd);
    let _ = std::fs::remove_file(&bin);
}

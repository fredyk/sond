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

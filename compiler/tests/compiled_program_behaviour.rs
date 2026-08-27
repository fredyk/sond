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

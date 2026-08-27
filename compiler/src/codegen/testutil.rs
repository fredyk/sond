//! Shared test helpers for the codegen tests: compile a Sond program (hosted,
//! stdout-captured, or freestanding) into a real binary with `cc` and run it.
//! Kept in their own file so the test modules stay under the source-size limit.

use super::*;
use crate::lexer::tokenize;
use crate::parser::Parser;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Pipeline provenance guard: every helper that emits AND runs code must call
/// [`parse_and_lower`] (or a function that delegates to it). This means all checks,
/// desugars, and contract verification run in exactly the same path as `sondc` itself.
/// Without it, a helper can silently skip the pipeline and test a compiler that no
/// user ever runs — the kind of silent divergence that has already bitten us.
///
/// **Rule:** every `compile_and_*` helper in this file must call `parse_and_lower`.
/// Any new compilation helper that wants to skip this path must explain why in code.
///
/// The guard is a function comment — Rust cannot force callers to follow a pattern.
/// What we CAN enforce is that the helpers themselves are the ONLY place `emit_program`
/// (and friends) are called from tests. If a new helper imports emit_* directly, we
/// will catch it in review.
pub(crate) fn parse_function(src: &str) -> Function {
    Parser::new(tokenize(src)).parse_function()
}

/// Parses a program, resolves RFC-0014 ports, and runs the shared compilation
/// pipeline (`lower_program`), so the returned data is ready for code emission
/// — exactly what `sondc` does between parsing and codegen.
pub(crate) fn parse_and_lower(src: &str, target: &str) -> (Vec<Function>, Vec<Global>, Vec<StructDef>) {
    let mut p = Parser::new(tokenize(src));
    let mut funcs = p.parse_program();
    let mut globals = p.take_globals();
    let mut structs = p.take_structs();
    let enums = p.take_enums();
    let ports = p.take_ports();
    let providers = p.take_providers();
    let uses = p.take_uses();
    let spliced = crate::ports::resolve_ports(&ports, &providers, &uses, target)
        .expect("port resolution should succeed in this test");
    funcs.extend(spliced);
    crate::lower_program(&mut funcs, &mut globals, &mut structs, &enums, target)
        .expect("lower_program should succeed in this test");
    (funcs, globals, structs)
}

/// Assembles the emitted program into a real Linux/amd64 binary with `cc`,
/// runs it, and returns its exit code.  Uses the full compilation pipeline
/// (`lower_program`) so the test exercises the same checks and desugars as
/// `sondc`.
pub(crate) fn compile_and_run(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "host");
    let asm = emit_program_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_cg_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    let status = Command::new("cc")
        .arg(&asm_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .expect("cc should be available");
    assert!(status.success(), "assembly failed to compile");
    let code = Command::new(&bin_path).status().unwrap().code().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    code
}

/// Compiles a hosted program, runs it, and returns its stdout.  Uses the full
/// compilation pipeline (`lower_program`)./// Compiles a program that has module-level globals, runs it, and returns its exit code.
/// Used by tests that check the return value of a function with globals.
pub(crate) fn compile_and_run_with_globals(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "host");
    let asm = emit_program_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_cgout_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    assert!(
        Command::new("cc")
            .arg(&asm_path)
            .arg("-o")
            .arg(&bin_path)
            .status()
            .unwrap()
            .success(),
        "compile failed"
    );
    let code = Command::new(&bin_path)
        .status()
        .unwrap()
        .code()
        .unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    code
}

/// Compiles a hosted program, runs it, and returns its stdout.
pub(crate) fn compile_and_capture(src: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "host");
    let asm = emit_program_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_out_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    assert!(
        Command::new("cc")
            .arg(&asm_path)
            .arg("-o")
            .arg(&bin_path)
            .status()
            .unwrap()
            .success(),
        "compile failed"
    );
    let out = Command::new(&bin_path).output().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Links a freestanding program and returns the `AddrAlign` that `.data` and `.rodata` declare
/// in the ELF, plus the address of `sond_ctid_0` (0 when the program has no spawn).
///
/// Freestanding on purpose: a hosted link pulls in crt objects that declare their own alignment,
/// and the merged section inherits the strongest one, so a hosted binary hides an emitter that
/// declares nothing. Alone in its section, the emitter's own declaration is all there is.
pub(crate) fn freestanding_section_alignment(src: &str) -> (u64, u64, u64) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "freestanding");
    let asm = emit_freestanding_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_align_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    let status = Command::new("cc")
        .args(["-nostdlib", "-static"])
        .arg(&asm_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .expect("cc should be available");
    assert!(status.success(), "freestanding link failed");

    let sections = Command::new("readelf")
        .args(["-SW"])
        .arg(&bin_path)
        .output()
        .unwrap();
    let sections = String::from_utf8_lossy(&sections.stdout).to_string();
    // `readelf -SW` puts the alignment in the last column of each section's row.
    let align_of = |name: &str| -> u64 {
        sections
            .lines()
            .find(|l| l.contains(&format!(" {name} ")))
            .and_then(|l| l.split_whitespace().last().map(|s| s.to_string()))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    let syms = Command::new("nm").arg(&bin_path).output().unwrap();
    let ctid = String::from_utf8_lossy(&syms.stdout)
        .lines()
        .find(|l| l.ends_with(" sond_ctid_0"))
        .and_then(|l| u64::from_str_radix(l.split_whitespace().next().unwrap_or("0"), 16).ok())
        .unwrap_or(0);

    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    (align_of(".data"), align_of(".rodata"), ctid)
}

/// Compiles a freestanding program (no libc) and runs it. The binary is
/// statically linked with its own `_start`: the shape of a Sond `/init`.
/// Uses the full compilation pipeline (`lower_program`).
pub(crate) fn compile_and_run_freestanding(src: &str) -> (i32, bool) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "freestanding");
    let asm = emit_freestanding_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_fs_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    let status = Command::new("cc")
        .args(["-nostdlib", "-static"])
        .arg(&asm_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .expect("cc should be available");
    assert!(status.success(), "freestanding link failed");
    // Confirm it is a static binary (no dynamic loader, bootable as /init).
    let is_static = Command::new("file")
        .arg(&bin_path)
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains("statically linked") || s.contains("static-pie")
        })
        .unwrap_or(false);
    let code = Command::new(&bin_path).status().unwrap().code().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    (code, is_static)
}

/// Links an emitted Windows program with the mingw toolchain and runs it under wine,
/// returning its exit code. The whole point of this target is a binary that runs on a machine
/// none of us has, so the test runs it on the closest thing available rather than reading the
/// assembly and hoping. Uses the full compilation pipeline (`lower_program`).
pub(crate) fn compile_and_run_windows(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "windows");
    let mut p2 = Parser::new(tokenize(src));
    let _ = p2.parse_program();
    let externs = p2.take_externs();
    let asm = emit_windows_g(&globals, &structs, &funcs, &externs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_win_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let exe_path = dir.join(format!("{stem}.exe"));
    std::fs::write(&asm_path, asm).unwrap();
    let linked = Command::new("x86_64-w64-mingw32-gcc")
        .arg(&asm_path)
        .arg("-o")
        .arg(&exe_path)
        .status()
        .expect("the mingw toolchain should be available");
    assert!(linked.success(), "the Windows link failed");
    let code = Command::new("wine")
        .arg(&exe_path)
        .env("WINEDEBUG", "-all")
        .status()
        .expect("wine should be available")
        .code()
        .unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&exe_path);
    code
}

/// Compiles a program that has module-level globals, runs it, and returns its stdout.
/// Used by tests that need globals (e.g. lock/atomic tests that share `mtx`, `counter`, and `done`).
pub(crate) fn compile_and_capture_with_globals(src: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, structs) = parse_and_lower(src, "host");
    let asm = emit_program_g(&globals, &structs, &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_cgout_{}_{}", std::process::id(), n);
    let asm_path = dir.join(format!("{stem}.s"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    assert!(
        Command::new("cc")
            .arg(&asm_path)
            .arg("-o")
            .arg(&bin_path)
            .status()
            .unwrap()
            .success(),
        "compile failed"
    );
    let out = Command::new(&bin_path).output().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    String::from_utf8_lossy(&out.stdout).to_string()
}

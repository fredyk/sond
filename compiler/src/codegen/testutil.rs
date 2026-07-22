//! Shared test helpers for the codegen tests: compile a Sond program (hosted,
//! stdout-captured, or freestanding) into a real binary with `cc` and run it.
//! Kept in their own file so the test modules stay under the source-size limit.

use super::*;
use crate::lexer::tokenize;
use crate::parser::Parser;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn parse_function(src: &str) -> Function {
    Parser::new(tokenize(src)).parse_function()
}

/// Parses a program and runs the same RFC-0014 port resolution + splicing the
/// driver does for the hosted target, so a `use Port` program reaches codegen
/// with its provider's methods present. A program with no ports is unaffected
/// (resolution returns nothing to splice).
pub(crate) fn parse_and_resolve_ports(src: &str) -> Vec<Function> {
    let mut p = Parser::new(tokenize(src));
    let mut funcs = p.parse_program();
    let ports = p.take_ports();
    let providers = p.take_providers();
    let uses = p.take_uses();
    let spliced = crate::ports::resolve_ports(&ports, &providers, &uses, "host")
        .expect("port resolution should succeed in this test");
    funcs.extend(spliced);
    funcs
}

/// Assembles the emitted program into a real Linux/amd64 binary with `cc`,
/// runs it, and returns its exit code.
pub(crate) fn compile_and_run(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let asm = emit_program(&parse_and_resolve_ports(src));
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

/// Compiles a hosted program, runs it, and returns its stdout.
pub(crate) fn compile_and_capture(src: &str) -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let asm = emit_program(&Parser::new(tokenize(src)).parse_program());
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

/// Compiles a freestanding program (no libc) and runs it. The binary is
/// statically linked with its own `_start`: the shape of a Sond `/init`.
pub(crate) fn compile_and_run_freestanding(src: &str) -> (i32, bool) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let asm = emit_freestanding(&Parser::new(tokenize(src)).parse_program());
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

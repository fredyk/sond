//! `sondc` command-line driver.
//!
//! Usage: `sondc <input.snd> [-o <output>] [--freestanding]`
//!
//! Compiles a single-function Sond source file to a native Linux/amd64
//! executable. By default it is hosted (linked with `cc`) and the function's
//! value becomes the process exit code. With `--freestanding` it links a static,
//! libc-free binary with its own `_start`: the shape of an initramfs `/init`
//! (the `kernel` profile). This is the tangible x86-64 compiler; it grows
//! alongside the backend.

use sondc::codegen::{
    emit_freestanding_g, emit_program_coverage_g, emit_program_debug_g, emit_program_g,
    emit_shared_g, emit_uefi_g, emit_windows_g,
};
use sondc::lexer::tokenize_located;
use sondc::parser::Parser;
use sondc::verify::verify_with_globals;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: sondc <input.snd> [-o <output>] [--freestanding | --uefi | --windows | --shared | --object]"
        );
        return ExitCode::from(2);
    }

    // The input is the first positional argument, skip flags (`--test`, `-o`, ...) and
    // the value that follows `-o`, so the source file may appear in any order.
    let mut input: Option<&String> = None;
    let mut skip_next = false;
    for a in &args[1..] {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-o" {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        input = Some(a);
        break;
    }
    let Some(input) = input else {
        eprintln!(
            "usage: sondc <input.snd> [-o <output>] [--test | --freestanding | --uefi | --windows | --shared | --object]"
        );
        return ExitCode::from(2);
    };
    let output = parse_output_flag(&args).unwrap_or_else(|| default_output(input));
    let freestanding = args.iter().any(|a| a == "--freestanding");
    let uefi = args.iter().any(|a| a == "--uefi");
    let windows = args.iter().any(|a| a == "--windows");
    let shared = args.iter().any(|a| a == "--shared");
    // A RELOCATABLE OBJECT with the same C ABI as `--shared`, for hosts that cannot load a shared
    // object at all: a Go binary for a distroless/static image has no dynamic loader, so the only
    // way to call Sond from it is to link the code in at build time. Same emitted code, stopped
    // one step earlier.
    let object = args.iter().any(|a| a == "--object");
    // The PIC16 target (16F684): emit MPASM/gpasm assembly and assemble it into a flashable
    // Intel HEX, the smallest target profile, an 8-bit microcontroller.
    let pic16 = args.iter().any(|a| a == "--pic16");
    let emit_header = args.iter().any(|a| a == "--emit-header");
    let debug = args.iter().any(|a| a == "--debug");

    // Resolve `include "lib.snd"` directives (source-level, deduped) so multi-file
    // programs (a reusable library and the code that uses it) compile as one. `cov_scope`
    // is the set of functions coverage should measure (the entry's own code, not its
    // already-tested transitive dependencies).
    // Silence the default panic hook HERE, before anything that can panic. Include resolution
    // tokenizes every file it pulls in, so a character the lexer refuses used to escape as a raw
    // Rust backtrace ("panicked at src/lexer.rs:343") with no file name and no line: the one
    // message a compiler must never show, because it names the compiler's own source instead of
    // the user's. The hook used to be set further down, after this phase.
    std::panic::set_hook(Box::new(|_| {}));
    let input_for_resolve = input.clone();
    let (mut src, cov_scope) = match std::panic::catch_unwind(move || {
        sondc::resolve::resolve_includes_scoped(std::path::Path::new(&input_for_resolve))
    })
    .unwrap_or_else(|e| {
        Err(std::io::Error::other(format!(
            "lex error while reading {input}: {}",
            panic_message(&e)
        )))
    }) {
        Ok(s) => s,
        Err(e) => {
            // `e` already names the file that failed (the entry, or the nested include that
            // could not be resolved), so it is not re-blamed on `input` here.
            eprintln!("sondc: {e}");
            return ExitCode::FAILURE;
        }
    };
    // RFC-0016: in a --debug build, append the debug agent, WRITTEN IN SOND (see `debug_agent`). The
    // per-statement checkpoint (emitted by codegen) calls `__dbg_step(line)`; the agent is the Sond
    // that serves the socket step protocol. Appended after include resolution, so it depends only on
    // builtins (syscall / poke / peek).
    if debug {
        src.push_str(sondc::debug_agent::SOURCE);
    }

    // Compilation cache: keyed on the resolved source, the target, and a
    // fingerprint of the compiler itself. On a hit, copy the cached binary and
    // skip the whole pipeline (parse/verify/codegen/link).
    let target_tag = if uefi {
        "uefi"
    } else if windows {
        "windows"
    } else if freestanding {
        "freestanding"
    } else if object {
        "object"
    } else if shared {
        "shared"
    } else if debug {
        // A --debug build has extra instrumentation, so it must not share a cache entry with the
        // normal build of the same source (they are different binaries).
        "host-debug"
    } else {
        "host"
    };
    // Test builds (`--test`, with or without `--coverage`/`--min-coverage`) are ephemeral
    // (synthesized per invocation and run once), and their output depends on flags not in the
    // cache key (the source alone). Never cache them, so a changed threshold or mode always
    // rebuilds.
    let is_test_build = args.iter().any(|a| a == "--test");
    // The PIC16 path uses gpasm and produces a HEX, not an ELF/PE, keep it out of the x86
    // binary cache (whose hit message and copied artefact assume a native binary).
    let cache_path = if is_test_build || pic16 {
        None
    } else {
        build_cache_path(&src, target_tag)
    };
    if let Some(cp) = &cache_path
        && cp.exists()
        && std::fs::copy(cp, &output).is_ok()
    {
        println!("sondc: wrote native x86-64 binary `{output}` (cached)");
        return ExitCode::SUCCESS;
    }

    // The parser and backend still `panic!`/`todo!` on constructs they do not
    // support yet. Catch that so the user (and the qwen example-writers, who
    // compile their own code) get a clean one-line error instead of a Rust
    // backtrace, a good oracle explains the rejection. (The hook is already
    // silenced above, before include resolution, which can panic too.)
    let src_for_parse = src.clone();
    let (mut funcs, mut globals, mut structs, externs, enums, ports, providers, uses) =
        match std::panic::catch_unwind(move || {
            let (toks, lines) = tokenize_located(&src_for_parse);
            // Keep the token stream: it is the only count of `test` blocks the parser cannot
            // influence, and comparing the two is what catches a parse that quietly dropped some.
            let declared_tests = toks.clone();
            let mut p = Parser::with_lines(toks, lines);
            let funcs = p.parse_program();
            let globals = p.take_globals();
            let structs = p.take_structs();
            let externs = p.take_externs();
            let enums = p.take_enums();
            let ports = p.take_ports();
            let providers = p.take_providers();
            let uses = p.take_uses();
            sondc::testing::check_all_tests_registered(&declared_tests, &funcs)
                .unwrap_or_else(|e| panic!("{e}"));
            (
                funcs, globals, structs, externs, enums, ports, providers, uses,
            )
        }) {
            Ok(fg) => fg,
            Err(e) => {
                eprintln!("sondc: parse error in {input}: {}", panic_message(&e));
                return ExitCode::FAILURE;
            }
        };

    // RFC-0016 §3.3: in a --debug build, fold the line-to-locals symbol table into the program as
    // data globals the agent reads, so it can name the debuggee's locals. Added before the checks and
    // the prover so the agent's references to these globals resolve like any other.
    if debug {
        globals.extend(sondc::debug_symtab::build_symtab_globals(&funcs));
    }

    // RFC-0014: bind each `use Port` to the one provider for the active target. A missing,
    // ambiguous, or signature-incomplete provider is a compile error here, before lowering.
    // The selected providers' methods are spliced into the function list, so the rest of the
    // pipeline (typeck, codegen) sees them as ordinary functions.
    let port_target = if pic16 { "pic16" } else { target_tag };
    match sondc::ports::resolve_ports(&ports, &providers, &uses, port_target) {
        Ok(spliced) => funcs.extend(spliced),
        Err(msg) => {
            eprintln!("sondc: {msg}");
            return ExitCode::FAILURE;
        }
    }

    // `test "..." { .. }` blocks are inert in an ordinary compile, only `sond test`
    // gathers and runs them. In `--test` mode, keep the tests, drop the file's own
    // `main`, and synthesize a harness `main` that runs each test; otherwise drop the
    // tests so a source file may carry them without pulling test-only calls (assert, ...)
    // into the shipped program.
    let test_mode = args.iter().any(|a| a == "--test");
    let coverage = args.iter().any(|a| a == "--coverage");
    if test_mode && !funcs.iter().any(|f| f.is_test) {
        eprintln!(
            "sondc: no `test` blocks found: Sond requires tests giving at least 80% coverage \
             to build. Add `test \"...\" {{ ... }}` blocks (and `include \
             \"x86_64/linux/testing.snd\"`)."
        );
        return ExitCode::FAILURE;
    }
    if test_mode {
        funcs.retain(|f| f.name != "main");
        let min_coverage = args
            .iter()
            .find_map(|a| a.strip_prefix("--min-coverage="))
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(80);
        let harness = if coverage {
            sondc::testing::synth_coverage_harness(&funcs, min_coverage)
        } else {
            sondc::testing::synth_harness(&funcs)
        };
        funcs.push(harness);
    } else {
        funcs.retain(|f| !f.is_test);
    }

    // The shared compilation pipeline: checks and desugars, used by both the
    // driver and the codegen test helpers so the two paths can never diverge.
    let lower_target = if pic16 {
        "pic16"
    } else if debug {
        "host-debug"
    } else {
        "host"
    };
    if let Err(msg) = sondc::lower_program(&mut funcs, &mut globals, &mut structs, &enums, lower_target) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }

    if emit_header {
        print!(
            "{}",
            sondc::header::emit_header(&src, &funcs, &structs, &globals, &ports)
        );
        return ExitCode::SUCCESS;
    }

    // A Sond program is a set of `fn` declarations. If parsing produced none, the
    // source starts with something else (e.g. a top-level `let`: module-level
    // statements are not supported). Fail clearly instead of emitting a broken
    // binary that references an undefined entry point.
    if funcs.is_empty() {
        eprintln!(
            "sondc: no functions found: a Sond program is made of `fn` declarations \
             (top-level statements such as `let` at module scope are not supported yet)"
        );
        return ExitCode::FAILURE;
    }

    // The oracle: prove the contracts before emitting anything. A postcondition
    // that cannot hold is a compile error, the program is rejected, not run.
    if let Err(e) = verify_with_globals(&funcs, &globals) {
        eprintln!("sondc: contract violation: {}", e.message);
        return ExitCode::FAILURE;
    }

    // Lower struct-field access in the contracts NOW, after the prover saw the source-level
    // form (`s.len`, readable), so the runtime guard codegen receives the lowered slot read.
    sondc::desugar::desugar_contracts(&mut funcs, &structs);

    let asm = match std::panic::catch_unwind(|| {
        if pic16 {
            sondc::codegen::pic16::emit_pic16(&funcs)
        } else if uefi {
            emit_uefi_g(&globals, &structs, &funcs)
        } else if windows {
            emit_windows_g(&globals, &structs, &funcs, &externs)
        } else if freestanding {
            emit_freestanding_g(&globals, &structs, &funcs)
        } else if shared || object {
            emit_shared_g(&globals, &structs, &funcs)
        } else if coverage {
            emit_program_coverage_g(&globals, &structs, &funcs, &cov_scope)
        } else if debug {
            emit_program_debug_g(&globals, &structs, &funcs)
        } else {
            emit_program_g(&globals, &structs, &funcs)
        }
    }) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "sondc: unsupported construct in {input}: {}",
                panic_message(&e)
            );
            return ExitCode::FAILURE;
        }
    };

    // The PIC16 target assembles with `gpasm` into a flashable Intel HEX, a wholly different
    // toolchain from the x86 ELF/PE path below, so it short-circuits here.
    if pic16 {
        let asm_path = format!("{output}.asm");
        if let Err(e) = std::fs::write(&asm_path, &asm) {
            eprintln!("sondc: cannot write {asm_path}: {e}");
            return ExitCode::FAILURE;
        }
        // gpasm chooses output format from the last dot in the FULL output path
        // (directories included), so a dot in the directory name (e.g. worktree
        // `foo.bar`) silently prevents .hex creation even with exit 0.
        // Always pass a path whose *file* ends in `.hex` so gpasm has no other
        // choice, and verify afterward that the file is actually on disk.
        let hex_path = ensure_pic16_output(&output);
        let pic16_status = Command::new("gpasm")
            .args(["-p", "p16f684"])
            .arg(&asm_path)
            .args(["-o", &hex_path])
            .status();
        let pic16_msg = match pic16_result(pic16_status, Path::new(&hex_path)) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("sondc: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!("{pic16_msg}");
        return ExitCode::SUCCESS;
    }

    let asm_path = format!("{output}.s");
    if let Err(e) = std::fs::write(&asm_path, &asm) {
        eprintln!("sondc: cannot write {asm_path}: {e}");
        return ExitCode::FAILURE;
    }

    // UEFI apps are PE32+/EFI (Microsoft ABI), linked with a PE toolchain;
    // everything else is a native ELF via `cc`.
    let status = if windows {
        // A console PE, linked by the same mingw toolchain the UEFI target uses. No `--subsystem`
        // flag: console is mingw's default, and it is what a tool that prints wants.
        Command::new("x86_64-w64-mingw32-gcc")
            .arg(&asm_path)
            .arg("-o")
            .arg(&output)
            .status()
    } else if uefi {
        Command::new("x86_64-w64-mingw32-gcc")
            .args(["-nostdlib", "-ffreestanding", "-e", "efi_main"])
            .args(["-Wl,--subsystem,10"])
            // The mingw linker defaults to marking the image DYNAMIC_BASE (ASLR)
            // but Sond emits no .reloc section, so the PE claims it is relocatable
            // while giving no relocations. Lenient loaders (OVMF) ignore this and
            // load at ImageBase, but a strict UEFI loader (VirtualBox's firmware)
            // refuses to hand off. Disable dynamic base + high-entropy VA so the
            // image is honestly load-at-ImageBase and boots on both.
            .args(["-Wl,--disable-dynamicbase", "-Wl,--disable-high-entropy-va"])
            .arg(&asm_path)
            .arg("-o")
            .arg(&output)
            .status()
    } else if object {
        // Assemble only. The result is an ordinary ELF relocatable that any toolchain can link
        // in: `cc prog.o main.c`, `ar rcs libX.a prog.o`, or cgo's LDFLAGS. Nothing from a C
        // runtime is needed, since Sond emits none.
        Command::new("cc")
            .arg("-c")
            .arg(&asm_path)
            .arg("-o")
            .arg(&output)
            .status()
    } else if shared {
        // A C-ABI shared object: position-independent, no C runtime (Sond is
        // libc-free), exported `pub fn` in the dynamic symbol table. Importable from
        // C/C++/Go/Rust/Python. Name it `libNAME.so` for link-time `-lNAME`.
        Command::new("cc")
            .args(["-shared", "-nostdlib"])
            .arg(&asm_path)
            .arg("-o")
            .arg(&output)
            .status()
    } else {
        let mut cc = Command::new("cc");
        if freestanding {
            cc.args(["-nostdlib", "-static"]);
        }
        cc.arg(&asm_path).arg("-o").arg(&output).status()
    };
    match status {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&asm_path);
            // Populate the cache so the next identical compile is instant.
            if let Some(cp) = &cache_path {
                if let Some(parent) = cp.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&output, cp);
            }
            println!("sondc: wrote native x86-64 binary `{output}`");
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("sondc: assembler/linker (cc) failed");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("sondc: could not run cc: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The cache path for a compile: `~/.cache/sond/build/<hash>-<target>`, where the
/// hash covers the resolved source, the target, and a fingerprint of `sondc` itself
/// (so a newer compiler invalidates stale artifacts).
fn build_cache_path(src: &str, target: &str) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let home = std::env::var_os("HOME")?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    target.hash(&mut h);
    if let Ok(exe) = std::env::current_exe()
        && let Ok(m) = std::fs::metadata(&exe)
    {
        m.len().hash(&mut h);
        if let Ok(mt) = m.modified()
            && let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH)
        {
            d.as_secs().hash(&mut h);
        }
    }
    let mut p = std::path::PathBuf::from(home);
    p.push(".cache/sond/build");
    p.push(format!("{:016x}-{target}", h.finish()));
    Some(p)
}

/// Extracts the human-readable message from a caught panic payload.
fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "unsupported construct".to_string())
}

fn parse_output_flag(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn default_output(input: &str) -> String {
    Path::new(input)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a")
        .to_string()
}

/// Pic16/gpasm always produces Intel HEX.  gpasm decides the output extension by looking at the
/// last dot in the *full* output path (directory components included), so a dot in the directory
/// name (e.g. worktree `foo.bar`) can silently prevent `.hex` creation.  We always pass a path
/// whose **file** ends in `.hex` so gpasm has no other option.  If the user already asked for
/// `.hex` we do not double it.
fn ensure_pic16_output(user_output: &str) -> String {
    if user_output.to_lowercase().ends_with(".hex") {
        user_output.to_string()
    } else {
        format!("{user_output}.hex")
    }
}

/// Turns the assembler's outcome into what the user is told, given the HEX path that was
/// asked for. Split out of `main` so it can be tested without ever running gpasm.
///
/// The case that matters is the third one: gpasm exits 0 whether or not it wrote anything,
/// so success is only success once the file is on disk.
///
/// - could not even be launched -> tell the user to install gputils
/// - ran and failed            -> report the failure, without looking at the disk
/// - ran, succeeded, no file   -> an error naming the file that is missing
/// - ran, succeeded, file there -> the message announcing it
fn pic16_result(
    status: std::io::Result<std::process::ExitStatus>,
    hex_path: &std::path::Path,
) -> Result<String, String> {
    let hex_display = hex_path.display().to_string();
    let s = status.map_err(|e| format!("sondc: cannot run gpasm (install gputils): {e}"))?;

    if !s.success() {
        return Err(format!(
            "sondc: gpasm failed to assemble (exit code: {})",
            s.code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ));
    }

    if !hex_path.exists() {
        return Err(format!(
            "sondc: gpasm returned success but `{hex_display}` does not exist"
        ));
    }

    Ok(format!(
        "sondc: wrote PIC16F684 Intel HEX `{hex_display}` (flash it to the chip)"
    ))
}

#[cfg(test)]
mod pic16_assemble_tests {
    use super::*;
    use std::io;
    use std::process::ExitStatus;

    // A real exit status that succeeded.
    fn success_exit_status() -> ExitStatus {
        Command::new("true").status().expect("true ran")
    }

    // A real exit status that failed.
    fn failure_exit_status() -> ExitStatus {
        Command::new("false").status().expect("false ran")
    }

    #[test]
    fn gpasm_success_but_hex_missing_returns_error_naming_path() {
        let hex_path = std::env::temp_dir().join(format!(
            "sondc_test_no_file_8f3a_2c4d-{}.hex",
            std::process::id()
        ));
        if hex_path.exists() {
            let _ = std::fs::remove_file(&hex_path);
        }
        // Make sure it truly is not there.
        let _ = std::fs::remove_file(&hex_path);
        let result = pic16_result(Ok(success_exit_status()), &hex_path);
        assert!(
            result.is_err(),
            "the assembler succeeded but the file is missing: must be an error; got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("does not exist"),
            "the error must say the file does not exist; got: {err}"
        );
        assert!(
            err.contains("sondc:"),
            "the error must carry the `sondc:` prefix; got: {err}"
        );
    }

    #[test]
    fn gpasm_success_and_hex_exists_returns_ok_naming_path() {
        let hex = std::env::temp_dir().join(format!(
            "sondc_test_exists_a1b2_7e9f-{}.hex",
            std::process::id()
        ));
        // Create the file; removed at the end of the test.
        std::fs::write(&hex, ":020000040000FA02").unwrap();
        let result = pic16_result(Ok(success_exit_status()), &hex);
        assert!(
            result.is_ok(),
            "the file is there: must be Ok; got {result:?}"
        );
        let msg = result.unwrap();
        assert!(
            msg.contains("wrote") && msg.contains(&hex.display().to_string()),
            "the success message must name the file it wrote; got: {msg}"
        );
        assert!(
            msg.contains("sondc:"),
            "the message must carry the `sondc:` prefix; got: {msg}"
        );
        let _ = std::fs::remove_file(&hex);
    }

    #[test]
    fn gpasm_failure_returns_error_without_checking_disk() {
        let hex_path = std::env::temp_dir().join(format!(
            "sondc_test_fail_c3d4_a5b6-{}.hex",
            std::process::id()
        ));
        // The file is deliberately absent: a failed run must not look at the disk at all.
        let result = pic16_result(Ok(failure_exit_status()), &hex_path);
        assert!(
            result.is_err(),
            "a failed assembler run must be an error; got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("failed") || err.contains("exit"),
            "the error must say gpasm failed; got: {err}"
        );
    }

    #[test]
    fn gpasm_cannot_run_returns_install_gputils_error() {
        let hex_path = std::env::temp_dir().join(format!(
            "sondc_test_norun_e5f6_c7d8-{}.hex",
            std::process::id()
        ));
        let io_err = io::Error::new(io::ErrorKind::NotFound, "gpasm: No such file or directory");
        let result = pic16_result(Err(io_err), &hex_path);
        assert!(
            result.is_err(),
            "no se puede lanzar gpasm → error; result={result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_lowercase().contains("gputils") || err.to_lowercase().contains("gpasm"),
            "the error must point at gputils/gpasm; got: {err}"
        );
    }
}

#[cfg(test)]
mod pic16_output_tests {
    use super::*;

    #[test]
    fn no_dot_in_name_becomes_hex() {
        let out = ensure_pic16_output("myprog");
        assert_eq!(out, "myprog.hex");
    }

    #[test]
    fn dot_in_directory_does_not_clobber_extension() {
        // Worktree name "the-ai-language.oc-gpasm-hex" has a dot but the file has none.
        let out = ensure_pic16_output("/work/pic/build/x");
        assert_eq!(out, "/work/pic/build/x.hex");
    }

    #[test]
    fn user_already_provided_hex_no_double() {
        let out = ensure_pic16_output("output.hex");
        assert_eq!(out, "output.hex");
    }

    #[test]
    fn user_already_provided_uppercase_hex_no_double() {
        let out = ensure_pic16_output("output.HEX");
        assert_eq!(out, "output.HEX");
    }

    #[test]
    fn dot_in_directory_with_different_extension_requested() {
        // User asked for .cod but pic16 always writes .hex.
        let out = ensure_pic16_output("/path/to/dir.with.dots/output.cod");
        assert_eq!(out, "/path/to/dir.with.dots/output.cod.hex");
    }

    #[test]
    fn only_filename_no_extension() {
        let out = ensure_pic16_output("blink");
        assert_eq!(out, "blink.hex");
    }

    #[test]
    fn relative_path_with_dir_dot() {
        let out = ensure_pic16_output("adir.oc/x");
        assert_eq!(out, "adir.oc/x.hex");
    }
}

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
        eprintln!("usage: sondc <input.snd> [-o <output>] [--freestanding | --uefi | --windows | --shared | --object]");
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
        eprintln!("usage: sondc <input.snd> [-o <output>] [--test | --freestanding | --uefi | --windows | --shared | --object]");
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
    let (mut src, cov_scope) =
        match std::panic::catch_unwind(move || {
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
            (funcs, globals, structs, externs, enums, ports, providers, uses)
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

    // Value/pointer struct rules: a value struct received as a parameter is
    // read-only (only its creator may mutate it). Reject field mutation of a
    // value-struct param before lowering, declare it `*Point` to mutate.
    if let Err(msg) = sondc::check::check_no_duplicate_functions(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_spawn_not_in_loop(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_par_let_arity(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_par_confluence(&funcs, &globals) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_graph_acyclic(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_no_process_exit_in_spawn(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_no_close_fd_after_spawn(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_syscall_retry_has_an_exit(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_no_raw_access_to_declared_buffers(&funcs) {
        eprintln!("sondc: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = sondc::check::check_receiver_kinds(&funcs) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }
    if let Err(msg) = sondc::check::check_declaration_kinds(&funcs) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }
    if let Err(msg) = sondc::check::check_struct_mutability(&funcs, &structs) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }
    // Exhaustiveness: every `match` must cover all of its enum's variants (checked on
    // the original `match`, before it is lowered to if/else).
    if let Err(msg) = sondc::check::check_enum_matches(&funcs, &enums) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }
    // The built-in `Slice { ptr, len }` fat pointer is available to every program.
    sondc::desugar::inject_builtin_structs(&mut structs);
    // `a.len` on a fixed-size array lowers to its constant size. Done before the constant
    // checks so `a[a.len]` is seen as `a[N]` (a provable out-of-bounds) and `for i < a.len`
    // as `for i < N`.
    sondc::desugar::lower_array_len(&mut funcs, &globals);
    // A slice range `s[lo:hi]` lowers to a sub-`Slice` view. After `a.len` (so an array's
    // `.len` is a constant), before the constant checks and struct lowering.
    sondc::desugar::lower_slice_ranges(&mut funcs);
    // (C) on-ramp for #3 (RFC-0012): a `+`/`-`/`*` of constants that overflows i64 is a
    // compile error, the non-breaking first step of checked-by-default arithmetic.
    if let Err(msg) = sondc::check::check_constant_overflow(&funcs) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }
    // (C) on-ramp for #2 (RFC-0012): a constant index outside a known array's bounds is a
    // compile error, the non-breaking first step of checked array access.
    if let Err(msg) = sondc::check::check_constant_bounds(&funcs, &globals) {
        eprintln!("sondc: {msg}");
        return ExitCode::FAILURE;
    }

    // `--emit-header`: print the module's public interface (contract, no bodies)
    // and stop. This is the `.snd` header for ~/.local/include/sond and dist/include.
    // A payloaded enum lowers to a synthetic `{__tag, __payload}` struct, append those
    // to the struct set so `desugar_structs` lowers the enum's field access and layout.
    let synth = sondc::desugar::desugar_enums(&mut funcs, &enums);
    structs.extend(synth);
    // Materialize struct literals used in expression position (call args, branch tails)
    // into temps, so a constructor works anywhere, before the struct pass lowers them.
    sondc::desugar::hoist_struct_literals(&mut funcs);
    sondc::desugar::desugar_structs(&mut funcs, &structs);

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
        return match Command::new("gpasm")
            .args(["-p", "p16f684"])
            .arg(&asm_path)
            .args(["-o", output.as_str()])
            .status()
        {
            Ok(s) if s.success() => {
                println!("sondc: wrote PIC16F684 Intel HEX `{output}` (flash it to the chip)");
                ExitCode::SUCCESS
            }
            Ok(_) => {
                eprintln!("sondc: gpasm failed to assemble {asm_path}");
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("sondc: cannot run gpasm (install gputils): {e}");
                ExitCode::FAILURE
            }
        };
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

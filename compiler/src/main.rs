//! `sondc` command-line driver.
//!
//! Usage: `sondc <input.snd> [-o <output>] [--freestanding]`
//!
//! Compiles a single-function Sond source file to a native Linux/amd64
//! executable. By default it is hosted (linked with `cc`) and the function's
//! value becomes the process exit code. With `--freestanding` it links a static,
//! libc-free binary with its own `_start`, the shape of an initramfs `/init`
//! (the `kernel` profile). This is the tangible x86-64 compiler; it grows
//! alongside the backend.

use sondc::codegen::{emit_freestanding_g, emit_program_g, emit_shared_g, emit_uefi_g};
use sondc::lexer::tokenize;
use sondc::parser::Parser;
use sondc::resolve::resolve_includes;
use sondc::verify::verify;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: sondc <input.snd> [-o <output>] [--freestanding | --uefi | --shared]");
        return ExitCode::from(2);
    }

    let input = &args[1];
    let output = parse_output_flag(&args).unwrap_or_else(|| default_output(input));
    let freestanding = args.iter().any(|a| a == "--freestanding");
    let uefi = args.iter().any(|a| a == "--uefi");
    let shared = args.iter().any(|a| a == "--shared");
    let emit_header = args.iter().any(|a| a == "--emit-header");

    // Resolve `include "lib.snd"` directives (source-level, deduped) so multi-file
    // programs, a reusable library and the code that uses it, compile as one.
    let src = match resolve_includes(std::path::Path::new(input)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sondc: cannot read {input}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Compilation cache: keyed on the resolved source, the target, and a
    // fingerprint of the compiler itself. On a hit, copy the cached binary and
    // skip the whole pipeline (parse/verify/codegen/link).
    let target_tag = if uefi {
        "uefi"
    } else if freestanding {
        "freestanding"
    } else if shared {
        "shared"
    } else {
        "host"
    };
    let cache_path = build_cache_path(&src, target_tag);
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
    // backtrace, a good oracle explains the rejection.
    std::panic::set_hook(Box::new(|_| {}));
    let src_for_parse = src.clone();
    let (mut funcs, globals, mut structs, enums) = match std::panic::catch_unwind(move || {
        let mut p = Parser::new(tokenize(&src_for_parse));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        let structs = p.take_structs();
        let enums = p.take_enums();
        (funcs, globals, structs, enums)
    }) {
        Ok(fg) => fg,
        Err(e) => {
            eprintln!("sondc: parse error in {input}: {}", panic_message(&e));
            return ExitCode::FAILURE;
        }
    };

    // Value/pointer struct rules: a value struct received as a parameter is
    // read-only (only its creator may mutate it). Reject field mutation of a
    // value-struct param before lowering, declare it `*Point` to mutate.
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
            sondc::header::emit_header(&src, &funcs, &structs, &globals)
        );
        return ExitCode::SUCCESS;
    }

    // A Sond program is a set of `fn` declarations. If parsing produced none, the
    // source starts with something else (e.g. a top-level `let`, module-level
    // statements are not supported). Fail clearly instead of emitting a broken
    // binary that references an undefined entry point.
    if funcs.is_empty() {
        eprintln!(
            "sondc: no functions found, a Sond program is made of `fn` declarations \
             (top-level statements such as `let` at module scope are not supported yet)"
        );
        return ExitCode::FAILURE;
    }

    // The oracle: prove the contracts before emitting anything. A postcondition
    // that cannot hold is a compile error, the program is rejected, not run.
    if let Err(e) = verify(&funcs) {
        eprintln!("sondc: contract violation, {}", e.message);
        return ExitCode::FAILURE;
    }

    let asm = match std::panic::catch_unwind(|| {
        if uefi {
            emit_uefi_g(&globals, &structs, &funcs)
        } else if freestanding {
            emit_freestanding_g(&globals, &structs, &funcs)
        } else if shared {
            emit_shared_g(&globals, &structs, &funcs)
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

    let asm_path = format!("{output}.s");
    if let Err(e) = std::fs::write(&asm_path, &asm) {
        eprintln!("sondc: cannot write {asm_path}: {e}");
        return ExitCode::FAILURE;
    }

    // UEFI apps are PE32+/EFI (Microsoft ABI), linked with a PE toolchain;
    // everything else is a native ELF via `cc`.
    let status = if uefi {
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

//! Native x86-64 code generation (Linux, System V AMD64 ABI, AT&T syntax).
//!
//! The first native backend for Sond. It emits assembly that `cc`/`as` turn
//! into a real Linux/amd64 executable. Handles integer expressions (constants,
//! `+ - *`), booleans, comparisons, `if/else` control flow, local `let`
//! bindings (stack slots) and `return`; grows via TDD.
//!
//! Strategy: a simple accumulator/stack scheme, every expression leaves its
//! result in `%rax`; binary operators push the left operand while the right is
//! computed; locals live in the function's stack frame. Correct (if not yet
//! optimal); a register allocator comes later.

use crate::ast::{
    BinOp, Block, Expr, Function, Global, Signature, Stmt, StructDef, Type, UnOp, Visibility,
};
use std::collections::{HashMap, HashSet};

/// System V AMD64 integer argument registers, in order.
const ARG_REGS: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// System V AMD64 SSE argument registers (`f64` params/returns live here), in order.
/// The Microsoft x64 integer argument registers, in order. Used for `extern fn` calls on the
/// Windows target; everything else in Sond uses System V.
pub(crate) const MS_ARG_REGS: [&str; 4] = ["%rcx", "%rdx", "%r8", "%r9"];

const SSE_ARG_REGS: [&str; 8] = [
    "%xmm0", "%xmm1", "%xmm2", "%xmm3", "%xmm4", "%xmm5", "%xmm6", "%xmm7",
];

/// The value kind the codegen distinguishes. Integers/pointers are INTEGER class (live
/// in `%rax`/`%rdi..`) and carry SIGNEDNESS (`I64` (signed) vs `U64` (unsigned)), which
/// selects the right shift/divide/compare instruction (killing the `>>`/`>>>` footgun).
/// `F64` is the SSE class (`%xmm0..`). Everything in Sond is `I64` unless typed `u64`/`f64`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F64,
}

impl Ty {
    pub(crate) fn is_float(self) -> bool {
        self == Ty::F64
    }
    /// Whether an integer of this kind is unsigned (selects `shr`/`div`/`setb..`).
    pub(crate) fn is_unsigned(self) -> bool {
        matches!(self, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64)
    }
    /// The integer width in bits (narrow integers are canonicalized to it after ops).
    pub(crate) fn bits(self) -> u32 {
        match self {
            Ty::I8 | Ty::U8 => 8,
            Ty::I16 | Ty::U16 => 16,
            Ty::I32 | Ty::U32 => 32,
            _ => 64,
        }
    }
    /// The integer kind of a given width and signedness.
    fn int(bits: u32, unsigned: bool) -> Ty {
        match (bits, unsigned) {
            (8, false) => Ty::I8,
            (8, true) => Ty::U8,
            (16, false) => Ty::I16,
            (16, true) => Ty::U16,
            (32, false) => Ty::I32,
            (32, true) => Ty::U32,
            (_, false) => Ty::I64,
            (_, true) => Ty::U64,
        }
    }
}

/// One element of a module-level table: a constant number, or the ADDRESS of a string literal's
/// bytes in `.rodata`. A literal's address is as much a compile-time constant as an integer is,
/// which is what makes a fixed list of texts a table of pointers rather than a runtime build-up.
#[derive(Clone, PartialEq)]
pub(crate) enum GlobalWord {
    Num(i64),
    Label(String),
}

impl GlobalWord {
    fn is_zero(&self) -> bool {
        *self == GlobalWord::Num(0)
    }
    fn operand(&self) -> String {
        match self {
            GlobalWord::Num(n) => n.to_string(),
            GlobalWord::Label(l) => l.clone(),
        }
    }
}

/// The written form of an element type, for error messages that quote the user's own spelling.
fn type_name_of(ty: &Type) -> String {
    match ty {
        Type::Named(n) => n.clone(),
        Type::Ptr(inner) => format!("*{}", type_name_of(inner)),
        Type::View(inner, _) => format!("{}[]", type_name_of(inner)),
    }
}

/// Whether a declared type is `f64` (the SSE-class float).
fn is_float_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n) if n == "f64")
}

/// The value kind of a declared type.
pub(crate) fn kind_of_type(ty: &Type) -> Ty {
    let Type::Named(n) = ty else {
        return Ty::I64;
    };
    match n.as_str() {
        "f64" => Ty::F64,
        "i8" => Ty::I8,
        "u8" => Ty::U8,
        "i16" => Ty::I16,
        "u16" => Ty::U16,
        "i32" => Ty::I32,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        _ => Ty::I64,
    }
}

/// The value kind a function returns (for typing a call expression).
fn ret_kind(f: &Function) -> Ty {
    f.ret.as_ref().map_or(Ty::I64, kind_of_type)
}

/// The kind of an arithmetic result over two operands: float wins; otherwise the wider
/// width wins, and unsigned wins over signed (an unsigned operand makes it unsigned).
pub(crate) fn combine_kind(a: Ty, b: Ty) -> Ty {
    if a.is_float() || b.is_float() {
        return Ty::F64;
    }
    Ty::int(a.bits().max(b.bits()), a.is_unsigned() || b.is_unsigned())
}

/// GP registers an interrupt handler pushes on entry and pops on exit (every GP
/// register except %rsp, which the interrupt frame restores, and %rbp, which the
/// function frame already saves). Pop order is the reverse of this list.
const ISR_SAVED_REGS: [&str; 14] = [
    "%rax", "%rcx", "%rdx", "%rbx", "%rsi", "%rdi", "%r8", "%r9", "%r10", "%r11", "%r12", "%r13",
    "%r14", "%r15",
];

/// The position of the entry function (matched by `name`, else the last one).
fn entry_pos(funcs: &[Function], name: &str) -> usize {
    funcs
        .iter()
        .position(|f| f.name == name)
        .unwrap_or(funcs.len().saturating_sub(1))
}

/// Emits every function. The entry function goes under the internal label
/// `sond_start` (so its Sond name can't collide with the platform entry point);
/// the rest keep their names and are user-callable.
fn emit_all(cg: &mut Codegen, structs: &[StructDef], funcs: &[Function], entry: &str) {
    let ep = entry_pos(funcs, entry);
    // The entry function is compiled under the internal label `sond_start`, not its
    // own name.  For a library file with no `main`, the last function becomes the
    // entry and other functions in the same file may call it (a forward reference).
    // Without a trampoline the call lands in the builtin catch-all (`todo!()`).
    // Fix: when the entry's name differs from the platform's C entry point, add it
    // to `fn_names` so the codegen emits `call <name>`, and emit a `jmp sond_start`
    // trampoline from that name so the call actually reaches the function body.
    let entry_name = &funcs[ep].name;
    let needs_trampoline =
        entry_name != "main" && entry_name != "efi_main" && entry_name != "_start";
    cg.fn_names = funcs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != ep)
        .map(|(_, f)| f.name.clone())
        .collect();
    if needs_trampoline {
        cg.fn_names.insert(entry_name.clone());
    }
    cg.fn_ret_types = funcs
        .iter()
        .map(|f| (f.name.clone(), ret_kind(f)))
        .collect();
    cg.set_struct_sizes(structs);
    // Per-function effect footprints, so a `graph` can serialize two same-level nodes that would
    // race on a mutable global (RFC-0008 §5.2). Only needed when there is shared mutable state.
    if !cg.mut_globals.is_empty() {
        cg.graph_effects = Some(crate::check::compute_global_effects(funcs, &cg.mut_globals));
    }
    for (i, f) in funcs.iter().enumerate() {
        if i == ep {
            cg.emit_function(f, "sond_start");
            if needs_trampoline {
                cg.line(&format!(".globl {entry_name}"));
                cg.line(&format!("{entry_name}:"));
                cg.line("    jmp sond_start");
            }
        } else {
            cg.emit_function(f, &f.name.clone());
        }
    }
}

/// Emits a **hosted** program: all functions plus a C-compatible `main` that
/// calls the entry and returns its value as the process exit code (linked `cc`).
pub fn emit_program(funcs: &[Function]) -> String {
    emit_program_g(&[], &[], funcs)
}

/// Like [`emit_program`], but with module-level globals and struct definitions (the
/// latter so a function may return a small struct/enum by value).
pub fn emit_program_g(globals: &[Global], structs: &[StructDef], funcs: &[Function]) -> String {
    emit_program_maybe_debug(globals, structs, funcs, false)
}

/// Like [`emit_program_g`] but with the RFC-0016 `--debug` checkpoint instrumentation (a per-
/// statement `__dbg_step(line)` trace). A separate entry so a normal build stays byte-identical.
pub fn emit_program_debug_g(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
) -> String {
    emit_program_maybe_debug(globals, structs, funcs, true)
}

fn emit_program_maybe_debug(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
    debug: bool,
) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        debug,
        ..Default::default()
    };
    cg.set_globals(globals);
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl main");
    cg.line("main:");
    // The C runtime hands us argc in %rdi and argv in %rsi, stash them so the
    // argc()/argv() builtins can read the process arguments.
    cg.line("    movq %rdi, sond_argc(%rip)");
    cg.line("    movq %rsi, sond_argv(%rip)");
    cg.line("    movq %rdx, sond_envp(%rip)");
    // Reserve %r15 as the thread-local arena pointer. It is callee-saved, so glibc (which called
    // main) expects it preserved: save it, point it at the default arena control block, and
    // restore it before returning.
    cg.line("    pushq %r15");
    cg.line("    leaq sond_arena_tcb(%rip), %r15");
    cg.line("    call sond_start");
    cg.line("    popq %r15");
    cg.line("    ret");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line(".p2align 3");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.emit_gnu_stack_note();
    cg.out
}

/// Emits a **Windows console** program: a PE32+ that the Windows loader accepts, linked with the
/// mingw toolchain the UEFI target already uses. Same code generation as the hosted target — the
/// instruction set is the same machine — but the platform differences are real and stated here:
/// the entry receives its arguments under the Microsoft x64 ABI (`%rcx`/`%rdx`), not the System V
/// one, and there is no `.note.GNU-stack` section in a PE.
pub fn emit_windows_g(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
    externs: &[Signature],
) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    cg.extern_fns = externs.iter().map(|e| e.name.clone()).collect();
    cg.fn_ret_types.extend(
        externs
            .iter()
            .map(|e| (e.name.clone(), e.ret.as_ref().map_or(Ty::I64, kind_of_type))),
    );
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl main");
    cg.line("main:");
    // Microsoft x64: argc in %rcx, argv in %rdx (System V puts them in %rdi/%rsi). The builtins
    // read the same two cells either way, so the difference is confined to this handshake.
    cg.line("    movq %rcx, sond_argc(%rip)");
    cg.line("    movq %rdx, sond_argv(%rip)");
    // %r15 is callee-saved in BOTH ABIs, and the C runtime that called us expects it back.
    cg.line("    pushq %r15");
    cg.line("    leaq sond_arena_tcb(%rip), %r15");
    // The Microsoft ABI requires 32 bytes of SHADOW SPACE below the return address for any call,
    // reserved by the CALLER, plus 16-byte stack alignment at the call. `sond_start` is Sond's
    // own code and uses neither, but anything it calls into on this platform will, so the space
    // is reserved here once rather than at every call site.
    cg.line("    subq $40, %rsp");
    cg.line("    call sond_start");
    cg.line("    addq $40, %rsp");
    cg.line("    popq %r15");
    cg.line("    ret");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line(".p2align 3");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.out
}

/// Like [`emit_program_g`], but with COVERAGE instrumentation: every statement records that
/// it executed into a shared buffer reached through the global `__cov_ptr` (which the test
/// harness points at an `mmap(MAP_SHARED)` region before forking each test). The total number
/// of points is published as `__cov_total`. Used only by the `sond test` coverage build.
/// `locate` turns a line of the SPLICED buffer the compiler parses into the `file:line` a person
/// can open. Without it the report would name lines of a file nobody wrote.
pub fn emit_program_coverage_g(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
    scope: Option<&HashSet<String>>,
    locate: &dyn Fn(u32) -> String,
) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        coverage: true,
        cov_scope: scope.cloned(),
        ..Default::default()
    };
    cg.set_globals(globals);
    // The harness reads `__cov_total` and writes `__cov_ptr`; register them so those `Var`s
    // resolve to the `.data` symbols emitted below.
    cg.global_names.insert("__cov_ptr".to_string());
    cg.global_names.insert("__cov_total".to_string());
    cg.global_names.insert("__cov_nfns".to_string());
    // The harness indexes `__cov_fn_table` and `__cov_loc_table` by address, so they resolve
    // like array globals.
    cg.global_arrays.insert("__cov_fn_table".to_string());
    cg.global_arrays.insert("__cov_loc_table".to_string());
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl main");
    cg.line("main:");
    cg.line("    movq %rdi, sond_argc(%rip)");
    cg.line("    movq %rsi, sond_argv(%rip)");
    cg.line("    movq %rdx, sond_envp(%rip)");
    cg.line("    pushq %r15");
    cg.line("    leaq sond_arena_tcb(%rip), %r15");
    cg.line("    call sond_start");
    cg.line("    popq %r15");
    cg.line("    ret");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    // Intern each instrumented function's name (before rodata is emitted) so the table can
    // point at the string. Each entry is [name_ptr, name_len, first_point_id, point_count].
    let cov_fns = cg.cov_fns.clone();
    let table: Vec<(String, usize, usize, usize)> = cov_fns
        .iter()
        .map(|(name, start, count)| (cg.intern_string(name), name.len(), *start, *count))
        .collect();
    // And one `file:line` per point, so a missed point is a place and not a search.
    let cov_lines = cg.cov_lines.clone();
    let locs: Vec<(String, usize)> = cov_lines
        .iter()
        .map(|line| {
            let texto = locate(*line);
            (cg.intern_string(&texto), texto.len())
        })
        .collect();
    cg.emit_rodata();
    let total = cg.cov_points;
    let nfns = table.len();
    cg.line(".data");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.line("__cov_ptr: .quad 0");
    cg.line(&format!("__cov_total: .quad {total}"));
    cg.line(&format!("__cov_nfns: .quad {nfns}"));
    cg.line("__cov_fn_table:");
    for (label, len, start, count) in table {
        cg.line(&format!("    .quad {label}, {len}, {start}, {count}"));
    }
    cg.line("__cov_loc_table:");
    for (label, len) in locs {
        cg.line(&format!("    .quad {label}, {len}"));
    }
    cg.emit_gnu_stack_note();
    cg.out
}

/// Emits a **shared library** (`.so`): every function keeps its own C-ABI name and
/// there is no entry point. `pub fn` are exported in the dynamic symbol table (callable
/// from C/C++/Go/Rust/Python via the System V AMD64 C ABI Sond already uses); private
/// functions are marked hidden so they stay internal. Methods `impl T { fn m }` export
/// as `T_m`. Linked `cc -shared -nostdlib`.
pub fn emit_shared_g(globals: &[Global], structs: &[StructDef], funcs: &[Function]) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        shared_arena_boundary: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    // Every function is user-callable by its own name; none is a hidden entry.
    cg.fn_names = funcs.iter().map(|f| f.name.clone()).collect();
    cg.fn_ret_types = funcs
        .iter()
        .map(|f| (f.name.clone(), ret_kind(f)))
        .collect();
    cg.set_struct_sizes(structs);
    if !cg.mut_globals.is_empty() {
        cg.graph_effects = Some(crate::check::compute_global_effects(funcs, &cg.mut_globals));
    }
    for f in funcs {
        cg.emit_function(f, &f.name.clone());
    }
    // Keep private functions out of the dynamic symbol table (still callable within
    // the library); only `pub fn` is exported from the `.so`.
    for f in funcs {
        if f.vis != Visibility::Pub {
            cg.line(&format!(".hidden {}", f.name));
        }
    }
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line(".p2align 3");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.emit_gnu_stack_note();
    cg.out
}

/// Emits a **freestanding** program: all functions plus a bare `_start` entry
/// that calls the entry and exits via a raw syscall, no libc, no C runtime.
/// Linked `-nostdlib -static`, this is the shape of a Sond `/init` (kernel
/// profile of RFC-0003).
pub fn emit_freestanding(funcs: &[Function]) -> String {
    emit_freestanding_g(&[], &[], funcs)
}

/// Like [`emit_freestanding`], but with module-level globals and struct definitions.
pub fn emit_freestanding_g(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl _start");
    cg.line("_start:");
    // The kernel lays out the initial stack at process entry: argc at (%rsp), then argv[0..argc],
    // a NULL, then envp[]. With no libc to do it, `_start` stashes them itself so the
    // argc()/argv()/envp() builtins work (the bug: freestanding left these undefined).
    cg.line("    movq (%rsp), %rax"); // argc
    cg.line("    movq %rax, sond_argc(%rip)");
    cg.line("    leaq 8(%rsp), %rcx"); // &argv[0]
    cg.line("    movq %rcx, sond_argv(%rip)");
    // envp begins after argv's NULL terminator: rsp + 8 (skip argc) + (argc+1)*8 = rsp + 16 + argc*8.
    cg.line("    leaq 16(%rsp,%rax,8), %rcx");
    cg.line("    movq %rcx, sond_envp(%rip)");
    cg.line("    leaq sond_arena_tcb(%rip), %r15"); // seed the thread-local arena pointer
    cg.line("    call sond_start");
    cg.line("    movq %rax, %rdi"); // exit code = the entry's value
    cg.line("    movq $60, %rax"); // sys_exit
    cg.line("    syscall");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line(".p2align 3");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.emit_gnu_stack_note();
    cg.out
}

/// Emits a **UEFI application** (PE32+, Microsoft x64 ABI): an `efi_main` that
/// the firmware loads and runs directly on bare hardware, no OS underneath.
/// The FULL language works here (variables, arithmetic, if/else, ...): the shared
/// codegen emits the user function, and `print` lowers to the UEFI Simple Text
/// Output protocol (`SystemTable->ConOut->OutputString`). `efi_main` stashes the
/// SystemTable into a global, calls the user function, and returns its value as
/// the EFI status.
///
/// This is the `uefi` target of RFC-0003: Sond booting from scratch. Linked with
/// a PE toolchain (mingw) as `BOOTX64.EFI`, subsystem 10 (EFI application).
pub fn emit_uefi(funcs: &[Function]) -> String {
    emit_uefi_g(&[], &[], funcs)
}

/// Like [`emit_uefi`], but with module-level globals and struct definitions.
pub fn emit_uefi_g(globals: &[Global], structs: &[StructDef], funcs: &[Function]) -> String {
    let mut cg = Codegen {
        uefi: true,
        check_contracts: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    // The entry (`efi_main`) is emitted under `sond_start` so its Sond name
    // never collides with the PE entry point `efi_main`.
    emit_all(&mut cg, structs, funcs, "efi_main");

    // efi_main entry (MS x64 ABI: rcx = ImageHandle, rdx = SystemTable).
    cg.line(".globl efi_main");
    cg.line("efi_main:");
    // Park the two arguments in scratch registers the zero-fill below does not touch (`stos`
    // uses %rdi/%rcx/%rax; %r10 and %r11 are caller-saved and dead on entry). They are stashed
    // AFTER the fill, so the fill cannot erase them no matter which section they end up in.
    cg.line("    movq %rcx, %r10"); // ImageHandle
    cg.line("    movq %rdx, %r11"); // SystemTable
    // Zero `.bss` before any Sond code runs. On ELF the zeros come from the program header
    // (`memsz > filesz`, the kernel supplies them); a PE has no such promise in the file, only
    // the loader's convention of zero-filling a section with no raw data. OVMF honours it, but
    // this target already boots on firmwares that disagree about the same PE, and a buffer that
    // is zeroed on one machine and full of garbage on another is a bug that surfaces far from
    // its cause. So the entry states it: `__bss_start__`/`__bss_end__` are the linker's own
    // bounds for the section. Safe when there is nothing there (start == end, count 0). `cld`
    // because the direction flag decides which way `stos` walks, and it is cheaper to set it
    // than to trust every firmware to hand it over clear.
    cg.line("    leaq __bss_start__(%rip), %rdi");
    cg.line("    leaq __bss_end__(%rip), %rcx");
    cg.line("    subq %rdi, %rcx"); // byte count
    cg.line("    xorl %eax, %eax");
    cg.line("    cld");
    cg.line("    rep stosb");
    cg.line("    movq %r10, sond_imghandle(%rip)"); // stash ImageHandle for EBS
    cg.line("    movq %r11, sond_systab(%rip)"); // stash SystemTable for print
    cg.line("    subq $8, %rsp"); // 16-align the stack before the call
    cg.line("    leaq sond_arena_tcb(%rip), %r15"); // seed the thread-local arena pointer
    cg.line("    call sond_start");
    cg.line("    addq $8, %rsp");
    cg.line("    ret"); // the entry's value becomes the EFI status

    cg.emit_print_int_uefi_helper();
    cg.emit_serial_helper();
    cg.emit_serial_int_helper();
    cg.emit_heap_helpers();
    cg.emit_serial_str_helper();
    cg.emit_swtch_helper();

    cg.line(".data");
    cg.line("sond_systab: .quad 0"); // EFI_SYSTEM_TABLE*
    cg.line("sond_key: .quad 0"); // scratch EFI_INPUT_KEY for readkey()
    cg.line("sond_char: .short 0, 0"); // scratch CHAR16[2] for print_char()
    cg.line("sond_intbuf: .space 48"); // scratch CHAR16[24] for print_int on UEFI
    cg.line("sond_allocptr: .quad 0"); // AllocatePool out-param for alloc()
    cg.line("sond_imghandle: .quad 0"); // EFI image handle (for ExitBootServices)
    cg.line("sond_mmap_size: .quad 0"); // memory-map size (in/out)
    cg.line("sond_mmap_cap: .quad 0"); // memory-map buffer capacity
    cg.line("sond_mmap_key: .quad 0"); // memory-map key (for ExitBootServices)
    cg.line("sond_mmap_dsize: .quad 0"); // descriptor size
    cg.line("sond_mmap_dver: .quad 0"); // descriptor version
    cg.line("sond_mmap_buf: .quad 0"); // allocated memory-map buffer
    cg.line("sond_ebs_done: .quad 0"); // 1 after ExitBootServices: alloc switches to the bump heap
    cg.line("sond_heap_ptr: .quad 0"); // post-EBS bump-allocator cursor
    cg.line("sond_heap_end: .quad 0"); // post-EBS bump-allocator limit
    cg.emit_view_alloc_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.out
}

/// The rbp-relative frame layout of a function: where every parameter, local, array, and the
/// optional return slot live. Computed once so both the codegen prologue and the RFC-0016 symbol
/// table (which lets the debugger name locals) agree on every offset. Offsets are negative (locals
/// sit below the saved frame pointer).
pub(crate) struct FrameLayout {
    /// Parameters as (name, offset, kind), in declaration order.
    pub params: Vec<(String, i32, Ty)>,
    /// Scalar and array-pointer locals as (name, offset), in first-seen order. Array names appear
    /// here too (their one-word pointer slot); `arrays` records their storage separately.
    pub locals: Vec<(String, i32)>,
    /// Arrays as (name, pointer-slot offset, storage-base offset).
    pub arrays: Vec<(String, i32, i32)>,
    /// The return-value slot offset (present iff the function has `ensures`).
    pub ret_slot: Option<i32>,
    /// The named return bound to that slot, if any.
    pub ret_name: Option<(String, i32)>,
    /// Total frame size, 16-byte aligned.
    pub frame: i32,
}

pub(crate) fn frame_layout(func: &Function) -> FrameLayout {
    let mut slot = 0i32;
    let mut params = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in &func.params {
        slot += 1;
        params.push((p.name.clone(), -8 * slot, kind_of_type(&p.ty)));
        seen.insert(p.name.clone());
    }
    let mut names = Vec::new();
    collect_locals(&func.body, &mut names);
    let mut locals = Vec::new();
    for name in &names {
        if !seen.contains(name) {
            slot += 1;
            seen.insert(name.clone());
            locals.push((name.clone(), -8 * slot));
        }
    }
    let off_of = |n: &str| -> i32 {
        if let Some((_, o, _)) = params.iter().find(|(nm, _, _)| nm == n) {
            return *o;
        }
        locals
            .iter()
            .find(|(nm, _)| nm == n)
            .map(|(_, o)| *o)
            .unwrap_or(0)
    };
    let mut arr = Vec::new();
    collect_arrays(&func.body, &mut arr);
    let mut arrays = Vec::new();
    for (name, len) in &arr {
        slot += *len as i32;
        arrays.push((name.clone(), off_of(name), -8 * slot));
    }
    let (ret_slot, ret_name) = if func.ensures.is_empty() {
        (None, None)
    } else {
        slot += 1;
        let off = -8 * slot;
        (Some(off), func.ret_name.clone().map(|rn| (rn, off)))
    };
    let frame = (slot * 8 + 15) & !15;
    FrameLayout {
        params,
        locals,
        arrays,
        ret_slot,
        ret_name,
        frame,
    }
}

/// Collects the names of all local bindings in a block (recursing into the
/// branches of `if` expressions), in definition order.
fn collect_locals(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                collect_locals_in_expr(value, out);
                out.push(name.clone());
            }
            Stmt::Assign { value, .. } => collect_locals_in_expr(value, out),
            Stmt::Expr(e) | Stmt::Spawn(e) => collect_locals_in_expr(e, out),
            Stmt::Par(peers) => {
                for p in peers {
                    collect_locals_in_expr(p, out);
                }
            }
            Stmt::ParLet { names, peers, .. } => {
                for p in peers {
                    collect_locals_in_expr(p, out);
                }
                for n in names {
                    out.push(n.clone());
                }
            }
            Stmt::ParReduce { name, peers, .. } => {
                for p in peers {
                    collect_locals_in_expr(p, out);
                }
                out.push(name.clone());
            }
            Stmt::GraphLet {
                name,
                nodes,
                yield_expr,
                ..
            } => {
                for (nn, ne) in nodes {
                    collect_locals_in_expr(ne, out);
                    out.push(nn.clone());
                }
                collect_locals_in_expr(yield_expr, out);
                out.push(name.clone());
            }
            Stmt::Return(Some(e)) => collect_locals_in_expr(e, out),
            Stmt::Return(None) => {}
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    collect_locals_in_expr(c, out);
                }
                collect_locals(body, out);
            }
            Stmt::Lock { body, .. } => {
                collect_locals(body, out);
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    if let Some(tail) = &block.tail {
        collect_locals_in_expr(tail, out);
    }
}

/// Collects fixed-size array declarations `let a[N]` as `(name, N)`, recursing
/// into loop bodies and `if` branches, same shape as [`collect_locals`].
///
/// `if` in Sond is an expression, so array declarations inside `if`/`else`
/// bodies live under `Stmt::Expr`, a `Let` value, or the block tail. Missing
/// those (as an earlier version did, only recursing into `For`) left the array's
/// pointer slot uninitialized, so using an array declared inside an `if`
/// dereferenced garbage and segfaulted.
fn collect_arrays(block: &Block, out: &mut Vec<(String, i64)>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let {
                name,
                array_len: Some(n),
                value,
                ..
            } => {
                collect_arrays_in_expr(value, out);
                out.push((name.clone(), *n));
            }
            Stmt::Let { value, .. } => collect_arrays_in_expr(value, out),
            Stmt::Assign { value, .. } => collect_arrays_in_expr(value, out),
            Stmt::Expr(e) | Stmt::Spawn(e) => collect_arrays_in_expr(e, out),
            Stmt::Par(peers) => {
                for p in peers {
                    collect_arrays_in_expr(p, out);
                }
            }
            Stmt::ParLet { peers, .. } => {
                for p in peers {
                    collect_arrays_in_expr(p, out);
                }
            }
            Stmt::ParReduce { peers, .. } => {
                for p in peers {
                    collect_arrays_in_expr(p, out);
                }
            }
            Stmt::GraphLet {
                nodes, yield_expr, ..
            } => {
                for (_, ne) in nodes {
                    collect_arrays_in_expr(ne, out);
                }
                collect_arrays_in_expr(yield_expr, out);
            }
            Stmt::Return(Some(e)) => collect_arrays_in_expr(e, out),
            Stmt::Return(None) => {}
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    collect_arrays_in_expr(c, out);
                }
                collect_arrays(body, out);
            }
            Stmt::Lock { body, .. } => {
                collect_arrays(body, out);
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    if let Some(tail) = &block.tail {
        collect_arrays_in_expr(tail, out);
    }
}

fn collect_arrays_in_expr(expr: &Expr, out: &mut Vec<(String, i64)>) {
    match expr {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_arrays_in_expr(cond, out);
            collect_arrays(then_branch, out);
            if let Some(eb) = else_branch {
                collect_arrays(eb, out);
            }
        }
        // An `if` (with array decls in its branches) can sit anywhere an expression
        // can (including a call argument or an operand), so the walk must reach every
        // sub-expression, not just `if`. (Missing this left arrays declared inside an
        // `if` used as a call arg with no reserved frame slot.)
        Expr::Call(callee, args) => {
            collect_arrays_in_expr(callee, out);
            for a in args {
                collect_arrays_in_expr(&a.value, out);
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            collect_arrays_in_expr(l, out);
            collect_arrays_in_expr(r, out);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => collect_arrays_in_expr(x, out),
        Expr::List(items) => items.iter().for_each(|it| collect_arrays_in_expr(it, out)),
        _ => {}
    }
}

/// Collects, into `out`, the indices of the graph nodes that `expr` references (a data-dependency
/// edge). Exhaustive over `Expr` so no reference is missed, which would let a node be scheduled
/// before a node it depends on.
pub(crate) fn collect_node_refs(
    expr: &Expr,
    node_idx: &HashMap<&str, usize>,
    out: &mut HashSet<usize>,
) {
    match expr {
        Expr::Var(name) => {
            if let Some(&i) = node_idx.get(name.as_str()) {
                out.insert(i);
            }
        }
        Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) => {}
        Expr::Field(b, _) | Expr::Unary(_, b) | Expr::Cast(b, _) => {
            collect_node_refs(b, node_idx, out)
        }
        Expr::Call(callee, args) => {
            collect_node_refs(callee, node_idx, out);
            for a in args {
                collect_node_refs(&a.value, node_idx, out);
            }
        }
        Expr::List(items) => {
            for it in items {
                collect_node_refs(it, node_idx, out);
            }
        }
        Expr::Index(b, i) => {
            collect_node_refs(b, node_idx, out);
            collect_node_refs(i, node_idx, out);
        }
        Expr::SliceRange(b, lo, hi) => {
            collect_node_refs(b, node_idx, out);
            if let Some(l) = lo {
                collect_node_refs(l, node_idx, out);
            }
            if let Some(h) = hi {
                collect_node_refs(h, node_idx, out);
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                collect_node_refs(v, node_idx, out);
            }
        }
        Expr::Binary(_, l, r) => {
            collect_node_refs(l, node_idx, out);
            collect_node_refs(r, node_idx, out);
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_node_refs(cond, node_idx, out);
            if let Some(t) = &then_branch.tail {
                collect_node_refs(t, node_idx, out);
            }
            if let Some(eb) = else_branch
                && let Some(t) = &eb.tail
            {
                collect_node_refs(t, node_idx, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_node_refs(scrutinee, node_idx, out);
            for arm in arms {
                if let Some(t) = &arm.body.tail {
                    collect_node_refs(t, node_idx, out);
                }
            }
        }
    }
}

/// The dependency edges of each graph node: `deps[i]` is the set of node indices that node `i`
/// references. An edge `i -> d` means `i` must be evaluated after `d`.
fn node_deps(nodes: &[(String, Expr)]) -> Vec<Vec<usize>> {
    let node_idx: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.as_str(), i))
        .collect();
    nodes
        .iter()
        .map(|(_, e)| {
            let mut refs = HashSet::new();
            collect_node_refs(e, &node_idx, &mut refs);
            refs.into_iter().collect()
        })
        .collect()
}

/// Groups graph nodes into TOPOLOGICAL LEVELS over the given dependency edges (`deps[i]` = the
/// nodes `i` must run after, DATA edges plus any effect edges): `level[i] = 1 + max(level over
/// deps[i])`, or 0 for a source node. Nodes in the SAME level share no edge, so they are mutually
/// independent and may run concurrently; levels run in order. Cycle-safe (relaxes at most `n`
/// times); a data cycle is anyway rejected by `check_graph_acyclic`.
fn topological_levels(deps: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = deps.len();
    let mut level = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for i in 0..n {
            let m = deps[i].iter().map(|&d| level[d] + 1).max().unwrap_or(0);
            if m != level[i] {
                level[i] = m;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let depth = level.iter().copied().max().unwrap_or(0);
    let mut groups = vec![Vec::new(); depth + 1];
    for (i, &l) in level.iter().enumerate() {
        groups[l].push(i);
    }
    groups
}

fn collect_locals_in_expr(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_locals_in_expr(cond, out);
            collect_locals(then_branch, out);
            if let Some(eb) = else_branch {
                collect_locals(eb, out);
            }
        }
        // A `let`-binding `if` (e.g. a lowered `match … { Some(x) => .. }`) can appear
        // in any expression position (a call argument, an operand), so its locals must
        // be reserved wherever it sits. Walking only `if` missed a `match` used as a
        // call argument, leaving its bound payload with no frame slot ("unknown variable").
        Expr::Call(callee, args) => {
            collect_locals_in_expr(callee, out);
            for a in args {
                collect_locals_in_expr(&a.value, out);
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            collect_locals_in_expr(l, out);
            collect_locals_in_expr(r, out);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => collect_locals_in_expr(x, out),
        Expr::List(items) => items.iter().for_each(|it| collect_locals_in_expr(it, out)),
        _ => {}
    }
}

/// One structured scope on the `scopes` stack: either a `for` loop or a `lock` body.
/// A non-local exit (`break`, `continue`, `return`) unwinds every `Lock` scope it crosses,
/// running that lock's release code, then jumps to its own destination.
#[derive(Clone)]
enum Scope {
    /// A `for` loop: `start` is the top-of-loop label (a `continue` target) and `end` is the
    /// label just after the loop (a `break` target).
    Loop { start: String, end: String },
    /// A `lock (m) { .. }` body. `name` is the lock variable, which an early exit reads in
    /// order to release the mutex on its way out.
    Lock { name: String },
}

#[derive(Default)]
struct Codegen {
    out: String,
    labels: usize,
    locals: HashMap<String, i32>,
    ret_label: String,
    /// Interned string literals: `(label, bytes)`, emitted in `.rodata`.
    strings: Vec<(String, String)>,
    /// UEFI target: `print` lowers to ConOut and strings are UTF-16.
    uefi: bool,
    /// Stack of enclosing structured scopes (`for` loops and `lock` bodies), innermost last.
    /// A `break`/`continue`/`return` unwinds the `lock`s it crosses — running each one's
    /// release code — before jumping to its own destination, so no exit can leave a lock held.
    scopes: Vec<Scope>,
    /// Names of the user-defined functions in the program (to tell a call to a
    /// user function from a call to a builtin).
    fn_names: HashSet<String>,
    /// Whether `print_int` is used (then the runtime helper is emitted once).
    uses_print_int: bool,
    /// Whether the SIGNED `print_int` helper is used (a `print_int` on a signed integer
    /// type, so a negative shows `-N` instead of its huge unsigned two's-complement form).
    uses_print_int_signed: bool,
    /// Whether `print_int` is used on the UEFI target (its own UTF-16 helper).
    uses_print_int_uefi: bool,
    /// Whether the serial (COM1) output helper is used, direct port I/O that
    /// survives ExitBootServices (a real kernel talking to hardware).
    uses_serial: bool,
    /// Whether `serial_int` is used (decimal integers over COM1, post-EBS).
    uses_serial_int: bool,
    /// Whether the post-EBS heap is used (`alloc` after ExitBootServices, or the
    /// `exit_boot_services` call that seeds it). Emits the bump-allocator helpers.
    uses_heap: bool,
    /// Names declared `extern fn`: called through the platform's C ABI and resolved by the
    /// linker, never emitted.
    extern_fns: HashSet<String>,
    /// Character literals (`let s: string = ".."`) interned in `.rodata` as one `.long` per code
    /// point: constant data, emitted once, not rebuilt on every call — a string in a function
    /// that runs per connection was reserving arena and copying itself each time.
    char_consts: Vec<(String, Vec<u32>)>,
    /// Views whose length is known at compile time (`u32[16]`, and every `let s: string = ".."`
    /// once the parser has counted the literal), so `s.len` is that constant.
    view_len: HashMap<String, i64>,
    /// Whether any declaration reserved its own storage (`let ar: i32[n]`), so the reservation
    /// helper is emitted. Off by default: a program that never declares one is byte-identical.
    uses_view_alloc: bool,
    /// Whether `print` on UEFI is used (then it also emits a serial fallback used
    /// after ExitBootServices, when ConOut is dead).
    uses_print_serial: bool,
    /// Whether `swtch` (cooperative context switch between two stacks) is used.
    uses_swtch: bool,
    /// Whether to emit runtime contract checks (`requires`/`ensures`). On for the
    /// oracle/debug build; a release build turns it off for zero overhead.
    check_contracts: bool,
    /// Coverage instrumentation: when on, `block()` emits a mark at each statement, recording
    /// that the point executed into the shared `__cov_ptr` buffer. OFF for ordinary builds
    /// (output is byte-identical), only the `sond test` coverage build sets it.
    coverage: bool,
    /// Running count of coverage points emitted (and the id of the next one). The total is
    /// published as `__cov_total` so the test harness can size its shared coverage buffer.
    cov_points: usize,
    /// Whether the function CURRENTLY being emitted is instrumented. Coverage measures the
    /// code UNDER TEST, so the synthesized harness, the test bodies, and the testing library
    /// are skipped (also avoids the harness marking through a not-yet-set `__cov_ptr`).
    cov_active: bool,
    /// Per-instrumented-function coverage ranges: `(name, first_point_id, point_count)`. Emitted
    /// as `__cov_fn_table` so the harness can name which functions are not fully covered.
    cov_fns: Vec<(String, usize, usize)>,
    /// The source line of each coverage point, by point id. Naming the function is not enough to
    /// act on: `tls_handshake_auth` is 130 lines, and "not fully covered" over it is a reading
    /// task, not a location. Emitted (resolved to file and line) as `__cov_loc_table`.
    cov_lines: Vec<u32>,
    /// The function names coverage is scoped to (the code the entry is responsible for).
    /// `None` = no scope was supplied, so cover every non-skipped function. `Some(empty)` is a
    /// scope like any other and covers NOTHING: an entry whose modules are all measured by gates
    /// of their own answers for nothing, and reading that as "cover everything" reported it at
    /// 100% over code it is not responsible for.
    cov_scope: Option<HashSet<String>>,
    /// Names of the module-level globals (to resolve a `Var` to a `.data` label).
    global_names: HashSet<String>,
    /// The scalar globals to emit in `.data`, as `(label, constant value)`.
    global_data: Vec<(String, i64)>,
    /// String-valued globals: `(global name, interned .rodata label)`. `let MSG = "hi"` stores the
    /// ADDRESS of the NUL-terminated bytes, a valid compile-time constant, so a `Var` load yields a
    /// cstr pointer (what strlen/json/etc. expect).
    global_str_data: Vec<(String, String)>,
    /// Names of globals that are ARRAYS (a `Var` resolves to their ADDRESS, not a
    /// loaded value, so `K[i]` indexes into the table).
    global_arrays: HashSet<String>,
    /// The array globals to emit, as `(label, the words GIVEN, the declared length)`. The words
    /// given are only the initializer's own elements: the rest of the declared length is a tail
    /// of zeros, which is RESERVED rather than written out (see `emit_globals`), so a buffer of
    /// millions of elements costs the compiler and the binary nothing per element.
    global_array_data: Vec<(String, Vec<GlobalWord>, usize)>,
    /// `let NAME: T[N]` at module level — the element type and declared element count, so
    /// `emit_globals` can reserve the right number of BYTES (not words) in `.bss`.
    global_view_lengths: HashMap<String, (Ty, i64)>,
    /// The value kind (`Int`/`Float`) of each in-scope local/parameter, so codegen
    /// knows to use the integer (`%rax`) or SSE (`%xmm0`) path for a `Var`. Reset per
    /// function alongside `locals`.
    var_types: HashMap<String, Ty>,
    /// The ELEMENT type of each `u32[]`-style view in scope. The variable itself holds a base
    /// address (an i64), so `var_types` says i64 for it; this is what `v[i]` needs, so the index
    /// scales by the element's width instead of assuming a 64-bit word. Reset per function.
    view_elem: HashMap<String, Ty>,
    /// The rodata label for each local `string` variable whose value the compiler
    /// can trace to a literal. When a tracked variable is passed as a `string` argument, emit
    /// `leaq label(%rip), %rax` (UTF-8 bytes) instead of loading the code-point pointer.
    /// Reset per function.
    string_rodata_cur: HashMap<String, String>,
    /// Functions whose return type is `string` (a view of code points). Populated in `emit_all`,
    /// used in lowering to track call results as string variables.
    string_returning_fns: HashSet<String>,
    /// Local variables known to be of `string` type (vs. an arbitrary view type). Reset per function.
    string_vars: HashSet<String>,
    /// For each string variable tracked in `string_rodata_cur`, the source lines at which
    /// the variable was assigned a string literal, together with the literal text.  Used to
    /// build the error message that names both conflicting assignments.
    string_assignments: HashMap<String, Vec<(u32, String)>>,
    /// The return kind of every user function, to type a call expression.
    fn_ret_types: HashMap<String, Ty>,
    /// Word count of each struct type (its by-value size), so a function returning a
    /// small struct/enum returns it in registers (RAX:RDX) rather than a dangling
    /// pointer, and a `let r = call()` reception knows how many words to store.
    struct_sizes: HashMap<String, i64>,
    /// If the function currently being emitted returns a small struct/enum by value,
    /// its word count (1 or 2), set in `emit_function`, drives the RAX:RDX expansion
    /// at the return point. `None` for scalar/float/unit returns.
    cur_ret_struct: Option<i64>,
    /// Interned `f64` literals: `(label, bits)`, emitted as `.quad` doubles in `.rodata`.
    float_consts: Vec<(String, u64)>,
    /// RFC-0010 Layer B. The `clear_child_tid` cells of the threads spawned in the function
    /// currently being emitted. A `spawn` allocates one and stores the child TID into it; the
    /// kernel clears it and futex-wakes on the thread's exit, and the function's epilogue waits
    /// on each, which is the structured join. Reset per function.
    spawn_ctids: Vec<String>,
    /// Every ctid cell across the whole program, emitted as `.quad 0` in `.bss`.
    all_ctids: Vec<String>,
    /// Slot TABLES for the `spawn` sites that sit inside a loop: `(label, slots)`. A looping site
    /// cannot own a single join cell -- the next turn would overwrite the tid of a thread still
    /// running -- so it owns a fixed ring of `(ctid, owner)` pairs instead, joined before reuse.
    spawn_tables: Vec<(String, usize)>,
    /// The same, for the function being emitted: joined at its return like the single cells.
    fn_spawn_tables: Vec<(String, usize)>,
    /// One owner-pid cell per spawn site: the pid of the process that started that thread,
    /// stored just before the clone. `fork` copies the address space, so a forked child inherits
    /// every join cell of its parent while inheriting none of the threads; comparing this against
    /// the joining process's own pid is what tells the two apart. See `owner_cell`.
    all_owner_cells: Vec<String>,
    /// Result cells for `par`-let peers: a spawned peer stores its return value here before it
    /// exits, and the parent reads it after the join binds it to a name. One per `par`-let peer
    /// site across the whole program, emitted as `.quad 0` alongside the ctid cells.
    all_result_cells: Vec<String>,
    /// Set once any `spawn` is emitted, so the thread stack size constant is emitted.
    uses_spawn: bool,
    /// `--shared` (`.so`) build: each exported (`pub`) function is a C-ABI boundary, entered from a
    /// FOREIGN host (Go/cgo, C). It must save the host's `%r15` (callee-saved in System V) and seed
    /// `%r15` with the arena TCB, or an allocation inside would fault on the host's `%r15`. Set in
    /// `emit_shared_g`; unset elsewhere (a hosted program seeds `%r15` once at its own entry).
    shared_arena_boundary: bool,
    /// `--debug` build (RFC-0016): emit a `__dbg_step(line)` callout before each statement. This
    /// first slice traces the source line as it executes; the socket-served step agent builds on
    /// the same seam. Off by default (a normal build is byte-identical, like coverage).
    debug: bool,
    /// The mutable globals of the program (the shared state a `graph`'s parallel nodes could race
    /// on), and the per-function effect footprints over them. Populated before emission when the
    /// program has both mutable globals and functions, so a `graph` can insert a serializing effect
    /// edge between two same-level nodes that conflict (RFC-0008 §5.2). `None` = no shared state,
    /// so no effect edges are needed.
    mut_globals: HashSet<String>,
    graph_effects: Option<crate::check::GlobalEffects>,
}

/// Functions excluded from coverage instrumentation: the synthesized `sond test` harness
/// (`main`), the test bodies themselves (always run, and their coverage is uninteresting),
/// and the testing library. Coverage measures the code under test, not the test scaffold.
fn should_skip_coverage(func: &Function) -> bool {
    func.is_test || never_instrumented(&func.name)
}

/// The names a coverage build never instruments, by name alone.
///
/// The scope is decided in `resolve.rs` and the instrumentation here, and while the two read the
/// same list from different places they can disagree: a scope holding only names that are never
/// instrumented looks non-empty there and measures nothing here, which is how the entry that had
/// delegated its whole denominator was reported as a hole. One list, one reading.
pub fn never_instrumented(name: &str) -> bool {
    matches!(
        name,
        "main" | "assert" | "assert_eq" | "assert_ne" | "fail" | "__test_report"
    )
}

impl Codegen {
    /// Emits the function itself: label, prologue, body, epilogue. Shared by the
    /// hosted and freestanding entry points.
    fn emit_function(&mut self, func: &Function, label: &str) {
        // Fresh per-function state (locals map + a unique return label).
        self.locals.clear();
        self.var_types.clear();
        self.view_elem.clear();
        self.string_rodata_cur.clear();
        self.string_vars.clear();
        self.string_assignments.clear();
        // Register module-level view-typed globals so their element width is known.
        for (name, (elem_kind, elem_count)) in &self.global_view_lengths {
            self.view_elem.insert(name.clone(), *elem_kind);
            self.view_len.insert(name.clone(), *elem_count);
        }
        // The debug agent itself must NOT be instrumented, or `__dbg_step` (and every helper it
        // calls to serve the socket) would re-enter the checkpoint at each statement: unbounded
        // recursion. The agent is the `__dbg_` namespace, so disable the checkpoint for all of it.
        let saved_debug = self.debug;
        if func.name.starts_with("__dbg_") {
            self.debug = false;
        }
        // Instrument only the code under test: skip the harness, the test bodies, and the
        // testing library (measuring those is noise, and the harness must run its `__cov_ptr`
        // setup before any mark fires); and, when a coverage scope is set, only functions in
        // it (the entry's own code, not already-tested transitive dependencies).
        let in_scope = self
            .cov_scope
            .as_ref()
            .is_none_or(|s| s.contains(&func.name));
        self.cov_active = self.coverage && !should_skip_coverage(func) && in_scope;
        // Does this function return a small struct/enum by value? If so, the body leaves
        // its base ADDRESS in %rax; the return point expands that to the value in RAX:RDX.
        self.cur_ret_struct = self.ret_struct_words(&func.ret);
        // The rbp-relative frame layout: parameters first, then locals, then array storage, then the
        // optional return slot. Computed by `frame_layout` so this prologue and the RFC-0016 symbol
        // table (which names the locals for the debugger) agree on every offset.
        let layout = frame_layout(func);
        for (name, off, kind) in &layout.params {
            self.locals.insert(name.clone(), *off);
            self.var_types.insert(name.clone(), *kind);
        }
        // A view PARAMETER carries its element type too. Registering this only for locals meant the
        // width was lost the moment the buffer was passed to the function that works on it, which
        // is how every real program is written, and indexing silently fell back to 8-byte words.
        for p in &func.params {
            if let crate::ast::Type::View(elem, _) = &p.ty {
                self.view_elem.insert(p.name.clone(), kind_of_type(elem));
            }
        }
        for (name, off) in &layout.locals {
            self.locals.insert(name.clone(), *off);
        }
        // Fixed-size stack arrays `let a[N]`: the name's pointer slot is made to point at the
        // reserved N-word storage region (element 0 at the lowest address).
        let array_bases: Vec<(i32, i32)> = layout
            .arrays
            .iter()
            .map(|(_, ptr, base)| (*ptr, *base))
            .collect();
        // The return slot binds the named return (e.g. `vat_amount`) so `ensures` can read it.
        if let Some((rn, off)) = &layout.ret_name {
            self.locals.insert(rn.clone(), *off);
        }
        let ret_slot = layout.ret_slot;
        let frame = layout.frame;
        self.ret_label = self.fresh_label("ret");
        self.spawn_ctids.clear();
        self.fn_spawn_tables.clear();

        self.line(".text");
        self.line(&format!(".globl {label}"));
        self.line(&format!("{label}:"));
        // An interrupt handler is entered by the CPU, not called: it must preserve
        // every GP register of the interrupted code (rbp is saved by the frame below).
        if func.is_interrupt {
            for r in ISR_SAVED_REGS {
                self.line(&format!("    pushq {r}"));
            }
        }
        // C-ABI boundary of a `.so` (`--shared`): save the host's `%r15` and seed ours to the arena
        // TCB. Two 8-byte pushes (the save + a padding slot) keep the 16-byte stack alignment the
        // body's `call`s need. Done before the frame, so rbp-relative locals are unaffected. Only
        // `pub` functions are entered from the host; a hidden callee inherits `%r15` (it is
        // invariant within a call, pointing at the same TCB), so it needs nothing.
        let shared_boundary = self.shared_arena_boundary && func.vis == Visibility::Pub;
        if shared_boundary {
            self.line("    pushq %r15"); // save the host's callee-saved %r15
            self.line("    subq $8, %rsp"); // padding, keep rsp 16-aligned
            self.line("    leaq sond_arena_tcb(%rip), %r15"); // seed the arena pointer
        }
        self.line("    pushq %rbp");
        self.line("    movq %rsp, %rbp");
        if frame > 0 {
            self.line(&format!("    subq ${frame}, %rsp"));
        }
        // Move incoming arguments into their parameter slots. System V AMD64 classes
        // each argument independently: `f64` (SSE class) is passed in %xmm0.., every
        // other type (INTEGER class) in %rdi..; excess of either spills to the caller's
        // stack (arg at 16(%rbp), 24(%rbp), ...), in declaration order.
        let mut int_i = 0usize;
        let mut sse_i = 0usize;
        let mut stack_i = 0usize;
        for p in &func.params {
            let off = self.locals[&p.name];
            let is_f = is_float_type(&p.ty);
            if is_f && sse_i < SSE_ARG_REGS.len() {
                self.line(&format!("    movsd {}, {off}(%rbp)", SSE_ARG_REGS[sse_i]));
                sse_i += 1;
            } else if !is_f && int_i < ARG_REGS.len() {
                self.line(&format!("    movq {}, {off}(%rbp)", ARG_REGS[int_i]));
                int_i += 1;
            } else {
                let stack_off = 16 + stack_i * 8;
                stack_i += 1;
                if is_f {
                    self.line(&format!("    movsd {stack_off}(%rbp), %xmm0"));
                    self.line(&format!("    movsd %xmm0, {off}(%rbp)"));
                } else {
                    self.line(&format!("    movq {stack_off}(%rbp), %rax"));
                    self.line(&format!("    movq %rax, {off}(%rbp)"));
                }
            }
        }
        // A narrow integer arg (i32/u32) arrives with its high 32 bits unspecified;
        // canonicalize each to its sign/zero-extended 64-bit form so later reads are
        // correct (the codegen keeps narrow integers canonical everywhere).
        for p in &func.params {
            let k = kind_of_type(&p.ty);
            if !k.is_float() && k.bits() < 64 {
                let off = self.locals[&p.name];
                self.line(&format!("    movq {off}(%rbp), %rax"));
                self.canon(k);
                self.line(&format!("    movq %rax, {off}(%rbp)"));
            }
        }

        // Point each array name's pointer slot at its reserved frame region.
        for (ptr_slot, base_off) in &array_bases {
            self.line(&format!("    leaq {base_off}(%rbp), %rax"));
            self.line(&format!("    movq %rax, {ptr_slot}(%rbp)"));
        }

        // RFC-0016: in a --debug build, bump the call depth on entry (and drop it at the single exit
        // label below), so the agent can offer step-over / step-out (stop only when the depth is back
        // at or below where the step began). Emitted after the args are spilled, so the callout may
        // clobber the caller-saved arg registers freely; rsp is 16-aligned here, so the call is ABI
        // correct. `self.debug` is already false for the __dbg_ agent, so its helpers, and the depth
        // functions themselves, do not perturb the depth they measure. Interrupt handlers are entered
        // asynchronously, not called, so they stay out of the call-depth accounting.
        if self.debug && !func.is_interrupt {
            self.line("    call __dbg_enter");
        }

        // Preconditions: checked on entry, with the parameters in scope.
        self.emit_contract_guard(&func.requires);

        let cov_start = self.cov_points;
        self.block(&func.body); // leaves the function's value in %rax
        if self.cov_active {
            self.cov_fns
                .push((func.name.clone(), cov_start, self.cov_points - cov_start));
        }

        self.line(&format!("{}:", self.ret_label.clone()));
        // RFC-0016: drop the call depth as the function exits (mirror of the entry bump). The single
        // exit label covers early `return`s too. The body's result is in %rax and the void callout
        // clobbers it, so it is saved across the call; the extra 8 bytes keep rsp 16-aligned.
        if self.debug && !func.is_interrupt {
            self.line("    pushq %rax");
            self.line("    subq $8, %rsp");
            self.line("    call __dbg_exit");
            self.line("    addq $8, %rsp");
            self.line("    popq %rax");
        }
        // Structured join (RFC-0010): before the function returns, wait for every thread it
        // spawned. This is where a spawned thread's lifetime ends, so none can outlive the
        // frame that started it. Emitted here at the single return label, so it covers early
        // `return`s too. The return value is in %rax and the futex syscall clobbers it, so it
        // is saved across the joins.
        if !self.spawn_ctids.is_empty() || !self.fn_spawn_tables.is_empty() {
            let ctids = std::mem::take(&mut self.spawn_ctids);
            let tables = std::mem::take(&mut self.fn_spawn_tables);
            self.line("    pushq %rax"); // preserve the return value across the joins
            self.emit_join(&ctids);
            for (tab, slots) in tables {
                self.emit_join_table(&tab, slots);
            }
            self.line("    popq %rax");
        }
        // Postconditions: checked at every exit, with the return value bound to
        // the named return so `ensures` can talk about the result.
        if let Some(off) = ret_slot {
            self.line(&format!("    movq %rax, {off}(%rbp)"));
            self.emit_contract_guard(&func.ensures);
            self.line(&format!("    movq {off}(%rbp), %rax"));
        }
        // By-value struct/enum return (System V: a ≤16-byte all-INTEGER aggregate is
        // returned in RAX:RDX). The body left the struct's base address in %rax; load its
        // words into the return registers so the value survives the frame teardown.
        if let Some(words) = self.cur_ret_struct {
            if words == 2 {
                self.line("    movq 8(%rax), %rdx"); // word 1 -> %rdx
            }
            self.line("    movq (%rax), %rax"); // word 0 -> %rax (after reading word 1)
        }
        self.line("    movq %rbp, %rsp");
        self.line("    popq %rbp");
        // Restore the host's %r15 at the C-ABI boundary (mirror of the prologue save + padding).
        if shared_boundary {
            self.line("    addq $8, %rsp"); // drop the padding slot
            self.line("    popq %r15"); // restore the host's callee-saved %r15
        }
        if func.is_interrupt {
            for r in ISR_SAVED_REGS.iter().rev() {
                self.line(&format!("    popq {r}"));
            }
            self.line("    iretq");
        } else {
            self.line("    ret");
        }
        self.debug = saved_debug; // restore instrumentation (disabled only for the __dbg_ agent)
    }

    /// Emits a check for each contract proposition: evaluate it and trap (`ud2`)
    /// if it is false. This is the oracle in the compiled binary, a violated
    /// `requires`/`ensures` stops the program at the fault instead of proceeding
    /// with a wrong result. Compiled only when `check_contracts` is set, so release
    /// builds (and, in the future, statically-proven propositions) pay nothing.
    fn emit_contract_guard(&mut self, props: &[Expr]) {
        if !self.check_contracts {
            return;
        }
        for p in props {
            self.expr(p); // proposition -> %rax (nonzero = holds)
            let ok = self.fresh_label("ok");
            self.line("    testq %rax, %rax");
            self.line(&format!("    jnz {ok}"));
            self.line("    ud2"); // contract violated: trap
            self.line(&format!("{ok}:"));
        }
    }

    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let l = format!(".L{prefix}{}", self.labels);
        self.labels += 1;
        l
    }

    fn slot(&self, name: &str) -> i32 {
        *self
            .locals
            .get(name)
            .unwrap_or_else(|| panic!("codegen: unknown variable `{name}`"))
    }

    /// Records each struct type's word count (its by-value size), so a function that
    /// returns a struct/enum returns it by value and a call reception knows its width.
    fn set_struct_sizes(&mut self, structs: &[StructDef]) {
        for s in structs {
            self.struct_sizes
                .insert(s.name.clone(), s.fields.len() as i64);
        }
    }

    /// The by-value word count of a return type IF it is a small (1-2 word) struct/enum:
    /// the sizes returnable in registers (RAX:RDX). `None` otherwise (scalar, float,
    /// unit, or a struct too large for registers, which is not yet supported).
    fn ret_struct_words(&self, ret: &Option<Type>) -> Option<i64> {
        match ret {
            Some(Type::Named(n)) => self
                .struct_sizes
                .get(n)
                .copied()
                .filter(|&w| (1..=2).contains(&w)),
            _ => None,
        }
    }

    /// Registers the module-level globals: const-evaluates each initializer and
    /// records its `.data` label. A non-constant initializer is an error.
    fn set_globals(&mut self, globals: &[Global]) {
        self.mut_globals = crate::check::mutable_globals(globals);
        for g in globals {
            if self.global_names.contains(&g.name) {
                panic!("global `{}` is declared more than once", g.name);
            }
            self.global_names.insert(g.name.clone());
            // A string literal global (`let NAME = "..."` or `let NAME: string = "..."`):
            // the global holds the ADDRESS of the interned bytes in `.rodata`, a valid
            // compile-time constant. When the parser lowered `let s: string = "..."` to
            // `Type::View(u32, n)` + `Expr::Str`, this must still be treated as a string,
            // not passed through to the typed-view branch below.
            //
            // THE LOWERED VIEW MEANS TWO DIFFERENT THINGS:
            //   - Inside a function: `Type::View(u32, n)` on a `let s: string = ".."` is the
            //     code-point view the compiler emits in `.rodata` and the binding holds its
            //     ADDRESS.  Indexing and `.len` work on it.
            //   - At module level: the *same* lowered form used to mean a typed buffer `u32[n]`
            //     with N·4 bytes in `.bss`.  That is WRONG — a module-level string literal must
            //     remain a pointer in `.rodata` just like the unannotated form.
            //
            // FIX: handle `Expr::Str` before `Type::View` in set_globals() so the lowered
            // module-level string takes the string-literal path, not the bss-buffer path.
            if let Expr::Str(s) = &g.init {
                let label = self.intern_string(s);
                self.global_str_data.push((g.name.clone(), label));
                continue;
            }
            // A typed view at module level (`let NAME: T[N]`). Must have a literal length,
            // reserves that many *bytes* (not words) in `.bss`, and the Var resolves to the
            // buffer address; indexing scales by the element width.
            if let Some(Type::View(elem, len_str)) = &g.ty {
                let elem_kind = kind_of_type(elem);
                let len: i64 = len_str
                    .clone()
                    .unwrap_or_else(|| {
                        panic!(
                            "global `{}`: view length must be a literal (not a variable); \
                         reserve it inside a function instead",
                            g.name
                        )
                    })
                    .parse()
                    .unwrap_or_else(|_| {
                        panic!(
                            "global `{}`: view length must be a literal (not a variable); \
                         reserve it inside a function instead",
                            g.name
                        )
                    });
                let byte_len = len * elem_kind.bits() as i64 / 8;
                assert_ne!(byte_len, 0, "global `{}`: view length must be > 0", g.name);
                self.global_arrays.insert(g.name.clone());
                self.global_view_lengths
                    .insert(g.name.clone(), (elem_kind, len));
                continue;
            }
            // An array global `let NAME[N] = [c0, c1, ...]` is a constant table in
            // `.data`; its `Var` resolves to the base ADDRESS so `NAME[i]` indexes.
            if let Some(n) = g.array_len {
                let mut words = match &g.init {
                    Expr::List(items) => items
                        .iter()
                        .map(|e| match e {
                            // A string literal contributes the address of its interned bytes.
                            Expr::Str(text) => GlobalWord::Label(self.intern_string(text)),
                            _ => GlobalWord::Num(self.const_eval(e).unwrap_or_else(|| {
                                panic!("global array `{}` needs constant elements", g.name)
                            })),
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                // Truncate to the declared size; do NOT pad up to it. Padding materialized one
                // `i64` per declared element, so declaring an image buffer cost the compiler
                // 8 bytes of its own memory per pixel before it emitted a single instruction.
                // The missing elements are zeros by definition, and zeros are reserved, not
                // stored (`emit_globals`).
                words.truncate(n as usize);
                self.global_arrays.insert(g.name.clone());
                self.global_array_data
                    .push((g.name.clone(), words, n as usize));
                continue;
            }
            let value = self.const_eval(&g.init).unwrap_or_else(|| {
                panic!(
                    "global `{}` initializer must be a constant: initialize it to 0 \
                     and assign the computed value inside a function (e.g. in efi_main)",
                    g.name
                )
            });
            self.global_data.push((g.name.clone(), value));
        }
    }

    /// Folds a constant integer expression (used for global initializers).
    fn const_eval(&self, e: &Expr) -> Option<i64> {
        match e {
            Expr::Int(n) => Some(*n),
            Expr::Bool(b) => Some(*b as i64),
            Expr::Unary(op, inner) => {
                let v = self.const_eval(inner)?;
                Some(match op {
                    UnOp::Neg => v.wrapping_neg(),
                    UnOp::BitNot => !v,
                    UnOp::Not => (v == 0) as i64,
                })
            }
            Expr::Binary(op, l, r) => {
                let a = self.const_eval(l)?;
                let b = self.const_eval(r)?;
                Some(match op {
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    BinOp::Div if b != 0 => a.wrapping_div(b),
                    BinOp::Mod if b != 0 => a.wrapping_rem(b),
                    BinOp::BitAnd => a & b,
                    BinOp::BitOr => a | b,
                    BinOp::BitXor => a ^ b,
                    BinOp::Shl => a.wrapping_shl(b as u32),
                    BinOp::Shr => a.wrapping_shr(b as u32),
                    BinOp::LShr => (a as u64).wrapping_shr(b as u32) as i64,
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    /// Emits `spawn f(args)` (RFC-0010 Layer B): run the call on a real OS thread.
    ///
    /// The thread shares the address space (`CLONE_VM`) and file descriptors (`CLONE_FILES`),
    /// runs on its own freshly mmap'd stack, and is joined at the enclosing function's return
    /// (structured concurrency). The child TID goes into a per-spawn cell that the kernel
    /// clears and futex-wakes on exit, which is what the join waits on.
    ///
    /// Arguments are evaluated in the parent into callee-saved registers, which the clone
    /// syscall preserves and the child inherits, so the child loads them into the argument
    /// registers and calls the function with no shared-memory hand-off.
    ///
    /// Raw syscalls throughout: no libc, so a spawning program still links and runs freestanding.
    ///
    /// SEMANTICS, and a real footgun: the join is at the spawning function's return, so every
    /// statement AFTER a spawn in that function runs CONCURRENTLY with the thread, not after it.
    /// Releasing a resource the thread uses in the same function (`spawn pump(fd); close_fd(fd)`)
    /// tears it out from under a live thread; the symptom is corruption, not a clean error. Put the
    /// spawns in their own function and release in the caller, which only resumes once the join is
    /// done. The capability system will make the wrong version a compile error (RFC-0010 section 7).
    /// The owner-pid cell paired with a spawn site's ctid cell. Derived from the name rather than
    /// carried alongside it because the two are allocated together and never separately, and the
    /// join receives only the ctid names.
    fn owner_cell(ctid: &str) -> String {
        ctid.replace("sond_ctid_", "sond_owner_")
    }

    /// Emits the futex-join for each clear_child_tid `cell`: wait until the kernel clears it to 0
    /// at the child thread's exit. Does NOT preserve %rax; a caller that needs a value across the
    /// join saves it (the scope-exit join wraps this in push/pop %rax, `par` has no value yet).
    fn emit_join(&mut self, cells: &[String]) {
        for cell in cells {
            // tid = [cell]; while tid != 0 { futex(cell, FUTEX_WAIT, tid, NULL); tid = [cell] }
            let wait = self.fresh_label("join_wait");
            let done = self.fresh_label("join_done");
            // First: is this even our thread to wait for? A forked child inherited the cell but
            // not the thread, so joining would block forever on a wake that cannot come. Checked
            // once, outside the retry loop: the pid cannot change under us.
            self.line("    movq $39, %rax"); // SYS_getpid
            self.line("    syscall");
            self.line(&format!("    cmpq {}(%rip), %rax", Self::owner_cell(cell)));
            self.line(&format!("    jne {done}"));
            self.line(&format!("{wait}:"));
            self.line(&format!("    movq {cell}(%rip), %rdx")); // rdx = expected tid
            self.line("    testq %rdx, %rdx");
            self.line(&format!("    jz {done}"));
            self.line(&format!("    leaq {cell}(%rip), %rdi")); // uaddr
            self.line("    xorq %rsi, %rsi"); // FUTEX_WAIT = 0
            // rdx already holds the expected value
            self.line("    xorq %r10, %r10"); // timeout = NULL
            self.line("    movq $202, %rax"); // SYS_futex
            self.line("    syscall");
            // Look at what the futex returned. Jumping back unconditionally turns any FAILING
            // futex into a spin, because a failed one returns immediately instead of sleeping:
            // that is how a misaligned cell (EINVAL) cost 743% of CPU with the machine at rest,
            // 71826 failed syscalls in six seconds and not one line of diagnostic. Woken (>= 0),
            // EAGAIN (the value changed first) and EINTR (a signal) are all normal and retry at
            // once; the loop still terminates, since the cell only ever goes 1 -> 0.
            self.line("    cmpq $-11, %rax"); // -EAGAIN
            self.line(&format!("    je {wait}"));
            self.line("    cmpq $-4, %rax"); // -EINTR
            self.line(&format!("    je {wait}"));
            self.line("    testq %rax, %rax");
            self.line(&format!("    jns {wait}"));
            // Any other error means this futex cannot block here. Waiting is still REQUIRED for
            // correctness (the parent must not run on before the thread ends), so fall back to a
            // sleeping poll: 1 ms per turn instead of a busy core. It should never run, and if it
            // ever does it degrades throughput rather than burning the machine.
            self.line("    pushq $1000000"); // timespec.tv_nsec = 1 ms
            self.line("    pushq $0"); //      timespec.tv_sec  = 0
            self.line("    movq %rsp, %rdi");
            self.line("    xorq %rsi, %rsi");
            self.line("    movq $35, %rax"); // SYS_nanosleep
            self.line("    syscall");
            self.line("    addq $16, %rsp");
            self.line(&format!("    jmp {wait}"));
            self.line(&format!("{done}:"));
        }
    }

    /// How many threads one looping `spawn` site can have in flight. The ring is what gives a
    /// loop a CEILING: the 65th spawn waits for the oldest to finish instead of adding to an
    /// unbounded pile. A fork per connection has no such limit, which is how a test VM reached
    /// 2920 processes at load 3.43 on two cores without anybody meaning to.
    const SPAWN_SLOTS: usize = 64;

    /// Emits the futex-join for the cell whose ADDRESS is in `base`, with its owner pid at
    /// `8(base)`. The register-addressed twin of [`emit_join`], for the ring slots, whose cell is
    /// chosen at run time and so has no name to `leaq`. Clobbers %rax/%rcx/%rdx/%rdi/%rsi/%r10/%r11
    /// and leaves `base` untouched.
    fn emit_join_at(&mut self, base: &str) {
        let wait = self.fresh_label("join_wait");
        let done = self.fresh_label("join_done");
        // Whose thread is this? A forked child inherited the cell but not the thread, so waiting
        // on it would block on a wake that cannot come.
        self.line("    movq $39, %rax"); // SYS_getpid
        self.line("    syscall");
        self.line(&format!("    cmpq 8({base}), %rax"));
        self.line(&format!("    jne {done}"));
        self.line(&format!("{wait}:"));
        self.line(&format!("    movq ({base}), %rdx")); // expected tid
        self.line("    testq %rdx, %rdx");
        self.line(&format!("    jz {done}")); // never used, or already finished
        self.line(&format!("    movq {base}, %rdi")); // uaddr
        self.line("    xorq %rsi, %rsi"); // FUTEX_WAIT
        self.line("    xorq %r10, %r10"); // timeout = NULL
        self.line("    movq $202, %rax"); // SYS_futex
        self.line("    syscall");
        // A failing futex returns at once, so jumping back unconditionally would spin. Retry only
        // on the answers that mean "try again" (EAGAIN, EINTR, or a genuine wake); anything else
        // falls through to a sleeping poll, which is slow but never a burning core.
        self.line("    cmpq $-11, %rax");
        self.line(&format!("    je {wait}"));
        self.line("    cmpq $-4, %rax");
        self.line(&format!("    je {wait}"));
        self.line("    testq %rax, %rax");
        self.line(&format!("    jns {wait}"));
        self.line("    pushq $1000000"); // 1 ms
        self.line("    pushq $0");
        self.line("    movq %rsp, %rdi");
        self.line("    xorq %rsi, %rsi");
        self.line("    movq $35, %rax"); // SYS_nanosleep
        self.line("    syscall");
        self.line("    addq $16, %rsp");
        self.line(&format!("    jmp {wait}"));
        self.line(&format!("{done}:"));
    }

    /// Joins every slot of a looping site's ring, at the spawning function's return. A loop over
    /// the table rather than `slots` copies of the join: at 64 slots the unrolled form would add
    /// some 1600 instructions per site, for code that runs once.
    fn emit_join_table(&mut self, table: &str, slots: usize) {
        let top = self.fresh_label("jointab");
        self.line("    pushq %rbx"); // callee-saved: the cursor has to survive the syscalls
        self.line(&format!("    leaq {table}(%rip), %rbx"));
        self.line(&format!("    pushq ${slots}")); // the counter lives on the stack, out of the way
        self.line(&format!("{top}:"));
        self.emit_join_at("%rbx");
        self.line("    addq $16, %rbx"); // next (ctid, owner) pair
        self.line("    decq (%rsp)");
        self.line(&format!("    jnz {top}"));
        self.line("    addq $8, %rsp");
        self.line("    popq %rbx");
    }

    /// Emits `par a() ~~ b() ~~ c()` (RFC-0008 §5.1a): spawn every peer on its own thread, then
    /// join them ALL before the next statement. Reuses the Layer B `spawn` substrate: each peer
    /// is spawned (which records its ctid cell), then this par's cells are split off the enclosing
    /// scope's join set and joined here, so control does not continue until every peer has
    /// finished, yet the scope-exit join does not wait on them again.
    fn emit_par(&mut self, peers: &[Expr]) {
        let start = self.spawn_ctids.len();
        for peer in peers {
            self.emit_spawn(peer);
        }
        let par_cells: Vec<String> = self.spawn_ctids.split_off(start);
        self.emit_join(&par_cells);
    }

    fn emit_spawn(&mut self, call: &Expr) {
        self.emit_spawn_capturing(call, None);
    }

    /// Emits `let r = graph { node a = e1  node b = e2  yield e3 }` (RFC-0008 §5.1). The nodes are
    /// grouped into topological LEVELS (dependencies first); each level's nodes are mutually
    /// independent, so its CALL nodes run CONCURRENTLY, each on its own thread (the same Layer B
    /// substrate as `par`), joined before the next level. A level with a single node, and any
    /// non-call node (cheap, not worth a thread), runs inline. Then `yield` is evaluated and bound
    /// to `name`. `check_graph_acyclic` has already rejected any cycle.
    /// The dependency edges to schedule a graph over: the DATA edges (a node references another),
    /// plus a serializing EFFECT edge (RFC-0008 §5.2) from an earlier node to a later one whenever
    /// their footprints conflict on a mutable global, so two nodes that would race are placed in
    /// different levels and run in sequence, source order deciding which goes first.
    fn combined_deps(&self, nodes: &[(String, Expr)]) -> Vec<Vec<usize>> {
        let mut deps = node_deps(nodes);
        if let Some(eff) = &self.graph_effects {
            let fps: Vec<(HashSet<String>, HashSet<String>)> = nodes
                .iter()
                .map(|(_, e)| crate::check::expr_footprint(e, eff, &self.mut_globals))
                .collect();
            for j in 0..nodes.len() {
                for i in 0..j {
                    if crate::check::footprints_conflict(&fps[i], &fps[j]) && !deps[j].contains(&i)
                    {
                        deps[j].push(i);
                    }
                }
            }
        }
        deps
    }

    fn emit_graph_let(&mut self, name: &str, nodes: &[(String, Expr)], yield_expr: &Expr) {
        let deps = self.combined_deps(nodes);
        for level in topological_levels(&deps) {
            let call_nodes: Vec<usize> = level
                .iter()
                .copied()
                .filter(|&i| matches!(nodes[i].1, Expr::Call(..)))
                .collect();
            // A single node, or a non-call node, runs inline: no thread is worth the overhead, and
            // a lone node has no peer to run beside.
            if level.len() == 1 || call_nodes.len() < 2 {
                for &idx in &level {
                    self.expr(&nodes[idx].1);
                    let off = self.slot(&nodes[idx].0);
                    self.line(&format!("    movq %rax, {off}(%rbp)"));
                    self.var_types
                        .insert(nodes[idx].0.clone(), self.ty_of(&nodes[idx].1));
                }
                continue;
            }
            // Two or more independent call nodes: spawn each on a thread, capturing its result;
            // evaluate any non-call node inline; join, then bind every result.
            let ctid_start = self.spawn_ctids.len();
            let mut cells: Vec<(usize, String)> = Vec::new();
            for &idx in &level {
                if matches!(nodes[idx].1, Expr::Call(..)) {
                    let cell = format!("sond_par_result_{}", self.all_result_cells.len());
                    self.all_result_cells.push(cell.clone());
                    self.emit_spawn_capturing(&nodes[idx].1, Some(&cell));
                    cells.push((idx, cell));
                } else {
                    self.expr(&nodes[idx].1);
                    let off = self.slot(&nodes[idx].0);
                    self.line(&format!("    movq %rax, {off}(%rbp)"));
                    self.var_types
                        .insert(nodes[idx].0.clone(), self.ty_of(&nodes[idx].1));
                }
            }
            let par_ctids: Vec<String> = self.spawn_ctids.split_off(ctid_start);
            self.emit_join(&par_ctids);
            for (idx, cell) in &cells {
                self.line(&format!("    movq {cell}(%rip), %rax"));
                let off = self.slot(&nodes[*idx].0);
                self.line(&format!("    movq %rax, {off}(%rbp)"));
                self.var_types
                    .insert(nodes[*idx].0.clone(), self.ty_of(&nodes[*idx].1));
            }
        }
        self.expr(yield_expr);
        let off = self.slot(name);
        self.line(&format!("    movq %rax, {off}(%rbp)"));
    }

    /// Emits `let a ~~ b ~~ c = par f() ~~ g() ~~ h()` (RFC-0008 §5.1a, positional result
    /// binding). Each peer is spawned with its own result cell (the child stores its return value
    /// there before exiting), the peers are joined, and each result is loaded into its name's
    /// frame slot BY SOURCE POSITION: `a` gets `f`'s result whichever thread finished first, so
    /// the binding is deterministic regardless of the schedule.
    fn emit_par_let(&mut self, names: &[String], peers: &[Expr]) {
        let ctid_start = self.spawn_ctids.len();
        let mut cells = Vec::with_capacity(peers.len());
        for peer in peers {
            let cell = format!("sond_par_result_{}", self.all_result_cells.len());
            self.all_result_cells.push(cell.clone());
            self.emit_spawn_capturing(peer, Some(&cell));
            cells.push(cell);
        }
        let par_ctids: Vec<String> = self.spawn_ctids.split_off(ctid_start);
        self.emit_join(&par_ctids);
        // Bind each peer's result to its name by position, after the join guarantees it is written.
        for (name, cell) in names.iter().zip(&cells) {
            self.line(&format!("    movq {cell}(%rip), %rax"));
            let off = self.slot(name);
            self.line(&format!("    movq %rax, {off}(%rbp)"));
        }
    }

    /// Emits `let s = par(op) f() ~~ g() ~~ h()` (RFC-0008 §5.1a, the parallel reduce). The peers
    /// are spawned with result cells and joined like `par`-let, then their results are folded with
    /// `op` into a single value bound to `name`. `op` is a commutative-associative operator (the
    /// checker guarantees it), so folding the cells in any order gives the same result: the value
    /// is independent of which thread finished first (RFC-0008 §5.3).
    fn emit_par_reduce(&mut self, name: &str, op: &BinOp, peers: &[Expr]) {
        let ctid_start = self.spawn_ctids.len();
        let mut cells = Vec::with_capacity(peers.len());
        for peer in peers {
            let cell = format!("sond_par_result_{}", self.all_result_cells.len());
            self.all_result_cells.push(cell.clone());
            self.emit_spawn_capturing(peer, Some(&cell));
            cells.push(cell);
        }
        let par_ctids: Vec<String> = self.spawn_ctids.split_off(ctid_start);
        self.emit_join(&par_ctids);
        // Fold the results into %rax with the monoid op.
        let insn = match op {
            BinOp::Add => "addq",
            BinOp::Mul => "imulq",
            BinOp::BitAnd => "andq",
            BinOp::BitOr => "orq",
            BinOp::BitXor => "xorq",
            other => unreachable!("check_par_reduce_op rejects a non-monoid reducer: {other:?}"),
        };
        self.line(&format!("    movq {}(%rip), %rax", cells[0]));
        for cell in &cells[1..] {
            self.line(&format!("    {insn} {cell}(%rip), %rax"));
        }
        let off = self.slot(name);
        self.line(&format!("    movq %rax, {off}(%rbp)"));
    }

    /// Futex-based `lock (m) { body }` acquire: Drepper's "Futexes Are Tricky" protocol,
    /// three states (0=free, 1=held, 2=held+waiters), uses FUTEX_WAKE. The lock
    /// variable `m` is a single 64-bit word (an lvalue). The body runs under the lock;
    /// any early exit (`return`, `break`) releases the lock before continuing.
    ///
    /// A critical section has three ways out — natural fall-through, `break`, `return` — and
    /// all three must release the lock and then each go to its own destination. The body is
    /// emitted while this lock is on the `scopes` stack, so `return`/`break`/`continue`
    /// inside it unwind the locks they cross (running each one's release) before jumping to
    /// their real target. The natural fall-through runs this lock's own release and lands on
    /// `body_cont`, the statement right after the `lock`.
fn emit_lock(&mut self, name: &str, body: &Block) {
        let acquired = self.fresh_label("lock_acq");
        let wait_loop = self.fresh_label("lock_wait_loop");
        let sleep = self.fresh_label("lock_sleep");
        let body_cont = self.fresh_label("lock_cont");

        // --- ACQUIRE (fast path: CAS 0→1, then slow path with futex) ---
        self.line(&format!("    leaq {name}(%rip), %rdi"));
        self.line("    xorq %rax, %rax");          // rax = 0 (expected: free)
        self.line("    movq $1, %rsi");           // rsi = 1 (new: held)
        self.line("    lock cmpxchgq %rsi, (%rdi)");
        self.line(&format!("    je {acquired}")); // fast path: grabbed it
        // Slow path
        self.line(&format!("{wait_loop}:"));
        self.line("    movq $2, %rsi");  // rsi = 2 (new: held+waiter)
        self.line("    movq $1, %rax");  // rax = 1 (expected: held)
        self.line("    lock cmpxchgq %rsi, (%rdi)");
        self.line(&format!("    je {sleep}")); // still held: sleep
        // Released to 0: try to grab
        self.line("    xorq %rax, %rax");   // rax = 0
        self.line("    movq $1, %rsi");     // rsi = 1
        self.line("    lock cmpxchgq %rsi, (%rdi)");
        self.line(&format!("    je {acquired}")); // grabbed it
        // Sleep until woken
        self.line(&format!("{sleep}:"));
        self.line(&format!("    leaq {name}(%rip), %rdi"));
        self.line("    xorq %rsi, %rsi");     // futex_op = FUTEX_WAIT
        self.line("    movq $2, %rdx");       // value = 2
        self.line("    xorq %r10, %r10");     // flags = 0 (no private)
        self.line("    movq $202, %rax");     // SYS_futex = 202
        self.line("    syscall");
        self.line("    cmpq $-11, %rax");     // EAGAIN (value changed): retry
        self.line(&format!("    je {wait_loop}"));
        self.line("    cmpq $-4, %rax");      // EINTR: retry
        self.line(&format!("    jmp {wait_loop}")); // other error: retry

        // --- BODY (early exits unwind this lock via the `scopes` stack) ---
        self.line(&format!("{acquired}:"));
        self.scopes.push(Scope::Lock {
            name: name.to_string(),
        });
        self.block(body);
        self.scopes.pop();

        // --- Natural exit (reached only by falling off the end of the body: release,
        // then continue at `body_cont`. `break`/`return` inside the body already ran their
        // own release and jump away, so they never reach this. ---
        self.line("    pushq %rax");         // save body's value across release (clobbers %rax)
        self.emit_lock_release(name);
        self.line("    popq %rax");          // restore body's value
        self.line(&format!("{body_cont}:"));
    }

    /// Emits the shared release of one `lock`: lower the word to 0, and if it was 2
    /// (someone was waiting) do a `SYS_futex` with `FUTEX_WAKE` on it. On both paths execution
    /// falls through to the next line, where the caller continues to its own destination. The
    /// syscall clobbers `%rax`, so a caller that must preserve a value in `%rax` (a `return`)
    /// saves it first.
    fn emit_lock_release(&mut self, name: &str) {
        let nowake = self.fresh_label("nolockwake");
        // Release: CAS 1→0. If value was 1, stored 0. If value was 2, need to release + wake.
        self.line(&format!("    leaq {name}(%rip), %rdi"));
        self.line("    movq $1, %rax");   // CAS expected: 1
        self.line("    movq $0, %rsi");   // CAS new: 0
        self.line("    lock cmpxchgq %rsi, (%rdi)");
        self.line("    cmpq $2, %rax");   // was value 2? (someone waiting)
        self.line(&format!("    jne {nowake}"));
        // Value was 2: CAS 1→0 failed because value is 2. Store 0 to release.
        // (A plain store is used here instead of CAS 2→0 because: if value is still 2, store 0 releases;
        //  if value changed (e.g., 2→1 by another thread), store 0 still releases — the other thread
        //  will CAS 1→0 themselves on their turn, making this a harmless additional release.)
        self.line("    movq $0, (%rdi)");
        // Wake 1 futex waiter: SYS_futex(202) with op FUTEX_WAKE(1).
        self.line("    movq $1, %rsi");    // futex_op: FUTEX_WAKE = 1
        self.line("    movq $1, %rdx");    // val: wake 1 thread
        self.line("    xorq %r10, %r10");  // flags = 0
        self.line("    movq $202, %rax");  // SYS_futex
        self.line("    syscall");
        self.line(&format!("{nowake}:"));
    }

    /// Releases the locks in `inner_first` (given innermost first) in order, then jumps to
    /// `dest`. When `preserve_rax`, saves `%rax` first and restores it after every release
    /// (they clobber `%rax` via `syscall`) before jumping to `dest`, so a `return` value
    /// survives. The save is the FIRST instruction emitted, straight after the caller has put
    /// the value in `%rax`, and the restore is emitted LAST (reached only by the jump that
    /// ends the last release), so control never falls into it prematurely.
    fn emit_release_chain(&mut self, inner_first: &[String], dest: &str, preserve_rax: bool) {
        if inner_first.is_empty() {
            self.line(&format!("    jmp {dest}"));
            return;
        }
        let n = inner_first.len();
        if preserve_rax {
            self.line("    pushq %rax");
        }
        // Emitted in place, so control falls into the innermost release (entries[0]) directly.
        let entries: Vec<String> = (0..n).map(|_| self.fresh_label("lckrel")).collect();
        let restored = self.fresh_label("lck_restore");
        for i in 0..n {
            let entry = entries[i].clone();
            let cont = if i == n - 1 {
                if preserve_rax {
                    restored.clone()
                } else {
                    dest.to_string()
                }
            } else {
                entries[i + 1].clone()
            };
            self.line(&format!("{entry}:"));
            self.emit_lock_release(&inner_first[i]);
            self.line(&format!("    jmp {cont}"));
        }
        if preserve_rax {
            // Reached only by the last release's jump; restore the return value, then return.
            self.line(&format!("{restored}:"));
            self.line("    popq %rax");
            self.line(&format!("    jmp {dest}"));
        }
    }

    /// Collect the names of the locks on the `scopes` stack, innermost first — the order in
    /// which they must be released (a lock acquired last must be released first).
    fn locks_inner_first(&self) -> Vec<String> {
        self.scopes
            .iter()
            .rev()
            .filter_map(|s| match s {
                Scope::Lock { name, .. } => Some(name.clone()),
                Scope::Loop { .. } => None,
            })
            .collect()
    }

    /// Spawns `call` on a new thread. If `result_cell` is `Some`, the child stores the call's
    /// return value (%rax) into that cell just before it exits, so the parent can read the peer's
    /// result after the join (the `par`-let binding). The store happens strictly before `SYS_exit`,
    /// and the join waits on the thread's exit, so the parent never reads the cell early.
    fn emit_spawn_capturing(&mut self, call: &Expr, result_cell: Option<&str>) {
        let Expr::Call(callee, args) = call else {
            panic!("`spawn` must be followed by a function call");
        };
        let Expr::Var(fname) = callee.as_ref() else {
            todo!("spawn of an indirect call {callee:?}");
        };
        // Arguments cross into the thread through the CHILD's stack, not through registers. A
        // register carried across the clone would have to be callee-saved (to survive evaluating
        // the other arguments), and with %r15 reserved for the arena only four of those remain;
        // writing the arguments into the child's own stack removes the register limit and matches
        // the six a normal call takes.
        let n = args.len();
        if n > ARG_REGS.len() {
            panic!(
                "spawn passes at most {} arguments (got {n})",
                ARG_REGS.len()
            );
        }
        // 256 KiB per thread stack. The top 32 bytes hold the arena control block; the arguments
        // sit just below it, in a 16-aligned block, and the child's stack grows down from there.
        const THREAD_STACK: i64 = 262144;
        let argspace = (n as i64 * 8 + 15) & !15;
        self.uses_spawn = true;
        // Inside a loop the site cannot own ONE cell: the next turn would overwrite the tid of a
        // thread still running, and that thread would never be joined -- a silent leak, which is
        // why this used to be a compile error. It owns a ring of slots instead, rotating through
        // them and joining the one it is about to reuse.
        let in_loop = self.scopes.iter().any(|s| matches!(s, Scope::Loop { .. }));
        let mut cell = String::new();
        let mut table = String::new();
        if in_loop {
            table = format!("sond_ctidtab_{}", self.spawn_tables.len());
            self.spawn_tables.push((table.clone(), Self::SPAWN_SLOTS));
            self.fn_spawn_tables
                .push((table.clone(), Self::SPAWN_SLOTS));
        } else {
            cell = format!("sond_ctid_{}", self.all_ctids.len());
            self.all_owner_cells.push(Self::owner_cell(&cell));
            self.all_ctids.push(cell.clone());
            self.spawn_ctids.push(cell.clone());
        }

        if in_loop {
            // %rbx holds the chosen slot's address from here to the clone. It is callee-saved, so
            // the syscalls in between leave it alone, and it is restored on the way out.
            self.line("    pushq %rbx");
        }
        // Evaluate the arguments and push them, so arg0 ends on top (at 0(%rsp), arg i at 8i).
        for arg in args.iter().rev() {
            self.expr(&arg.value); // -> %rax
            self.line("    pushq %rax");
        }

        if in_loop {
            // Rotate to the next slot and take its address. The index is per site and lives in
            // `.data`; two threads running the SAME spawn site at once would race on it, which is
            // the same single-writer assumption the static cell already made.
            let idx = format!("{table}_i");
            let wrap = self.fresh_label("slot_wrap");
            self.line(&format!("    movq {idx}(%rip), %rax"));
            self.line("    incq %rax");
            self.line(&format!("    cmpq ${}, %rax", Self::SPAWN_SLOTS));
            self.line(&format!("    jb {wrap}"));
            self.line("    xorq %rax, %rax");
            self.line(&format!("{wrap}:"));
            self.line(&format!("    movq %rax, {idx}(%rip)"));
            self.line("    shlq $4, %rax"); // 16 bytes per (ctid, owner) pair
            self.line(&format!("    leaq {table}(%rip), %rbx"));
            self.line("    addq %rax, %rbx");
            // Wait for whoever had this slot before. On the first `SPAWN_SLOTS` turns the cell is
            // zero and this returns at once; after that it is what puts a ceiling on the loop.
            self.emit_join_at("%rbx");
        }
        // mmap the child stack. The pushed arguments are untouched (a syscall does not use the
        // stack), so they are still there to copy across.
        self.line("    xorq %rdi, %rdi");
        self.line(&format!("    movq ${THREAD_STACK}, %rsi"));
        self.line("    movq $3, %rdx"); // PROT_READ|PROT_WRITE
        self.line("    movq $0x20022, %r10"); // MAP_PRIVATE|MAP_ANONYMOUS|MAP_STACK
        self.line("    movq $-1, %r8");
        self.line("    xorq %r9, %r9");
        self.line("    movq $9, %rax"); // SYS_mmap -> base in %rax
        self.line("    syscall");
        // Copy each argument from the parent stack to the child's argument block. arg i lands at
        // TCB - 8*(i+1), where TCB = base + THREAD_STACK - 32; i.e. base + (THREAD_STACK - 40 - 8i).
        for i in 0..n {
            self.line(&format!("    movq {}(%rsp), %rcx", i * 8));
            self.line(&format!(
                "    movq %rcx, {}(%rax)",
                THREAD_STACK - 40 - 8 * i as i64
            ));
        }
        // The child's stack pointer: below the arguments, still 16-aligned.
        self.line(&format!(
            "    leaq {}(%rax), %rsi",
            THREAD_STACK - 32 - argspace
        ));

        // The join waits on a sentinel the parent stores BEFORE the clone, which the kernel
        // clears to 0 and futex-wakes at the child's exit (CLONE_CHILD_CLEARTID). Doing it this way
        // avoids two races at once: storing the tid AFTER clone would let a fast child clear the
        // cell first and then have it overwritten, so the join waits on a dead thread forever; and
        // CLONE_CHILD_SETTID does not help, since the man page notes its store "may not have
        // completed before the clone call returns in the parent" under CLONE_VM, so the parent could
        // read 0 and skip the wait entirely. A sentinel written before the clone is reliably 1 (set
        // by the parent) or 0 (cleared by the kernel), and depends on neither ordering.
        if in_loop {
            self.line("    movq $1, (%rbx)");
        } else {
            self.line(&format!("    movq $1, {cell}(%rip)"));
        }

        // Record WHICH PROCESS owns this thread, for the join to check. `fork` copies the address
        // space but only the calling thread, so a forked child inherits this cell with the
        // sentinel still set while the thread it names does not exist in it: without the owner
        // check the child's join would futex-wait on something the kernel will never wake, and a
        // program that forks per connection would leak one stuck process per connection. getpid
        // returns the thread group id, identical in every thread of a process and different in a
        // forked child, which is exactly the distinction needed. Clobbers %rax/%rcx/%r11, all dead
        // here; %rsi (the child stack) is untouched.
        self.line("    movq $39, %rax"); // SYS_getpid
        self.line("    syscall");
        if in_loop {
            self.line("    movq %rax, 8(%rbx)");
        } else {
            self.line(&format!("    movq %rax, {}(%rip)", Self::owner_cell(&cell)));
        }

        // clone(flags, child_stack, ptid=0, ctid=&cell, tls=0).
        // CLONE_VM|FS|FILES|SIGHAND|THREAD|SYSVSEM|CHILD_CLEARTID.
        self.line("    movq $0x250f00, %rdi");
        self.line("    xorq %rdx, %rdx"); // parent_tid = NULL
        if in_loop {
            self.line("    movq %rbx, %r10"); // child_tid cell: the slot chosen above
        } else {
            self.line(&format!("    leaq {cell}(%rip), %r10")); // child_tid cell
        }
        self.line("    xorq %r8, %r8"); // tls = NULL
        self.line("    movq $56, %rax"); // SYS_clone
        self.line("    syscall");

        let child = self.fresh_label("thread_child");
        self.line("    testq %rax, %rax");
        self.line(&format!("    jz {child}"));
        // Parent: the sentinel is already in the cell and the kernel owns clearing it, so there is
        // nothing to record. Drop the pushed arguments and continue.
        if n > 0 {
            self.line(&format!("    addq ${}, %rsp", n * 8));
        }
        if in_loop {
            self.line("    popq %rbx");
        }
        let after = self.fresh_label("thread_parent");
        self.line(&format!("    jmp {after}"));

        // Child: rax = 0, rsp = its own stack. The arena control block is `argspace` above rsp
        // (the reserved top 32 bytes), zeroed by mmap so the first allocation grows a fresh block.
        // The arguments sit between rsp and the block; load them into the argument registers, then
        // call, then exit THIS thread (SYS_exit, not exit_group), which clears + futex-wakes the
        // ctid cell.
        self.line(&format!("{child}:"));
        self.line(&format!("    leaq {argspace}(%rsp), %r15")); // %r15 = arena control block
        for (i, argreg) in ARG_REGS.iter().take(n).enumerate() {
            self.line(&format!(
                "    movq {}(%rsp), {argreg}",
                argspace - 8 * (i as i64 + 1)
            ));
        }
        self.line(&format!("    call {fname}"));
        if let Some(cell) = result_cell {
            // Store the peer's return value before exiting; the join (on the ctid cell) fires only
            // after this thread exits, so the parent reads the result strictly after this store.
            self.line(&format!("    movq %rax, {cell}(%rip)"));
        }
        self.line("    xorq %rdi, %rdi"); // thread exit status 0
        self.line("    movq $60, %rax"); // SYS_exit (this thread only)
        self.line("    syscall");
        self.line(&format!("{after}:"));
    }

    /// Marks the stack NON-EXECUTABLE. A GNU-format object that carries no `.note.GNU-stack`
    /// section makes the linker assume the worst and mark the whole program's stack executable,
    /// which is a live W^X hole in every binary and a hard error on newer linkers. Sond never
    /// executes from the stack, so the note is empty: it just states the fact.
    fn emit_gnu_stack_note(&mut self) {
        self.line(".section .note.GNU-stack,\"\",@progbits");
    }

    /// Emits the module-level globals into `.data`.
    fn emit_globals(&mut self) {
        let data = std::mem::take(&mut self.global_data);
        let arrays = std::mem::take(&mut self.global_array_data);
        let ctids = std::mem::take(&mut self.all_ctids);
        let tables = std::mem::take(&mut self.spawn_tables);
        self.line(".data");
        // DECLARE the section's alignment, do not hope for it. Without a directive the assembler
        // records AddrAlign 1 and the linker may start `.data` at any address, so whether an
        // 8-byte datum in it is actually aligned depends on how much came before: a hosted link
        // hides this (crt objects declare 8 and the merged section inherits it), a freestanding
        // one does not. The cost was a futex whose address landed 2 mod 4, which makes FUTEX_WAIT
        // fail with EINVAL WITHOUT EVER BLOCKING, turning every join into a spin: 743% of CPU on
        // an idle machine, with no error and no crash, while the same code in a smaller program
        // worked by luck. Everything emitted here is 8-byte data, so 8 is the floor.
        self.line(".p2align 3");
        // The default thread's arena control block: [next_free_byte, block_end, total]. Every
        // program reads the arena through %r15, seeded at entry to point here; a spawned thread
        // gets its own block instead. Emitted unconditionally, since any allocation needs it.
        self.line("sond_arena_tcb: .quad 0, 0, 0");
        if self.uses_spawn {
            // The per-thread stack size, used as both an mmap length and the offset from the
            // mapping's base to its top.
            self.line(".set sond_thread_stack, 262144");
        }
        for (name, value) in data {
            self.line(&format!("{name}: .quad {value}"));
        }
        // A string global holds the address of its interned `.rodata` bytes (a `.quad <label>`).
        let strs = std::mem::take(&mut self.global_str_data);
        for (name, label) in strs {
            self.line(&format!("{name}: .quad {label}"));
        }
        // An array with values to write down stays here as literals; an ALL-ZERO one is reserved
        // in `.bss` below instead (see `emit_zeroed_arrays`). A partly initialized one writes its
        // values and reserves the rest: `let buf[N] = [hdr0, hdr1]` is two literals plus a
        // reservation, not N literals of which N-2 say nothing.
        let (zeroed, valued): (Vec<_>, Vec<_>) = arrays
            .into_iter()
            .partition(|(_, words, _)| words.iter().all(GlobalWord::is_zero));
        for (name, words, len) in valued {
            let quads = words
                .iter()
                .map(GlobalWord::operand)
                .collect::<Vec<_>>()
                .join(", ");
            self.line(&format!("{name}: .quad {quads}"));
            let tail = len.saturating_sub(words.len());
            if tail > 0 {
                self.line(&format!("    .zero {}", tail * 8));
            }
        }
        // One clear_child_tid cell per spawn site, zeroed (no live thread yet). Realigned here as
        // a belt on top of the section's own declaration: these are futex addresses, where being
        // off by two bytes costs a core and says nothing, so the guarantee is worth stating twice.
        self.line(".p2align 3");
        for cell in ctids {
            self.line(&format!("{cell}: .quad 0"));
        }
        // The ring of a looping spawn site: `slots` pairs of (clear_child_tid, owner pid), plus
        // the rotating index. All zero, which reads as "slot never used" everywhere it is
        // checked, so the first turns of the loop join nothing and start immediately.
        for (table, slots) in tables {
            self.line(&format!(
                "{table}: .quad {}",
                vec!["0"; slots * 2].join(", ")
            ));
            self.line(&format!("{table}_i: .quad {}", slots - 1)); // first rotation lands on 0
        }
        // The pid that started each of those threads. Zero until the spawn runs, which no real
        // pid ever is, so a join over a site that never executed skips instead of waiting.
        let owners = std::mem::take(&mut self.all_owner_cells);
        for cell in owners {
            self.line(&format!("{cell}: .quad 0"));
        }
        // One result cell per `par`-let peer site, where the peer stores its return value.
        let result_cells = std::mem::take(&mut self.all_result_cells);
        for cell in result_cells {
            self.line(&format!("{cell}: .quad 0"));
        }
        self.emit_zeroed_arrays(zeroed);
        self.emit_view_globals();
    }

    /// Reserves the ALL-ZERO module-level arrays in `.bss`, one `.zero <bytes>` each.
    ///
    /// A zeroed buffer carries no information, so writing it out as `.quad 0` literals pays for
    /// it twice: the assembler parses one literal per word, and the zeros then travel inside the
    /// binary. An image buffer of 2.7M pixels is a ~21 MB file of nothing that way. `.bss` states
    /// the size only; the loader (or the ELF program header's `memsz > filesz`) supplies the
    /// zeros, which is exactly the same guarantee the program had before.
    fn emit_zeroed_arrays(&mut self, zeroed: Vec<(String, Vec<GlobalWord>, usize)>) {
        if zeroed.is_empty() {
            return;
        }
        self.line(".bss");
        // Same 8-byte floor as `.data`: everything here is word-indexed (`a[i]` is a `movq` at
        // `base + i*8`), and an unaligned base would make those loads straddle cache lines.
        self.line(".p2align 3");
        for (name, _, len) in zeroed {
            self.line(&format!("{name}: .zero {}", len * 8));
        }
    }

    /// Reserves a typed view global (`let NAME: T[N]`) in `.bss` with the correct byte size.
    /// A `u8[32]` reserves 32 BYTES; a `u32[16]` reserves 64 BYTES.
    fn emit_view_globals(&mut self) {
        if self.global_view_lengths.is_empty() {
            return;
        }
        // Sorted, and WITHOUT draining the map: `enter_function` reads it to learn each
        // global's element width AND its length, so emptying it here would leave every function
        // compiled after this point indexing at the wrong stride, depending on the order of two
        // passes. Same reason it is not a `HashMap` iteration: emitted code is a build artefact
        // and must not depend on hash order.
        //
        // The map holds ELEMENT COUNTS, and `.bss` is reserved in BYTES: the multiplication
        // belongs here, at the one place that speaks bytes. Storing bytes instead would be one
        // conversion fewer and would make `NAME.len` -- which is in elements -- report 56 for an
        // `i64[7]`, silently, since nothing else in the compiler would disagree.
        let mut views: Vec<_> = self
            .global_view_lengths
            .iter()
            .map(|(name, (ty, elem_count))| {
                let byte_len = elem_count * ty.bits() as i64 / 8;
                (name.clone(), byte_len)
            })
            .collect();
        views.sort();
        self.line(".bss");
        self.line(".p2align 3");
        for (name, byte_len) in views {
            self.line(&format!("{name}: .zero {byte_len}"));
        }
    }

    /// Emits a block: its statements, then its tail expression (leaving the
    /// tail's value, or the last computed value, in `%rax`).
    /// Emits a coverage mark for the next statement, records that this point executed, into
    /// the shared buffer via `__cov_ptr`. A no-op unless `coverage` is on (so ordinary builds
    /// are byte-identical). Uses `%r11`, a scratch register free at a statement boundary.
    fn cov_mark(&mut self, line: u32) {
        if !self.cov_active {
            return;
        }
        let id = self.cov_points;
        self.cov_points += 1;
        self.cov_lines.push(line);
        self.line("    movq __cov_ptr(%rip), %r11");
        self.line(&format!("    movq $1, {}(%r11)", id * 8));
    }

    fn block(&mut self, block: &Block) {
        // One coverage mark per BASIC BLOCK (this block's entry). A block runs all-or-nothing;
        // each `if`/`match` branch is emitted as its own block (recursive `block()` call) and
        // gets its own mark, so this is branch-level coverage, and it captures expression-
        // bodied blocks (a tail with no statements) that per-statement marking would miss.
        self.cov_mark(block.stmt_lines.first().copied().unwrap_or(0));
        for (i, stmt) in block.stmts.iter().enumerate() {
            // RFC-0016 --debug checkpoint: announce the statement's source line before running it,
            // and hand the agent this function's frame base (%rbp) so it can read the debuggee's
            // locals (at negative offsets) and walk the saved-rbp chain for a backtrace.
            if self.debug {
                let line = block.stmt_lines.get(i).copied().unwrap_or(0);
                self.line(&format!("    movq ${line}, %rdi"));
                self.line("    movq %rbp, %rsi");
                self.line("    call __dbg_step");
            }
            match stmt {
                Stmt::Let {
                    name,
                    ty,
                    value,
                    array_len,
                    ..
                } => {
                    // For `let a[N]` the name's slot already points at the reserved
                    // region (set in the prologue); do not overwrite it. But a list
                    // initializer `let a[N] = [e0, e1, ...]` stores each element into
                    // the reserved words (a[i] = base + i*8).
                    if let Some(words) = array_len.filter(|_| matches!(value, Expr::Call(..))) {
                        // `let r = f(..)` where `f` returns a small struct/enum by value:
                        // the call returns its words in RAX:RDX; store them into r's region.
                        self.expr(value); // call -> word 0 in %rax, word 1 in %rdx
                        let off = self.slot(name);
                        self.line(&format!("    movq {off}(%rbp), %rcx")); // r's base ptr
                        self.line("    movq %rax, (%rcx)"); // word 0
                        if words == 2 {
                            self.line("    movq %rdx, 8(%rcx)"); // word 1
                        }
                    } else if array_len.is_some() {
                        if let Expr::List(items) = value {
                            let off = self.slot(name);
                            for (i, item) in items.iter().enumerate() {
                                self.expr(item); // element value -> %rax
                                self.line(&format!("    movq {off}(%rbp), %rcx")); // base ptr
                                self.line(&format!("    movq %rax, {}(%rcx)", i * 8));
                            }
                        }
                    } else {
                        // A type annotation (`let h: u64` / `let x: u32`) is AUTHORITATIVE
                        // for the local's kind, its sign drives later shifts, and a narrow
                        // width truncates the stored value.
                        let value_kind = self.ty_of(value);
                        let recorded = ty.as_ref().map_or(value_kind, kind_of_type);
                        // `let ar: i32[n]` with nothing to the right of it: the declaration
                        // already said the element width and how many, which is everything a
                        // reservation needs, so it RESERVES rather than binding a bare address.
                        // Without this the same line compiled to a null pointer that faulted on
                        // first use — the worst possible answer to the one spelling that reads as
                        // "give me a buffer". A view initialized from an expression (an existing
                        // address, an allocator's return) keeps binding that expression.
                        // `let s: string = ".."`: the code points are CONSTANT, so they are
                        // interned in `.rodata` and the binding is their address. Building them
                        // at run time meant a function called per connection reserved arena and
                        // copied its own literal on every call.
                        if let (Some(crate::ast::Type::View(elem, Some(_))), Expr::Str(text)) =
                            (ty.as_ref(), value)
                            && kind_of_type(elem) == Ty::U32
                        {
                            let label = format!(".Lchars{}", self.char_consts.len());
                            let cps: Vec<u32> = text.chars().map(|c| c as u32).collect();
                            self.char_consts.push((label.clone(), cps));
                            self.line(&format!("    leaq {label}(%rip), %rax"));
                            let off = self.slot(name);
                            self.line(&format!("    movq %rax, {off}(%rbp)"));
                            self.var_types.insert(name.clone(), Ty::I64);
                            self.view_elem.insert(name.clone(), Ty::U32);
                            // Also record the UTF-8 rodata label so passing this
                            // variable as a `string` argument sends the real
                            // source bytes, not the code-point array.
                            let utflabel = self.intern_string(text);
                            let asgn_line = block.stmt_lines.get(i).copied().unwrap_or(0);
                            let desc = format!("{} = \"{}\"", name, text);
                            self.string_assignments
                                .entry(name.clone())
                                .or_default()
                                .push((asgn_line, desc));
                            self.string_rodata_cur.insert(name.clone(), utflabel);
                            self.string_vars.insert(name.clone());
                            if let Some(crate::ast::Type::View(_, Some(l))) = ty.as_ref()
                                && let Ok(n) = l.parse::<i64>()
                            {
                                self.view_len.insert(name.clone(), n);
                            }
                            continue;
                        }
                        let reserves = matches!(
                            (ty.as_ref(), value),
                            (
                                Some(crate::ast::Type::View(_, Some(_))),
                                Expr::Int(0) | Expr::List(_)
                            ) if !matches!(value, Expr::Str(_))
                        );
                        if reserves {
                            let Some(crate::ast::Type::View(elem, Some(len))) = ty.as_ref() else {
                                unreachable!("checked by `reserves`")
                            };
                            // The arena grows with `mmap`, which needs a kernel underneath. On
                            // UEFI there is none, and emitting the syscall anyway would produce a
                            // binary that compiles and then executes a `syscall` instruction on
                            // bare metal. Say so instead, and name the way out.
                            assert!(
                                !self.uefi,
                                "reserving `{name}` with a declared length is not available on \
                                 UEFI yet (its arena grows with a syscall); allocate it \
                                 explicitly there: `let {name}: {}[n] = alloc(n * {})`",
                                type_name_of(elem),
                                (kind_of_type(elem).bits().max(8) / 8),
                            );
                            let width = kind_of_type(elem).bits().max(8) as i64 / 8;
                            self.uses_view_alloc = true;
                            // The length is either a NAME holding the count or the count itself
                            // written down; reading the second as a variable name looked for a
                            // variable called `32`.
                            match len.parse::<i64>() {
                                Ok(n) => self.line(&format!("    movq ${n}, %rax")),
                                Err(_) => self.expr(&Expr::Var(len.clone())),
                            }
                            self.line(&format!("    imulq ${width}, %rax, %rdi"));
                            self.line("    call sond_view_alloc");
                            // `= {a, b, c}`: write the given elements into the fresh storage, at
                            // the element's width. What is left keeps the zeros the reservation
                            // already guarantees, so a short list is a prefix, not a surprise.
                            if let Expr::List(items) = value {
                                let elem_kind = kind_of_type(elem);
                                self.line("    pushq %rax"); // the base address
                                for (i, item) in items.iter().enumerate() {
                                    self.expr(item);
                                    self.canon(elem_kind);
                                    self.line("    movq (%rsp), %rcx");
                                    let off = i as i64 * width;
                                    match width {
                                        1 => self.line(&format!("    movb %al, {off}(%rcx)")),
                                        2 => self.line(&format!("    movw %ax, {off}(%rcx)")),
                                        4 => self.line(&format!("    movl %eax, {off}(%rcx)")),
                                        _ => self.line(&format!("    movq %rax, {off}(%rcx)")),
                                    }
                                }
                                self.line("    popq %rax");
                            }
                        } else {
                            self.expr(value);
                        }
                        let off = self.slot(name);
                        if value_kind.is_float() {
                            self.line(&format!("    movsd %xmm0, {off}(%rbp)"));
                        } else {
                            if recorded.bits() < 64 && !recorded.is_float() {
                                self.canon(recorded); // truncate to the declared width
                            }
                            self.line(&format!("    movq %rax, {off}(%rbp)"));
                        }
                        self.var_types.insert(name.clone(), recorded);
                        // `let px: u32[] = heap_alloc(..)`: the variable holds the base address,
                        // and the element type is what indexing needs.
                        if let Some(crate::ast::Type::View(elem, len)) = ty.as_ref() {
                            self.view_elem.insert(name.clone(), kind_of_type(elem));
                            if let Some(n) = len.as_ref().and_then(|l| l.parse::<i64>().ok()) {
                                self.view_len.insert(name.clone(), n);
                            }
                        }
                        // `let q: string = l` where l's type is `string` (view of code points):
                        // inherit the source's length and element info so `.len` works on the copy.
                        // The parser does NOT lower `let q: string = l` to a view type when l is
                        // a variable (it lowers only for string *literals*). So `ty` here is
                        // `Some(Type::Named("string"))`, which does NOT match the `Type::View`
                        // check above. Yet `q` holds the SAME representation as `l` (a pointer to
                        // `.rodata` code points), so `.len` must be inherited from `l`.
                        if let Expr::Var(src_name) = value {
                            if let Some(&n) = self.view_len.get(src_name) {
                                self.view_len.insert(name.clone(), n);
                            }
                            if let Some(&k) = self.view_elem.get(src_name) {
                                self.view_elem.insert(name.clone(), k);
                            }
                            // Propagate the UTF-8 rodata label too, so `q` can be passed
                            // as a `string` argument (even when the source was itself a
                            // copy of a string literal).
                            if let Some(label) = self.string_rodata_cur.get(src_name) {
                                self.string_rodata_cur.insert(name.clone(), label.clone());
                            }
                            // Only mark as a string if the source IS a string variable.
                            if self.string_vars.contains(src_name) {
                                self.string_vars.insert(name.clone());
                                // Copy assignment info so the error can trace the copy chain.
                                if let Some(src_asgns) = self.string_assignments.get(src_name) {
                                    let src_asgns = src_asgns.clone();
                                    self.string_assignments
                                        .entry(name.clone())
                                        .or_default()
                                        .extend(
                                            src_asgns.iter().map(|&(l, ref s)| {
                                                (l, format!("{} = {}", name, s))
                                            }),
                                        );
                                }
                            }
                        }
                        // `let l = f()`: if `f` returns `string`, treat `l` as a string
                        // variable so the call site can know the variable's representation.
                        if let Expr::Call(callee, _) = value {
                            let callee_name = match callee.as_ref() {
                                Expr::Var(n) => n.as_str(),
                                _ => "",
                            };
                            if self.string_returning_fns.contains(callee_name) {
                                // It is a string, but we don't know its rodata label
                                // (the function parameter is untraceable). It will
                                // still enter the `string_vars` check at call sites.
                                self.string_vars.insert(name.clone());
                            }
                        }
                    }
                }
                Stmt::Assign {
                    target: Expr::Var(name),
                    value,
                } => {
                    let kind = self.ty_of(value);
                    self.expr(value);
                    // When assigning a string literal to a string variable,
                    // update the tracked rodata label so future uses of the
                    // variable in a `string` argument send the correct bytes.
                    let label = if let Expr::Str(text) = value
                        && self.view_elem.contains_key(name)
                    {
                        // Record the assignment location for error messages.
                        let asgn_line = block.stmt_lines.get(i).copied().unwrap_or(0);
                        let desc = format!("{} = \"{}\"", name, text);
                        self.string_assignments
                            .entry(name.clone())
                            .or_default()
                            .push((asgn_line, desc));
                        Some(self.intern_string(text))
                    } else {
                        None
                    };
                    if let Some(l) = label {
                        self.string_rodata_cur.insert(name.clone(), l);
                        self.string_vars.insert(name.clone());
                    }
                    // When assigning one tracked string to another, copy the rodata key
                    // and the assignment chain.
                    if let Expr::Var(src) = value {
                        let src = src.clone();
                        if let Some(label) = self.string_rodata_cur.get(&src) {
                            let label = label.clone();
                            self.string_rodata_cur.insert(name.clone(), label);
                            self.string_vars.insert(name.clone());
                            if let Some(src_asgns) = self.string_assignments.get(&src) {
                                let src_asgns = src_asgns.clone();
                                self.string_assignments
                                    .entry(name.clone())
                                    .or_default()
                                    .extend(
                                        src_asgns
                                            .iter()
                                            .map(|&(l, ref s)| (l, format!("{} = {}", name, s))),
                                    );
                            }
                        }
                    }
                    if let Some(&off) = self.locals.get(name) {
                        if kind.is_float() {
                            self.line(&format!("    movsd %xmm0, {off}(%rbp)"));
                        } else {
                            self.line(&format!("    movq %rax, {off}(%rbp)"));
                        }
                    } else if self.global_names.contains(name) {
                        self.line(&format!("    movq %rax, {name}(%rip)"));
                    } else {
                        panic!("codegen: assignment to unknown variable `{name}`");
                    }
                }
                // `base[index] = value`: an 8-byte word, or the view's element width. The store
                // must write EXACTLY the element's bytes: a 64-bit store into a u32 slot would
                // silently overwrite the next element, which is the failure this type prevents.
                Stmt::Assign {
                    target: Expr::Index(base, index),
                    value,
                } => {
                    let elem = match base.as_ref() {
                        Expr::Var(n) => self.view_elem.get(n).copied(),
                        _ => None,
                    };
                    self.expr(base);
                    self.line("    pushq %rax");
                    self.expr(index);
                    let shift = match elem.map(|e| e.bits()) {
                        Some(8) => 0,
                        Some(16) => 1,
                        Some(32) => 2,
                        _ => 3,
                    };
                    if shift > 0 {
                        self.line(&format!("    shlq ${shift}, %rax"));
                    }
                    self.line("    pushq %rax");
                    self.expr(value); // value -> %rax
                    self.line("    popq %rcx"); // scaled index
                    self.line("    popq %rdx"); // base
                    self.line("    addq %rdx, %rcx"); // address
                    match elem.map(|e| e.bits()) {
                        Some(8) => self.line("    movb %al, (%rcx)"),
                        Some(16) => self.line("    movw %ax, (%rcx)"),
                        Some(32) => self.line("    movl %eax, (%rcx)"),
                        _ => self.line("    movq %rax, (%rcx)"),
                    }
                }
                Stmt::Assign { .. } => todo!("x64 codegen: assignment to non-variable target"),
                Stmt::Expr(e) => self.expr(e),
                Stmt::Spawn(call) => self.emit_spawn(call),
                Stmt::Par(peers) => self.emit_par(peers),
                Stmt::ParLet { names, peers, .. } => self.emit_par_let(names, peers),
                Stmt::ParReduce {
                    name, op, peers, ..
                } => self.emit_par_reduce(name, op, peers),
                Stmt::GraphLet {
                    name,
                    nodes,
                    yield_expr,
                    ..
                } => self.emit_graph_let(name, nodes, yield_expr),
                Stmt::Return(e) => {
                    match e {
                        Some(e) => self.expr(e),
                        None => self.line("    movq $0, %rax"),
                    }
                    // A `return` must release every lock it leaves held, then return the value
                    // it has already left in `%rax`. The release code clobbers `%rax` (futex
                    // `syscall`), so `emit_release_chain` saves and restores it.
                    let locks = self.locks_inner_first();
                    if locks.is_empty() {
                        self.line(&format!("    jmp {}", self.ret_label.clone()));
                    } else {
                        self.emit_release_chain(&locks, &self.ret_label.clone(), true);
                    }
                }
                Stmt::For { cond, body, .. } => {
                    /* Track string-variable state before the loop body so we can detect
                     * which variables were modified inside it.  The language rule: a
                     * string variable that is assigned anywhere in a loop body becomes
                     * *unknown* after the loop (because the body may run 0..N times and
                     * the value at the end of one iteration might differ from what it
                     * was before entering the loop). */
                    let before_loop = self.save_string_state();
                    // A string assigned anywhere in the body already differs at the TOP of the
                    // second iteration, so every use inside the loop, even one written before the
                    // assignment, must treat its text as unknown.
                    let mut assigned = HashSet::new();
                    assigned_vars(body, &mut assigned);
                    if let Some(c) = cond {
                        assigned_vars_in_expr(c, &mut assigned);
                    }
                    for var in &assigned {
                        self.string_rodata_cur.remove(var);
                    }
                    let start = self.fresh_label("for");
                    let end = self.fresh_label("endfor");
                    self.scopes.push(Scope::Loop {
                        start: start.clone(),
                        end: end.clone(),
                    });
                    self.line(&format!("{start}:"));
                    if let Some(c) = cond {
                        self.expr(c);
                        self.line("    cmpq $0, %rax");
                        self.line(&format!("    je {end}"));
                    }
                    self.block(body);
                    // Invalidate any string variable that was modified inside the
                    // loop body — after N iterations its value may differ from
                    // what it was before the loop started.
                    let after_loop = self.save_string_state();
                    for var in &before_loop.1 {
                        if after_loop.0.get(var) != before_loop.0.get(var) {
                            self.string_rodata_cur.remove(var);
                        }
                    }
                    self.line(&format!("    jmp {start}"));
                    self.line(&format!("{end}:"));
                    self.scopes.pop();
                }
                Stmt::Lock { name, body } => self.emit_lock(name, body),
                Stmt::Break => {
                    // A `break` releases every `lock` between here and its destination, then
                    // jumps to the nearest enclosing loop's end. With no enclosing loop it is
                    // a compile error (as it always was before `lock` existed).
                    let mut release: Vec<String> = Vec::new();
                    let mut dest: Option<String> = None;
                    for scope in self.scopes.iter().rev() {
                        match scope {
                            Scope::Lock { name, .. } => release.push(name.clone()),
                            Scope::Loop { end, .. } => {
                                dest = Some(end.clone());
                                break;
                            }
                        }
                    }
                    match dest {
                        Some(d) => self.emit_release_chain(&release, &d, false),
                        None => {
                            // No enclosing loop: reject, as `break` never had any meaning
                            // outside a loop before the `lock` feature existed.
                            panic!("`break` outside a loop")
                        }
                    }
                }
                Stmt::Continue => {
                    // A `continue` releases every `lock` inside the loop body it crosses (so
                    // none is left held as the next iteration begins), then jumps to the loop
                    // top.
                    let mut release: Vec<String> = Vec::new();
                    let mut dest: Option<String> = None;
                    for scope in self.scopes.iter().rev() {
                        match scope {
                            Scope::Lock { name, .. } => release.push(name.clone()),
                            Scope::Loop { start, .. } => {
                                dest = Some(start.clone());
                                break;
                            }
                        }
                    }
                    let start = dest.expect("continue outside loop");
                    self.emit_release_chain(&release, &start, false);
                }
            }
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
    }

    /// String-literal branch tracking helpers (see `if_else`) so we can merge
    /// per-branch values and either propagate a known text or flag an unknown one.
    pub(crate) fn save_string_state(&self) -> (HashMap<String, String>, HashSet<String>) {
        (self.string_rodata_cur.clone(), self.string_vars.clone())
    }
    /// Build a compile-time error message for a string variable whose value is
    /// unknown (present in `string_vars` but missing from `string_rodata_cur`).
    pub(crate) fn string_unknown_error(&self, var: &str) -> String {
        if let Some(assigns) = self.string_assignments.get(var) {
            // Variables with tracked assignments: name the conflicting ones.
            let mut msg = format!(
                "passing `{var}`: its value as a `string` is unknown \
                 after a branch or loop — the compiler cannot prove which text \
                 the variable holds. The assignments that make it impossible to \
                 decide:\n"
            );
            // The assignments are named by what they say, not by a line number: after an
            // `include` the source the codegen walks is the spliced one, so the count here is
            // 150 lines past anything the programmer can point at in their own file. A number
            // that names a line the reader cannot find is worse than no number at all.
            for (_line, desc) in assigns {
                msg.push_str(&format!("  {desc}\n"));
            }
            msg.push_str(
                "Remove one of the conflicting assignments so there is a \
                          single, provable value.\n",
            );
            msg
        } else {
            format!(
                "passing `{var}`: its value as a `string` is unknown \
                 because {var} was not initialized from a literal or \
                 another traceable string variable. Assign a literal to \
                 {var} before passing it, or restructure the code \
                 so the compiler can see which text flows."
            )
        }
    }
    /// Merge two post-branch states into `string_rodata_cur` / `string_vars`.
    /// For every variable that is a tracked string after at least one branch,
    /// keep the rodata label only when both branches agree; otherwise mark it
    /// as unknown.  Variables present only in one branch are also marked
    /// unknown (one branch has a value, the other does not).
    ///
    /// `has_else: false` means no else block was present, so `post_else` /
    /// `post_else_vars` are actually a copy of the pre-if state.  In that
    /// case we still merge — variables that changed in *then* will have the
    /// label in `post_then` but not in `post_else`, producing the same
    /// "unknown" result.  The implementation is identical so we only need
    /// to compute the union from the post-branch states regardless.
    pub(crate) fn merge_string_branches(
        &mut self,
        _pre_if: &HashMap<String, String>,
        _pre_vars: &HashSet<String>,
        post_then: &HashMap<String, String>,
        post_then_vars: &HashSet<String>,
        post_else: &HashMap<String, String>,
        post_else_vars: &HashSet<String>,
    ) {
        // For each variable in string_vars after the if, decide its post-if label.
        let final_vars: HashSet<String> = {
            let mut v = post_then_vars.clone();
            v.extend(post_else_vars.iter().cloned());
            v
        };
        for var in &final_vars {
            let then_label = post_then.get(var);
            let else_label = post_else.get(var);

            if then_label == else_label {
                // Both branches agree -- keep the label.
                if let Some(l) = then_label {
                    self.string_rodata_cur.insert(var.clone(), l.clone());
                }
            } else {
                // Disagree or one side has no value → unknown.
                self.string_rodata_cur.remove(var);
            }
        }
    }
}

mod builtins;
mod expr;
pub mod pic16;
mod runtime;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

/// Every variable a block assigns (`name = ..`), at any depth: nested blocks, loops, branches.
fn assigned_vars(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Assign {
                target: Expr::Var(n),
                ..
            } => {
                out.insert(n.clone());
            }
            Stmt::For { body, .. } | Stmt::Lock { body, .. } => assigned_vars(body, out),
            _ => {}
        }
        let mut inner = Vec::new();
        crate::check::nested_blocks_of_stmt(s, &mut inner);
        for blk in inner {
            assigned_vars(blk, out);
        }
    }
    if let Some(t) = &b.tail {
        assigned_vars_in_expr(t, out);
    }
}

/// `assigned_vars` for the blocks an expression contains (an `if`, a `match`).
fn assigned_vars_in_expr(e: &Expr, out: &mut HashSet<String>) {
    let mut inner = Vec::new();
    crate::check::nested_blocks(e, &mut inner);
    for blk in inner {
        assigned_vars(blk, out);
    }
}

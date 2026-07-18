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

use crate::ast::{BinOp, Block, Expr, Function, Global, Stmt, StructDef, Type, UnOp, Visibility};
use std::collections::{HashMap, HashSet};

/// System V AMD64 integer argument registers, in order.
const ARG_REGS: [&str; 6] = ["%rdi", "%rsi", "%rdx", "%rcx", "%r8", "%r9"];

/// System V AMD64 SSE argument registers (`f64` params/returns live here), in order.
const SSE_ARG_REGS: [&str; 8] = [
    "%xmm0", "%xmm1", "%xmm2", "%xmm3", "%xmm4", "%xmm5", "%xmm6", "%xmm7",
];

/// The value kind the codegen distinguishes. Integers/pointers are INTEGER class (live
/// in `%rax`/`%rdi..`) and carry SIGNEDNESS, `I64` (signed) vs `U64` (unsigned), which
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
    cg.fn_names = funcs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != ep)
        .map(|(_, f)| f.name.clone())
        .collect();
    cg.fn_ret_types = funcs
        .iter()
        .map(|f| (f.name.clone(), ret_kind(f)))
        .collect();
    cg.set_struct_sizes(structs);
    for (i, f) in funcs.iter().enumerate() {
        if i == ep {
            cg.emit_function(f, "sond_start");
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
    let mut cg = Codegen {
        check_contracts: true,
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
    cg.line("    call sond_start");
    cg.line("    ret");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.out
}

/// Like [`emit_program_g`], but with COVERAGE instrumentation: every statement records that
/// it executed into a shared buffer reached through the global `__cov_ptr` (which the test
/// harness points at an `mmap(MAP_SHARED)` region before forking each test). The total number
/// of points is published as `__cov_total`. Used only by the `sond test` coverage build.
pub fn emit_program_coverage_g(
    globals: &[Global],
    structs: &[StructDef],
    funcs: &[Function],
) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        coverage: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    // The harness reads `__cov_total` and writes `__cov_ptr`; register them so those `Var`s
    // resolve to the `.data` symbols emitted below.
    cg.global_names.insert("__cov_ptr".to_string());
    cg.global_names.insert("__cov_total".to_string());
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl main");
    cg.line("main:");
    cg.line("    movq %rdi, sond_argc(%rip)");
    cg.line("    movq %rsi, sond_argv(%rip)");
    cg.line("    movq %rdx, sond_envp(%rip)");
    cg.line("    call sond_start");
    cg.line("    ret");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_globals();
    cg.emit_rodata();
    let total = cg.cov_points;
    cg.line(".data");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
    cg.line("__cov_ptr: .quad 0");
    cg.line(&format!("__cov_total: .quad {total}"));
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
    cg.emit_globals();
    cg.emit_rodata();
    cg.line(".data");
    cg.line("sond_argc: .quad 0");
    cg.line("sond_argv: .quad 0");
    cg.line("sond_envp: .quad 0");
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
pub fn emit_freestanding_g(globals: &[Global], structs: &[StructDef], funcs: &[Function]) -> String {
    let mut cg = Codegen {
        check_contracts: true,
        ..Default::default()
    };
    cg.set_globals(globals);
    emit_all(&mut cg, structs, funcs, "main");
    cg.line(".globl _start");
    cg.line("_start:");
    cg.line("    call sond_start");
    cg.line("    movq %rax, %rdi"); // exit code = the entry's value
    cg.line("    movq $60, %rax"); // sys_exit
    cg.line("    syscall");
    cg.emit_print_int_helper();
    cg.emit_print_int_signed_helper();
    cg.emit_globals();
    cg.emit_rodata();
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
    cg.line("    movq %rcx, sond_imghandle(%rip)"); // stash ImageHandle for EBS
    cg.line("    movq %rdx, sond_systab(%rip)"); // stash SystemTable for print
    cg.line("    subq $8, %rsp"); // 16-align the stack before the call
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
    cg.emit_globals();
    cg.emit_rodata();
    cg.out
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
            Stmt::Expr(e) => collect_locals_in_expr(e, out),
            Stmt::Return(Some(e)) => collect_locals_in_expr(e, out),
            Stmt::Return(None) => {}
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    collect_locals_in_expr(c, out);
                }
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
            Stmt::Expr(e) => collect_arrays_in_expr(e, out),
            Stmt::Return(Some(e)) => collect_arrays_in_expr(e, out),
            Stmt::Return(None) => {}
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    collect_arrays_in_expr(c, out);
                }
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
        // can, including a call argument or an operand, so the walk must reach every
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
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            collect_arrays_in_expr(x, out)
        }
        Expr::List(items) => items.iter().for_each(|it| collect_arrays_in_expr(it, out)),
        _ => {}
    }
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
        // in any expression position, a call argument, an operand, so its locals must
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
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            collect_locals_in_expr(x, out)
        }
        Expr::List(items) => items.iter().for_each(|it| collect_locals_in_expr(it, out)),
        _ => {}
    }
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
    /// Stack of `(continue_label, break_label)` for the enclosing loops.
    loop_labels: Vec<(String, String)>,
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
    /// Names of the module-level globals (to resolve a `Var` to a `.data` label).
    global_names: HashSet<String>,
    /// The scalar globals to emit in `.data`, as `(label, constant value)`.
    global_data: Vec<(String, i64)>,
    /// Names of globals that are ARRAYS (a `Var` resolves to their ADDRESS, not a
    /// loaded value, so `K[i]` indexes into the table).
    global_arrays: HashSet<String>,
    /// The array globals to emit in `.data`, as `(label, constant words)`.
    global_array_data: Vec<(String, Vec<i64>)>,
    /// The value kind (`Int`/`Float`) of each in-scope local/parameter, so codegen
    /// knows to use the integer (`%rax`) or SSE (`%xmm0`) path for a `Var`. Reset per
    /// function alongside `locals`.
    var_types: HashMap<String, Ty>,
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
}

/// Functions excluded from coverage instrumentation: the synthesized `sond test` harness
/// (`main`), the test bodies themselves (always run, and their coverage is uninteresting),
/// and the testing library. Coverage measures the code under test, not the test scaffold.
fn should_skip_coverage(func: &Function) -> bool {
    func.is_test
        || func.name == "main"
        || matches!(
            func.name.as_str(),
            "assert" | "assert_eq" | "assert_ne" | "fail" | "__test_report"
        )
}

impl Codegen {
    /// Emits the function itself: label, prologue, body, epilogue. Shared by the
    /// hosted and freestanding entry points.
    fn emit_function(&mut self, func: &Function, label: &str) {
        // Fresh per-function state (locals map + a unique return label).
        self.locals.clear();
        self.var_types.clear();
        // Instrument only the code under test: skip the harness, the test bodies, and the
        // testing library (measuring those is noise, and the harness must run its `__cov_ptr`
        // setup before any mark fires).
        self.cov_active = self.coverage && !should_skip_coverage(func);
        // Does this function return a small struct/enum by value? If so, the body leaves
        // its base ADDRESS in %rax; the return point expands that to the value in RAX:RDX.
        self.cur_ret_struct = self.ret_struct_words(&func.ret);
        let mut slot = 0i32;
        // Parameters take the first stack slots (filled from the arg registers
        // in the prologue); then the block's locals.
        for p in &func.params {
            slot += 1;
            self.locals.insert(p.name.clone(), -8 * slot);
            self.var_types.insert(p.name.clone(), kind_of_type(&p.ty));
        }
        let mut names = Vec::new();
        collect_locals(&func.body, &mut names);
        for name in &names {
            if !self.locals.contains_key(name) {
                slot += 1;
                self.locals.insert(name.clone(), -8 * slot);
            }
        }
        // Fixed-size stack arrays `let a[N]`: reserve N words below the scalars and
        // point the name's slot at the base (element 0 at the lowest address).
        let mut arrays = Vec::new();
        collect_arrays(&func.body, &mut arrays);
        let mut array_bases = Vec::new();
        for (name, len) in &arrays {
            slot += *len as i32;
            let base_off = -8 * slot; // element 0
            array_bases.push((self.locals[name], base_off));
        }
        // A slot to hold the return value while the postconditions are checked;
        // the named return (e.g. `vat_amount`) binds to it so `ensures` can read it.
        let ret_slot = if func.ensures.is_empty() {
            None
        } else {
            slot += 1;
            let off = -8 * slot;
            if let Some(rn) = &func.ret_name {
                self.locals.insert(rn.clone(), off);
            }
            Some(off)
        };
        let frame = (slot * 8 + 15) & !15; // 16-byte aligned
        self.ret_label = self.fresh_label("ret");

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

        // Preconditions: checked on entry, with the parameters in scope.
        self.emit_contract_guard(&func.requires);

        self.block(&func.body); // leaves the function's value in %rax

        self.line(&format!("{}:", self.ret_label.clone()));
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
        if func.is_interrupt {
            for r in ISR_SAVED_REGS.iter().rev() {
                self.line(&format!("    popq {r}"));
            }
            self.line("    iretq");
        } else {
            self.line("    ret");
        }
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

    /// The by-value word count of a return type IF it is a small (1–2 word) struct/enum
    ///, the sizes returnable in registers (RAX:RDX). `None` otherwise (scalar, float,
    /// unit, or a struct too large for registers, which is not yet supported).
    fn ret_struct_words(&self, ret: &Option<Type>) -> Option<i64> {
        match ret {
            Some(Type::Named(n)) => self.struct_sizes.get(n).copied().filter(|&w| (1..=2).contains(&w)),
            _ => None,
        }
    }

    /// Registers the module-level globals: const-evaluates each initializer and
    /// records its `.data` label. A non-constant initializer is an error.
    fn set_globals(&mut self, globals: &[Global]) {
        for g in globals {
            if self.global_names.contains(&g.name) {
                panic!("global `{}` is declared more than once", g.name);
            }
            self.global_names.insert(g.name.clone());
            // An array global `let NAME[N] = [c0, c1, ...]` is a constant table in
            // `.data`; its `Var` resolves to the base ADDRESS so `NAME[i]` indexes.
            if let Some(n) = g.array_len {
                let mut words = match &g.init {
                    Expr::List(items) => items
                        .iter()
                        .map(|e| {
                            self.const_eval(e).unwrap_or_else(|| {
                                panic!("global array `{}` needs constant elements", g.name)
                            })
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                words.resize(n as usize, 0); // pad/truncate to the declared size
                self.global_arrays.insert(g.name.clone());
                self.global_array_data.push((g.name.clone(), words));
                continue;
            }
            let value = self.const_eval(&g.init).unwrap_or_else(|| {
                panic!(
                    "global `{}` initializer must be a constant, initialize it to 0 \
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

    /// Emits the module-level globals into `.data`.
    fn emit_globals(&mut self) {
        if self.global_data.is_empty() && self.global_array_data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.global_data);
        let arrays = std::mem::take(&mut self.global_array_data);
        self.line(".data");
        for (name, value) in data {
            self.line(&format!("{name}: .quad {value}"));
        }
        for (name, words) in arrays {
            let quads = words
                .iter()
                .map(|w| w.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.line(&format!("{name}: .quad {quads}"));
        }
    }

    /// Emits a block: its statements, then its tail expression (leaving the
    /// tail's value, or the last computed value, in `%rax`).
    /// Emits a coverage mark for the next statement, records that this point executed, into
    /// the shared buffer via `__cov_ptr`. A no-op unless `coverage` is on (so ordinary builds
    /// are byte-identical). Uses `%r11`, a scratch register free at a statement boundary.
    fn cov_mark(&mut self) {
        if !self.cov_active {
            return;
        }
        let id = self.cov_points;
        self.cov_points += 1;
        self.line("    movq __cov_ptr(%rip), %r11");
        self.line(&format!("    movq $1, {}(%r11)", id * 8));
    }

    fn block(&mut self, block: &Block) {
        // One coverage mark per BASIC BLOCK (this block's entry). A block runs all-or-nothing;
        // each `if`/`match` branch is emitted as its own block (recursive `block()` call) and
        // gets its own mark, so this is branch-level coverage, and it captures expression-
        // bodied blocks (a tail with no statements) that per-statement marking would miss.
        self.cov_mark();
        for stmt in &block.stmts {
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
                        self.expr(value);
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
                    }
                }
                Stmt::Assign {
                    target: Expr::Var(name),
                    value,
                } => {
                    let kind = self.ty_of(value);
                    self.expr(value);
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
                // Word-array write `base[index] = value`.
                Stmt::Assign {
                    target: Expr::Index(base, index),
                    value,
                } => {
                    self.expr(base);
                    self.line("    pushq %rax");
                    self.expr(index);
                    self.line("    shlq $3, %rax"); // index * 8
                    self.line("    pushq %rax");
                    self.expr(value); // value -> %rax
                    self.line("    popq %rcx"); // index * 8
                    self.line("    popq %rdx"); // base
                    self.line("    addq %rdx, %rcx"); // address
                    self.line("    movq %rax, (%rcx)");
                }
                Stmt::Assign { .. } => todo!("x64 codegen: assignment to non-variable target"),
                Stmt::Expr(e) => self.expr(e),
                Stmt::Return(e) => {
                    match e {
                        Some(e) => self.expr(e),
                        None => self.line("    movq $0, %rax"),
                    }
                    self.line(&format!("    jmp {}", self.ret_label.clone()));
                }
                Stmt::For { cond, body, .. } => {
                    let start = self.fresh_label("for");
                    let end = self.fresh_label("endfor");
                    self.loop_labels.push((start.clone(), end.clone()));
                    self.line(&format!("{start}:"));
                    if let Some(c) = cond {
                        self.expr(c);
                        self.line("    cmpq $0, %rax");
                        self.line(&format!("    je {end}"));
                    }
                    self.block(body);
                    self.line(&format!("    jmp {start}"));
                    self.line(&format!("{end}:"));
                    self.loop_labels.pop();
                }
                Stmt::Break => {
                    let end = self
                        .loop_labels
                        .last()
                        .expect("break outside loop")
                        .1
                        .clone();
                    self.line(&format!("    jmp {end}"));
                }
                Stmt::Continue => {
                    let start = self
                        .loop_labels
                        .last()
                        .expect("continue outside loop")
                        .0
                        .clone();
                    self.line(&format!("    jmp {start}"));
                }
            }
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
    }
}

mod builtins;
mod expr;
mod runtime;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

use super::testutil::{
    compile_and_capture, compile_and_run, compile_and_run_freestanding, parse_function,
};
use super::*;
use crate::lexer::tokenize;
use crate::parser::Parser;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn emits_a_constant_return() {
    let asm = emit_program(&[parse_function("fn answer() -> i64 { 42 }")]);
    assert!(asm.contains("movq $42, %rax"));
    // The sole function is the entry, under `sond_start`, called by the C `main`.
    assert!(asm.contains(".globl sond_start"));
    assert!(asm.contains(".globl main"));
    assert!(asm.contains("ret"));
}

#[test]
fn coverage_build_instruments_each_statement() {
    let funcs = Parser::new(tokenize("fn f() -> i64 { let x = 1\nx + 1 }")).parse_program();
    let asm = emit_program_coverage_g(&[], &[], &funcs, &std::collections::HashSet::new());
    // Each statement records that it executed into the shared buffer via __cov_ptr.
    assert!(
        asm.contains("movq __cov_ptr(%rip), %r11"),
        "coverage build marks statements"
    );
    // The total point count is published for the harness to size its buffer.
    assert!(asm.contains("__cov_total:"), "publishes the total point count");
}

#[test]
fn ordinary_build_is_uninstrumented() {
    let funcs = Parser::new(tokenize("fn f() -> i64 { let x = 1\nx + 1 }")).parse_program();
    let asm = emit_program_g(&[], &[], &funcs);
    assert!(
        !asm.contains("__cov_ptr"),
        "an ordinary build carries no coverage marks (byte-identical to before)"
    );
}

#[test]
fn compiles_and_runs_arithmetic_with_precedence_on_real_x64() {
    assert_eq!(compile_and_run("fn f() -> i64 { 2 + 3 * 4 }"), 14);
    assert_eq!(compile_and_run("fn g() -> i64 { (10 - 2) * 5 }"), 40);
}

#[test]
fn compiles_and_runs_a_write_syscall_on_real_x64() {
    // write(fd, "literal") issues a raw sys_write, Sond's first I/O.
    assert_eq!(
        compile_and_capture("fn init() -> i64 { write(1, \"Sond boots\") }"),
        "Sond boots"
    );
}

#[test]
fn compiles_and_runs_print_int_on_real_x64() {
    // print_int converts an integer to decimal ASCII and writes it.
    assert_eq!(
        compile_and_capture("fn main() -> i64 { print_int(42)\n0 }"),
        "42"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { print_int(6 * 7)\n0 }"),
        "42"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { print_int(0)\n0 }"),
        "0"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { print_int(1000000)\n0 }"),
        "1000000"
    );
}

#[test]
fn compiles_and_runs_portable_print_on_real_x64() {
    // print("...") is the portable console primitive (write(1,..) on ELF, ConOut on UEFI).
    assert_eq!(
        compile_and_capture("fn f() -> i64 { print(\"hola metal\")\n0 }"),
        "hola metal"
    );
}

#[test]
fn compiles_and_runs_control_flow_on_real_x64() {
    assert_eq!(
        compile_and_run("fn f() -> i64 { if 3 > 2 { 10 } else { 20 } }"),
        10
    );
    assert_eq!(
        compile_and_run("fn h() -> i64 { if 5 == 5 { if 2 < 1 { 1 } else { 7 } } else { 0 } }"),
        7
    );
}

#[test]
fn compiles_and_runs_local_variables_on_real_x64() {
    // Locals in the stack frame: definition, use in a branch, and reassignment.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let x = 10\nlet y = 20\nx + y }"),
        30
    );
    assert_eq!(
        compile_and_run("fn g() -> i64 { let x = 7\nif x > 5 { x * 2 } else { 0 } }"),
        14
    );
    assert_eq!(
        compile_and_run("fn h() -> i64 { let mut x = 1\nx = x + 41\nx }"),
        42
    );
}

#[test]
fn a_debug_build_emits_a_per_statement_checkpoint_a_normal_build_does_not() {
    // The codegen emits the per-statement CHECKPOINT (`call __dbg_step`); the `__dbg_step` runtime
    // itself is a Sond function appended to the program in a --debug build (RFC-0016), not codegen.
    let funcs = Parser::new(tokenize("fn main() -> i64 { let a = 1\nlet b = 2\na + b }")).parse_program();
    let debug_asm = emit_program_debug_g(&[], &[], &funcs);
    let normal_asm = emit_program_g(&[], &[], &funcs);
    assert!(debug_asm.contains("call __dbg_step"), "the --debug build calls the checkpoint");
    assert!(
        !normal_asm.contains("__dbg_step"),
        "a normal build carries no debug instrumentation (byte-identical to before)"
    );
}

#[test]
fn a_debug_build_tracks_call_depth_for_step_over_and_step_out() {
    // RFC-0016: to offer step-over / step-out the agent needs the call depth, so a --debug build
    // bumps it on entry (`call __dbg_enter`) and drops it at the single exit (`call __dbg_exit`).
    // A normal build carries neither.
    let funcs = Parser::new(tokenize("fn main() -> i64 { let a = 1\na }")).parse_program();
    let debug_asm = emit_program_debug_g(&[], &[], &funcs);
    let normal_asm = emit_program_g(&[], &[], &funcs);
    assert!(debug_asm.contains("call __dbg_enter"), "the --debug build bumps the depth on entry");
    assert!(debug_asm.contains("call __dbg_exit"), "the --debug build drops the depth on exit");
    assert!(
        !normal_asm.contains("__dbg_enter") && !normal_asm.contains("__dbg_exit"),
        "a normal build tracks no call depth"
    );
}

#[test]
fn a_string_literal_global_is_a_pointer_to_rodata() {
    // `let MSG = "hi"` at module level: the global holds the ADDRESS of the interned bytes (a valid
    // compile-time constant), not an integer, so it works like any `.data` pointer.
    let mut p = Parser::new(tokenize("pub let MSG = \"hi\"\npub fn f() -> i64 { 0 }"));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let structs = p.take_structs();
    let asm = emit_shared_g(&globals, &structs, &funcs);
    assert!(
        asm.contains("MSG: .quad .Lstr"),
        "the global stores the string's rodata address: {asm}"
    );
    assert!(asm.contains("hi"), "the string bytes are emitted in rodata");
}

#[test]
fn a_shared_pub_function_seeds_the_arena_at_the_c_abi_boundary() {
    // A `.so` (`--shared`) is called from a foreign host (Go/cgo, C) whose `%r15` is not Sond's
    // arena pointer. Each EXPORTED (`pub`) function must save the host's `%r15` (callee-saved) and
    // seed the arena TCB, or an allocation inside faults. A hidden (`priv`) function is internal
    // only and inherits `%r15`, so it must NOT re-seed.
    let mut p = Parser::new(tokenize(
        "pub fn ex() -> i64 { 1 }\npriv fn hidden() -> i64 { 2 }",
    ));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let structs = p.take_structs();
    let asm = emit_shared_g(&globals, &structs, &funcs);
    assert!(asm.contains("pushq %r15"), "the pub boundary saves the host's %r15");
    assert!(asm.contains("popq %r15"), "and restores it before returning");
    // The seed appears exactly once: for the single `pub` function, not the hidden one.
    assert_eq!(
        asm.matches("leaq sond_arena_tcb(%rip), %r15").count(),
        1,
        "only the exported function seeds the arena, not the hidden one"
    );
}

#[test]
fn par_runs_every_peer_on_a_thread_and_joins_before_continuing() {
    // `par w(a,0) ~~ w(a,1) ~~ w(a,2)` spawns each peer on its own OS thread and joins ALL
    // of them before the next statement. Each peer writes 1 into a distinct slot of a shared
    // stack array (the threads share the address space, CLONE_VM); the sum read AFTER the
    // `par` is 3 only if every peer ran AND was joined first. A missing join would let `main`
    // read a slot before its writer finished (a flake or a low result); the deterministic 3
    // is the join working.
    let src = "\
        fn w(p: i64, idx: i64) { poke64(p + idx * 8, 1) }\n\
        fn main() -> i64 {\n\
            let a[3]\n\
            par w(a, 0) ~~ w(a, 1) ~~ w(a, 2)\n\
            peek64(a) + peek64(a + 8) + peek64(a + 16)\n\
        }";
    assert_eq!(compile_and_run(src), 3, "all three peers ran and were joined");
}

#[test]
fn par_let_binds_each_peer_result_by_source_position() {
    // `let a ~~ b ~~ c = par one() ~~ two() ~~ three()` runs the peers on threads, joins, and
    // binds each RETURN VALUE to its name by SOURCE position: `a` is one()'s 1, `b` is two()'s 2,
    // `c` is three()'s 3, whichever thread finished first. The positional formula a*16 + b*4 + c =
    // 16 + 8 + 3 = 27 is 27 only if each name got its own peer's result (a swap would change it).
    let src = "\
        fn one() -> i64 { 1 }\n\
        fn two() -> i64 { 2 }\n\
        fn three() -> i64 { 3 }\n\
        fn main() -> i64 {\n\
            let a ~~ b ~~ c = par one() ~~ two() ~~ three()\n\
            a * 16 + b * 4 + c\n\
        }";
    assert_eq!(compile_and_run(src), 27, "results bound by position: a=1, b=2, c=3");
}

#[test]
fn par_reduce_folds_peer_results_with_a_commutative_op() {
    // `let s = par(+) ...` runs the peers on threads and folds their results with `+`. 10+20+12=42.
    let sum = "\
        fn a() -> i64 { 10 }\n\
        fn b() -> i64 { 20 }\n\
        fn c() -> i64 { 12 }\n\
        fn main() -> i64 { let s = par(+) a() ~~ b() ~~ c()\n s }";
    assert_eq!(compile_and_run(sum), 42, "par(+) sums the peer results");
    // `par(*)` multiplies: 2*3*7 = 42.
    let prod = "\
        fn a() -> i64 { 2 }\n\
        fn b() -> i64 { 3 }\n\
        fn c() -> i64 { 7 }\n\
        fn main() -> i64 { let s = par(*) a() ~~ b() ~~ c()\n s }";
    assert_eq!(compile_and_run(prod), 42, "par(*) multiplies the peer results");
}

#[test]
fn graph_evaluates_nodes_in_dependency_order_not_source_order() {
    // The `graph` nodes are written in a NON-topological source order (`z` before its inputs), yet
    // the block evaluates them in dependency order: `fx = 20`, `fy = 21`, then `z = fx + fy = 41`.
    // Reordering the `node` lines changes nothing (RFC-0008 §5.1), which the topological schedule is.
    let src = "\
        fn dbl(x: i64) -> i64 { x + x }\n\
        fn inc(x: i64) -> i64 { x + 1 }\n\
        fn add(a: i64, b: i64) -> i64 { a + b }\n\
        fn main() -> i64 {\n\
            let r = graph {\n\
                node z = add(fx, fy)\n\
                node fx = dbl(10)\n\
                node fy = inc(20)\n\
                yield z\n\
            }\n\
            r\n\
        }";
    assert_eq!(compile_and_run(src), 41, "nodes evaluate in dependency order regardless of source order");
}

#[test]
fn a_graph_serializes_conflicting_nodes_but_parallelizes_disjoint_ones() {
    // `0x250f00` is the clone flags of a thread spawn, so its presence means a level ran in
    // parallel. Two nodes that write the SAME global conflict (RFC-0008 §5.2): an effect edge
    // serializes them into separate levels, so NO thread is spawned. Two nodes on DISJOINT globals
    // are independent and DO spawn.
    let conflict = "let mut G = 0\n\
        fn a() -> i64 { G = 1\n1 }\nfn b() -> i64 { G = 2\n2 }\nfn m(x: i64, y: i64) -> i64 { 0 }\n\
        fn main() -> i64 { let r = graph { node x = a()\n node y = b()\n node z = m(x, y)\n yield z }\n r }";
    let mut p = Parser::new(tokenize(conflict));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let asm = emit_program_g(&globals, &[], &funcs);
    assert!(
        !asm.contains("0x250f00"),
        "conflicting nodes are serialized by an effect edge, not spawned in parallel"
    );

    let disjoint = "let mut G = 0\nlet mut H = 0\n\
        fn a() -> i64 { G = 1\n1 }\nfn b() -> i64 { H = 2\n2 }\nfn m(x: i64, y: i64) -> i64 { 0 }\n\
        fn main() -> i64 { let r = graph { node x = a()\n node y = b()\n node z = m(x, y)\n yield z }\n r }";
    let mut p2 = Parser::new(tokenize(disjoint));
    let funcs2 = p2.parse_program();
    let globals2 = p2.take_globals();
    let asm2 = emit_program_g(&globals2, &[], &funcs2);
    assert!(
        asm2.contains("0x250f00"),
        "nodes on disjoint globals stay parallel (an effect edge must not over-serialize)"
    );
}

#[test]
fn graph_runs_independent_nodes_in_parallel_and_binds_them_correctly() {
    // Level 0 = { a, b, c }, three INDEPENDENT call nodes: they run concurrently on threads. Level
    // 1 = { total } combines them. The positional formula a*16 + b*4 + c = 16 + 8 + 3 = 27 is 27
    // only if each node was bound to its own result after the parallel level joined.
    let src = "\
        fn one() -> i64 { 1 }\n\
        fn two() -> i64 { 2 }\n\
        fn three() -> i64 { 3 }\n\
        fn mix(a: i64, b: i64, c: i64) -> i64 { a * 16 + b * 4 + c }\n\
        fn main() -> i64 {\n\
            let r = graph {\n\
                node total = mix(a, b, c)\n\
                node a = one()\n\
                node b = two()\n\
                node c = three()\n\
                yield total\n\
            }\n\
            r\n\
        }";
    assert_eq!(compile_and_run(src), 27, "parallel level nodes bound to their own results");
}

#[test]
fn compiles_and_runs_early_return_on_real_x64() {
    assert_eq!(
        compile_and_run("fn f() -> i64 { let x = 5\nreturn x + x\n }"),
        10
    );
}

#[test]
fn compiles_and_runs_a_conditional_loop_on_real_x64() {
    // while-style `for cond`: sum 0+1+2+3+4 = 10.
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 { let mut i = 0\nlet mut s = 0\nfor i < 5 { s = s + i\ni = i + 1 }\ns }"
        ),
        10
    );
}

#[test]
fn compiles_and_runs_calls_between_functions_on_real_x64() {
    // main calls double(21) -> 42: exercises parameters (arg registers) and
    // user-function calls between multiple functions in one program.
    assert_eq!(
        compile_and_run("fn double(x: i64) -> i64 { x + x }\nfn main() -> i64 { double(21) }"),
        42
    );
    // Two args and nested calls.
    assert_eq!(
        compile_and_run(
            "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { add(add(1, 2), 39) }"
        ),
        42
    );
}

#[test]
fn compiles_and_runs_break_in_an_infinite_loop_on_real_x64() {
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 { let mut i = 0\nfor { i = i + 1\nif i == 3 { break } }\ni }"
        ),
        3
    );
}

#[test]
fn compiles_and_runs_a_freestanding_static_binary() {
    // No libc: own _start + raw exit syscall, statically linked. This is the
    // shape a Sond program takes to boot as an initramfs /init (kernel profile).
    let (code, is_static) = compile_and_run_freestanding("fn init() -> i64 { 7 * 6 }");
    assert_eq!(code, 42);
    assert!(
        is_static,
        "the freestanding binary must be statically linked"
    );
}

#[test]
fn freestanding_argc_reads_the_process_arguments() {
    // A freestanding `_start` must stash argc/argv/envp from the stack (there is no libc to
    // do it), and the globals must be declared, else `argc()` fails to link. Run with no
    // args, argc is 1 (the program name).
    let (code, _) = compile_and_run_freestanding("fn main() -> i64 { argc() }");
    assert_eq!(code, 1, "argc() should be 1 when run with no extra arguments");
}

#[test]
fn uefi_print_int_lowers_to_the_utf16_helper() {
    // On UEFI, print_int(n) calls the UTF-16 helper (divide-by-10 -> ConOut, buffer in .data).
    let asm = emit_uefi(
        &Parser::new(tokenize("fn efi_main() -> i64 { print_int(7)\n0 }")).parse_program(),
    );
    assert!(asm.contains("call sond_print_int_uefi"), "must call helper");
    assert!(asm.contains("sond_print_int_uefi:"), "must emit the helper");
    assert!(
        asm.contains("sond_intbuf:"),
        "must reserve the scratch buffer"
    );
    assert!(asm.contains("movq 64(%rax), %rcx"), "must reach ConOut");
}

#[test]
fn uefi_alloc_reaches_boot_services_and_peek_poke_touch_memory() {
    // alloc(n) calls BootServices->AllocatePool (system-table offset 96,
    // AllocatePool at 64); peek/poke lower to plain memory moves.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { let p = alloc(64)\npoke64(p, 7)\npeek64(p) }",
        ))
        .parse_program(),
    );
    assert!(asm.contains("movq 96(%rax), %rax"), "reaches BootServices");
    assert!(
        asm.contains("leaq sond_allocptr(%rip), %r8"),
        "passes &Buffer"
    );
    assert!(asm.contains("movq %rax, (%rcx)"), "poke64 stores a word");
    assert!(asm.contains("movq (%rax), %rax"), "peek64 loads a word");
}

#[test]
fn while_is_an_alias_for_the_conditional_for_loop() {
    // `while cond` is a synonym of `for cond`. Sum 0+1+2+3+4 = 10.
    let s =
        "fn f() -> i64 { let mut i = 0\nlet mut s = 0\nwhile i < 5 { s = s + i\ni = i + 1 }\ns }";
    assert_eq!(compile_and_run(s), 10);
}

#[test]
fn global_array_constant_table() {
    // A module-level constant table `let K[N] = [...]` (SHA K[64]), indexed anywhere.
    assert_eq!(
        compile_and_run_with_globals(
            "let K[4] = [10, 20, 30, 40]\nfn main() -> i64 { K[0] + K[3] }"
        ),
        50
    );
    // Multi-line table, summed in a loop.
    let src = "let K[3] = [\n  5,\n  7,\n  9,\n]\n\
               fn sum() -> i64 { let mut s = 0\nlet mut i = 0\nfor i < 3 { s = s + K[i]\ni = i + 1 }\ns }";
    assert_eq!(compile_and_run_with_globals(src), 21);
}

#[test]
fn logical_right_shift_zero_fills() {
    // `>>>` is LOGICAL (zero-fill), `>>` is arithmetic. -1 is all-ones; >>> 60 = 15.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let x = -1\nx >>> 60 }"),
        15
    );
    // Arithmetic >> keeps the sign: -1 >> 60 is still -1 (& 255 = 255).
    assert_eq!(
        compile_and_run("fn f() -> i64 { let x = -1\n(x >> 60) & 255 }"),
        255
    );
}

#[test]
fn stack_array_with_a_literal_initializer() {
    // `let a[N] = [e0, e1, ...]` reserves N words AND initializes them, what
    // crypto tables (SHA round constants) want instead of N hand-written stores.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let a[3] = [10, 20, 30]\na[0] + a[1] + a[2] }"),
        60
    );
    // The array is still indexable/mutable afterwards.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let a[4] = [1, 2, 3, 4]\na[2] = 100\na[0] + a[2] }"),
        101
    );
}

#[test]
fn peek_poke_have_16_and_32_bit_widths_for_device_rings() {
    // virtio virtqueues are laid out with mixed field widths (descriptor:
    // addr=64, len=32, flags=16, next=16; avail/used rings use 16-bit indices),
    // so a driver needs sized memory access, not just 8/64.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { let p = alloc(64)\n\
             poke16(p, 1)\npoke32(p, 2)\nlet a = peek16(p)\npeek32(p) }",
        ))
        .parse_program(),
    );
    assert!(
        asm.contains("movw %ax, (%rcx)"),
        "poke16 stores a word (16-bit)"
    );
    assert!(asm.contains("movl %eax, (%rcx)"), "poke32 stores a dword");
    assert!(
        asm.contains("movzwq (%rax), %rax"),
        "peek16 zero-extends a word"
    );
    assert!(asm.contains("movl (%rax), %eax"), "peek32 loads a dword");
}

#[test]
fn uefi_serial_int_converts_and_writes_over_com1() {
    // serial_int lowers to a helper that converts to decimal and writes each
    // digit via sond_serial_putc, numbers on the wire after boot services.
    let asm = emit_uefi(
        &Parser::new(tokenize("fn efi_main() -> i64 { serial_int(42)\n0 }")).parse_program(),
    );
    assert!(asm.contains("sond_serial_int:"), "emits the helper");
    assert!(
        asm.contains("call sond_serial_putc"),
        "writes digits to COM1"
    );
    assert!(
        asm.contains("sond_serial_putc:"),
        "pulls in the UART helper too"
    );
}

#[test]
fn uefi_exit_boot_services_gets_the_map_then_exits() {
    // exit_boot_services disables the watchdog, gets the memory map (offset 56)
    // for its key, and calls ExitBootServices (offset 232) with the image
    // handle, retrying on a stale key. halt() stops the CPU with hlt.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { exit_boot_services()\nhalt()\n0 }",
        ))
        .parse_program(),
    );
    assert!(asm.contains("movq 56(%rax), %r11"), "gets the memory map");
    assert!(
        asm.contains("movq 232(%rax), %r11"),
        "reaches ExitBootServices"
    );
    assert!(
        asm.contains("movq sond_imghandle(%rip), %rcx"),
        "passes the handle"
    );
    assert!(asm.contains("hlt"), "halt stops the CPU");
}

/// Compiles a hosted program and returns the raw exit status (so a trap from a
/// violated contract, which kills the process with SIGILL, can be detected).
fn compile_and_status(src: &str) -> std::process::ExitStatus {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let asm = emit_program(&Parser::new(tokenize(src)).parse_program());
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_st_{}_{}", std::process::id(), n);
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
    let status = Command::new(&bin_path).status().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    status
}

#[test]
fn a_satisfied_requires_lets_the_function_run() {
    // Precondition holds (5 > 0): the function returns its value normally.
    let status = compile_and_status(
        "fn checked(x: i64) -> i64 requires { x > 0 } { x }\nfn main() -> i64 { checked(5) }",
    );
    assert_eq!(status.code(), Some(5), "should return 5, not trap");
}

#[test]
fn a_violated_requires_traps_the_program() {
    // Precondition fails (0 > 0 is false): the compiled binary traps (ud2 -> SIGILL)
    // instead of running with a bad input. The oracle, in native code.
    let status = compile_and_status(
        "fn checked(x: i64) -> i64 requires { x > 0 } { x }\nfn main() -> i64 { checked(0) }",
    );
    assert!(
        !status.success(),
        "a violated precondition must not succeed"
    );
    assert_eq!(
        status.code(),
        None,
        "must die from a signal (SIGILL from ud2)"
    );
}

#[test]
fn a_violated_ensures_traps_the_program() {
    // A buggy body that breaks its postcondition is caught at the exit. Here the
    // contract promises a positive result but the body returns 0 -> trap.
    let status = compile_and_status(
        "fn bad() -> (r: i64) ensures { r > 0 } { 0 }\nfn main() -> i64 { bad() }",
    );
    assert_eq!(status.code(), None, "a broken postcondition must trap");
}

/// Compiles a program that has module-level globals and runs it (exit code).
fn compile_and_run_with_globals(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let mut p = Parser::new(tokenize(src));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let asm = emit_program_g(&globals, &[], &funcs);
    let dir = std::env::temp_dir();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("sondc_g_{}_{}", std::process::id(), n);
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
    let code = Command::new(&bin_path).status().unwrap().code().unwrap();
    let _ = std::fs::remove_file(&asm_path);
    let _ = std::fs::remove_file(&bin_path);
    code
}

#[test]
fn compiles_and_runs_module_level_globals_on_real_x64() {
    // A constant global read from a function.
    assert_eq!(
        compile_and_run_with_globals("let BASE = 40\nfn main() -> i64 { BASE + 2 }"),
        42
    );
    // A const-folded global initializer (bitwise), and a mutable global updated at
    // runtime then read back.
    assert_eq!(
        compile_and_run_with_globals(
            "let MASK = 0xFF & 0x3C\nlet mut counter = 0\nfn bump() -> i64 { counter = counter + MASK\ncounter }\nfn main() -> i64 { bump()\nbump()\ncounter }"
        ),
        120
    );
}

#[test]
fn compiles_and_runs_fixed_stack_arrays_on_real_x64() {
    // `let a[N]` reserves N words in the frame; a[i] indexes it, no alloc needed,
    // so crypto tables (K[64], W[64]) work on the hosted target too.
    let prog = "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 4 { a[i] = i * i\ni = i + 1 }\na[0] + a[1] + a[2] + a[3] }";
    assert_eq!(compile_and_run(prog), 14); // 0+1+4+9
}

#[test]
fn a_let_without_initializer_defaults_to_zero_on_real_x64() {
    // `let mut x` (declare now, assign later) defaults to 0.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let mut x: i64\nx = 40\nx + 2 }"),
        42
    );
    assert_eq!(compile_and_run("fn f() -> i64 { let x: i64\nx }"), 0);
}

#[test]
fn compiles_and_runs_more_than_six_arguments_on_real_x64() {
    // Args 7+ pass on the stack (System V). sum8(1..8) == 36; and an odd count of
    // stack args keeps %rsp aligned (sum7(1..7) == 28).
    let sum8 = "fn sum8(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64) -> i64 { a + b + c + d + e + f + g + h }\nfn main() -> i64 { sum8(1, 2, 3, 4, 5, 6, 7, 8) }";
    assert_eq!(compile_and_run(sum8), 36);
    let sum7 = "fn sum7(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64) -> i64 { a + b + c + d + e + f + g }\nfn main() -> i64 { sum7(1, 2, 3, 4, 5, 6, 7) }";
    assert_eq!(compile_and_run(sum7), 28);
}

#[test]
fn compiles_and_runs_else_if_chains_on_real_x64() {
    // `else if` chains, grade a score into buckets.
    let prog = "fn grade(n: i64) -> i64 { if n >= 90 { 4 } else if n >= 80 { 3 } else if n >= 70 { 2 } else { 0 } }\nfn main() -> i64 { grade(85) }";
    assert_eq!(compile_and_run(prog), 3);
    assert_eq!(compile_and_run(&prog.replace("grade(85)", "grade(95)")), 4);
    assert_eq!(compile_and_run(&prog.replace("grade(85)", "grade(50)")), 0);
}

#[test]
fn word_array_indexing_lowers_to_scaled_address() {
    // a[i] reads/writes the 8-byte word at a + i*8, sugar over peek64/poke64 for
    // tables (SHA-256 K[t]/W[t], connection tables).
    let asm = emit_program(
        &Parser::new(tokenize("fn f(a: i64) -> i64 { a[2] = 7\na[2] }")).parse_program(),
    );
    assert!(asm.contains("shlq $3, %rax"), "scales the index by 8");
    assert!(asm.contains("movq %rax, (%rcx)"), "stores a[i] = v");
    assert!(asm.contains("movq (%rax), %rax"), "loads a[i]");
}

#[test]
fn compiles_and_runs_logical_and_or_on_real_x64() {
    // `&&` / `||` with short-circuit, result 0/1.
    assert_eq!(
        compile_and_run("fn f() -> i64 { if 1 < 2 && 3 < 4 { 1 } else { 0 } }"),
        1
    );
    assert_eq!(
        compile_and_run("fn f() -> i64 { if 1 < 2 && 4 < 3 { 1 } else { 0 } }"),
        0
    );
    assert_eq!(
        compile_and_run("fn f() -> i64 { if 4 < 3 || 2 < 5 { 1 } else { 0 } }"),
        1
    );
    // Driver idiom: test two flag bits at once.
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 { let s = 0x18\nif s & 0x10 == 0x10 && s & 0x08 == 0x08 { 1 } else { 0 } }"
        ),
        1
    );
}

#[test]
fn compiles_and_runs_division_and_modulo_on_real_x64() {
    // Integer division and remainder, checksums, frequencies, decimal digits.
    // (Big results go through print_int; exit codes are only a byte.)
    assert_eq!(
        compile_and_capture("fn main() -> i64 { print_int(1193182 / 100)\n0 }"),
        "11931"
    );
    assert_eq!(compile_and_run("fn f() -> i64 { 100 / 7 }"), 14);
    assert_eq!(compile_and_run("fn f() -> i64 { 100 % 7 }"), 2);
    assert_eq!(compile_and_run("fn f() -> i64 { 255 % 16 }"), 15);
    // Precedence: same as *, so 2 + 12 / 4 == 5.
    assert_eq!(compile_and_run("fn f() -> i64 { 2 + 12 / 4 }"), 5);
}

#[test]
fn compiles_and_runs_unary_operators_on_real_x64() {
    // Negation and bitwise complement, needed for masks and signed math.
    assert_eq!(
        compile_and_run("fn f() -> i64 { let x = 5\n0 - x + 100 }"),
        95
    );
    assert_eq!(compile_and_run("fn f() -> i64 { -7 + 10 }"), 3);
    // Exit codes are unsigned bytes, so keep results non-negative: ~0 is all-ones,
    // masked to 255; double complement round-trips; and (n + 7) & ~7 aligns up.
    // NOTE: `~~` is the `par` peer delimiter (one token), so a double complement is written
    // `~(~x)` or `~ ~x` (a space), never `~~x`.
    assert_eq!(compile_and_run("fn f() -> i64 { ~0 & 255 }"), 255);
    assert_eq!(compile_and_run("fn f() -> i64 { ~(~7) }"), 7);
    assert_eq!(compile_and_run("fn f() -> i64 { (3 + 7) & ~7 }"), 8);
}

#[test]
fn compiles_and_runs_bitwise_operators_on_real_x64() {
    // Masks, flags and shifts, the arithmetic of drivers and packets.
    assert_eq!(compile_and_run("fn f() -> i64 { 0xF0 & 0x18 }"), 0x10);
    assert_eq!(compile_and_run("fn f() -> i64 { 0x10 | 0x01 }"), 0x11);
    assert_eq!(compile_and_run("fn f() -> i64 { 0xFF ^ 0x0F }"), 0xF0);
    assert_eq!(compile_and_run("fn f() -> i64 { 1 << 5 }"), 32);
    assert_eq!(compile_and_run("fn f() -> i64 { 0x40 >> 3 }"), 8);
    // Driver idiom: (status & 0x20) tests a bit; bitwise binds tighter than ==.
    assert_eq!(
        compile_and_run("fn f() -> i64 { if 0x60 & 0x20 == 0x20 { 1 } else { 0 } }"),
        1
    );
}

#[test]
fn port_io_lowers_to_in_and_out_instructions() {
    // outb/inb/outw/inw/outl/inl give drivers first-class port access. The
    // 32-bit forms (outl/inl) are what PCI configuration space needs: write a
    // register address to 0xCF8, read/write the 32-bit datum at 0xCFC.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { outb(1016, 75)\nlet s = inb(1021)\noutw(1016, 9)\ninw(1016)\n\
             outl(3320, 2147483648)\ninl(3324) }",
        ))
        .parse_program(),
    );
    assert!(asm.contains("outb %al, %dx"), "outb writes a byte");
    assert!(asm.contains("inb %dx, %al"), "inb reads a byte");
    assert!(asm.contains("outw %ax, %dx"), "outw writes a word");
    assert!(asm.contains("inw %dx, %ax"), "inw reads a word");
    assert!(asm.contains("outl %eax, %dx"), "outl writes a dword");
    assert!(asm.contains("inl %dx, %eax"), "inl reads a dword");
}

#[test]
fn compiles_and_runs_syscall_exit() {
    // syscall(60, 7) = exit(7): the process terminates with exit code 7.
    assert_eq!(compile_and_run("fn f() -> i64 { syscall(60, 7) }"), 7);
}

#[test]
fn compiles_and_runs_syscall_write() {
    // syscall(1, 1, buf, n) = write(1, buf, n). Build a buffer with poke8.
    assert_eq!(
        compile_and_capture(
            "fn f() -> i64 { let buf[4]\npoke8(buf, 72)\npoke8(buf + 1, 105)\
             \npoke8(buf + 2, 33)\nsyscall(1, 1, buf, 3)\n0 }"
        ),
        "Hi!"
    );
}

#[test]
fn argc_is_one_with_no_extra_args() {
    // The test harness runs the binary with no extra args -> argc == 1 (argv[0] only).
    assert_eq!(compile_and_run("fn main() -> i64 { argc() }"), 1);
}

#[test]
fn argv0_points_at_a_nonempty_string() {
    // argv(0) is the program name; its first byte is non-NUL.
    assert_ne!(compile_and_run("fn main() -> i64 { peek8(argv(0)) }"), 0);
}

#[test]
fn array_declared_inside_if_gets_frame_space() {
    // Regression: `collect_arrays` used to recurse only into `for` bodies, not
    // `if` branches. An array declared inside an `if` then had no reserved frame
    // slot, so writing to it dereferenced garbage and segfaulted. Here `buf` is
    // declared inside nested ifs inside a loop (as in parse_demand): it must get
    // real storage so the writes land and the sum is correct.
    let src = "fn f() -> i64 {\n\
               let mut acc = 0\n\
               let mut i = 0\n\
               for i < 3 {\n\
                 if i >= 0 {\n\
                   if i < 100 {\n\
                     let buf[16]\n\
                     poke8(buf, 65)\n\
                     poke8(buf + 15, 2)\n\
                     acc = acc + peek8(buf) + peek8(buf + 15)\n\
                   }\n\
                 }\n\
                 i = i + 1\n\
               }\n\
               acc }";
    assert_eq!(compile_and_run(src), 3 * (65 + 2));
}

#[test]
fn envp0_points_at_a_nonempty_string() {
    // envp(0) is the first "KEY=VALUE" entry; the test process always has env vars,
    // so its first byte is non-NUL.
    assert_ne!(compile_and_run("fn main() -> i64 { peek8(envp(0)) }"), 0);
}

#[test]
fn asm_emits_the_instruction_verbatim() {
    // The inline-assembly escape hatch drops its string straight into the output,
    // so a kernel can reach instructions no builtin covers.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { asm(\"cli\")\nasm(\"hlt\")\n0 }",
        ))
        .parse_program(),
    );
    assert!(asm.contains("\n    cli\n"), "emits cli verbatim");
    assert!(asm.contains("\n    hlt\n"), "emits hlt verbatim");
}

#[test]
fn uefi_serial_writes_to_com1_port() {
    // serial_print/serial_putc lower to COM1 (0x3F8) iface I/O via a helper
    // that polls the UART line-status register, no firmware, survives EBS.
    let asm = emit_uefi(
        &Parser::new(tokenize("fn efi_main() -> i64 { serial_print(\"hi\")\n0 }")).parse_program(),
    );
    assert!(asm.contains("sond_serial_putc:"), "emits the UART helper");
    assert!(asm.contains("movw $0x3F8, %dx"), "writes the data register");
    assert!(asm.contains("outb %al, %dx"), "uses port output");
    assert!(
        asm.contains("call sond_serial_putc"),
        "serial_print calls it"
    );
}

#[test]
fn uefi_readkey_polls_conin_and_print_char_uses_conout() {
    // readkey() polls ConIn->ReadKeyStroke (offset 48 of the system table)
    // until a key arrives; print_char echoes one CHAR16 via ConOut.
    let asm = emit_uefi(
        &Parser::new(tokenize(
            "fn efi_main() -> i64 { let c = readkey()\nprint_char(c)\n0 }",
        ))
        .parse_program(),
    );
    assert!(asm.contains("movq 48(%rax), %rcx"), "readkey reaches ConIn");
    assert!(
        asm.contains("movzwq sond_key+2(%rip), %rax"),
        "reads UnicodeChar"
    );
    assert!(
        asm.contains("movw %ax, sond_char(%rip)"),
        "print_char stages CHAR16"
    );
}

#[test]
fn a_string_literal_lowers_to_the_address_of_null_terminated_bytes() {
    // "A" should emit leaq and the string data; peek8 reads 'A' (65).
    let asm = emit_program(
        &Parser::new(tokenize("fn f() -> i64 { peek8(\"A\") }")).parse_program(),
    );
    assert!(
        asm.contains("leaq .Lstr0(%rip), %rax"),
        "cstr must lea the string address into %rax"
    );
    assert!(
        asm.contains(".asciz"),
        "string data must be emitted with NUL terminator"
    );
}

#[test]
fn a_string_literal_read_by_peek8_gives_its_first_byte() {
    // peek8("A") reads the byte 'A' = 65 at the literal's address.
    assert_eq!(compile_and_run("fn f() -> i64 { peek8(\"A\") }"), 65);
}

#[test]
fn a_string_literal_address_flows_into_a_syscall() {
    // syscall(60, peek8("A")) -> exit(65)
    assert_eq!(
        compile_and_run("fn f() -> i64 { syscall(60, peek8(\"A\")) }"),
        65
    );
}

#[test]
fn a_string_literals_bytes_are_readable_by_offset() {
    // "Hi!" emits "Hi!\0"; peek8 at offset gives each byte.
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 { \
             let p = \"Hi!\"\n\
             syscall(60, peek8(p + 2)) \
             }"
        ),
        33
    );
}

#[test]
#[should_panic(expected = "a string literal is already a pointer")]
fn cstr_is_gone_with_a_helpful_message() {
    // `cstr("x")` used to turn a literal into a pointer. A bare literal now IS one,
    // so the builtin is removed, and calling it points at the replacement rather
    // than failing as an unknown function.
    emit_program(&[parse_function("fn main() -> i64 { cstr(\"x\") }")]);
}

#[test]
fn a_port_program_compiles_and_runs_end_to_end() {
    // The full RFC-0014 pipeline on real x86-64: parse, resolve `use Doubler` to the host
    // provider, splice its method, verify the body against the port's contract, codegen, run.
    // `main` calls the spliced provider method; dbl(21) = 42 is the process exit code.
    let code = compile_and_run(
        "iface Doubler { fn dbl(x: i64) -> (r: i64) ensures { r == x + x } }\n\
         impl Doubler for host { fn dbl(x: i64) -> i64 { x + x } }\n\
         use Doubler\n\
         fn main() -> i64 { dbl(21) }",
    );
    assert_eq!(code, 42, "the spliced provider method runs on real hardware");
}

#[test]
fn a_port_provider_compiles_to_a_pic16_subroutine_and_is_called() {
    // RFC-0014 on the 8-bit target: a `impl Doubler for pic16` method, once resolved and
    // spliced, compiles as a real PIC16 subroutine, and main's `use`-resolved call reaches it.
    // Locks in that ports compose with user-function codegen on --pic16.
    let mut p = Parser::new(tokenize(
        "iface Doubler { fn dbl(x: i64) -> i64 }\n\
         impl Doubler for pic16 { fn dbl(x: i64) -> i64 { x + x } }\n\
         use Doubler\n\
         fn main() { let y = dbl(5) }",
    ));
    let mut funcs = p.parse_program();
    let ports = p.take_ports();
    let providers = p.take_providers();
    let uses = p.take_uses();
    funcs.extend(
        crate::ports::resolve_ports(&ports, &providers, &uses, "pic16")
            .expect("the pic16 provider resolves"),
    );
    let asm = super::pic16::emit_pic16(&funcs);
    assert!(asm.contains("dbl"), "the provider method is emitted as a subroutine:\n{asm}");
    assert!(asm.contains("call"), "main reaches the spliced provider via a call:\n{asm}");
    assert!(
        !asm.contains("unsupported call"),
        "the spliced provider call must not be a no-op:\n{asm}"
    );
}

#[test]
fn a_pic16_serial_provider_replaces_the_backend_intrinsic() {
    // The end of the whole arc: with a `impl Serial for pic16` defining serial_getc, the
    // backend cedes its hardcoded bit-bang intrinsic and the provider's Sond body is the
    // implementation. The hidden asm dies; the firmware need no longer stub. Composition test
    // of the port mechanism (resolve + splice) with the backend's user-function cede.
    let mut p = Parser::new(tokenize(
        "iface Serial { fn serial_getc() -> i64 }\n\
         impl Serial for pic16 { fn serial_getc() -> i64 { 7 } }\n\
         use Serial\n\
         fn main() { let c = serial_getc() }",
    ));
    let mut funcs = p.parse_program();
    let ports = p.take_ports();
    let providers = p.take_providers();
    let uses = p.take_uses();
    funcs.extend(
        crate::ports::resolve_ports(&ports, &providers, &uses, "pic16").expect("resolves for pic16"),
    );
    let asm = super::pic16::emit_pic16(&funcs);
    assert!(asm.contains("serial_getc"), "the provider's serial_getc is emitted:\n{asm}");
    assert!(
        !asm.contains("_sg_wait"),
        "the backend's hidden bit-bang intrinsic ceded to the provider:\n{asm}"
    );
}

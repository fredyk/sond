use super::testutil::{
    compile_and_capture, compile_and_run, compile_and_run_freestanding,
    freestanding_section_alignment, parse_function,
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
    // The checkpoint passes the CURRENT frame base (%rbp) as the second argument, so the agent can
    // read the debuggee's locals and walk its call stack.
    assert!(
        debug_asm.contains("movq %rbp, %rsi"),
        "the checkpoint hands the agent the debuggee frame pointer"
    );
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

/// Emits a program that has module-level globals, returning the assembly.
fn emit_with_globals(src: &str) -> String {
    let mut p = Parser::new(tokenize(src));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    emit_program_g(&globals, &[], &funcs)
}

#[test]
fn an_all_zero_global_array_is_reserved_in_bss_not_written_out() {
    // A zeroed buffer `let mut px[N]` used to be emitted as N `.quad 0` LITERALS in `.data`, so
    // the zeros travelled inside the binary: an image buffer of 2.7M pixels became a ~21 MB file
    // of nothing, and the assembler had to chew through 2.7M literals to produce it. Zeros are
    // reserved, not written: `.bss` states the size and the loader supplies the zeros.
    let small = emit_with_globals("let mut px[8]\nfn main() -> i64 { px[0] }");
    let big = emit_with_globals("let mut px[200000]\nfn main() -> i64 { px[0] }");
    // Not byte-identical (the reserved size itself is written out, and 1600000 has more digits
    // than 64), but the growth must be the digits of one number, not one literal per word.
    assert!(
        big.len() - small.len() < 64,
        "the assembly for a zeroed array must not grow with its size: {} bytes for 8 words, \
         {} bytes for 200000",
        small.len(),
        big.len()
    );
    assert!(
        big.contains(".bss") && big.contains("px: .zero 1600000"),
        "a zeroed array is reserved in .bss as its byte size, got:\n{big}"
    );
}

#[test]
fn a_global_array_of_string_literals_holds_their_addresses() {
    // A fixed list of texts (event names, menu labels) is a module-level table of pointers, and a
    // string literal IS a compile-time constant: its address in `.rodata`. A scalar
    // `let MSG = "hi"` already worked, so rejecting the same literal one bracket deeper was an
    // inconsistency that pushed every fixed list into a function body.
    let asm = emit_with_globals(
        "let NAMES[2] = [\"uno\", \"dos\"]\nfn main() -> i64 { NAMES[0] }",
    );
    assert!(
        asm.contains("NAMES: .quad .Lstr"),
        "the table holds the literals' rodata addresses, got:\n{asm}"
    );
    assert_eq!(
        compile_and_run_with_globals(
            "let NAMES[2] = [\"uno\", \"dos\"]\nfn main() -> i64 { peek8(NAMES[1]) }"
        ),
        100, // 'd' of "dos": the second entry really points at its own bytes
    );
}

#[test]
fn a_global_array_with_nonzero_elements_stays_in_data() {
    // Only ALL-ZERO arrays move: a constant table has values to write down, and `.bss` cannot
    // carry them. This is the guard that the size win does not silently drop initializers.
    let asm = emit_with_globals("let K[4] = [10, 20, 30, 40]\nfn main() -> i64 { K[0] }");
    assert!(
        asm.contains("K: .quad 10, 20, 30, 40"),
        "a table with values keeps its literals in .data, got:\n{asm}"
    );
}

#[test]
fn a_partly_initialized_global_array_writes_only_its_values() {
    // The hermano of the all-zero case: `let buf[N] = [a, b]` declares N words and initializes
    // the first two, so the assembler was handed N literals of which N-2 were padding zeros. A
    // 2.7M-word buffer with a two-word header hit exactly the problem the .bss fix removes, only
    // through the other door. The values are written; the TAIL is reserved.
    let small = emit_with_globals("let buf[8] = [1, 2]\nfn main() -> i64 { buf[0] }");
    let big = emit_with_globals("let buf[100000] = [1, 2]\nfn main() -> i64 { buf[0] }");
    assert!(
        big.len() - small.len() < 64,
        "only the given values are written out: {} bytes for 8 words, {} bytes for 100000",
        small.len(),
        big.len()
    );
    assert!(
        big.contains("buf: .quad 1, 2") && big.contains(".zero 799984"),
        "the values stay literal and the tail is reserved, got:\n{}",
        &big[..big.len().min(4000)]
    );
}

#[test]
fn a_partly_initialized_global_array_still_owns_its_whole_declared_size_on_real_x64() {
    // The reservation is not a size optimization, it is the array's EXTENT. Emitting only the
    // given literals would leave `buf[2..N)` overlapping whatever the assembler placed next, so a
    // write inside the buffer would silently corrupt an unrelated global. The NEXT array is the
    // sentinel, because arrays are emitted in declaration order and land back to back: without
    // the reservation `buf[2]` IS `sentinel[0]`.
    assert_eq!(
        compile_and_run_with_globals(
            "let buf[8] = [1, 2]\nlet sentinel[2] = [9, 9]\n\
             fn main() -> i64 { buf[2] = 100\nbuf[0] + buf[1] + buf[7] + sentinel[0] }"
        ),
        12
    );
}

#[test]
fn a_reserved_array_owns_its_whole_size_and_reads_as_zero_throughout_on_real_x64() {
    // The `.data` side of this has its own test; `.bss` needs the same guard, because a
    // reservation of the WRONG size fails in the one way no happy-path test notices: the array
    // still works near element 0, and only its far end lands on the next global. So write EVERY
    // element — under any short reservation, some write lands on the neighbour — and read the
    // sentinel back. It stays zero only if the array really owns all 64 words. The sentinel's
    // own read also pins the other half of the promise: memory nobody wrote reads as zero.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut px[64]\nlet mut sentinel[2]\n\
             fn main() -> i64 { let mut i = 0\nfor i < 64 { px[i] = 1\ni = i + 1 }\n\
             let mut s = 0\nlet mut j = 0\nfor j < 64 { s = s + px[j]\nj = j + 1 }\n\
             s + sentinel[0] + sentinel[1] }"
        ),
        64
    );
}

#[test]
fn the_uefi_entry_zeroes_bss_itself_instead_of_trusting_the_firmware() {
    // On ELF the zeros are a guarantee: the program header says `memsz > filesz` and the kernel
    // supplies them. On UEFI they are a POLITENESS: the PE loader is *supposed* to zero-fill a
    // section with no raw data, and EDK2/OVMF does, but this project has already been bitten by a
    // strict firmware (VirtualBox) that disagreed with a lenient one about the same PE. A buffer
    // that starts out full of garbage on one machine and zeroed on another is exactly the kind of
    // bug that shows up as a corrupted image months later, so the entry zeroes its own `.bss`
    // between the linker's `__bss_start__`/`__bss_end__` before any Sond code runs.
    let asm = emit_uefi_with_globals("let mut px[8]\nfn efi_main() -> i64 { px[0] }");
    assert!(
        asm.contains("__bss_start__") && asm.contains("__bss_end__"),
        "the UEFI entry bounds its .bss with the linker's symbols, got:\n{asm}"
    );
    assert!(
        asm.contains("rep stosb") || asm.contains("rep stosq"),
        "the UEFI entry zero-fills that range, got:\n{asm}"
    );
}

/// Emits a UEFI program that has module-level globals, returning the assembly.
fn emit_uefi_with_globals(src: &str) -> String {
    let mut p = Parser::new(tokenize(src));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    emit_uefi_g(&globals, &[], &funcs)
}

#[test]
fn a_bss_global_array_still_reads_and_writes_on_real_x64() {
    // The point of the section change is that NOTHING else changes: the array is still writable,
    // still indexable, and still starts out zeroed.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut px[64]\nfn main() -> i64 { px[10] = 7\npx[63] = 35\npx[10] + px[63] + px[0] }"
        ),
        42
    );
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

#[test]
fn the_data_section_declares_its_alignment() {
    // A futex uaddr must be 4-byte aligned or FUTEX_WAIT returns EINVAL WITHOUT EVER BLOCKING.
    // The join's cells live in `.data`, so if the emitter declares no alignment for that section
    // the ELF carries AddrAlign 1, the linker is free to start it anywhere, and whether every
    // spawn in the program can sleep comes down to where the section happened to land. That is
    // the worst shape a bug can take: it cost 743% of CPU on a machine at rest, with no error,
    // no hang and no crash, while a smaller program with the same code was fine by luck.
    //
    // Freestanding, because a hosted link merges crt objects that declare 8 and hide the miss.
    // Carries a string so `.rodata` exists to be checked too, and a spawn so a join cell does.
    let src = "fn w() -> i64 { write(1, \"x\") }\nfn f() -> i64 { spawn w() 0 }\nfn main() -> i64 { f() }";
    let (data_align, rodata_align, ctid) = freestanding_section_alignment(src);
    assert!(
        data_align >= 8,
        ".data must declare at least 8-byte alignment, got {data_align}"
    );
    assert!(
        rodata_align >= 8,
        ".rodata must declare at least 8-byte alignment, got {rodata_align}"
    );
    assert_ne!(ctid, 0, "the test program should have a spawn cell to check");
    assert_eq!(ctid % 8, 0, "the join cell must be 8-byte aligned, got {ctid:#x}");
}

#[test]
fn the_join_does_not_retry_a_failed_futex_at_full_speed() {
    // The join loops until the kernel clears the cell, and the loop used to jump back whatever
    // the futex returned. A futex that fails (EINVAL on a misaligned address, and any other
    // error) returns IMMEDIATELY, so the loop stopped being a wait and became a pure spin: 71826
    // failed syscalls in six seconds, one core burned per thread, no diagnostic anywhere.
    // Alignment is fixed separately; this pins the other half, that an unexpected error can never
    // cost a core again, by sleeping before the retry instead of hammering.
    let asm = emit_program(
        &Parser::new(tokenize(
            "fn w() -> i64 { 0 }\nfn f() -> i64 { spawn w() 0 }\nfn main() -> i64 { f() }",
        ))
        .parse_program(),
    );
    assert!(
        asm.contains("cmpq $-11, %rax") && asm.contains("cmpq $-4, %rax"),
        "the join must tell a retryable futex return (EAGAIN, EINTR) from a real error:\n{asm}"
    );
    assert!(
        asm.contains("movq $35, %rax"),
        "an unexpected futex error must sleep (nanosleep) before retrying, not spin:\n{asm}"
    );
}

#[test]
fn shared_abi_code_assembles_into_a_relocatable_object_and_links_statically() {
    // A `.so` is not the only way to call Sond from another language, and for some targets it is
    // not a usable one: a Go binary built for a distroless/static image has no dynamic loader, so
    // a shared object cannot be loaded at all. The same C-ABI code must therefore also work as a
    // RELOCATABLE OBJECT, linked into the host binary at build time. This pins that the emitted
    // assembly is complete on its own (no C runtime, no hidden startup) by assembling it to a `.o`
    // and linking that into a C program with -static: it runs, and its answer comes from Sond.
    let mut p = Parser::new(tokenize("pub fn sond_add(a: i64, b: i64) -> i64 { a + b }"));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let structs = p.take_structs();
    let asm = emit_shared_g(&globals, &structs, &funcs);

    let dir = std::env::temp_dir();
    let stem = format!("sondc_obj_{}", std::process::id());
    let asm_path = dir.join(format!("{stem}.s"));
    let obj_path = dir.join(format!("{stem}.o"));
    let c_path = dir.join(format!("{stem}.c"));
    let bin_path = dir.join(&stem);
    std::fs::write(&asm_path, asm).unwrap();
    std::fs::write(
        &c_path,
        "long sond_add(long, long);\nint main(void){ return (int)sond_add(19, 23); }\n",
    )
    .unwrap();

    let asm_ok = Command::new("cc")
        .args(["-c"])
        .arg(&asm_path)
        .arg("-o")
        .arg(&obj_path)
        .status()
        .expect("cc should be available");
    assert!(asm_ok.success(), "the shared-ABI assembly must assemble to a .o");

    let link = Command::new("cc")
        .arg("-static")
        .arg(&c_path)
        .arg(&obj_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .unwrap();
    assert!(link.success(), "the object must link statically into a C program");

    let code = Command::new(&bin_path).status().unwrap().code().unwrap();
    assert_eq!(code, 42, "the statically linked Sond function must run and return 19 + 23");

    for p in [&asm_path, &obj_path, &c_path, &bin_path] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn the_emitted_object_marks_the_stack_non_executable() {
    // Without a `.note.GNU-stack` section the linker assumes the worst and marks the program's
    // stack EXECUTABLE: a W^X hole in every binary Sond produces, announced only as a warning
    // that scrolls past, and a hard error on newer linkers. Sond never executes from the stack.
    for asm in [
        emit_program(&[parse_function("fn main() -> i64 { 0 }")]),
        emit_freestanding(&[parse_function("fn main() -> i64 { 0 }")]),
    ] {
        assert!(
            asm.contains(".section .note.GNU-stack,\"\",@progbits"),
            "every ELF target must state that its stack is not executable:\n{asm}"
        );
    }
}

#[test]
fn a_logical_shift_right_respects_the_operand_width() {
    // `>>>` means "shift the value, bringing in zeros". For a NARROW signed type the value sits
    // sign-extended in a 64-bit register, so shifting all 64 bits drags the sign bits down into
    // the result: `-1 as i32 >>> 24` gave 1099511627775 (0xFF_FFFF_FFFF) instead of 255, and the
    // answer does not even fit in an i32. It survived because the usual spelling masks afterwards
    // ((px >>> 24) & 0xFF), which hides it, and because the same expression on a u32 is correct by
    // accident: an unsigned narrow value is held ZERO-extended, so the 64-bit shift agrees.
    //
    // The pixel case is the one that made it matter: an opaque RGBA pixel has the top bit set, so
    // as i32 it is negative, and reading its alpha with a shift returns garbage unless the author
    // remembers the mask.
    //
    // Asserted through STDOUT, not through the exit status: an exit code is eight bits, so
    // `1099511627775 & 0xFF` is also 255 and the broken compiler would have passed. Written that
    // way first, and it did.
    assert_eq!(
        compile_and_capture("fn main() -> i64 { let x: i32 = 0 - 1\nprint_int(x >>> 24)\n0 }"),
        "255",
        "nothing above bit 31 may survive a logical shift of an i32"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { let u: u32 = 0xFF4080C0\nprint_int(u >>> 24)\n0 }"),
        "255",
        "an opaque pixel's alpha through a u32 view (this already worked)"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { let x: i16 = 0 - 1\nprint_int(x >>> 8)\n0 }"),
        "255",
        "16-bit follows the same rule"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { let x: i8 = 0 - 1\nprint_int(x >>> 4)\n0 }"),
        "15",
        "and 8-bit"
    );
    assert_eq!(
        compile_and_capture("fn main() -> i64 { let x: i64 = 0 - 1\nprint_int(x >>> 56)\n0 }"),
        "255",
        "an i64 keeps its own meaning: every bit participates"
    );
}

#[test]
fn a_view_passed_to_a_function_keeps_its_element_width() {
    // The width has to survive the call, and this is the shape every real user writes: the buffer
    // is created in one function and worked on in another (`fn fill(px: u32[n], n: i64)`). Knowing
    // the element type only for LOCALS meant a view addressed at 8 bytes as soon as it was passed,
    // so a loop over n elements wrote 2n words and ran off the end of the buffer — a segfault on a
    // 2.7M-pixel image, and silent corruption on a small one.
    let src = "\
        fn write_two(px: u32[n], n: i64) {\n\
            px[0] = 0xAAAAAAAA\n\
            px[1] = 0xBBBBBBBB\n\
        }\n\
        fn main() -> i64 {\n\
            let backing[8]\n\
            let px: u32[] = backing\n\
            write_two(px, 8)\n\
            print_int(px[0])\n\
            print(\" \")\n\
            print_int(px[1])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "2863311530 3149642683");
}

#[test]
fn the_view_surface_a_caller_writes_against_stays_working() {
    // A CONTRACT, not a feature test. This is the exact shape callers write against — length in
    // the declaration, length in the signature, canonical loop, sub-view at a constant offset —
    // and it is here so that a change to the compiler which breaks that shape breaks the suite
    // first, instead of breaking somebody's build. Anything added later (more widths, 2D sugar)
    // must leave this compiling unchanged; if it cannot, the callers get told BEFORE it lands,
    // not after.
    let src = "\
        fn fill(px: u32[n], n: i64, v: i64) {\n\
            let mut i = 0\n\
            for i < n { px[i] = v\ni = i + 1 }\n\
        }\n\
        fn first(px: u32[n], n: i64, i: i64 where 0 <= i && i < n) -> i64 { px[i] }\n\
        fn main() -> i64 {\n\
            let backing[8]\n\
            let buf: u32[16] = backing\n\
            fill(buf, 16, 0xFF0000FF)\n\
            let tail: u32[] = backing + 32\n\
            print_int(first(buf, 16, 0))\n\
            print(\" \")\n\
            print_int(tail[7])\n\
            0\n\
        }";
    // Every element written through the view, read back through a bounds-proven accessor and
    // through a sub-view over the second half of the same store.
    assert_eq!(compile_and_capture(src), "4278190335 4278190335");
}

#[test]
fn an_eight_bit_view_addresses_single_bytes_and_carries_the_element_s_sign() {
    // Bytes are what a camera hands over: a JPEG is a byte stream, and reading it through a word
    // view would swallow seven neighbours per read. Eight `u8` writes must land in ONE word, and
    // the SIGN must follow the element type, not the machine: the same 0xFF byte is 255 read as
    // `u8` and -1 read as `i8`. That difference is the whole reason a pixel is `u32` and not
    // `i32`, one width down.
    let src = "\
        fn fill(b: u8[n], n: i64) {\n\
            let mut i = 0\n\
            for i < n { b[i] = 0xFF\ni = i + 1 }\n\
        }\n\
        fn main() -> i64 {\n\
            let backing[1]\n\
            let b: u8[] = backing\n\
            fill(b, 8)\n\
            let s: i8[] = backing\n\
            print_int(b[0])\n\
            print(\" \")\n\
            print_int(s[0])\n\
            print(\" \")\n\
            print_int(peek64(backing))\n\
            0\n\
        }";
    // 255 unsigned, -1 signed, and the eight bytes filled exactly one 64-bit word (-1).
    assert_eq!(compile_and_capture(src), "255 -1 -1");
}

#[test]
fn a_typed_view_indexes_memory_at_its_element_width() {
    // An image buffer is millions of pixels: too big for the stack (2.7M RGBA is 11 MB against an
    // 8 MB limit) and wrong as a global (the declared extent belongs to the whole program). It has
    // to live in memory the program asks for, and be indexed as what it is. `u32[]` is a VIEW: an
    // element type with no length of its own, over memory the caller owns. Backed by a stack array
    // here so the test needs no allocator; the mechanism is the same, a base address plus a width.
    //
    // u32 rather than i32 for pixels, and the difference is not cosmetic: an opaque RGBA pixel has
    // its top bit set, so as a signed 32-bit value it is negative, `px > 0` is false for every
    // opaque pixel, and a shift without a mask returns garbage.
    let src = "\
        fn main() -> i64 {\n\
            let backing[16]\n\
            let px: u32[] = backing\n\
            px[0] = 0xFF4080C0\n\
            px[3] = 0x11223344\n\
            print_int(px[0] >>> 24)\n\
            print(\" \")\n\
            print_int(px[0] & 0xFF)\n\
            print(\" \")\n\
            print_int(px[3])\n\
            0\n\
        }";
    // The first pixel's alpha, its blue channel, and the fourth pixel read back whole.
    assert_eq!(compile_and_capture(src), "255 192 287454020");
}

#[test]
fn a_typed_view_reads_four_bytes_not_eight() {
    // The width is the whole point: two adjacent u32 slots are independent values. An 8-byte store
    // into slot 0 would swallow slot 1, and an 8-byte load would return both at once. Two full
    // 32-bit patterns with their top bits set catch either mistake.
    let src = "\
        fn main() -> i64 {\n\
            let backing[8]\n\
            let a: u32[] = backing\n\
            a[0] = 0xAAAAAAAA\n\
            a[1] = 0xBBBBBBBB\n\
            print_int(a[0])\n\
            print(\" \")\n\
            print_int(a[1])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "2863311530 3149642683");
}

#[test]
fn an_untyped_array_still_indexes_as_64_bit_words() {
    // Nothing written before this feature may change meaning: an array with no element type is
    // still a word array, and `a[1]` is still the second 8-byte word.
    let src = "\
        fn main() -> i64 {\n\
            let a[4]\n\
            a[0] = 0xAAAAAAAA\n\
            a[1] = 0xBBBBBBBB\n\
            print_int(a[0])\n\
            print(\" \")\n\
            print_int(a[1])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "2863311530 3149642683");
}

#[test]
fn the_canonical_view_form_a_caller_writes_keeps_working() {
    // This is not a feature test. It is a PROMISE, written where a promise can fail: the exact
    // shape an outside project builds its image module on, both spellings of it, pinned so that
    // changing the view again turns this red here instead of breaking someone else's code
    // silently.
    //
    // It exists because that already happened. The form without a declared length was given out
    // as frozen, a later fix rejected it, and the consumer found out by their build failing. The
    // fix was right; announcing it as a promise instead of a test was not.
    //
    // A local carrying its own length, and a function taking the view with the length beside it:
    assert_eq!(
        compile_and_capture(
            "fn fill(px: u32[n], n: i64) {\n\
                 let mut i = 0\n\
                 for i < n { px[i] = 0xFF4080C0\ni = i + 1 }\n\
             }\n\
             fn main() -> i64 {\n\
                 let n = 4\n\
                 let backing[2]\n\
                 let px: u32[n] = backing\n\
                 fill(px, n)\n\
                 print_int(px[0] >>> 24)\n\
                 print(\" \")\n\
                 print_int(px[n - 1] & 0xFF)\n\
                 0\n\
             }"
        ),
        "255 192",
        "the alpha of the first pixel and the blue of the last, through the form a caller writes"
    );
}

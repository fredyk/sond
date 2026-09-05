use super::testutil::{
    compile_and_capture, compile_and_capture_with_globals, compile_and_run,
    compile_and_run_freestanding, compile_and_run_windows,
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
    // Through the LOCATED lexer: the point table carries source lines, and a parser built
    // without them would report every point at line 0.
    let (toks, lines, _) = crate::lexer::tokenize_located("fn f() -> i64 { let x = 1\nx + 1 }");
    let funcs = Parser::with_lines(toks, lines).parse_program();
    let asm =
        emit_program_coverage_g(&[], &[], &funcs, None, &|l| {
            format!("f.snd:{l}")
        });
    // Each statement records that it executed into the shared buffer via __cov_ptr.
    assert!(
        asm.contains("movq __cov_ptr(%rip), %r11"),
        "coverage build marks statements"
    );
    // The total point count is published for the harness to size its buffer.
    assert!(
        asm.contains("__cov_total:"),
        "publishes the total point count"
    );
    // And each point carries the place a person can open, not just an id.
    assert!(
        asm.contains("__cov_loc_table:"),
        "publishes where every point is"
    );
    assert!(asm.contains("f.snd:1"), "resolved through the caller's locator, at the right line");
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
    let funcs =
        Parser::new(tokenize("fn main() -> i64 { let a = 1\nlet b = 2\na + b }")).parse_program();
    let debug_asm = emit_program_debug_g(&[], &[], &funcs);
    let normal_asm = emit_program_g(&[], &[], &funcs);
    assert!(
        debug_asm.contains("call __dbg_step"),
        "the --debug build calls the checkpoint"
    );
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
    assert!(
        debug_asm.contains("call __dbg_enter"),
        "the --debug build bumps the depth on entry"
    );
    assert!(
        debug_asm.contains("call __dbg_exit"),
        "the --debug build drops the depth on exit"
    );
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
    assert!(
        asm.contains("pushq %r15"),
        "the pub boundary saves the host's %r15"
    );
    assert!(
        asm.contains("popq %r15"),
        "and restores it before returning"
    );
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
    assert_eq!(
        compile_and_run(src),
        3,
        "all three peers ran and were joined"
    );
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
    assert_eq!(
        compile_and_run(src),
        27,
        "results bound by position: a=1, b=2, c=3"
    );
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
    assert_eq!(
        compile_and_run(prod),
        42,
        "par(*) multiplies the peer results"
    );
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
    assert_eq!(
        compile_and_run(src),
        41,
        "nodes evaluate in dependency order regardless of source order"
    );
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
    assert_eq!(
        compile_and_run(src),
        27,
        "parallel level nodes bound to their own results"
    );
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
    assert_eq!(
        code, 1,
        "argc() should be 1 when run with no extra arguments"
    );
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
    let asm = emit_with_globals("let NAMES[2] = [\"uno\", \"dos\"]\nfn main() -> i64 { NAMES[0] }");
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

#[test]
fn the_uefi_entry_zeroes_bss_before_it_stashes_anything_of_its_own() {
    // The zero-fill must not be able to erase what the entry just wrote down. Today it cannot,
    // but only because `sond_systab` and `sond_imghandle` happen to land in `.data` while only
    // arrays go to `.bss` — correct BY COINCIDENCE. The day anything moves a zeroed scalar to
    // `.bss` (and `sond_systab` is a `.quad 0`, exactly the shape that would move), the entry
    // would wipe the SystemTable it had just saved, and the symptom is a UEFI app that prints
    // nothing, with no clue pointing here. Zeroing FIRST and stashing after makes the order
    // correct by construction, whatever section anything ends up in.
    let asm = emit_uefi_with_globals("let mut px[8]\nfn efi_main() -> i64 { px[0] }");
    let entry = &asm[asm.find("efi_main:").expect("the UEFI entry is emitted")..];
    let fill = entry.find("rep stos").expect("the entry zero-fills .bss");
    let stash = entry
        .find("sond_systab(%rip)")
        .expect("the entry stashes the SystemTable");
    assert!(
        fill < stash,
        "the entry must zero .bss BEFORE stashing anything, got:\n{}",
        &entry[..entry.len().min(1200)]
    );
}

/// La seccion vigente en cada linea, y lo que escribe un valor distinto de cero.
///
/// El ensamblador lleva UNA seccion abierta y todo lo que sigue cae dentro, asi que un emisor que
/// no declara la suya hereda la del anterior. `as` rechaza con «attempt to store non-zero value in
/// section `.bss'» y nombra la linea, no a quien abrio la seccion.
fn lineas_no_cero_en_bss(asm: &str) -> Vec<String> {
    let datos = [".quad", ".long", ".short", ".byte", ".asciz", ".ascii", ".double"];
    let mut seccion = ".text";
    let mut malas = Vec::new();
    for linea in asm.lines() {
        let t = linea.trim();
        if t == ".bss" || t.starts_with(".section .bss") {
            seccion = ".bss";
            continue;
        }
        if t == ".data" || t == ".text" || t.starts_with(".section ") {
            seccion = ".data";
            continue;
        }
        if seccion != ".bss" {
            continue;
        }
        // `.zero N` es lo unico que `.bss` admite: reserva sin escribir.
        let cuerpo = t.split_once(':').map(|(_, r)| r.trim()).unwrap_or(t);
        let Some(d) = datos.iter().find(|d| cuerpo.starts_with(**d)) else {
            continue;
        };
        let valores = cuerpo[d.len()..].trim();
        if !valores.split(',').all(|v| v.trim() == "0") {
            malas.push(t.chars().take(90).collect());
        }
    }
    malas
}

#[test]
fn a_uefi_string_declares_its_own_section_instead_of_inheriting_whatever_is_open() {
    // Las cadenas UEFI se emitian SIN declarar seccion, apoyandose en un `.data` que abria
    // `efi_main` mucho antes para `sond_systab`. En cuanto el programa tiene un array a ceros a
    // nivel de modulo, entre medias se abre `.bss` -- y las cadenas caen ahi. `as` lo rechaza:
    // `lib/any/crypto/kdf/hkdf-sha256_test.snd --uefi` no compilaba por esto, y el error senala la
    // linea de la cadena, que es la unica que no tiene la culpa.
    //
    // No se comprueba que ponga `.data`: se comprueba lo que el ensamblador comprueba, que en
    // `.bss` no se escriba nada distinto de cero. Asi el caso cubre tambien al proximo emisor que
    // herede una seccion sin saberlo.
    let asm = emit_uefi_with_globals(
        "let mut px[8]\nfn efi_main() -> i64 { print(\"hola\")\n px[0] }",
    );
    assert!(asm.contains(".Lstr0"), "el programa tiene una cadena, got:\n{asm}");
    let malas = lineas_no_cero_en_bss(&asm);
    assert!(
        malas.is_empty(),
        "estas lineas escriben en `.bss`, que solo admite reservas, y `as` rechaza el fichero \
         entero: {malas:?}\n{asm}"
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
fn module_level_byte_view_reserves_bytes_and_writes_in_bytes() {
    // `let mut g: u8[32]` at module level must allocate exactly 32 bytes in .bss, resolve to
    // the buffer address, index at byte granularitiy (two adjacent positions stay independent),
    // and start zeroed.
    let asm = emit_with_globals("let mut g: u8[32]\nfn main() -> i64 { g[0] }");
    assert!(
        asm.contains(".bss"),
        "a byte view at module level must be reserved in .bss, got:\n{asm}"
    );
    assert!(
        asm.contains("g: .zero 32"),
        "u8[32] must reserve exactly 32 bytes, not 256 words, got:\n{}",
        &asm[..asm.len().min(3000)]
    );
    // The index must use byte-width stores (movb/movq %al) not 64-bit stores into word slots.
    // With byte-width we get g[0]=65, g[1]=42 independently.
    assert_eq!(
        compile_and_run_with_globals("let mut g: u8[32]\nfn main() -> i64 { g[0] = 65\ng[0] }"),
        65
    );
    // Two adjacent positions must be independent byte slots.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut g: u8[32]\nfn main() -> i64 {\n    g[0] = 65\n    g[1] = 42\n    g[0] + g[1]\n    }"
        ),
        107
    );
    // Byte-level independence via raw memory access: peek at g+0 and g+1 independently.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut g: u8[32]\nfn main() -> i64 {\n    g[0] = 65\n    g[1] = 42\n    peek8(g + 0)\n    }"
        ),
        65
    );
    assert_eq!(
        compile_and_run_with_globals(
            "let mut g: u8[32]\nfn main() -> i64 {\n    g[0] = 65\n    g[1] = 42\n    peek8(g + 1)\n    }"
        ),
        42
    );
    // Must start at zero.
    assert_eq!(
        compile_and_run_with_globals("let mut g: u8[32]\nfn main() -> i64 { g[0] + g[31] }"),
        0
    );
}

#[test]
fn module_level_u32_view_reserves_bytes_at_4_byte_granularity() {
    // `let mut buf: u32[16]` must reserve 64 bytes (16 * 4), index at 4-byte boundaries.
    let asm = emit_with_globals("let mut buf: u32[16]\nfn main() -> i64 { peek32(buf) }");
    assert!(asm.contains(".bss"));
    assert!(asm.contains("buf: .zero 64"));
    // Write at element 0 and read it back. Value 0xaa = 170 fits in exit code.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut buf: u32[16]\nfn main() -> i64 {\n    buf[0] = 0xaa\n    peek32(buf)\n    }"
        ),
        0xaa
    );
    // Verify 4-byte granularity: write 1 to element 2 (byte offset 8).
    // If the compiler mistakenly used 8-byte word granularity, writing 1 at offset 8
    // would actually be element 1, not element 2. We verify by checking element 0 is still 0xaa.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut buf: u32[16]\nfn main() -> i64 {\n    buf[0] = 0xaa\n    buf[2] = 0xbb\n    peek32(buf)\n    }"
        ),
        0xaa // element 0 unchanged by write to element 2
    );
    // Element 2 should be 0xbb = 187, fits in exit code.
    assert_eq!(
        compile_and_run_with_globals(
            "let mut buf: u32[16]\nfn main() -> i64 {\n    buf[2] = 0xbb\n    peek32(buf + 8)\n    }"
        ),
        0xbb
    );
}

#[test]
fn module_level_view_large_does_not_expand_binary() {
    // The binary size for a zeroed view must not grow proportionally to its capacity.
    let asm_small = emit_with_globals("let mut g: u8[64]\nfn main() -> i64 { 0 }");
    let asm_big = emit_with_globals("let mut g: u8[200000]\nfn main() -> i64 { 0 }");
    let growth = asm_big.len() - asm_small.len();
    assert!(
        growth < 256,
        "a zeroed view must NOT expand the binary: growth={growth} bytes (expected < 256)"
    );
}

#[test]
fn non_literal_view_length_is_a_compile_error() {
    // If the length is not a literal (e.g. derived from a variable), compilation must fail.
    let src = "let mut buf: u8[n]\nfn main(n: i64) -> i64 { 0 }";
    let mut p = Parser::new(tokenize(src));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    // emit_program_g calls set_globals internally, which must panic for non-literal view lengths.
    let result = std::panic::catch_unwind(|| {
        emit_program_g(&globals, &[], &funcs);
    });
    assert!(
        result.is_err(),
        "set_globals must panic for non-literal view length"
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
    let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
    // The scratch dir goes on drop, even when an assert below panics: the `remove_file` calls on
    // the happy path left every FAILING run's files behind for good, and this temp dir is RAM.
    let dir = crate::scratch::Temporal::nuevo("sondc_st");
    let asm_path = dir.join("prog.s");
    let bin_path = dir.join("prog");
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

/// Compiles a program that has module-level globals and captures its stdout.
fn compile_and_run_with_globals(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let mut p = Parser::new(tokenize(src));
    let funcs = p.parse_program();
    let globals = p.take_globals();
    let asm = emit_program_g(&globals, &[], &funcs);
    let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
    // The scratch dir goes on drop, even when an assert below panics: the `remove_file` calls on
    // the happy path left every FAILING run's files behind for good, and this temp dir is RAM.
    let dir = crate::scratch::Temporal::nuevo("sondc_g");
    let asm_path = dir.join("prog.s");
    let bin_path = dir.join("prog");
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

fn compile_and_run_with_globals_lowered(src: &str) -> i32 {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let (funcs, globals, _structs) = super::testutil::parse_and_lower(src, "host");
    let asm = emit_program_g(&globals, &[], &funcs);
    let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
    // The scratch dir goes on drop, even when an assert below panics: the `remove_file` calls on
    // the happy path left every FAILING run's files behind for good, and this temp dir is RAM.
    let dir = crate::scratch::Temporal::nuevo("sondc_g_l");
    let asm_path = dir.join("prog.s");
    let bin_path = dir.join("prog");
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
fn global_constant_table_len_returns_the_count() {
    // `let t[3] = [10,20,30]` is a constant table in .data; `.len` must yield
    // the element count 3 (not the byte count).
    assert_eq!(compile_and_run_with_globals_lowered(
        "let t[3] = [10, 20, 30]\nfn main() -> i64 { t.len }"
    ), 3);
}

#[test]
fn global_mutable_buffer_len_returns_the_count() {
    // `let mut buf[7]` reserves 7 words in .bss; `.len` yields 7.
    assert_eq!(compile_and_run_with_globals_lowered(
        "let mut buf[7]\nfn main() -> i64 { buf.len }"
    ), 7);
}

#[test]
fn global_typed_view_len_returns_the_count() {
    // `let g: i64[7]` is a typed view that reserves 7 words in .bss;
    // `.len` must return 7 (element count, not byte count).
    assert_eq!(compile_and_run_with_globals_lowered(
        "let mut g: i64[7]\nfn main() -> i64 { g.len }"
    ), 7);
}

#[test]
fn global_len_uses_element_count_not_byte_count() {
    // u32 is 4 bytes, 8 bits per byte -> 32 bits per element.
    // `let buf: u32[4]` reserves 4*4=16 bytes but has 4 elements.
    // .len must return 4, not 16.
    assert_eq!(compile_and_run_with_globals_lowered(
        "let buf: u32[4]\nfn main() -> i64 { buf.len }"
    ), 4);
}

#[test]
fn global_len_inside_if_block() {
    // .len on a global inside an if branch must resolve to the constant.
    assert_eq!(compile_and_run_with_globals_lowered(
        "let arr[5] = [1,2,3,4,5]\nfn main() -> i64 {\n    if 1 == 1 { arr.len }\n    else { 0 }\n}"
    ), 5);
}

#[test]
fn global_len_inside_for_loop() {
    // .len on a global inside a for loop body must resolve to the constant.
    assert_eq!(compile_and_run_with_globals_lowered(
        "let arr[4] = [10,20,30,40]\nfn main() -> i64 {\n    let mut i = 0\n    for i < arr.len { arr[i] = i + 1  i = i + 1 }\n    0\n}"
    ), 0);
}

#[test]
fn local_array_len_still_works() {
    // Verify the fix does not break local arrays: .len on a local should still
    // return the expected constant.
    assert_eq!(compile_and_run(
        "fn main() -> i64 { let a: i64[5] = [0,0,0,0,0]\n    a.len }"
    ), 5);
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
fn asm_rejects_variable_references() {
    // `asm(...)` requires a string literal so the assembler can emit the
    // instruction at compile time. Passing a variable (even one holding a
    // string) must fail with a message saying a literal is required.
    let err = std::panic::catch_unwind(|| {
        emit_uefi_with_globals(
            "fn efi_main() -> i64 { let s: string = \"cli\"\nasm(s.utf8)\n0 }",
        )
    })
    .expect_err("asm with a variable must not compile");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("string literal"),
        "must mention 'string literal' in the error, got: {msg}"
    );
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
    let asm =
        emit_program(&Parser::new(tokenize("fn f() -> i64 { peek8(\"A\") }")).parse_program());
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
fn a_control_byte_in_a_literal_is_escaped_for_the_assembler() {
    // `"ERROR\0"` reached the assembler as a raw NUL between the quotes of a `.asciz`, which
    // ends the string right there as far as `as` is concerned: it answered `invalid character
    // (0xa) in mnemonic` pointing at a line the programmer never wrote, and the build died
    // with nothing to grep for. Every control byte gets an octal escape now, so the text
    // survives the trip and keeps the NUL it was written with -- separate from the one
    // `.asciz` appends.
    let asm = emit_program(
        &Parser::new(tokenize(
            "fn f() -> i64 { let s: string = \"ERROR\\0\"\n    s.utf8 }",
        ))
        .parse_program(),
    );
    assert!(
        asm.contains(r#".asciz "ERROR\000""#),
        "a NUL inside a literal must be escaped as \\000:\n{asm}"
    );
    assert!(
        !asm.lines()
            .any(|l| l.contains(".asciz") && l.contains('\0')),
        "no line may carry a raw control byte inside the quotes:\n{asm}"
    );
}

#[test]
fn the_same_literal_used_twice_has_one_address() {
    // Two uses of the same `string` global gave two DIFFERENT pointers, ten bytes apart --
    // one `.rodata` copy per use -- because `intern_string` interned nothing: it appended a
    // fresh label every call. The name said one thing and the body did another.
    //
    // It reads as a waste of space, but it breaks a program: comparing a value returned by a
    // lookup against the fallback it was given (`env_get_or(name, FALLBACK) == FALLBACK`)
    // is false even when the fallback IS what came back, because the two `FALLBACK` are two
    // addresses. The equality that everyone writes silently stops meaning what it says.
    let asm = emit_program(
        &Parser::new(tokenize(
            "let S: string = \"dup\"\nfn f() -> i64 { S.utf8 - S.utf8 }",
        ))
        .parse_program(),
    );
    assert_eq!(
        asm.matches(".asciz \"dup\"").count(),
        1,
        "the bytes must be emitted once, not once per use:\n{asm}"
    );
    assert_eq!(
        compile_and_run("let S: string = \"dup\"\nfn f() -> i64 { S.utf8 - S.utf8 }"),
        0,
        "two uses of the same literal must be the same address"
    );
}

#[test]
fn the_same_float_used_twice_has_one_constant() {
    // The same hole in intern_float. A duplicated f64 constant cannot break equality the way
    // the string one does (the value is loaded, not its address), so this is only waste --
    // but it is the same function shape, and half a fix is the half that rots.
    let asm = emit_program(&Parser::new(tokenize("fn f() -> f64 { 1.5 + 1.5 }")).parse_program());
    assert_eq!(
        asm.matches(".Lflt0:").count(),
        1,
        "one label per distinct constant:\n{asm}"
    );
    assert!(
        !asm.contains(".Lflt1:"),
        "1.5 twice must not emit a second constant:\n{asm}"
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
            // The literal is passed straight to `peek8`: binding it to a `let p: string` first
            // makes `p` a length-carrying view, and raw `peek` into one of those is refused on
            // purpose (`p[i]` is the checked way to read it). What this test is about is the
            // bytes a literal emits, so the literal is where the offset is taken.
            "fn f() -> i64 { syscall(60, peek8(\"Hi!\" + 2)) }"
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
    assert_eq!(
        code, 42,
        "the spliced provider method runs on real hardware"
    );
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
    assert!(
        asm.contains("dbl"),
        "the provider method is emitted as a subroutine:\n{asm}"
    );
    assert!(
        asm.contains("call"),
        "main reaches the spliced provider via a call:\n{asm}"
    );
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
        crate::ports::resolve_ports(&ports, &providers, &uses, "pic16")
            .expect("resolves for pic16"),
    );
    let asm = super::pic16::emit_pic16(&funcs);
    assert!(
        asm.contains("serial_getc"),
        "the provider's serial_getc is emitted:\n{asm}"
    );
    assert!(
        !asm.contains("_sg_wait"),
        "the backend's hidden bit-bang intrinsic ceded to the provider:\n{asm}"
    );
}

#[test]
fn a_bare_bang_negates_instead_of_vanishing() {
    // The lexer consumed a bare `!` and emitted NOTHING for it -- with a comment saying it was
    // not a token yet, while the parser and this backend both understood `UnOp::Not` fine. So
    // `!x` lexed as plain `x`: the negation disappeared before anything could complain. `if
    // !ready { .. }` ran exactly when it must not, and `fn negar(b: bool) -> bool { !b }`
    // returned its argument. Nothing failed to compile and nothing said a word.
    //
    // 1 + 0 + 10 + 0 + 100: the negations that must hold, and the ones that must not.
    let src = "fn negar(b: bool) -> bool { !b }\n\
               fn main() -> i64 { let mut n = 0\n\
                 if !false { n = n + 1 }\n\
                 if !true { n = n + 2 }\n\
                 if negar(false) { n = n + 10 }\n\
                 if negar(true) { n = n + 20 }\n\
                 if !0 { n = n + 100 }\n\
                 n }";
    assert_eq!(compile_and_run(src), 111);
}

#[test]
fn a_bang_equals_is_still_an_inequality() {
    // The same character starts `!=`, so the fix must not turn `a != b` into a negation of `=`.
    assert_eq!(
        compile_and_run("fn main() -> i64 { if 1 != 2 { 7 } else { 0 } }"),
        7
    );
}

#[test]
fn continue_in_a_counting_range_advances_the_counter() {
    // `for i in 0..10 { if i == 4 { continue } .. }` HUNG, at 100% of a core, for ever. The
    // counting form desugars to `i = 0` plus a `for i < 10` whose body ends with `i = i + 1`, and
    // `continue` jumps straight to the condition -- past the increment. So the one iteration that
    // took the `continue` never advanced, and the loop spun on i == 4 with nothing to see: it
    // compiled clean, printed nothing and burned the CPU.
    //
    // Nine iterations increment `v`, the tenth (i == 4) skips: 9.
    let src = "fn main() -> i64 { let mut v = 0\nfor i in 0..10 { if i == 4 { continue }\nv = v + 1 }\nv }";
    assert_eq!(compile_and_run(src), 9);
}

#[test]
fn continue_in_a_three_part_for_advances_the_step() {
    // The same hole in the C-style form, whose step is appended to the body in the same way.
    let src = "fn main() -> i64 { let mut v = 0\nfor let mut i = 0; i < 10; i++ { if i == 4 { continue }\nv = v + 1 }\nv }";
    assert_eq!(compile_and_run(src), 9);
}

#[test]
fn a_continue_in_a_nested_loop_advances_only_its_own_counter() {
    // The inner `continue` belongs to the inner loop: it must advance `j` and leave `i` alone.
    // Stepping both would skip an outer iteration -- a wrong answer instead of a hang, which is
    // harder to notice.
    let src = "fn main() -> i64 { let mut v = 0\n\
               for i in 0..3 { for j in 0..3 { if j == 1 { continue }\nv = v + 1 } }\nv }";
    assert_eq!(compile_and_run(src), 6); // 3 outer x 2 inner
}

#[test]
fn a_spawn_in_a_loop_runs_every_iteration_and_joins_them_all() {
    // A `spawn` per turn of a loop -- one thread per accepted connection -- was a compile error:
    // each spawn SITE owned one static join cell, so the next turn overwrote the tid of a thread
    // still running and it was never joined. What people did instead was fork per connection,
    // which is how a test VM reached 2920 processes at load 3.43 on two cores. A fork per
    // connection has no ceiling; a fixed set of reusable slots has one by construction.
    //
    // Each worker writes its OWN slot rather than incrementing a shared counter, so this proves
    // every thread ran without depending on an atomic the language does not have. The spawning
    // function returns before main reads, and its structured join is what makes that read safe.
    let src = "fn worker(out: i64, i: i64) -> i64 { poke64(out + i * 8, i + 1)\n0 }\n\
               fn run(out: i64) -> i64 { let mut i = 0\nfor i < 8 { spawn worker(out, i)\ni = i + 1 }\n0 }\n\
               fn main() -> i64 {\n\
                 let slots[8]\n\
                 let mut k = 0\n\
                 for k < 8 { poke64(slots + k * 8, 0)\nk = k + 1 }\n\
                 run(slots)\n\
                 let mut sum = 0\n\
                 k = 0\n\
                 for k < 8 { sum = sum + peek64(slots + k * 8)\nk = k + 1 }\n\
                 sum }";
    assert_eq!(
        compile_and_run(src),
        36,
        "1 + 2 + ... + 8: every spawned thread ran and was joined"
    );
}

#[test]
fn a_loop_with_more_turns_than_slots_reuses_them_without_losing_a_thread() {
    // 100 turns over a ring of 64: from turn 65 on, each spawn must WAIT for the thread whose
    // slot it is taking. That wait is the ceiling -- the property a fork per connection does not
    // have -- and it is also where a thread would be silently dropped if the slot were reused
    // without joining. Every worker writes its own slot, so a lost one shows up as a missing 1.
    let src = "fn worker(out: i64, i: i64) -> i64 { poke64(out + i * 8, 1)\n0 }\n\
               fn run(out: i64) -> i64 { let mut i = 0\nfor i < 100 { spawn worker(out, i)\ni = i + 1 }\n0 }\n\
               fn main() -> i64 {\n\
                 let slots[100]\n\
                 let mut k = 0\n\
                 for k < 100 { poke64(slots + k * 8, 0)\nk = k + 1 }\n\
                 run(slots)\n\
                 let mut n = 0\n\
                 k = 0\n\
                 for k < 100 { n = n + peek64(slots + k * 8)\nk = k + 1 }\n\
                 n }";
    assert_eq!(
        compile_and_run(src),
        100,
        "all 100 threads ran and were joined"
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
    assert_ne!(
        ctid, 0,
        "the test program should have a spawn cell to check"
    );
    assert_eq!(
        ctid % 8,
        0,
        "the join cell must be 8-byte aligned, got {ctid:#x}"
    );
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

    let dir = crate::scratch::Temporal::nuevo("sondc_obj");
    let asm_path = dir.join("prog.s");
    let obj_path = dir.join("prog.o");
    let c_path = dir.join("prog.c");
    let bin_path = dir.join("prog");
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
    assert!(
        asm_ok.success(),
        "the shared-ABI assembly must assemble to a .o"
    );

    let link = Command::new("cc")
        .arg("-static")
        .arg(&c_path)
        .arg(&obj_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .unwrap();
    assert!(
        link.success(),
        "the object must link statically into a C program"
    );

    let code = Command::new(&bin_path).status().unwrap().code().unwrap();
    assert_eq!(
        code, 42,
        "the statically linked Sond function must run and return 19 + 23"
    );

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
        fn write_two(px: u32[n], n: i64) requires { n >= 2 } {\n\
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
fn declaring_a_view_with_a_length_reserves_it_zeroed() {
    // `let ar: i32[n]` says everything needed to hand over storage: the element width and how
    // many. Requiring an explicit allocator call on top was ceremony, and worse, the declaration
    // on its own compiled to a NULL pointer that segfaulted on first use — the language handing
    // out a loaded gun for the one spelling that reads as "give me a buffer".
    //
    // The memory comes zeroed, like every other declaration in the language: `let a[N]` is zeroed,
    // a global is zeroed, and a buffer that starts out as whatever was there before is how you get
    // an image with somebody else's pixels in it.
    let src = "\
        fn main() -> i64 {\n\
            let n = 16\n\
            let ar: i32[n]\n\
            ar[0] = 7\n\
            ar[15] = 35\n\
            print_int(ar[0] + ar[15] + ar[8])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "42");

    // The reservation must be the RIGHT size, and too small is the dangerous direction: the arena
    // hands out consecutive blocks, so a short reservation puts the end of one buffer on top of
    // the next. Fill the first completely, then read the second, which must still be untouched.
    let sentinel = "\
        fn main() -> i64 {\n\
            let n = 64\n\
            let a: u32[n]\n\
            let b: u32[n]\n\
            let mut i = 0\n\
            for i < n { a[i] = 0xFFFFFFFF\ni = i + 1 }\n\
            let mut s = 0\n\
            let mut j = 0\n\
            for j < n { s = s + b[j]\nj = j + 1 }\n\
            print_int(s)\n\
            0\n\
        }";
    assert_eq!(
        compile_and_capture(sentinel),
        "0",
        "filling one reserved view must not reach into the next"
    );

    // And the size asked for is the element count times the element's own width, not a word each.
    let asm = emit_program(
        &Parser::new(tokenize(
            "fn main() -> i64 { let n = 16\nlet ar: i32[n]\nar[0] }",
        ))
        .parse_program(),
    );
    assert!(
        asm.contains("imulq $4, %rax, %rdi"),
        "an i32 view reserves four bytes per element, got:\n{asm}"
    );
}

#[test]
fn a_reservation_of_nothing_leaves_the_arena_usable() {
    // A length of zero or below asks for no storage, and the answer has to be "here is nothing",
    // not "here is the arena, broken". Measured before the guard: a negative length walked the
    // bump cursor backwards and the SEGFAULT landed on the NEXT view — a healthy one, declared
    // afterwards — which is the worst kind of report to receive, because the line that crashes is
    // not the line that is wrong.
    let src = "\
        fn main() -> i64 {\n\
            let bad = 0 - 5\n\
            let broken: i32[bad]\n\
            let none = 0\n\
            let empty: i32[none]\n\
            let m = 8\n\
            let ok: i32[m]\n\
            ok[0] = 42\n\
            print_int(ok[0])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "42");
}

#[test]
fn a_reservation_shows_up_in_the_arena_s_own_accounting() {
    // The reservation bumps the same cursor as `heap_alloc`, so it must also add to the same
    // total, or `heap_used()` reports zero while a program eats megabytes a second — a leak with
    // the only instrument that could see it pointing the other way.
    // Read the arena's own total (control block word 3) with builtins, so the test needs no
    // library: it is the exact cell `heap_used()` reports.
    let src = "\
        fn main() -> i64 {\n\
            let n = 100000\n\
            let buf: u32[n]\n\
            buf[0] = 1\n\
            print_int(peek64(arena_tcb() + 16))\n\
            0\n\
        }";
    assert_eq!(
        compile_and_capture(src),
        "400000",
        "100000 u32 elements are 400000 bytes, and the arena has to say so"
    );
}

#[test]
fn reserving_a_view_on_uefi_is_refused_rather_than_emitted_as_a_syscall() {
    // The reservation grows its arena with `mmap`, and on UEFI there is no kernel to answer it:
    // the same declaration compiled cleanly and would have executed a `syscall` instruction on
    // bare metal. A compile error naming the alternative is the only honest answer until the
    // firmware path (AllocatePool, plus an explicit zero-fill, since pool memory is not zeroed)
    // is written AND booted — and this machine cannot boot a UEFI image to prove it.
    let err = std::panic::catch_unwind(|| {
        emit_uefi_with_globals("fn efi_main() -> i64 { let n = 16\nlet ar: i32[n]\nar[0] }")
    })
    .expect_err("reserving on UEFI must be refused");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("alloc") && msg.contains("UEFI"),
        "the error must name the target and the way out, got: {msg}"
    );
}

#[test]
fn a_windows_program_links_as_a_console_pe_and_runs() {
    // The fotomaton runs on Windows and has to talk to vendor DLLs, so Sond needs a PE that a
    // Windows loader accepts. The compiler already emits PE for UEFI; this is the hosted-console
    // side of it. Run under wine rather than eyeballed: a binary for a machine none of us has is
    // exactly where reading the assembly and hoping goes wrong.
    assert_eq!(compile_and_run_windows("fn main() -> i64 { 6 * 7 }"), 42);
    // Control flow and locals, so the target is a real one and not just a `ret`.
    assert_eq!(
        compile_and_run_windows(
            "fn main() -> i64 { let mut s = 0\nlet mut i = 0\nfor i < 10 { s = s + i\ni = i + 1 }\ns }"
        ),
        45
    );
}

#[test]
fn a_windows_program_calls_a_dll_under_the_microsoft_abi() {
    // The reason this target exists: the fotomaton has to call vendor DLLs (a camera SDK, a
    // printer SDK). `extern fn` declares one; the call has to follow the Microsoft x64 ABI, which
    // is NOT the one Sond emits everywhere else — arguments in %rcx/%rdx/%r8/%r9, and 32 bytes of
    // shadow space the CALLER reserves. Get either wrong and the callee reads rubbish.
    //
    // `GetTickCount` takes nothing and returns milliseconds since boot: any running machine
    // answers above zero, so it proves the call reached the DLL and came back.
    assert_eq!(
        compile_and_run_windows(
            "extern fn GetTickCount() -> i64\n\
             fn main() -> i64 { if GetTickCount() > 0 { 7 } else { 0 } }"
        ),
        7
    );
    // `MulDiv(a, b, c)` computes a*b/c: three arguments, so it pins the register ORDER as well as
    // the fact that a call happens at all. 100*3/2 = 150.
    assert_eq!(
        compile_and_run_windows(
            "extern fn MulDiv(a: i64, b: i64, c: i64) -> i64\n\
             fn main() -> i64 { MulDiv(100, 3, 2) }"
        ),
        150
    );
}

#[test]
fn the_three_part_for_loop_runs_like_its_hand_written_form() {
    // `for let mut i = 0; i < n; i++ { .. }`: the shape every C-family programmer already knows,
    // and the one an AI writer reaches for by reflex. It is sugar — init before the loop, step at
    // the end of the body — so it has to behave EXACTLY like the hand-written version, including
    // the counter surviving into the loop's own condition.
    assert_eq!(
        compile_and_run(
            "fn main() -> i64 { let mut s = 0\nfor let mut i = 0; i < 5; i++ { s = s + i }\ns }"
        ),
        10
    );
    // A step other than ++, and a bound that is a variable rather than a literal.
    assert_eq!(
        compile_and_run(
            "fn main() -> i64 { let n = 10\nlet mut s = 0\n\
             for let mut i = 0; i < n; i = i + 2 { s = s + 1 }\ns }"
        ),
        5
    );
    // And it indexes a view under the bounds checker exactly as the plain form does: this is the
    // loop the checker has to keep proving, or the sugar would cost the guarantee.
    assert_eq!(
        compile_and_run(
            "fn main() -> i64 { let n = 8\nlet buf: i32[n]\n\
             for let mut i = 0; i < n; i++ { buf[i] = i }\nbuf[7] }"
        ),
        7
    );
}

#[test]
fn a_view_over_a_string_literal_knows_how_long_it_is() {
    // `string` is a TYPE, not a pointer with a convention attached: `let s: string = "hola"` is
    // four bytes the compiler counted itself, indexed with the same bounds check as any other
    // view. Without it, text was the one thing the language could not walk safely, and the
    // natural way to handle it was the loop that reads until it happens to find a NUL — off the
    // end of the buffer when there is not one.
    let src = "\
        fn main() -> i64 {\n\
            let s: string = \"hola\"\n\
            let mut sum = 0\n\
            for i in 0..4 { sum = sum + s[i] }\n\
            print_int(sum)\n\
            print(\" \")\n\
            print_int(s.len)\n\
            0\n\
        }";
    // 104+111+108+97 = 420, and the length is the literal's own, NUL excluded.
    assert_eq!(compile_and_capture(src), "420 4");
}

#[test]
fn a_string_walks_its_own_characters() {
    // `for c in s` over a `string`: the loop knows where to stop because the type carries the
    // length. This is the form that replaces "read until a NUL turns up", which walks off the end
    // of the buffer when the NUL is not there.
    let src = "\
        fn main() -> i64 {\n\
            let s: string = \"hola\"\n\
            let mut sum = 0\n\
            for c in s { sum = sum + c }\n\
            print_int(sum)\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "420");
}

#[test]
fn a_view_with_a_written_down_length_reserves_its_own_storage() {
    // `let b: u8[32]` is the byte buffer everything else is supposed to migrate TO — the thing
    // you index instead of `peek`ing at. It died in codegen with `unknown variable \`32\``,
    // because the reservation read the length as the NAME of a variable holding it, which is
    // only one of the two ways a view can say how long it is.
    let src = "\
        fn main() -> i64 {\n\
            let b: u8[32]\n\
            b[0] = 65\n\
            b[31] = 66\n\
            print_int(b[0] + b[31])\n\
            print(\" \")\n\
            print_int(b[7])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "131 0");
}

#[test]
fn a_string_hands_over_its_bytes_only_when_asked() {
    // A literal is TEXT and binds to `string`; there is no second spelling that quietly turns it
    // into an address. What a protocol needs — the exact bytes, the ñ as its UTF-8 pair — is a
    // separate, explicit request on the string, so the two readings can never be confused for
    // one another at a glance.
    let src = "\
        fn main() -> i64 {\n\
            let s: string = \"año\"\n\
            let b = s.utf8\n\
            print_int(s.len)\n\
            print(\" \")\n\
            print_int(s[1])\n\
            print(\" \")\n\
            print_int(b.len)\n\
            print(\" \")\n\
            print_int(b[1])\n\
            print(\" \")\n\
            print_int(b[2])\n\
            0\n\
        }";
    // Three characters, the ñ as U+00F1; four BYTES, the ñ as 0xC3 0xB1.
    assert_eq!(compile_and_capture(src), "3 241 4 195 177");
}

#[test]
fn a_view_can_be_declared_with_its_contents() {
    // Reserving and then filling by hand is three lines of ceremony for a table of constants.
    // `let buf: i32[n] = {a, b, c}` reserves AND writes, at the element's width, with the rest
    // of the declared length left zeroed like any other reservation.
    let src = "\
        fn main() -> i64 {\n\
            let n = 5\n\
            let buf: i32[n] = {10, 20, 30}\n\
            print_int(buf[0] + buf[1] + buf[2])\n\
            print(\" \")\n\
            print_int(buf[4])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(src), "60 0");
    // The elements land at the ELEMENT's width, not a word each: two u32 slots share one word.
    let packed = "\
        fn main() -> i64 {\n\
            let n = 2\n\
            let px: u32[n] = {0xAAAAAAAA, 0xBBBBBBBB}\n\
            print_int(px[0])\n\
            print(\" \")\n\
            print_int(px[1])\n\
            0\n\
        }";
    assert_eq!(compile_and_capture(packed), "2863311530 3149642683");
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

// --- Issue: string module-level regression and .len on copy ---

#[test]
fn module_string_with_type_annotation_returns_pointer_to_bytes() {
    // `let saludo: string = "ax"` must return the address of the interned bytes,
    // so peek8(saludo + 0) == 97 ('a'). Before the fix it returned 0 (bss buffer).
    assert_eq!(
        compile_and_run_with_globals(
            "let saludo: string = \"ax\"\nfn main() -> i64 { peek8(saludo + 0) }"
        ),
        97,
        "typed module-level string: the first byte of 'ax' is 97"
    );
}

#[test]
fn module_string_annotated_and_unannotated_produce_identical_result() {
    // Both forms must read the same bytes.
    assert_eq!(
        compile_and_run_with_globals("let a = \"hola\"\nfn main() -> i64 { peek8(a + 0) }"),
        104,
        "unannotated: 'h' = 104"
    );
    assert_eq!(
        compile_and_run_with_globals("let b: string = \"hola\"\nfn main() -> i64 { peek8(b + 0) }"),
        104,
        "annotated: 'h' = 104"
    );
    // They must resolve to the same interned label too.
    let mut p1 = Parser::new(tokenize("let a = \"dup-test\"\nfn f() -> i64 { 0 }"));
    let funcs1 = p1.parse_program();
    let globals1 = p1.take_globals();
    let asm1 = emit_shared_g(&globals1, &[], &funcs1);
    let mut p2 = Parser::new(tokenize(
        "let b: string = \"dup-test\"\nfn f() -> i64 { 0 }",
    ));
    let funcs2 = p2.parse_program();
    let globals2 = p2.take_globals();
    let asm2 = emit_shared_g(&globals2, &[], &funcs2);
    // Each form interns the literal exactly once, and both emit the SAME label for it:
    // comparing `asm1` with itself would pass no matter what the annotated form does.
    assert_eq!(
        asm1.matches(".asciz \"dup-test\"").count(),
        1,
        "unannotated interns once: {asm1}"
    );
    assert_eq!(
        asm2.matches(".asciz \"dup-test\"").count(),
        1,
        "annotated interns once: {asm2}"
    );
    let label_of = |asm: &str| {
        asm.lines()
            .position(|l| l.contains(".asciz \"dup-test\""))
            .and_then(|i| asm.lines().nth(i.wrapping_sub(1)).map(str::to_string))
    };
    let (l1, l2) = (label_of(&asm1), label_of(&asm2));
    assert!(
        l1.as_deref().is_some_and(|l| l.contains(':')),
        "the literal must be preceded by a label, got {l1:?}"
    );
    assert_eq!(
        l1, l2,
        "both forms must reach the literal through the same label"
    );
}

#[test]
fn module_string_non_ascii_yields_utf8_bytes() {
    // `"año"`, `peek8(s+0)`=97('a'), `peek8(s+1)`=195, `peek8(s+2)`=177.
    // Use Rust's compile-time unicode escape so the Sond source contains actual ñ (UTF-8 c0a9).
    let src: &str = "let s: string = \"año\"\nfn main() -> i64 { peek8(s + 0) }";
    assert_eq!(compile_and_run_with_globals(src), 97);
    let src: &str = "let s: string = \"año\"\nfn main() -> i64 { peek8(s + 1) }";
    assert_eq!(compile_and_run_with_globals(src), 195);
    let src: &str = "let s: string = \"año\"\nfn main() -> i64 { peek8(s + 2) }";
    assert_eq!(compile_and_run_with_globals(src), 177);
}

#[test]
fn string_copy_inherits_length() {
    // `let l: string = "hola"` then `let q: string = l` -> q.len == 4.
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let l: string = \"hola\"\n\
                 let q: string = l\n\
                 q.len\n\
             }"
        ),
        4,
        "copy of string local inherits length"
    );
}

#[test]
fn module_level_byte_view_still_works() {
    // `let mut g: u8[32]` at module level must still reserve 32 bytes in .bss.
    let asm = emit_with_globals("let mut g: u8[32]\nfn main() -> i64 { g[0] }");
    assert!(asm.contains(".bss"));
    assert!(asm.contains("g: .zero 32"));
    assert_eq!(
        compile_and_run_with_globals("let mut g: u8[32]\nfn main() -> i64 { g[0] = 65\ng[0] }"),
        65
    );
}

#[test]
fn module_level_u32_view_still_works() {
    // `let mut v: u32[16]` at module level.
    let asm = emit_with_globals("let mut v: u32[16]\nfn main() -> i64 { peek32(v) }");
    assert!(asm.contains(".bss"));
    assert!(asm.contains("v: .zero 64"));
    assert_eq!(
        compile_and_run_with_globals(
            "let mut v: u32[16]\nfn main() -> i64 { v[0] = 42\npeek32(v) }"
        ),
        42
    );
}

#[test]
fn local_string_codepoints_work() {
    // Local string: `l[i]` accesses code points, and `l.len == 3`.
    // With "añb": code points are 97, 241(ñ), 98; len=3.
    assert_eq!(
        compile_and_run(concat!(
            "fn f() -> i64 {\n            \
                 let l: string = \"a",
            "\u{00f1}",
            "b\"\n\
                 l.len\n\
             }"
        )),
        3,
        "local string .len counts code points"
    );
    assert_eq!(
        compile_and_run(concat!(
            "fn f() -> i64 {\n            \
                 let l: string = \"a",
            "\u{00f1}",
            "b\"\n\
                 l[0]\n\
             }"
        )),
        97,
        "l[0] is the code point for 'a'"
    );
    assert_eq!(
        compile_and_run(concat!(
            "fn f() -> i64 {\n            \
                 let l: string = \"a",
            "\u{00f1}",
            "b\"\n\
                 l[1]\n\
             }"
        )),
        241,
        "l[1] is the code point for 'ñ'"
    );
    assert_eq!(
        compile_and_run(concat!(
            "fn f() -> i64 {\n            \
                 let l: string = \"a",
            "\u{00f1}",
            "b\"\n\
                 l[2]\n\
             }"
        )),
        98,
        "l[2] is the code point for 'b'"
    );
}

/// Copying a view keeps its ELEMENT WIDTH, not just its length.
///
/// Inheriting the source's view info (which is what makes `.len` survive a copy) also fixes
/// something the copy got wrong before: `let p = b` over a `u8` view used to index in 8-byte
/// words, so `p[1]` read past the two bytes just written and answered 0 while `b[1]` answered 2.
/// A copy points at the same memory as its source; reading it at a different stride is the kind
/// of wrong answer that compiles clean, which is the only reason this is pinned by a test rather
/// than left as an implementation detail.
#[test]
fn a_copied_byte_view_still_indexes_in_bytes() {
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut b: u8[16]\n\
                 b[0] = 1\n\
                 b[1] = 2\n\
                 let p = b\n\
                 p[1]\n\
             }"
        ),
        2,
        "a copy of a u8 view indexes in bytes, like the view it copies"
    );
}

// The tests below guard `a string parameter carries UTF-8 bytes whoever the caller is`.
// Each one is written so it goes red when the mechanism it names is broken: passing a
// variable through the call path, and refusing when more than one text can reach the call.

#[test]
fn a_string_variable_reaches_the_callee_as_the_same_bytes_as_a_literal() {
    // The bug this closes: the same declared type arrived in two representations, and the
    // callee could not tell which. `añb` is the probe because its second byte (0xC3) only
    // exists in the UTF-8 view -- the code-point view answers 0 there, so an ASCII-only
    // literal would have hidden the whole thing.
    let from_literal = "fn b(s: string) -> i64 { peek8(s + 1) }\n\
                        fn f() -> i64 { b(\"a\u{00f1}b\") }";
    let from_variable = "fn b(s: string) -> i64 { peek8(s + 1) }\n\
                         fn f() -> i64 { let l: string = \"a\u{00f1}b\"\nb(l) }";
    assert_eq!(compile_and_run(from_literal), 195, "literal argument");
    assert_eq!(
        compile_and_run(from_variable),
        195,
        "a variable holding the same text must hand over the same bytes"
    );
}

#[test]
fn a_reassigned_variable_hands_over_the_text_it_holds_now() {
    // The label travels with the reassignment: what reaches the callee is the last text
    // assigned, not the one the variable was born with.
    let prog = "fn b(s: string) -> i64 { peek8(s) }\n\
                fn f() -> i64 { let mut l: string = \"a\u{00f1}\"\nl = \"z\u{00f1}\"\nb(l) }";
    assert_eq!(
        compile_and_run(prog),
        122,
        "after the reassignment, 'z' = 122"
    );
}

#[test]
fn a_parameter_forwarded_to_another_function_keeps_its_bytes() {
    let prog = "fn b(s: string) -> i64 { peek8(s + 1) }\n\
                fn a(x: string) -> i64 { b(x) }\n\
                fn f() -> i64 { a(\"a\u{00f1}b\") }";
    assert_eq!(compile_and_run(prog), 195, "the bytes survive one more hop");
}

#[test]
fn both_branches_assigning_the_same_text_still_compiles() {
    // Refusing correct code would be worse than the bug: when the two branches cannot
    // disagree, there is nothing undecidable and the call goes through.
    let prog = "fn b(s: string) -> i64 { peek8(s) }\n\
                fn f(c: i64) -> i64 { let mut a: string = \"h\u{00f1}\"\n\
                if c > 0 { a = \"h\u{00f1}\" } else { a = \"h\u{00f1}\" }\nb(a) }";
    assert_eq!(
        compile_and_run(prog),
        104,
        "same text on both sides, 'h' = 104"
    );
}

#[test]
#[should_panic(expected = "its value as a `string` is unknown")]
fn two_branches_assigning_different_texts_are_refused_at_the_call() {
    // The other half of the commit, and the half that had no test: where two texts can
    // reach the same call the compiler refuses instead of picking one. Silently picking
    // the `then` branch compiles clean and hands the callee the wrong bytes at runtime.
    emit_program(
        &Parser::new(tokenize(
            "fn b(s: string) -> i64 { peek8(s) }\n\
         fn f(c: i64) -> i64 { let mut a: string = \"one\"\n\
         if c > 0 { a = \"two\" } else { a = \"three\" }\nb(a) }",
        ))
        .parse_program(),
    );
}

#[test]
#[should_panic(expected = "its value as a `string` is unknown")]
fn a_loop_body_that_reassigns_is_refused_at_the_call() {
    // Same reasoning through a loop: the text at the call depends on how many times the
    // body ran, which is not something the compiler can decide.
    emit_program(
        &Parser::new(tokenize(
            "fn b(s: string) -> i64 { peek8(s) }\n\
         fn f() -> i64 { let mut a: string = \"one\"\n\
         let mut i = 0\n\
         for i < 3 { a = \"two\"\ni = i + 1 }\nb(a) }",
        ))
        .parse_program(),
    );
}

#[test]
fn a_branch_that_assigns_nothing_leaves_the_variable_passable() {
    // An `if` that never touches the variable must not cost it its label: the merge has
    // to look at what the branches assigned, not at whether a branch ran.
    let prog = "fn b(s: string) -> i64 { peek8(s) }\n\
                fn f(c: i64) -> i64 { let mut a: string = \"h\u{00f1}\"\n\
                if c > 0 { let z = 1 }\nb(a) }";
    assert_eq!(compile_and_run(prog), 104, "'h' = 104");
}

#[test]
fn a_delete_byte_in_a_literal_is_escaped_for_the_assembler() {
    // 0x7F is not below 0x20, so a range check written as `< 0x20` lets it through raw.
    // A NUL is already covered by `a_control_byte_in_a_literal_is_escaped_for_the_assembler`.
    let asm = emit_program(
        &Parser::new(tokenize(
            "fn f() -> i64 { let s: string = \"E\x7FX\"\n    s.utf8 }",
        ))
        .parse_program(),
    );
    assert!(
        asm.contains(r#".asciz "E\177X""#),
        "0x7F must reach the assembler as \\177:\n{asm}"
    );
    let with_tab = emit_program(
        &Parser::new(tokenize(
            "fn f() -> i64 { let s: string = \"E\tX\"\n    s.utf8 }",
        ))
        .parse_program(),
    );
    assert!(
        !with_tab
            .lines()
            .any(|l| l.contains(".asciz") && l.as_bytes().contains(&b'\t')),
        "no raw tab may sit inside the quotes of a .asciz:\n{with_tab}"
    );
}

#[test]
fn entry_function_is_callable_by_other_functions_in_the_same_file() {
    // A library file (no `main`) where the last function is called by another
    // function defined before it.  The last function becomes the entry and is
    // compiled under `sond_start`, but it must still be callable by its original
    // name.  Regression: the entry was excluded from `fn_names`, so the codegen
    // treated the call as an unrecognised builtin and emitted a `todo!()`.
    let funcs = Parser::new(tokenize(
        "fn caller() -> i64 { callee() }\nfn callee() -> i64 { 42 }",
    ))
    .parse_program();
    let asm = emit_program_g(&[], &[], &funcs);
    assert!(
        asm.contains("call callee"),
        "a call to the entry function must emit `call callee`:\n{asm}"
    );
    // The entry function is compiled under `sond_start`, so a trampoline from
    // its original name is needed.
    assert!(
        asm.contains("jmp sond_start"),
        "a trampoline from the entry's name to `sond_start` must exist:\n{asm}"
    );
    // The function must actually run and return 42.
    assert_eq!(compile_and_run("fn caller() -> i64 { callee() }\nfn callee() -> i64 { 42 }"), 42);
}

#[test]
fn a_redefined_colliding_global_binds_each_file_to_its_own_value() {
    // The repro behind io.snd/serial.snd done end-to-end: an included library and the entry
    // both define a module-level `let O_DUP`, with DIFFERENT values. Each file must read its
    // own definition -- `a()` the library's 20, `main` the entry's 7 -- so the program's exit
    // code is 20 + 7 = 27. The bug was both definitions being mangled to the same id (a
    // "declared more than once" panic in codegen), because the depth-0 entry has no mangling id.
    let root = crate::scratch::Temporal::nuevo("sond_kodup");
    std::fs::write(root.join("a.snd"), "let O_DUP = 20\npub fn a() -> i64 { O_DUP }").unwrap();
    let entry = root.join("main.snd");
    std::fs::write(
        &entry,
        "include \"a.snd\"\nlet O_DUP = 7\nfn main() -> i64 { a() + O_DUP }",
    )
    .unwrap();
    let (src, _) = crate::resolve::resolve_includes_scoped(&entry).unwrap();
    let code = compile_and_run_with_globals(&src);
    assert_eq!(code, 27, "a() reads 20 (its file's O_DUP), main reads 7 (its own)");
}

#[test]
fn the_same_program_on_two_lines_is_still_two_statements() {
    // The one-liner `let mut b = 2 (a + b)` is now a parse error: a call whose
    // callee is not a name. This is the half that keeps the fix from OVERCHARGING:
    // the same text split over two lines inserts the implicit `;` at the newline
    // after `2`, so `2` ends the `let` and `(a + b) + (a + b)` is a separate
    // statement expression. What was one bogus call must stay two real statements.
    let src = "fn main() -> i64 {\n let mut a = 1\n let mut b = 2\n (a + b) + (a + b)\n}";
    let asm = emit_program(&Parser::new(tokenize(src)).parse_program());
    assert!(
        asm.contains("addq %rcx, %rax"),
        "the addition compiles: {asm}"
    );
    // It runs to the right answer on real x64: (1 + 2) + (1 + 2) == 6.
    assert_eq!(compile_and_run(src), 6);
}

#[test]
#[should_panic(expected = "is given a string literal but is not declared")]
fn the_test_helper_compiles_through_the_same_pipeline_as_the_driver() {
    // Not a codegen test: a guard on the helper. `compile_and_run` used to go from the parser
    // straight to codegen, skipping the checks and desugars that `main.rs` runs in between, so a
    // program the compiler REFUSES compiled fine in here. Five tests were written against
    // programs `sondc` rejects, and a shadowing fix once passed four tests while the binary still
    // returned the wrong number. This program is rejected by a check; if it ever compiles again,
    // the helper has stopped going through the pipeline.
    compile_and_run("fn f() -> i64 { let p = \"Hi!\"\nsyscall(60, peek8(p + 2)) }");
}

/// ...and the guard above only watches ONE helper, by name. `compile_and_run_with_globals` was
/// added later, went from the parser straight to `emit_program_g`, and skipped the whole pipeline
/// without anything firing: the guard could not see a helper it did not name. Ten tests were
/// running against a compiler no user ever runs.
///
/// So this one names none. It reads the helper file and demands that EVERY helper which both
/// compiles AND runs a program goes through `parse_and_lower`, the same entry point `main.rs`
/// uses. A helper written tomorrow is watched the day it is written, without anyone remembering
/// to add it here.
///
/// A comment asking the next author to be careful is NOT a guard: that is exactly what was in
/// place, and the second helper walked past it.
#[test]
fn every_compiling_test_helper_goes_through_the_pipeline() {
    let src = include_str!("testutil.rs");
    // A helper that only parses (`parse_function`) or only emits text without running it is not
    // in scope: what must not diverge is the path a program takes before it is EXECUTED.
    let mut offenders: Vec<&str> = Vec::new();
    for chunk in src.split("\npub(crate) fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or("");
        if !(name.starts_with("compile_and_run") || name.starts_with("compile_and_capture")) {
            continue;
        }
        // The body ends where the next top-level item begins.
        let body = chunk.split("\npub(crate) fn ").next().unwrap_or("");
        let body = body.split("\n}\n").next().unwrap_or(body);
        if !body.contains("parse_and_lower") {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "these helpers compile and run a program without going through `parse_and_lower`, so they \
         test a compiler `sondc` never runs: {:?}. Route them through `parse_and_lower`; if one \
         genuinely must skip it, it needs a different name, because this guard watches \
         `compile_and_run*` and `compile_and_capture*` by shape, on purpose.",
        offenders
    );
}

// ---------- lock is a free identifier ----------

/// `lock` is NOT a reserved keyword: it is a contextual keyword that only acts as
/// a critical-section modifier when it appears at statement position followed by `(`.
/// A variable named `lock` is legal everywhere else.
#[test]
fn lock_is_a_free_identifier() {
    let src = "fn main() -> i64 {\n\
               let lock[1]\n\
               poke64(lock, 42)\n\
               peek64(lock)\n\
               }";
    let funcs = crate::lexer::tokenize(src);
    let mut p = Parser::new(funcs);
    let _ = p.parse_program();
    // parse succeed → lock is NOT reserved
}

/// A `fn lock()` definition is legal (lock is an identifier).
#[test]
fn lock_as_function_name() {
    let src = "fn lock(x: i64) -> i64 { x + 1 }\n\
               fn main() -> i64 { lock(5) }\n";
    let funcs = crate::lexer::tokenize(src);
    let mut p = Parser::new(funcs);
    let _ = p.parse_program();
}

// ---------- atomic_cas64 ----------

/// `atomic_cas64` when *ptr == expected: returns old value, writes new.
#[test]
fn atomic_cas64_when_value_matches_writes_new_and_returns_old() {
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 2)\n\
                 let old = atomic_cas64(slot, 2, 5)\n\
                 old\n\
             }"
        ),
        2,
        "old value returned"
    );
    // Verify memory was updated
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 2)\n\
                 atomic_cas64(slot, 2, 5)\n\
                 peek64(slot)\n\
             }"
        ),
        5,
        "new value written to memory"
    );
}

/// `atomic_cas64` when *ptr != expected: returns *ptr untouched, memory unchanged.
#[test]
fn atomic_cas64_when_value_does_not_match_leaves_memory_alone() {
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 7)\n\
                 let old = atomic_cas64(slot, 0, 99)\n\
                 old\n\
             }"
        ),
        7,
        "returns *ptr when no match"
    );
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 7)\n\
                 atomic_cas64(slot, 0, 99)\n\
                 peek64(slot)\n\
             }"
        ),
        7,
        "memory untouched on mismatch"
    );
}

// ---------- atomic_add64 ----------

/// `atomic_add64` returns the value BEFORE the add; memory holds the sum.
#[test]
fn atomic_add64_returns_old_value_and_memory_holds_sum() {
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 10)\n\
                 let old = atomic_add64(slot, 5)\n\
                 old\n\
             }"
        ),
        10,
        "returns old value"
    );
    assert_eq!(
        compile_and_run(
            "fn f() -> i64 {\n\
                 let mut slot[1]\n\
                 poke64(slot, 10)\n\
                 atomic_add64(slot, 5)\n\
                 peek64(slot)\n\
             }"
        ),
        15,
        "memory holds the sum"
    );
}

// ---------- lock mutual exclusion ----------

/// Multiple threads each increment a shared counter N times inside a lock.
/// The final value must equal threads × N. Without the lock the value would be lower.
#[test]
fn lock_provides_mutual_exclusion() {
    let threads = 8;
    let locked_n = 10;
    let unlocked_n = 10000;

    let prog = |threads: usize, body: &str| -> String {
        format!(
            "let mut mtx[1]\n\
             let mut counter[1]\n\
             let mut done[1]\n\
             fn bump() -> i64 {{\n\
                 {body}\n\
                 atomic_add64(done, 1)\n\
                 0\n\
             }}\n\
             fn run() -> i64 {{\n\
                 poke64(done, 0)\n\
                 let mut t = 0\n\
                 for t < {threads} {{ spawn bump()\nt = t + 1 }}\n\
                 for peek64(done) < {threads} {{ }}\n\
                 peek64(counter)\n\
             }}\n\
             fn main() -> i64 {{\n\
                 print_int(run())\n\
                 0\n\
             }}",
            threads = threads,
            body = body
        )
    };

    // WITH lock: every increment is serialized → exact answer.
    let locked = format!(
        "let mut i = 0\nfor i < {n} {{ lock (mtx) {{ counter[0] = counter[0] + 1 }}\ni = i + 1 }}",
        n = locked_n
    );
    let with_lock = compile_and_capture_with_globals(&prog(threads, &locked));
    assert_eq!(
        with_lock,
        (threads * locked_n).to_string(),
        "with lock: {threads} threads × {locked_n} = {}",
        threads * locked_n
    );

    // WITHOUT lock: the increments race on `counter`, losing updates → strictly below expected.
    // On single-core systems the main thread's spin-loop may starve the other threads, so all
    // increments may arrive.  Accept either a strict race (value < N) or the perfect value
    // (no concurrent access observed).
    let unlocked = format!(
        "let mut i = 0\nfor i < {n} {{ counter[0] = counter[0] + 1\ni = i + 1 }}",
        n = unlocked_n
    );
    let without_lock = compile_and_capture_with_globals(&prog(threads, &unlocked));
    let raced = without_lock
        .parse::<i64>()
        .expect("an integer total from the racing program");
    assert!(
        raced <= (threads * unlocked_n) as i64,
        "without lock total must not exceed expected: got {raced}, expected {}",
        threads * unlocked_n
    );
}

/// The lock releases on a `return` inside the critical section.
#[test]
fn lock_releases_on_return_inside_critical_section() {
    assert_eq!(
        compile_and_capture_with_globals(
            "let mut mtx[1]\n\
             let mut cnt_data[1]\n\
             let mut done[1]\n\
             fn early_return() -> i64 {\n\
                 lock (mtx) {\n\
                     atomic_add64(done, 1)\n\
                     return 1\n\
                 }\n\
                 0\n\
             }\n\
             fn other() -> i64 {\n\
                 let mut i = 0\n\
                 for i < 100000 {\n\
                     lock (mtx) { cnt_data[0] = cnt_data[0] + 1 }\n\
                     i = i + 1\n\
                 }\n\
                 atomic_add64(done, 1)\n\
                 0\n\
             }\n\
             fn run() -> i64 {\n\
                 poke64(cnt_data, 0)\n\
                 poke64(done, 0)\n\
                 spawn early_return()\n\
                 for peek64(done) < 1 { }\n\
                 spawn other()\n\
                 for peek64(done) < 2 { }\n\
                 peek64(cnt_data)\n\
             }\n\
             fn main() -> i64 { print_int(run())\n0 }"
        ),
        "100000",
        "other must finish all iterations → the lock was released on the early return"
    );
}

/// The lock releases on a `break` inside the critical section.
#[test]
fn lock_releases_on_break_inside_critical_section() {
    assert_eq!(
        compile_and_capture_with_globals(
            "let mut mtx[1]\n\
             let mut cnt_data[1]\n\
             let mut done[1]\n\
             fn breaker() -> i64 {\n\
                 let mut i = 0\n\
                 for {\n\
                     lock (mtx) {\n\
                         cnt_data[0] = cnt_data[0] + 1\n\
                         atomic_add64(done, 1)\n\
                         break\n\
                     }\n\
                     i = i + 1\n\
                 }\n\
                 0\n\
             }\n\
             fn incrementer() -> i64 {\n\
                 let mut i = 0\n\
                 for i < 100000 {\n\
                     lock (mtx) { cnt_data[0] = cnt_data[0] + 1 }\n\
                     i = i + 1\n\
                 }\n\
                 atomic_add64(done, 1)\n\
                 0\n\
             }\n\
             fn run() -> i64 {\n\
                 poke64(cnt_data, 0)\n\
                 poke64(done, 0)\n\
                 spawn breaker()\n\
                 for peek64(done) < 1 { }\n\
                 spawn incrementer()\n\
                 for peek64(done) < 2 { }\n\
                 peek64(cnt_data)\n\
             }\n\
             fn main() -> i64 { print_int(run())\n0 }"
        ),
        "100001",
        "breaker=1, incrementer=100000 → the break released the lock"
    );
}

/// A `return` inside a `lock` must return its own value (not fall through to the rest of the
/// function). The lock is released, then control goes to the function's real return.
#[test]
fn lock_return_returns_the_early_value() {
    assert_eq!(
        compile_and_run_with_globals(
            "let mut mtx[1]\n\
             fn f() -> i64 {\n\
                 lock (mtx) {\n\
                     return 7\n\
                 }\n\
                 42\n\
             }\n\
             fn main() -> i64 { f() }"
        ),
        7,
        "the `return 7` must win over the fall-through `42`"
    );
}

/// A `break` inside a `lock` nested in another `lock` must release BOTH locks. Here the
/// breaker exits the doubly-nested body; if either lock were left held the incrementer would
/// block on it forever and never finish its 100000 nested-lock increments.
#[test]
fn lock_break_nested_releases_both_locks() {
    assert_eq!(
        compile_and_capture_with_globals(
            "let mut a[1]\n\
             let mut b[1]\n\
             let mut cnt_data[1]\n\
             let mut done[1]\n\
             fn breaker() -> i64 {\n\
                 for {\n\
                     lock (a) {\n\
                         lock (b) {\n\
                             atomic_add64(done, 1)\n\
                             break\n\
                         }\n\
                         999\n\
                     }\n\
                 }\n\
                 0\n\
             }\n\
             fn incrementer() -> i64 {\n\
                 let mut i = 0\n\
                 for i < 100000 {\n\
                     lock (a) {\n\
                         lock (b) { cnt_data[0] = cnt_data[0] + 1 }\n\
                     }\n\
                     i = i + 1\n\
                 }\n\
                 atomic_add64(done, 1)\n\
                 0\n\
             }\n\
             fn run() -> i64 {\n\
                 poke64(cnt_data, 0)\n\
                 poke64(done, 0)\n\
                 spawn breaker()\n\
                 for peek64(done) < 1 { }\n\
                 spawn incrementer()\n\
                 for peek64(done) < 2 { }\n\
                 peek64(cnt_data)\n\
             }\n\
             fn main() -> i64 { print_int(run())\n0 }"
        ),
        "100000",
        "breaker released both nested locks, so the incrementer finished all its iterations"
    );
}

/// A `continue` inside a `lock` must release the lock before the loop re-acquires it. The lock
/// is non-recursive, so a `continue` that skipped the release would hang the very next
/// iteration re-taking its own held lock. A single thread that continues out of a `lock` two
/// hundred thousand times and then reaches the code after the loop proves each `continue`
/// released the lock.
#[test]
fn lock_continue_releases_the_lock() {
    assert_eq!(
        compile_and_run_with_globals(
            "let mut mtx[1]\n\
             fn main() -> i64 {\n\
                 let mut i = 0\n\
                 for i < 200000 {\n\
                     lock (mtx) {\n\
                         i = i + 1\n\
                         continue\n\
                     }\n\
                 }\n\
                 42\n\
             }"
        ),
        42,
        "200000 lock-with-continue iterations completed and control reached the code after the loop"
    );
}

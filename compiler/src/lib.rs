//! `sondc`: the reference compiler for the Sond language.
//!
//! Built bottom-up with strict TDD (red → green → refactor): first the lexer
//! (tokenizer), then the parser (AST), then the contract verifier. Each layer
//! is driven by tests that fail before the implementation exists.

pub mod ast;
pub mod check;
pub mod codegen;
pub mod debug_agent;
pub mod debug_symtab;
pub mod decimal;
pub mod desugar;
pub mod eval;
pub mod header;
pub mod lexer;
pub mod parser;
pub mod ports;
pub mod deps;
pub mod sha256;
pub mod resolve;
pub mod testing;
pub mod token;
pub mod typeck;
pub mod verify;

use ast::{EnumDef, Function, Global, Signature, StructDef};
use std::collections::HashSet;

/// The shared compilation pipeline: checks and desugars run between parsing
/// and code generation. Called by both the `sondc` driver (`main.rs`) and the
/// test helpers (`codegen/testutil.rs`) so the two paths can never diverge.
///
/// `target` is `"host"`, `"freestanding"`, `"uefi"`, `"windows"`, `"shared"`,
/// `"object"`, `"pic16"`, or `"host-debug"` — the same target tag the driver
/// computes from CLI flags. The only target-gated check today is PIC16
/// (`check_pic16_constraints`), which fires when `target == "pic16"`.
///
/// Order matters: `lower_array_len` before the constant checks so `a.len`
/// looks like a constant; `lower_slice_ranges` after it but before struct
/// lowering; `inject_builtin_structs` before `lower_slice_ranges` so the
/// `Slice` struct exists when ranges are lowered; enum desugaring before
/// struct desugaring; contract lowering AFTER verification (the prover sees
/// source-level `s.len`, the codegen sees the lowered slot read).
pub fn lower_program(
    funcs: &mut [Function],
    globals: &mut [Global],
    structs: &mut Vec<StructDef>,
    enums: &[EnumDef],
    externs: &[Signature],
    target: &str,
) -> Result<(), String> {
    // ── checks (pre-desugar) ─────────────────────────────────────────────
    check::check_undeclared_variables(funcs, globals, enums)?;
    check::check_no_duplicate_functions(funcs)?;
    check::check_spawn_not_in_loop(funcs)?;
    check::check_call_arity(funcs)?;
    check::check_calls_resolve(funcs, externs, structs, enums)?;
    check::check_unreachable_statements(funcs)?;
    check::check_par_let_arity(funcs)?;
    check::check_par_confluence(funcs, globals)?;
    check::check_graph_acyclic(funcs)?;
    check::check_no_process_exit_in_spawn(funcs)?;
    check::check_no_close_fd_after_spawn(funcs)?;
    check::check_syscall_retry_has_an_exit(funcs)?;
    check::check_no_raw_access_to_declared_buffers(funcs, globals)?;
    check::check_receiver_kinds(funcs)?;
    check::check_declaration_kinds(funcs)?;
    check::check_struct_mutability(funcs, structs)?;
    // Exhaustiveness: every `match` must cover all of its enum's variants
    // (checked on the original `match`, before it is lowered to if/else).
    check::check_enum_matches(funcs, enums)?;

    // ── desugar (phase 1) ────────────────────────────────────────────────
    // The built-in `Slice { ptr, len }` fat pointer is available to every program.
    desugar::inject_builtin_structs(structs);
    // `a.len` on a fixed-size array lowers to its constant size. Done before
    // the constant checks so `a[a.len]` is seen as `a[N]` (a provable
    // out-of-bounds) and `for i < a.len` as `for i < N`.
    desugar::lower_array_len(funcs, globals);
    // A slice range `s[lo:hi]` lowers to a sub-`Slice` view. After `a.len`
    // (so an array's `.len` is a constant), before the constant checks and
    // struct lowering.
    desugar::lower_slice_ranges(funcs);

    // ── checks (post-first-desugar) ──────────────────────────────────────
    // (C) on-ramp for #3 (RFC-0012): a `+`/`-`/`*` of constants that overflows
    // i64 is a compile error, the non-breaking first step of checked-by-default
    // arithmetic.
    check::check_constant_overflow(funcs)?;
    // (C) on-ramp for #2 (RFC-0012): text has a type: a string literal binds to
    // `string`, never to a bare address.
    check::check_string_literal_types(funcs, globals)?;
    // PIC16-specific constraints: the backend stores each local/parameter in one
    // byte and only emits 8-bit immediates. Values that do not fit must be
    // rejected before codegen, never silently truncated.
    if target == "pic16" {
        check::check_pic16_constraints(funcs)?;
    }
    let string_globals: HashSet<String> = globals
        .iter()
        .filter(|g| matches!(g.init, ast::Expr::Str(_)))
        .map(|g| g.name.clone())
        .collect();
    check::check_module_string_indexing(funcs, &string_globals)?;
    check::check_constant_bounds(funcs, globals)?;
    // Reject inverted range loops: `for i in a..b` where a > b means the body
    // never executes.
    check::check_for_range_bounds(funcs)?;

    // ── desugar (phase 2) ────────────────────────────────────────────────
    // A payloaded enum lowers to a synthetic `{__tag, __payload}` struct,
    // append those to the struct set so `desugar_structs` lowers the enum's
    // field access and layout.
    let synth = desugar::desugar_enums(funcs, enums);
    structs.extend(synth);
    // Materialize struct literals used in expression position (call args,
    // branch tails) into temps, so a constructor works anywhere, before the
    // struct pass lowers them.
    desugar::hoist_struct_literals(funcs);
    desugar::desugar_structs(funcs, structs);

    Ok(())
}

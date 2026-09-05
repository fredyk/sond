//! Static struct-mutability checks: the value/pointer rules.
//!
//! A struct passed BY VALUE (`p: Point`) is immutable in a function that RECEIVED
//! it: only the function that CREATED it (where it is a local) may set its fields.
//! To share a struct for mutation you declare it a POINTER (`p: *Point`) up front;
//! Sond has no `&` address-of. So assigning to `p.field` is a COMPILE ERROR when
//! `p` is a value-struct parameter. This kills the Go footgun (silently mutating a
//! value you thought was shared, then debugging for hours) at compile time.
//!
//! Runs BEFORE struct desugaring, so it sees the original `p.field` form.

use crate::ast::{
    Arg, BinOp, Block, EnumDef, Expr, Function, Global, Param, Stmt, StructDef, Type, UnOp,
};

/// Exhaustiveness: every `match` on a sum type must cover ALL of its enum's variants,
/// use only variants that exist, and not repeat one. A missing case is a COMPILE ERROR:
/// the guarantee that makes sum types "illegal states unrepresentable" (the thing an
/// AI can no longer forget). Runs BEFORE enum desugaring, on the original `match`.
pub fn check_enum_matches(funcs: &[Function], enums: &[EnumDef]) -> Result<(), String> {
    for f in funcs {
        check_matches_block(&f.body, enums, &f.name)?;
    }
    Ok(())
}

fn check_matches_block(b: &Block, enums: &[EnumDef], fname: &str) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                check_matches_expr(value, enums, fname)?
            }
            Stmt::Assign { target, value } => {
                check_matches_expr(target, enums, fname)?;
                check_matches_expr(value, enums, fname)?;
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    check_matches_expr(c, enums, fname)?;
                }
                check_matches_block(body, enums, fname)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        check_matches_expr(t, enums, fname)?;
    }
    Ok(())
}

fn check_matches_expr(e: &Expr, enums: &[EnumDef], fname: &str) -> Result<(), String> {
    match e {
        Expr::Match { scrutinee, arms } => {
            check_matches_expr(scrutinee, enums, fname)?;
            // The set of variants named by the arms (detecting a duplicate).
            let mut seen: HashSet<&str> = HashSet::new();
            for a in arms {
                if !seen.insert(a.variant.as_str()) {
                    return Err(format!(
                        "function `{fname}`: duplicate match arm `{}`",
                        a.variant
                    ));
                }
                check_matches_block(&a.body, enums, fname)?;
            }
            // The enum these arms belong to: the one whose variants ⊇ the arms'.
            let owner = enums.iter().find(|en| {
                seen.iter()
                    .all(|v| en.variants.iter().any(|ev| ev.name == **v))
            });
            let Some(owner) = owner else {
                return Err(format!(
                    "function `{fname}`: match arms name no known enum's variants \
                     (or mix variants from different enums): {seen:?}"
                ));
            };
            let missing: Vec<&String> = owner
                .variants
                .iter()
                .map(|v| &v.name)
                .filter(|v| !seen.contains(v.as_str()))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "function `{fname}`: non-exhaustive match on `{}`: missing {missing:?}. \
                     Sond requires every variant to be handled.",
                    owner.name
                ));
            }
            Ok(())
        }
        Expr::Binary(_, l, r) => {
            check_matches_expr(l, enums, fname)?;
            check_matches_expr(r, enums, fname)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            check_matches_expr(x, enums, fname)
        }
        Expr::Index(a, b) => {
            check_matches_expr(a, enums, fname)?;
            check_matches_expr(b, enums, fname)
        }
        Expr::Call(c, args) => {
            check_matches_expr(c, enums, fname)?;
            for a in args {
                check_matches_expr(&a.value, enums, fname)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                check_matches_expr(it, enums, fname)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            check_matches_expr(cond, enums, fname)?;
            check_matches_block(then_branch, enums, fname)?;
            if let Some(eb) = else_branch {
                check_matches_block(eb, enums, fname)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
use std::collections::{HashMap, HashSet};

/// Rejects any mutation of a value-struct parameter's fields. Locals (structs
/// created in this function) and pointer parameters (`*Point`) stay mutable.
pub fn check_struct_mutability(funcs: &[Function], structs: &[StructDef]) -> Result<(), String> {
    let struct_names: HashSet<&str> = structs.iter().map(|s| s.name.as_str()).collect();
    for f in funcs {
        // Map each value-struct parameter to its struct type name (for the hint).
        let mut immutable: HashMap<&str, &str> = HashMap::new();
        for p in &f.params {
            // A VALUE struct param is `Point` (Named); a `*Point` (Ptr) is mutable.
            let Type::Named(tn) = &p.ty else { continue };
            if struct_names.contains(tn.as_str()) {
                immutable.insert(p.name.as_str(), tn.as_str());
            }
        }
        check_block(&f.body, &immutable, &f.name)?;
    }
    Ok(())
}

/// A struct type is value XOR pointer: all its methods must share one receiver
/// kind. `fn m(self)` is a value method, `fn m(self: *T)` is a pointer method;
/// mixing them in a type is rejected (Go allows this and only lints, Sond does
/// not). A method is any function whose first parameter is named `self`.
/// Every block nested inside an expression: the branches of an `if`, the arms of a `match`, and
/// the same again inside any sub-expression that can hold one.
///
/// `if` and `match` are EXPRESSIONS in Sond, so their bodies are blocks that a statement-level
/// walk never reaches on its own. A check that does not come through here quietly stops applying
/// the moment the author writes `if c { .. }` around the thing being guarded, and the clean
/// compile then reads as permission. That is worse than having no check at all, and it lands on
/// the shape every accept loop has: `for { let c = accept(); if c >= 0 { spawn handle(c) } }`.
fn nested_blocks<'a>(e: &'a Expr, out: &mut Vec<&'a Block>) {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            nested_blocks(cond, out);
            out.push(then_branch);
            if let Some(eb) = else_branch {
                out.push(eb);
            }
        }
        Expr::Match { scrutinee, arms } => {
            nested_blocks(scrutinee, out);
            for a in arms {
                out.push(&a.body);
            }
        }
        Expr::Field(b, _) | Expr::Unary(_, b) | Expr::Cast(b, _) => nested_blocks(b, out),
        Expr::Binary(_, a, b) | Expr::Index(a, b) => {
            nested_blocks(a, out);
            nested_blocks(b, out);
        }
        Expr::Call(callee, args) => {
            nested_blocks(callee, out);
            for a in args {
                nested_blocks(&a.value, out);
            }
        }
        Expr::List(xs) => {
            for x in xs {
                nested_blocks(x, out);
            }
        }
        Expr::SliceRange(b, lo, hi) => {
            nested_blocks(b, out);
            if let Some(l) = lo {
                nested_blocks(l, out);
            }
            if let Some(h) = hi {
                nested_blocks(h, out);
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                nested_blocks(v, out);
            }
        }
        _ => {}
    }
}

/// Every block nested inside a BLOCK: through its statements, and through its TAIL expression.
///
/// The tail is the block's value (`{ let r = ..\n if r > 0 { .. } }` parses the `if` as the tail,
/// not as a statement), so a walk over `stmts` alone silently skips whatever the author wrote
/// last, which for a loop body or a function body is very often the interesting part.
fn nested_blocks_of_block<'a>(b: &'a Block, out: &mut Vec<&'a Block>) {
    for s in &b.stmts {
        nested_blocks_of_stmt(s, out);
    }
    if let Some(t) = &b.tail {
        nested_blocks(t, out);
    }
}

/// Every block reachable from a statement's own expressions (its `for` body is separate: a caller
/// that tracks loop nesting needs to treat that one differently).
fn nested_blocks_of_stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a Block>) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Expr(value)
        | Stmt::Spawn(value)
        | Stmt::Return(Some(value)) => nested_blocks(value, out),
        Stmt::Assign { target, value } => {
            nested_blocks(target, out);
            nested_blocks(value, out);
        }
        Stmt::For {
            cond, invariant, ..
        } => {
            if let Some(c) = cond {
                nested_blocks(c, out);
            }
            for i in invariant {
                nested_blocks(i, out);
            }
        }
        Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
            for p in peers {
                nested_blocks(p, out);
            }
        }
        Stmt::GraphLet {
            nodes, yield_expr, ..
        } => {
            for (_, e) in nodes {
                nested_blocks(e, out);
            }
            nested_blocks(yield_expr, out);
        }
        _ => {}
    }
}

/// Rejects a retry loop that never looks at WHY its syscall failed.
///
/// The shape: a loop whose only way out is a counter that ONLY advances when the syscall
/// succeeded, with no `break` and no `return` anywhere in the body. A short read is real and
/// retrying it is right, but a syscall that FAILS returns immediately instead of blocking, so the
/// loop stops being a wait and becomes a spin at syscall speed. It is silent by nature: no error,
/// no hang, no crash, just a core at 100% and a program that never finishes what it was doing.
///
/// This family cost a full day of production across six separate instances in one week: two fill
/// loops over `getrandom` (`tls_random`, `pg_random`), and a futex retry that burned 743% of CPU
/// on machines at rest. Every one of them was written by someone who knew better; the shape is
/// just very easy to write and impossible to see. That is what makes it a compiler's job.
///
/// Deliberately narrow, so a diagnostic never fires on correct code: it needs a DIRECT `syscall`
/// binding in the body, every update to the loop counter guarded by a condition on that result,
/// and no `break` or `return` at all. Any of those absent means the author has an exit and this
/// says nothing.
pub fn check_syscall_retry_has_an_exit(funcs: &[Function]) -> Result<(), String> {
    // Does this expression name `v` anywhere?
    fn mentions(e: &Expr, v: &str) -> bool {
        match e {
            Expr::Var(n) => n == v,
            Expr::Field(b, _) | Expr::Unary(_, b) | Expr::Cast(b, _) => mentions(b, v),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => mentions(a, v) || mentions(b, v),
            Expr::Call(c, args) => mentions(c, v) || args.iter().any(|a| mentions(&a.value, v)),
            Expr::List(xs) => xs.iter().any(|x| mentions(x, v)),
            Expr::If { cond, .. } => mentions(cond, v),
            _ => false,
        }
    }
    fn is_syscall(e: &Expr) -> bool {
        matches!(e, Expr::Call(c, _) if matches!(c.as_ref(), Expr::Var(n) if n == "syscall"))
    }
    // Any `break` or `return` in the body (at any depth) means there is a way out that does not
    // depend on the counter, so this check stays quiet.
    fn has_an_exit(b: &Block) -> bool {
        let in_stmts = b.stmts.iter().any(|s| match s {
            Stmt::Break | Stmt::Return(_) => true,
            Stmt::For { body, .. } => has_an_exit(body),
            _ => {
                let mut inner = Vec::new();
                nested_blocks_of_stmt(s, &mut inner);
                inner.iter().any(|blk| has_an_exit(blk))
            }
        });
        if in_stmts {
            return true;
        }
        let mut inner = Vec::new();
        if let Some(t) = &b.tail {
            nested_blocks(t, &mut inner);
        }
        inner.iter().any(|blk| has_an_exit(blk))
    }
    // Every assignment to `counter` in this block, paired with whether it sits under a condition
    // that mentions the syscall result.
    fn assignments_to(b: &Block, counter: &str, result: &str, guarded: bool, out: &mut Vec<bool>) {
        for s in &b.stmts {
            if let Stmt::Assign { target, .. } = s
                && let Expr::Var(n) = target
                && n == counter
            {
                out.push(guarded);
            }
            if let Stmt::Expr(Expr::If {
                cond,
                then_branch,
                else_branch,
            }) = s
            {
                let g = guarded || mentions(cond, result);
                assignments_to(then_branch, counter, result, g, out);
                if let Some(eb) = else_branch {
                    assignments_to(eb, counter, result, g, out);
                }
                continue;
            }
            let mut inner = Vec::new();
            nested_blocks_of_stmt(s, &mut inner);
            for blk in inner {
                assignments_to(blk, counter, result, guarded, out);
            }
        }
        // The tail: `for got < n { let r = ..\n if r > 0 { got = got + r } }` puts the guarded
        // update HERE, not in stmts, which is exactly the shape being looked for.
        if let Some(t) = &b.tail
            && let Expr::If {
                cond,
                then_branch,
                else_branch,
            } = t.as_ref()
        {
            let g = guarded || mentions(cond, result);
            assignments_to(then_branch, counter, result, g, out);
            if let Some(eb) = else_branch {
                assignments_to(eb, counter, result, g, out);
            }
        }
    }
    fn walk(stmts: &[Stmt], fname: &str) -> Result<(), String> {
        for s in stmts {
            if let Stmt::For {
                cond: Some(cond),
                body,
                ..
            } = s
            {
                // The loop's own exit variable, and the name bound to a direct syscall result.
                let mut result: Option<&str> = None;
                for bs in &body.stmts {
                    if let Stmt::Let { name, value, .. } = bs
                        && is_syscall(value)
                    {
                        result = Some(name);
                    }
                }
                if let Some(result) = result
                    && !has_an_exit(body)
                {
                    // Which variable does the loop condition turn on? Take every name in it.
                    let mut counters = Vec::new();
                    collect_expr_vars_local(cond, &mut counters);
                    for c in counters {
                        let mut updates = Vec::new();
                        assignments_to(body, &c, result, false, &mut updates);
                        if !updates.is_empty() && updates.iter().all(|g| *g) {
                            return Err(format!(
                                "function `{fname}`: this loop retries `syscall` forever without \
                                 looking at WHY it failed. `{c}` only advances when `{result}` \
                                 succeeded, and there is no `break` or `return`, so a syscall that \
                                 FAILS (rather than returning short) spins here at syscall speed: \
                                 no error, no hang, just a core at 100%. Branch on the failure: \
                                 retry only what is transient (`-EINTR` is -4) and return the rest."
                            ));
                        }
                    }
                }
            }
            if let Stmt::For { body, .. } = s {
                walk(&body.stmts, fname)?;
            }
            let mut inner = Vec::new();
            nested_blocks_of_stmt(s, &mut inner);
            for b in inner {
                walk(&b.stmts, fname)?;
            }
        }
        Ok(())
    }
    for f in funcs {
        let mut blocks = vec![&f.body];
        let mut seen = 0;
        while seen < blocks.len() {
            let b = blocks[seen];
            seen += 1;
            walk(&b.stmts, &f.name)?;
            let mut inner = Vec::new();
            nested_blocks_of_block(b, &mut inner);
            blocks.extend(inner);
        }
    }
    Ok(())
}

/// Every variable named in an expression (local helper: the module's other collector walks
/// statements, this one only needs the names inside a loop condition).
fn collect_expr_vars_local(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Var(n) => out.push(n.clone()),
        Expr::Field(b, _) | Expr::Unary(_, b) | Expr::Cast(b, _) => collect_expr_vars_local(b, out),
        Expr::Binary(_, a, b) | Expr::Index(a, b) => {
            collect_expr_vars_local(a, out);
            collect_expr_vars_local(b, out);
        }
        Expr::Call(c, args) => {
            collect_expr_vars_local(c, out);
            for a in args {
                collect_expr_vars_local(&a.value, out);
            }
        }
        _ => {}
    }
}

/// Was: rejects a `spawn` inside a loop. A looping site now owns a RING of join slots instead of
/// one cell, rotating through them and joining the slot it is about to reuse, so no thread is
/// left unjoined and the loop has a ceiling by construction. Kept as a no-op rather than deleted
/// so the call site, and this note, stay where anybody looking for the old error will find them.
pub fn check_spawn_not_in_loop(funcs: &[Function]) -> Result<(), String> {
    let _ = funcs;
    Ok(())
}

/// The source symbol of a binary operator, for diagnostics.
fn op_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        _ => "?",
    }
}

/// Rejects a `par`-let whose name count does not match its peer count (`let a ~~ b = par f() ~~
/// g() ~~ h()` binds three results to two names), and a `par(op)` reduce whose operator is not a
/// commutative-associative monoid. Both are source errors, caught here instead of miscompiling.
pub fn check_par_let_arity(funcs: &[Function]) -> Result<(), String> {
    fn walk(stmts: &[Stmt], fname: &str) -> Result<(), String> {
        for s in stmts {
            match s {
                Stmt::ParLet { names, peers, .. } if names.len() != peers.len() => {
                    return Err(format!(
                        "function `{fname}`: this `par` binding has {} name(s) but {} peer(s); \
                         each name binds exactly one peer's result, so the counts must match.",
                        names.len(),
                        peers.len()
                    ));
                }
                Stmt::ParReduce { op, .. }
                    if !matches!(
                        op,
                        BinOp::Add | BinOp::Mul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
                    ) =>
                {
                    return Err(format!(
                        "function `{fname}`: a `par(op)` reducer must be COMMUTATIVE and \
                         associative (`+ * & | ^`), because the peers finish in an unspecified \
                         order and the result must not depend on it. `{}` is not; use a positional \
                         binding `let a ~~ b = par ..` if you need order.",
                        op_symbol(op)
                    ));
                }
                Stmt::For { body, .. } => walk(&body.stmts, fname)?,
                _ => {}
            }
        }
        Ok(())
    }
    for f in funcs {
        walk(&f.body.stmts, &f.name)?;
    }
    Ok(())
}

/// Per-function effect on the program's MUTABLE globals: the set it reads and the set it writes,
/// transitively across its call graph. This is the footprint that decides whether two `par` peers
/// (or two `graph` nodes) can run concurrently: disjoint footprints are confluent (RFC-0008 §5.3).
pub(crate) struct GlobalEffects {
    reads: HashMap<String, HashSet<String>>,
    writes: HashMap<String, HashSet<String>>,
}

/// The set of MUTABLE globals declared by a program, the shared state a race could occur on.
pub(crate) fn mutable_globals(globals: &[Global]) -> HashSet<String> {
    globals
        .iter()
        .filter(|g| g.mutable)
        .map(|g| g.name.clone())
        .collect()
}

/// The (reads, writes) footprint over the mutable globals of one EXPRESSION, transitively: its own
/// direct reads/writes plus every global touched by a function it calls. Used to decide whether two
/// `graph` nodes conflict (and so need a serializing effect edge, RFC-0008 §5.2).
pub(crate) fn expr_footprint(
    e: &Expr,
    eff: &GlobalEffects,
    mut_globals: &HashSet<String>,
) -> (HashSet<String>, HashSet<String>) {
    let mut reads = HashSet::new();
    let mut writes = HashSet::new();
    let mut callees = HashSet::new();
    expr_effects(e, mut_globals, &mut reads, &mut writes, &mut callees);
    for c in &callees {
        if let Some(r) = eff.reads.get(c) {
            reads.extend(r.iter().cloned());
        }
        if let Some(w) = eff.writes.get(c) {
            writes.extend(w.iter().cloned());
        }
    }
    (reads, writes)
}

/// Whether two footprints CONFLICT: a write on one side meets a read or write of the same global on
/// the other. Read-read never conflicts.
pub(crate) fn footprints_conflict(
    a: &(HashSet<String>, HashSet<String>),
    b: &(HashSet<String>, HashSet<String>),
) -> bool {
    let (ra, wa) = a;
    let (rb, wb) = b;
    wa.iter().any(|g| wb.contains(g) || rb.contains(g)) || wb.iter().any(|g| ra.contains(g))
}

/// The root variable an assignment writes: `G` for `G = v`, `G[i] = v`, `G.f = v`, `G[i].f = v`.
/// A write through it is a write to `G`.
fn lvalue_root(e: &Expr) -> Option<&str> {
    match e {
        Expr::Var(n) => Some(n),
        Expr::Index(b, _) | Expr::Field(b, _) => lvalue_root(b),
        _ => None,
    }
}

/// Collects, from one expression, the mutable globals it READS and WRITES (writes reach here only
/// through statements nested in an `if`/`match` branch) and the functions it CALLS. Walks every
/// sub-expression AND the tail of every nested block: missing one would under-report an effect and
/// could admit a real race, so this is exhaustive over `Expr` by construction (no catch-all).
fn expr_effects(
    e: &Expr,
    mut_globals: &HashSet<String>,
    reads: &mut HashSet<String>,
    writes: &mut HashSet<String>,
    callees: &mut HashSet<String>,
) {
    match e {
        Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) => {}
        Expr::Var(name) => {
            if mut_globals.contains(name) {
                reads.insert(name.clone());
            }
        }
        Expr::Field(b, _) => expr_effects(b, mut_globals, reads, writes, callees),
        Expr::Call(callee, args) => {
            if let Expr::Var(fname) = callee.as_ref() {
                callees.insert(fname.clone());
            } else {
                expr_effects(callee, mut_globals, reads, writes, callees);
            }
            for a in args {
                expr_effects(&a.value, mut_globals, reads, writes, callees);
            }
        }
        Expr::List(items) => {
            for it in items {
                expr_effects(it, mut_globals, reads, writes, callees);
            }
        }
        Expr::Index(b, i) => {
            expr_effects(b, mut_globals, reads, writes, callees);
            expr_effects(i, mut_globals, reads, writes, callees);
        }
        Expr::SliceRange(b, lo, hi) => {
            expr_effects(b, mut_globals, reads, writes, callees);
            if let Some(l) = lo {
                expr_effects(l, mut_globals, reads, writes, callees);
            }
            if let Some(h) = hi {
                expr_effects(h, mut_globals, reads, writes, callees);
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                expr_effects(v, mut_globals, reads, writes, callees);
            }
        }
        Expr::Unary(_, x) | Expr::Cast(x, _) => {
            expr_effects(x, mut_globals, reads, writes, callees)
        }
        Expr::Binary(_, l, r) => {
            expr_effects(l, mut_globals, reads, writes, callees);
            expr_effects(r, mut_globals, reads, writes, callees);
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_effects(cond, mut_globals, reads, writes, callees);
            block_effects(then_branch, mut_globals, reads, writes, callees);
            if let Some(eb) = else_branch {
                block_effects(eb, mut_globals, reads, writes, callees);
            }
        }
        Expr::Match { scrutinee, arms } => {
            expr_effects(scrutinee, mut_globals, reads, writes, callees);
            for arm in arms {
                block_effects(&arm.body, mut_globals, reads, writes, callees);
            }
        }
    }
}

/// Collects reads/writes/callees over a block: its statements AND its tail expression (the block's
/// value, which is NOT in `stmts` and would otherwise be missed).
fn block_effects(
    block: &Block,
    mut_globals: &HashSet<String>,
    reads: &mut HashSet<String>,
    writes: &mut HashSet<String>,
    callees: &mut HashSet<String>,
) {
    stmts_effects(&block.stmts, mut_globals, reads, writes, callees);
    if let Some(t) = &block.tail {
        expr_effects(t, mut_globals, reads, writes, callees);
    }
}

/// Collects reads, writes and callees over a statement list.
fn stmts_effects(
    stmts: &[Stmt],
    mut_globals: &HashSet<String>,
    reads: &mut HashSet<String>,
    writes: &mut HashSet<String>,
    callees: &mut HashSet<String>,
) {
    for s in stmts {
        match s {
            Stmt::Let { value, .. } => expr_effects(value, mut_globals, reads, writes, callees),
            Stmt::Assign { target, value } => {
                // A write to a global (by name, or through its array/field) is a write to its root.
                if let Some(root) = lvalue_root(target)
                    && mut_globals.contains(root)
                {
                    writes.insert(root.to_string());
                }
                expr_effects(target, mut_globals, reads, writes, callees);
                expr_effects(value, mut_globals, reads, writes, callees);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => {
                expr_effects(e, mut_globals, reads, writes, callees)
            }
            Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                for p in peers {
                    expr_effects(p, mut_globals, reads, writes, callees);
                }
            }
            Stmt::GraphLet {
                nodes, yield_expr, ..
            } => {
                for (_, e) in nodes {
                    expr_effects(e, mut_globals, reads, writes, callees);
                }
                expr_effects(yield_expr, mut_globals, reads, writes, callees);
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    expr_effects(c, mut_globals, reads, writes, callees);
                }
                block_effects(body, mut_globals, reads, writes, callees);
            }
            Stmt::Lock { name, body } => {
                // `lock` writes the mutex word (CAS + futex ops). If the mutex is a mutable
                // global, record that write so `par` confluence checking sees it.
                if mut_globals.contains(name.as_str()) {
                    writes.insert(name.clone());
                }
                block_effects(body, mut_globals, reads, writes, callees);
            }
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
}

/// Computes each function's transitive read/write footprint over the mutable globals: the direct
/// sets unioned with every callee's, to a fixpoint (so recursion and deep call graphs converge).
pub(crate) fn compute_global_effects(
    funcs: &[Function],
    mut_globals: &HashSet<String>,
) -> GlobalEffects {
    let mut reads: HashMap<String, HashSet<String>> = HashMap::new();
    let mut writes: HashMap<String, HashSet<String>> = HashMap::new();
    let mut callees: HashMap<String, HashSet<String>> = HashMap::new();
    for f in funcs {
        let mut r = HashSet::new();
        let mut w = HashSet::new();
        let mut c = HashSet::new();
        block_effects(&f.body, mut_globals, &mut r, &mut w, &mut c);
        reads.insert(f.name.clone(), r);
        writes.insert(f.name.clone(), w);
        callees.insert(f.name.clone(), c);
    }
    loop {
        let mut changed = false;
        for f in funcs {
            let cs = callees[&f.name].clone();
            for callee in &cs {
                if let Some(cr) = reads.get(callee).cloned() {
                    let r = reads.get_mut(&f.name).unwrap();
                    for g in cr {
                        changed |= r.insert(g);
                    }
                }
                if let Some(cw) = writes.get(callee).cloned() {
                    let w = writes.get_mut(&f.name).unwrap();
                    for g in cw {
                        changed |= w.insert(g);
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    GlobalEffects { reads, writes }
}

/// Rejects a `par` whose peers CONFLICT on a mutable global: two peers writing the same global, or
/// one writing while another reads it, would race, and the result would depend on the schedule. A
/// `~~` is a claim that its peers are independent (RFC-0008 §5.3); this proves the claim for the
/// mutable-global dimension. Pure peers, and peers touching DISJOINT globals, are admitted.
///
/// LIMIT (tracked): this analyzes coupling through named mutable globals only. Coupling through raw
/// pointers/heap (`poke64` into shared memory) is not yet tracked; the capability/effect system
/// (RFC-0011) is what will close that. Until then a `~~` is a verified claim w.r.t. globals, and an
/// unchecked one w.r.t. pointer aliasing.
pub fn check_par_confluence(funcs: &[Function], globals: &[Global]) -> Result<(), String> {
    let mut_globals: HashSet<String> = globals
        .iter()
        .filter(|g| g.mutable)
        .map(|g| g.name.clone())
        .collect();
    if mut_globals.is_empty() {
        return Ok(()); // no shared mutable state to race on
    }
    let eff = compute_global_effects(funcs, &mut_globals);
    // The (reads, writes) footprint of one peer, an owned clone (peers are few, sets small), so
    // there are no lifetime knots. A peer that is not a direct call has an empty footprint.
    fn peer_footprint(peer: &Expr, eff: &GlobalEffects) -> (HashSet<String>, HashSet<String>) {
        if let Expr::Call(callee, _) = peer
            && let Expr::Var(fname) = callee.as_ref()
        {
            return (
                eff.reads.get(fname).cloned().unwrap_or_default(),
                eff.writes.get(fname).cloned().unwrap_or_default(),
            );
        }
        (HashSet::new(), HashSet::new())
    }
    // Checks one `par`'s peers pairwise for a conflict on a shared mutable global.
    fn check_peers(peers: &[Expr], fname: &str, eff: &GlobalEffects) -> Result<(), String> {
        let fps: Vec<(HashSet<String>, HashSet<String>)> =
            peers.iter().map(|p| peer_footprint(p, eff)).collect();
        for i in 0..fps.len() {
            for j in (i + 1)..fps.len() {
                let (ri, wi) = &fps[i];
                let (rj, wj) = &fps[j];
                // Conflict = a write on one side meets a read or write of the same global on the
                // other. Read-read never conflicts.
                let clash = wi
                    .intersection(wj)
                    .chain(wi.intersection(rj))
                    .chain(wj.intersection(ri))
                    .next();
                if let Some(g) = clash {
                    return Err(format!(
                        "function `{fname}`: two `par` peers race on the mutable global `{g}` \
                         (one writes it while the other reads or writes it), so the result would \
                         depend on which thread runs first. `~~` requires the peers to be \
                         independent; give each its own state, or serialize them."
                    ));
                }
            }
        }
        Ok(())
    }
    // A `par` can appear anywhere a statement can, including inside an `if`/`match` branch (which
    // is an expression), so the walk descends through expressions into every nested block, not
    // just the top level and `for` bodies.
    fn walk_expr(e: &Expr, fname: &str, eff: &GlobalEffects) -> Result<(), String> {
        match e {
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                walk_expr(cond, fname, eff)?;
                walk_block(then_branch, fname, eff)?;
                if let Some(eb) = else_branch {
                    walk_block(eb, fname, eff)?;
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, fname, eff)?;
                for arm in arms {
                    walk_block(&arm.body, fname, eff)?;
                }
            }
            Expr::Call(c, args) => {
                walk_expr(c, fname, eff)?;
                for a in args {
                    walk_expr(&a.value, fname, eff)?;
                }
            }
            Expr::Binary(_, l, r) => {
                walk_expr(l, fname, eff)?;
                walk_expr(r, fname, eff)?;
            }
            Expr::Index(b, i) => {
                walk_expr(b, fname, eff)?;
                walk_expr(i, fname, eff)?;
            }
            Expr::Unary(_, x) | Expr::Cast(x, _) | Expr::Field(x, _) => walk_expr(x, fname, eff)?,
            Expr::List(items) => {
                for it in items {
                    walk_expr(it, fname, eff)?;
                }
            }
            Expr::SliceRange(b, lo, hi) => {
                walk_expr(b, fname, eff)?;
                if let Some(l) = lo {
                    walk_expr(l, fname, eff)?;
                }
                if let Some(h) = hi {
                    walk_expr(h, fname, eff)?;
                }
            }
            Expr::StructLit(_, fields) => {
                for (_, v) in fields {
                    walk_expr(v, fname, eff)?;
                }
            }
            Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) => {}
        }
        Ok(())
    }
    fn walk_block(block: &Block, fname: &str, eff: &GlobalEffects) -> Result<(), String> {
        for s in &block.stmts {
            match s {
                Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                    check_peers(peers, fname, eff)?;
                    for p in peers {
                        walk_expr(p, fname, eff)?;
                    }
                }
                Stmt::GraphLet {
                    nodes, yield_expr, ..
                } => {
                    for (_, e) in nodes {
                        walk_expr(e, fname, eff)?;
                    }
                    walk_expr(yield_expr, fname, eff)?;
                }
                Stmt::Let { value, .. } => walk_expr(value, fname, eff)?,
                Stmt::Assign { target, value } => {
                    walk_expr(target, fname, eff)?;
                    walk_expr(value, fname, eff)?;
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => walk_expr(e, fname, eff)?,
                Stmt::For { cond, body, .. } => {
                    if let Some(c) = cond {
                        walk_expr(c, fname, eff)?;
                    }
                    walk_block(body, fname, eff)?;
                }
                Stmt::Lock { body, .. } => {
                    walk_block(body, fname, eff)?;
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        if let Some(t) = &block.tail {
            walk_expr(t, fname, eff)?;
        }
        Ok(())
    }
    for f in funcs {
        walk_block(&f.body, &f.name, &eff)?;
    }
    Ok(())
}

/// Rejects a `graph { .. }` block whose nodes form a dependency CYCLE (`node a = b  node b = a`):
/// there is no order in which to evaluate them, so it cannot be scheduled (RFC-0008 §10 forbids
/// pure data cycles in v1). A DFS with an in-progress mark finds a back edge.
/// Rejects a call whose argument count does not match the callee's declaration.
///
/// Both directions compiled in silence. Too MANY arguments: the extras were evaluated and
/// dropped, so a test written against a wider signature before widening it -- the ordinary
/// red-first step -- came out green, and TDD stopped being able to fail. Too FEW: the callee
/// reads a register nobody set, and its value is whatever the last call left there, which is
/// worse for being repeatable enough to look deliberate.
///
/// Only calls to DECLARED functions are checked. A name that is not one is a builtin, and those
/// have their own rules (`syscall` takes what it is given).
pub fn check_call_arity(funcs: &[Function]) -> Result<(), String> {
    let arity: HashMap<&str, usize> = funcs
        .iter()
        .map(|f| (f.name.as_str(), f.params.len()))
        .collect();

    fn walk_expr(e: &Expr, arity: &HashMap<&str, usize>, fname: &str) -> Result<(), String> {
        if let Expr::Call(callee, args) = e
            && let Expr::Var(name) = callee.as_ref()
            && let Some(&want) = arity.get(name.as_str())
            && args.len() != want
        {
            let got = args.len();
            return Err(format!(
                "function `{fname}`: `{name}` takes {want} argument(s), but this call passes \
                 {got}"
            ));
        }
        match e {
            Expr::Binary(_, l, r) | Expr::Index(l, r) => {
                walk_expr(l, arity, fname)?;
                walk_expr(r, arity, fname)
            }
            Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => walk_expr(x, arity, fname),
            Expr::Call(c, args) => {
                walk_expr(c, arity, fname)?;
                for a in args {
                    walk_expr(&a.value, arity, fname)?;
                }
                Ok(())
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                walk_expr(cond, arity, fname)?;
                walk_block(then_branch, arity, fname)?;
                match else_branch {
                    Some(b) => walk_block(b, arity, fname),
                    None => Ok(()),
                }
            }
            Expr::Match { scrutinee, arms } => {
                walk_expr(scrutinee, arity, fname)?;
                for a in arms {
                    walk_block(&a.body, arity, fname)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn walk_block(b: &Block, arity: &HashMap<&str, usize>, fname: &str) -> Result<(), String> {
        for s in &b.stmts {
            match s {
                Stmt::Let { value, .. } => walk_expr(value, arity, fname)?,
                Stmt::Assign { target, value } => {
                    walk_expr(target, arity, fname)?;
                    walk_expr(value, arity, fname)?;
                }
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => {
                    walk_expr(e, arity, fname)?
                }
                Stmt::For { cond, body, .. } => {
                    if let Some(c) = cond {
                        walk_expr(c, arity, fname)?;
                    }
                    walk_block(body, arity, fname)?;
                }
                Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                    for p in peers {
                        walk_expr(p, arity, fname)?;
                    }
                }
                _ => {}
            }
        }
        match &b.tail {
            Some(t) => walk_expr(t, arity, fname),
            None => Ok(()),
        }
    }

    for f in funcs {
        walk_block(&f.body, &arity, &f.name)?;
    }
    Ok(())
}

pub fn check_graph_acyclic(funcs: &[Function]) -> Result<(), String> {
    fn check_nodes(nodes: &[(String, Expr)], fname: &str) -> Result<(), String> {
        let idx: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.as_str(), i))
            .collect();
        let deps: Vec<Vec<usize>> = nodes
            .iter()
            .map(|(_, e)| {
                let mut r = HashSet::new();
                crate::codegen::collect_node_refs(e, &idx, &mut r);
                r.into_iter().collect()
            })
            .collect();
        // 0 = unvisited, 1 = on the current DFS path, 2 = fully explored.
        let mut state = vec![0u8; nodes.len()];
        // Explicit stack DFS carrying whether we are entering (false) or leaving (true) a node.
        for root in 0..nodes.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, false)];
            while let Some((n, leaving)) = stack.pop() {
                if leaving {
                    state[n] = 2;
                    continue;
                }
                if state[n] == 2 {
                    continue;
                }
                state[n] = 1;
                stack.push((n, true));
                for &d in &deps[n] {
                    match state[d] {
                        1 => {
                            return Err(format!(
                                "function `{fname}`: the `graph` nodes `{}` and `{}` depend on each \
                                 other (a cycle), so there is no order to evaluate them in. Break \
                                 the cycle; a `graph` is a DAG.",
                                nodes[n].0, nodes[d].0
                            ));
                        }
                        0 => stack.push((d, false)),
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
    fn walk(stmts: &[Stmt], fname: &str) -> Result<(), String> {
        for s in stmts {
            match s {
                Stmt::GraphLet { nodes, .. } => check_nodes(nodes, fname)?,
                Stmt::For { body, .. } => walk(&body.stmts, fname)?,
                _ => {}
            }
        }
        Ok(())
    }
    for f in funcs {
        walk(&f.body.stmts, &f.name)?;
    }
    Ok(())
}

/// Rejects two top-level functions with the same name (after include flattening and method
/// mangling, so methods `Type_m` never false-match). Two same-named free functions become two
/// identical assembler labels: a `symbol already defined` link error whose cause is opaque. This
/// catches it early and names the fix. Colliding PRIVATE helpers across files are already
/// file-scoped away by the resolver, so a collision that reaches here is between PUBLIC functions
/// (or the entry file), a genuine name clash the author must resolve.
pub fn check_no_duplicate_functions(funcs: &[Function]) -> Result<(), String> {
    let mut seen: HashSet<&str> = HashSet::new();
    for f in funcs {
        if !seen.insert(f.name.as_str()) {
            return Err(format!(
                "function `{}` is defined more than once (two `pub fn` of the same name across \
                 files, or a clash with the entry file). Rename one so the program has a single \
                 function of that name.",
                f.name
            ));
        }
    }
    Ok(())
}

/// The `__cov_` prefix is RESERVED for the compiler's coverage bookkeeping: `sond test
/// --coverage` publishes `__cov_total`, `__cov_ptr`, `__cov_nfns` and `__cov_fn_table` as
/// `.data` symbols on its own, and its synthesized harness reads and writes them. Author
/// code may neither USE nor BIND an identifier with that prefix — any such name would
/// either reach into compiler internals as if it were an ordinary variable, or collide
/// with an emitted symbol as an opaque link error. This is the check that makes the
/// reservation real rather than a promise in a comment.
const COV_RESERVED_PREFIX: &str = "__cov_";

/// Rejects one identifier for wearing the reserved `__cov_` prefix. `kind` names what is
/// being declared (variable, parameter, global, function, …) so the error says what to
/// rename, not just that something is wrong.
fn reject_cov_prefix(kind: &str, name: &str) -> Result<(), String> {
    if name.starts_with(COV_RESERVED_PREFIX) {
        Err(format!(
            "cannot declare {kind} `{name}`: the `__cov_` prefix is reserved for the coverage \
             bookkeeping the compiler publishes itself (`__cov_total`, `__cov_ptr`, \
             `__cov_nfns`, `__cov_fn_table`, for `sond test --coverage`). Rename it without \
             the `__cov_` prefix."
        ))
    } else {
        Ok(())
    }
}

/// Rejects function-body references to variables that are never declared and do not exist.
/// A variable used in any sub-expression must have been declared at some enclosing scope
/// (parameter, `let`, or module-level global). The check is intentionally simple and
/// catches the common case of using a name that the author forgot to define. Function-call
/// targets are skipped so that `f(x)` does not reject a legitimate `f` (a function) as
/// an undeclared variable. Function names used as values (e.g. `addr_of(f)`) are also
/// excluded because a function name in Sond is a first-class identifier, not a variable.
/// Enum variant constructors are excluded for the same reason.
pub fn check_undeclared_variables(
    funcs: &[Function],
    globals: &[Global],
    enums: &[EnumDef],
) -> Result<(), String> {
    // The `__cov_` reservation is program-wide, so it is imposed here on every program-level
    // name the author controls: globals (a `let __cov_ptr` at module scope would collide with
    // the `.data` symbol the coverage build emits), function names (an assembler label equal
    // to a coverage symbol), and enum names and variant constructors (identifiers like any
    // other).
    for g in globals {
        reject_cov_prefix("global", &g.name)?;
    }
    for f in funcs {
        reject_cov_prefix("function", &f.name)?;
    }
    for e in enums {
        reject_cov_prefix("enum", &e.name)?;
        for v in &e.variants {
            reject_cov_prefix("enum variant", &v.name)?;
        }
    }

    // Variables the COVERAGE code generator publishes on its own, with no `let` in the
    // source: `sond test --coverage` synthesizes a harness `main` (as ordinary Sond source,
    // which goes through this very check) that reads `__cov_total`/`__cov_nfns` and writes
    // `__cov_ptr`, and indexes the `__cov_fn_table` array. They are emitted as `.data`
    // symbols by emit_program_coverage_g, so they are not declared anywhere the check can
    // see. They are predeclared ONLY for the synthesized harness (marked `is_harness`):
    // everywhere else the names are refused, by the prefix rejection on binds above and by
    // the reserved-prefix error on unresolved uses below. Predeclaring them in every
    // function of every program is what let a user program read `__cov_ptr` in silence.
    let cov_reserved: [&str; 4] = ["__cov_total", "__cov_ptr", "__cov_nfns", "__cov_fn_table"];
    // Collect all enum variant names and function names — they are identifiers, not variables.
    let mut known_names: HashSet<String> = HashSet::new();
    for e in enums {
        for v in &e.variants {
            known_names.insert(v.name.clone());
        }
        known_names.insert(e.name.clone());
    }
    for f in funcs {
        known_names.insert(f.name.clone());
    }

    fn check_func(
        func: &Function,
        globals: &[Global],
        known_names: &HashSet<String>,
        cov_reserved: &[&str; 4],
    ) -> Result<(), String> {
        let mut scopes: ScopeStack = Vec::new();
        let mut top: HashSet<String> = HashSet::new();
        for g in globals {
            top.insert(g.name.clone());
        }
        if func.is_harness {
            for name in cov_reserved {
                top.insert(name.to_string());
            }
        }
        for p in &func.params {
            reject_cov_prefix("parameter", &p.name)?;
            top.insert(p.name.clone());
        }
        for n in known_names {
            top.insert(n.clone());
        }
        scopes.push(top);
        check_undeclared_block(&func.body, &mut scopes, &func.name, 0)?;
        // `requires` lives at the top of the function, so parameters (and
        // globals) are in scope.  The caller of the function is what
        // guarantees the preconditions, so the expressions are still valid
        // Sond and need the normal undeclared-variable check.
        for e in &func.requires {
            check_undeclared_expr(e, &mut scopes, &func.name, 0)?;
        }
        // `ensures` also sees all parameters; and if the function has a
        // named return value (`-> (r: T)`) then `r` is bound in the scope
        // by a `let` placed by the compiler before this check runs (the
        // desugar pass inserts it).  Until desugaring the name may be
        // absent from our scope stack, so we add the named return
        // explicitly when present.
        if let Some(ref ret_name) = func.ret_name {
            reject_cov_prefix("return value", ret_name)?;
            scopes.last_mut().unwrap().insert(ret_name.clone());
        }
        for e in &func.ensures {
            check_undeclared_expr(e, &mut scopes, &func.name, 0)?;
        }
        Ok(())
    }
    for f in funcs {
        check_func(f, globals, &known_names, &cov_reserved)?;
    }
    Ok(())
}

type ScopeStack = Vec<HashSet<String>>;

fn check_undeclared_block(
    b: &Block,
    scopes: &mut ScopeStack,
    fname: &str,
    line: u32,
) -> Result<(), String> {
    for (i, s) in b.stmts.iter().enumerate() {
        let stmt_line = b.stmt_lines.get(i).copied().unwrap_or(line);
        check_undeclared_stmt(s, scopes, fname, stmt_line)?;
    }
    if let Some(tail) = &b.tail {
        let tail_line = b.stmt_lines.last().copied().unwrap_or(line);
        check_undeclared_expr(tail, scopes, fname, tail_line)?;
    }
    Ok(())
}

fn check_undeclared_stmt(
    s: &Stmt,
    scopes: &mut ScopeStack,
    fname: &str,
    line: u32,
) -> Result<(), String> {
    match s {
        Stmt::Let {
            mutable: _,
            name,
            ty: _,
            value,
            array_len: _,
        } => {
            check_undeclared_expr(value, scopes, fname, line)?;
            reject_cov_prefix("variable", name)?;
            scopes.last_mut().unwrap().insert(name.clone());
        }
        Stmt::Assign { target, value } => {
            check_undeclared_expr(target, scopes, fname, line)?;
            check_undeclared_expr(value, scopes, fname, line)?;
        }
        Stmt::Return(Some(e)) => {
            check_undeclared_expr(e, scopes, fname, line)?;
        }
        Stmt::Return(None) => {}
        Stmt::Expr(expr) | Stmt::Spawn(expr) => {
            check_undeclared_expr(expr, scopes, fname, line)?;
        }
        Stmt::Par(peers) => {
            for p in peers {
                check_undeclared_expr(p, scopes, fname, line)?;
            }
        }
        Stmt::ParLet {
            mutable: _,
            names,
            peers,
        } => {
            for p in peers {
                check_undeclared_expr(p, scopes, fname, line)?;
            }
            for n in names {
                reject_cov_prefix("variable", n)?;
                scopes.last_mut().unwrap().insert(n.clone());
            }
        }
        Stmt::ParReduce {
            mutable: _,
            name,
            op: _,
            peers,
        } => {
            for p in peers {
                check_undeclared_expr(p, scopes, fname, line)?;
            }
            reject_cov_prefix("variable", name)?;
            scopes.last_mut().unwrap().insert(name.clone());
        }
        Stmt::GraphLet {
            mutable: _,
            name,
            nodes,
            yield_expr,
        } => {
            // Every node name is declared BEFORE any node body is walked: the source order of
            // `node` lines carries no meaning (RFC-0008 5.1 — the block is evaluated in dependency
            // order, and reordering the lines changes nothing), so `node z = add(fx, fy)` written
            // above `node fx = ..` is a normal graph, not a use of an undeclared name.
            for (n, _) in nodes.iter() {
                reject_cov_prefix("graph node", n)?;
                scopes.last_mut().unwrap().insert(n.clone());
            }
            for (_, e) in nodes {
                check_undeclared_expr(e, scopes, fname, line)?;
            }
            check_undeclared_expr(yield_expr, scopes, fname, line)?;
            reject_cov_prefix("variable", name)?;
            scopes.last_mut().unwrap().insert(name.clone());
        }
        Stmt::For {
            cond,
            invariant,
            body,
            is_range_for: _,
        } => {
            if let Some(c) = cond {
                check_undeclared_expr(c, scopes, fname, line)?;
            }
            // The invariant is checked in the scope OUTSIDE the body, which is where it is
            // evaluated: it fires at the loop boundary, before the body has declared anything.
            // Without this walk a typo like `indx` for `idx` compiled in silence and the loop
            // was left unverified -- with the programmer believing the opposite, which is the
            // one thing this language exists to prevent.
            for inv in invariant {
                check_undeclared_expr(inv, scopes, fname, line)?;
            }
            scopes.push(HashSet::new());
            check_undeclared_block(body, scopes, fname, line)?;
            scopes.pop();
        }
        Stmt::Lock { name, body } => {
            // The mutex name is a variable reference like any other: check it against scopes.
            check_undeclared_expr(&Expr::Var(name.clone()), scopes, fname, line)?;
            scopes.push(HashSet::new());
            check_undeclared_block(body, scopes, fname, line)?;
            scopes.pop();
        }
        Stmt::Break | Stmt::Continue => {}
    }
    Ok(())
}

fn check_undeclared_expr(
    e: &Expr,
    scopes: &mut ScopeStack,
    fname: &str,
    line: u32,
) -> Result<(), String> {
    match e {
        Expr::Var(name) => {
            if !scopes.iter().any(|s| s.contains(name.as_str())) {
                if name.starts_with(COV_RESERVED_PREFIX) {
                    // An unresolved `__cov_*` name is never a typo to report as "not
                    // declared": the prefix is reserved, and in the one function allowed
                    // to use the four real symbols they resolve, so reaching here means
                    // author code is reaching for compiler internals. Say so, and name
                    // the fix (the same one for every `__cov_` identifier: rename).
                    if line == 0 {
                        return Err(format!(
                            "`{name}` starts with the reserved `__cov_` prefix: those names \
                             are the coverage bookkeeping the compiler publishes itself for \
                             `sond test --coverage`, so a program that read one would reach \
                             into compiler internals. Rename it without the `__cov_` prefix."
                        ));
                    } else {
                        return Err(format!(
                            "`{name}` starts with the reserved `__cov_` prefix: those names \
                             are the coverage bookkeeping the compiler publishes itself for \
                             `sond test --coverage`, so a program that read one would reach \
                             into compiler internals. Rename it without the `__cov_` prefix \
                             (function `{fname}`, line {line})."
                        ));
                    }
                }
                if line == 0 {
                    return Err(format!(
                        "variable `{name}` is not declared in function `{fname}`"
                    ));
                } else {
                    return Err(format!(
                        "variable `{name}` is not declared in function `{fname}` at line {line}"
                    ));
                }
            }
        }
        Expr::Field(base, _) => {
            check_undeclared_expr(base, scopes, fname, line)?;
        }
        Expr::Call(_callee, args) => {
            for arg in args {
                check_undeclared_expr(&arg.value, scopes, fname, line)?;
            }
        }
        Expr::Index(base, index) => {
            check_undeclared_expr(base, scopes, fname, line)?;
            check_undeclared_expr(index, scopes, fname, line)?;
        }
        Expr::Unary(_, inner) => {
            check_undeclared_expr(inner, scopes, fname, line)?;
        }
        Expr::Binary(_, l, r) => {
            check_undeclared_expr(l, scopes, fname, line)?;
            check_undeclared_expr(r, scopes, fname, line)?;
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            check_undeclared_expr(cond, scopes, fname, line)?;
            scopes.push(HashSet::new());
            check_undeclared_block(then_branch, scopes, fname, line)?;
            scopes.pop();
            if let Some(eb) = else_branch {
                scopes.push(HashSet::new());
                check_undeclared_block(eb, scopes, fname, line)?;
                scopes.pop();
            }
        }
        Expr::Match {
            scrutinee,
            arms,
        } => {
            check_undeclared_expr(scrutinee, scopes, fname, line)?;
            for arm in arms {
                scopes.push(HashSet::new());
                if let Some(binding) = &arm.binding {
                    reject_cov_prefix("match binding", binding)?;
                    scopes.last_mut().unwrap().insert(binding.clone());
                }
                check_undeclared_block(&arm.body, scopes, fname, line)?;
                scopes.pop();
            }
        }
        Expr::Cast(inner, _) => {
            check_undeclared_expr(inner, scopes, fname, line)?;
        }
        Expr::List(items) => {
            for it in items {
                check_undeclared_expr(it, scopes, fname, line)?;
            }
        }
        Expr::SliceRange(base, lo, hi) => {
            check_undeclared_expr(base, scopes, fname, line)?;
            if let Some(e) = lo {
                check_undeclared_expr(e, scopes, fname, line)?;
            }
            if let Some(e) = hi {
                check_undeclared_expr(e, scopes, fname, line)?;
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, expr) in fields {
                check_undeclared_expr(expr, scopes, fname, line)?;
            }
        }
        Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
    Ok(())
}

/// Rejects the process-termination effect inside a spawned call graph (RFC-0011, footgun #3).
/// `syscall(SYS_exit, N)` ends the calling *thread* under process-per-connection semantics but
/// the whole *process* when it runs on a spawned OS thread, so one connection's error path would
/// take the entire program (and every other connection) down with it, silently. This is the first
/// concrete effect the checker learns: a `ProcExit` effect that propagates up the call graph and
/// is forbidden under `spawn`. The general mechanism (declared effects + capabilities) is RFC-0011;
/// this is its first enforced case.
pub fn check_no_process_exit_in_spawn(funcs: &[Function]) -> Result<(), String> {
    // Every `Call` node reachable in a block, decomposed into (callee, args). Used both to walk
    // the call graph (by callee name) and to spot the exit syscall (by the first argument).
    fn collect_calls<'a>(b: &'a Block, out: &mut Vec<(&'a Expr, &'a Vec<Arg>)>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { value, .. } | Stmt::Expr(value) | Stmt::Spawn(value) => {
                    collect_calls_expr(value, out)
                }
                Stmt::Par(peers) => {
                    for p in peers {
                        collect_calls_expr(p, out);
                    }
                }
                Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                    for p in peers {
                        collect_calls_expr(p, out);
                    }
                }
                Stmt::GraphLet {
                    nodes, yield_expr, ..
                } => {
                    for (_, e) in nodes {
                        collect_calls_expr(e, out);
                    }
                    collect_calls_expr(yield_expr, out);
                }
                Stmt::Assign { target, value } => {
                    collect_calls_expr(target, out);
                    collect_calls_expr(value, out);
                }
                Stmt::Return(Some(e)) => collect_calls_expr(e, out),
                Stmt::For { cond, body, .. } => {
                    if let Some(c) = cond {
                        collect_calls_expr(c, out);
                    }
                    collect_calls(body, out);
                }
                Stmt::Lock { body, .. } => {
                    collect_calls(body, out);
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        if let Some(t) = &b.tail {
            collect_calls_expr(t, out);
        }
    }
    fn collect_calls_expr<'a>(e: &'a Expr, out: &mut Vec<(&'a Expr, &'a Vec<Arg>)>) {
        match e {
            Expr::Call(callee, args) => {
                out.push((callee, args));
                collect_calls_expr(callee, out);
                for a in args {
                    collect_calls_expr(&a.value, out);
                }
            }
            Expr::Field(b, _) => collect_calls_expr(b, out),
            Expr::Index(b, i) => {
                collect_calls_expr(b, out);
                collect_calls_expr(i, out);
            }
            Expr::SliceRange(b, lo, hi) => {
                collect_calls_expr(b, out);
                if let Some(x) = lo {
                    collect_calls_expr(x, out);
                }
                if let Some(x) = hi {
                    collect_calls_expr(x, out);
                }
            }
            Expr::Unary(_, b) => collect_calls_expr(b, out),
            Expr::Binary(_, l, r) => {
                collect_calls_expr(l, out);
                collect_calls_expr(r, out);
            }
            Expr::List(xs) => {
                for x in xs {
                    collect_calls_expr(x, out);
                }
            }
            Expr::StructLit(_, fields) => {
                for (_, v) in fields {
                    collect_calls_expr(v, out);
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                collect_calls_expr(cond, out);
                collect_calls(then_branch, out);
                if let Some(eb) = else_branch {
                    collect_calls(eb, out);
                }
            }
            _ => {}
        }
    }
    // `syscall(SYS_exit, N)` or `syscall(60, N)` (231 = exit_group): the process-termination call.
    fn is_process_exit(callee: &Expr, args: &[Arg]) -> bool {
        let Expr::Var(name) = callee else {
            return false;
        };
        if name != "syscall" {
            return false;
        }
        let Some(first) = args.first() else {
            return false;
        };
        match &first.value {
            Expr::Var(n) => n == "SYS_exit" || n == "SYS_exit_group",
            Expr::Int(n) => *n == 60 || *n == 231,
            _ => false,
        }
    }

    let map: HashMap<&str, &Function> = funcs.iter().map(|f| (f.name.as_str(), f)).collect();

    // Seed the "runs on a spawned thread" set with every direct spawn target, then close it over
    // the call graph: the process-exit effect propagates up the calls a spawned body makes.
    // A spawn is a statement, so scan statements directly for spawn targets.
    fn spawn_targets<'a>(b: &'a Block, out: &mut Vec<&'a str>) {
        for s in &b.stmts {
            match s {
                Stmt::Spawn(Expr::Call(callee, _)) => {
                    if let Expr::Var(n) = callee.as_ref() {
                        out.push(n);
                    }
                }
                // Every `par` form runs its peers on spawned threads, the result binding included:
                // seeding only the bare `Stmt::Par` would leave `let a ~~ b = par f() ~~ g()`
                // outside the effect check even though its peers are threads just the same.
                Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                    for p in peers {
                        if let Expr::Call(callee, _) = p
                            && let Expr::Var(n) = callee.as_ref()
                        {
                            out.push(n);
                        }
                    }
                }
                Stmt::For { body, .. } => spawn_targets(body, out),
                _ => {}
            }
            // `if`/`match` bodies are blocks inside an expression, so a spawn written in one is
            // invisible to the walk above: seeding must go through them or the effect check
            // silently does not apply to a conditionally spawned worker.
            let mut inner = Vec::new();
            nested_blocks_of_stmt(s, &mut inner);
            for blk in inner {
                spawn_targets(blk, out);
            }
        }
        if let Some(t) = &b.tail {
            let mut inner = Vec::new();
            nested_blocks(t, &mut inner);
            for blk in inner {
                spawn_targets(blk, out);
            }
        }
    }
    let mut reachable: Vec<&str> = Vec::new();
    for f in funcs {
        spawn_targets(&f.body, &mut reachable);
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let mut work = reachable;
    while let Some(name) = work.pop() {
        if !seen.insert(name) {
            continue;
        }
        let Some(f) = map.get(name) else { continue };
        let mut calls = Vec::new();
        collect_calls(&f.body, &mut calls);
        // This function runs (transitively) under a spawn: it must not terminate the process.
        for (callee, args) in &calls {
            if is_process_exit(callee, args) {
                return Err(format!(
                    "function `{name}` runs on a spawned thread but calls the process-exit \
                     syscall (`syscall(SYS_exit, ..)`). On a thread that ends the whole process, \
                     not just the thread, so one task's error path would tear down every other \
                     task with it. Return from the spawned body (which ends only that thread) \
                     instead of exiting; a `task_return` value is the intended way to end a task."
                ));
            }
            if let Expr::Var(n) = callee
                && map.contains_key(n.as_str())
            {
                work.push(n);
            }
        }
    }
    Ok(())
}

/// Rejects `close_fd(fd)` in the continuation of a `spawn` that moved `fd` into the thread
/// (RFC-0011 footgun #2, the use-after-move-into-a-thread case). Because a spawn is joined at
/// the enclosing function's return, every statement after the spawn runs CONCURRENTLY with the
/// child; closing a descriptor there closes it out from under the live thread, and the symptom
/// is corruption, not an error. This is a conservative, order-sensitive static check (the sound
/// general fix is the capability move of RFC-0011): a variable passed to a spawned call is
/// "moved"; a later `close_fd` of it in the same function is a compile error, until the variable
/// is reassigned (which un-moves it).
pub fn check_no_close_fd_after_spawn(funcs: &[Function]) -> Result<(), String> {
    // Any `close_fd(v)` where `v` is a moved variable, appearing in this expression.
    fn closes_moved<'a>(e: &'a Expr, moved: &[&str]) -> Option<&'a str> {
        if let Expr::Call(callee, args) = e
            && let Expr::Var(name) = callee.as_ref()
            && name == "close_fd"
            && let Some(first) = args.first()
            && let Expr::Var(v) = &first.value
            && moved.contains(&v.as_str())
        {
            return Some(v);
        }
        // Recurse into subexpressions so a nested `close_fd` is still caught.
        match e {
            Expr::Call(callee, args) => closes_moved(callee, moved)
                .or_else(|| args.iter().find_map(|a| closes_moved(&a.value, moved))),
            Expr::Field(b, _) | Expr::Unary(_, b) => closes_moved(b, moved),
            Expr::Index(b, i) => closes_moved(b, moved).or_else(|| closes_moved(i, moved)),
            Expr::Binary(_, l, r) => closes_moved(l, moved).or_else(|| closes_moved(r, moved)),
            Expr::List(xs) => xs.iter().find_map(|x| closes_moved(x, moved)),
            _ => None,
        }
    }
    fn walk<'a>(stmts: &'a [Stmt], moved: &mut Vec<&'a str>, fname: &str) -> Result<(), String> {
        for s in stmts {
            // 1. A close of a moved fd anywhere in this statement's expressions is the footgun.
            let exprs: Vec<&Expr> = match s {
                Stmt::Let { value, .. } | Stmt::Expr(value) | Stmt::Spawn(value) => vec![value],
                Stmt::Par(peers) => peers.iter().collect(),
                Stmt::Assign { target, value } => vec![target, value],
                Stmt::Return(Some(e)) => vec![e],
                Stmt::For { cond, .. } => cond.iter().collect(),
                _ => vec![],
            };
            for e in exprs {
                if let Some(v) = closes_moved(e, moved) {
                    return Err(format!(
                        "function `{fname}`: `close_fd({v})` runs after `{v}` was moved into a \
                         spawned thread, so it closes the descriptor while the thread is still \
                         using it (the continuation runs concurrently with the spawn). Close the \
                         descriptor in the thread that owns it, or release it in the caller after \
                         the join, not in the same function that spawned."
                    ));
                }
            }
            // 2. Update the moved set from this statement's effect.
            match s {
                Stmt::Spawn(Expr::Call(_, args)) => {
                    for a in args {
                        if let Expr::Var(v) = &a.value {
                            moved.push(v);
                        }
                    }
                }
                Stmt::Par(peers) => {
                    for p in peers {
                        if let Expr::Call(_, args) = p {
                            for a in args {
                                if let Expr::Var(v) = &a.value {
                                    moved.push(v);
                                }
                            }
                        }
                    }
                }
                // Reassigning a variable un-moves it: it now names a fresh descriptor.
                Stmt::Let { name, .. } => moved.retain(|m| *m != name.as_str()),
                Stmt::Assign {
                    target: Expr::Var(name),
                    ..
                } => moved.retain(|m| *m != name.as_str()),
                // A nested block's statements run after the spawn too, so carry `moved` in.
                Stmt::For { body, .. } => walk(&body.stmts, moved, fname)?,
                _ => {}
            }
        }
        Ok(())
    }
    for f in funcs {
        let mut moved = Vec::new();
        walk(&f.body.stmts, &mut moved, &f.name)?;
    }
    Ok(())
}

pub fn check_receiver_kinds(funcs: &[Function]) -> Result<(), String> {
    // struct name -> (is_pointer, first method name seen) of its receiver kind.
    let mut kind: HashMap<&str, (bool, &str)> = HashMap::new();
    for f in funcs {
        let Some(recv) = f.params.first() else {
            continue;
        };
        if recv.name != "self" {
            continue;
        }
        let (tn, is_ptr) = match &recv.ty {
            Type::Named(t) => (t.as_str(), false),
            Type::Ptr(inner) => match inner.as_ref() {
                Type::Named(t) => (t.as_str(), true),
                _ => continue,
            },
            // A view is not a struct, so it never selects a method receiver kind.
            Type::View(..) => continue,
        };
        match kind.get(tn) {
            Some((prev_ptr, prev_m)) if *prev_ptr != is_ptr => {
                return Err(format!(
                    "struct `{tn}` mixes value and pointer methods (`{prev_m}` and \
                     `{}`): a struct type is value XOR pointer. Make all its methods \
                     use the same receiver, either `self` or `self: *{tn}`.",
                    f.name
                ));
            }
            None => {
                kind.insert(tn, (is_ptr, f.name.as_str()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Maps each struct type that HAS methods to its receiver kind (true = pointer).
/// Assumes `check_receiver_kinds` already ran, so a type is not mixed.
fn receiver_kind_map(funcs: &[Function]) -> HashMap<&str, bool> {
    let mut m = HashMap::new();
    for f in funcs {
        let Some(recv) = f.params.first() else {
            continue;
        };
        if recv.name != "self" {
            continue;
        }
        match &recv.ty {
            Type::Named(t) => {
                m.insert(t.as_str(), false);
            }
            Type::Ptr(inner) => {
                if let Type::Named(t) = inner.as_ref() {
                    m.insert(t.as_str(), true);
                }
            }
            Type::View(..) => {}
        }
    }
    m
}

/// A struct type's kind is FIXED by its methods: a type with value methods must be
/// declared `T` (not `*T`); a type with pointer methods must be declared `*T` (not
/// `T`). Rejects any `let`/parameter declaration whose pointer-ness disagrees with
/// the type's method kind. Types without methods are unconstrained.
pub fn check_declaration_kinds(funcs: &[Function]) -> Result<(), String> {
    let kinds = receiver_kind_map(funcs);
    if kinds.is_empty() {
        return Ok(());
    }
    for f in funcs {
        for p in &f.params {
            check_decl_ty(&p.ty, &kinds)?;
        }
        check_decls_in_block(&f.body, &kinds)?;
    }
    Ok(())
}

/// Rejects a single type annotation if its pointer-ness contradicts the type kind.
fn check_decl_ty(ty: &Type, kinds: &HashMap<&str, bool>) -> Result<(), String> {
    let (name, declared_ptr) = match ty {
        Type::Named(n) => (n.as_str(), false),
        Type::Ptr(inner) => match inner.as_ref() {
            Type::Named(n) => (n.as_str(), true),
            _ => return Ok(()),
        },
        Type::View(..) => return Ok(()),
    };
    match kinds.get(name) {
        Some(true) if !declared_ptr => Err(format!(
            "struct `{name}` has pointer methods, so it must be declared `*{name}` \
             (a mutable, shared handle), not the value form `{name}`."
        )),
        Some(false) if declared_ptr => Err(format!(
            "struct `{name}` has value methods, so it must be declared `{name}`, not \
             the pointer form `*{name}`."
        )),
        _ => Ok(()),
    }
}

fn check_decls_in_block(b: &Block, kinds: &HashMap<&str, bool>) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let { ty: Some(t), .. } => check_decl_ty(t, kinds)?,
            Stmt::For { body, .. } => check_decls_in_block(body, kinds)?,
            Stmt::Expr(e) | Stmt::Return(Some(e)) => check_decls_in_expr(e, kinds)?,
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        check_decls_in_expr(t, kinds)?;
    }
    Ok(())
}

fn check_decls_in_expr(e: &Expr, kinds: &HashMap<&str, bool>) -> Result<(), String> {
    if let Expr::If {
        then_branch,
        else_branch,
        ..
    } = e
    {
        check_decls_in_block(then_branch, kinds)?;
        if let Some(eb) = else_branch {
            check_decls_in_block(eb, kinds)?;
        }
    }
    Ok(())
}

fn check_block(b: &Block, immutable: &HashMap<&str, &str>, fname: &str) -> Result<(), String> {
    for s in &b.stmts {
        check_stmt(s, immutable, fname)?;
    }
    if let Some(t) = &b.tail {
        check_expr(t, immutable, fname)?;
    }
    Ok(())
}

fn check_stmt(s: &Stmt, immutable: &HashMap<&str, &str>, fname: &str) -> Result<(), String> {
    match s {
        Stmt::Assign { target, value } => {
            let bad = mutated_base(target).and_then(|n| immutable.get(n).map(|ty| (n, *ty)));
            if let Some((name, tyname)) = bad {
                return Err(format!(
                    "cannot mutate value struct `{name}` in `{fname}`: it was passed \
                     by value (created elsewhere), so it is read-only here. Declare \
                     the parameter `{name}: *{tyname}` to take a mutable pointer."
                ));
            }
            check_expr(value, immutable, fname)
        }
        Stmt::Let { value, .. } => check_expr(value, immutable, fname),
        Stmt::Expr(e) | Stmt::Spawn(e) | Stmt::Return(Some(e)) => check_expr(e, immutable, fname),
        Stmt::Par(peers) => {
            for p in peers {
                check_expr(p, immutable, fname)?;
            }
            Ok(())
        }
        Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
            for p in peers {
                check_expr(p, immutable, fname)?;
            }
            Ok(())
        }
        Stmt::GraphLet {
            nodes, yield_expr, ..
        } => {
            for (_, e) in nodes {
                check_expr(e, immutable, fname)?;
            }
            check_expr(yield_expr, immutable, fname)
        }
        Stmt::For { body, cond, .. } => {
            if let Some(c) = cond {
                check_expr(c, immutable, fname)?;
            }
            check_block(body, immutable, fname)
        }
        Stmt::Lock { body, .. } => {
            check_block(body, immutable, fname)
        }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => Ok(()),
    }
}

/// Assignments can also hide inside `if`/`for` bodies used as expressions.
fn check_expr(e: &Expr, immutable: &HashMap<&str, &str>, fname: &str) -> Result<(), String> {
    if let Expr::If {
        then_branch,
        else_branch,
        ..
    } = e
    {
        check_block(then_branch, immutable, fname)?;
        if let Some(eb) = else_branch {
            check_block(eb, immutable, fname)?;
        }
    }
    Ok(())
}

/// The root variable a mutation targets: `p.x`, `p.x[i]`, `p` all root at `p`.
fn mutated_base(target: &Expr) -> Option<&str> {
    match target {
        Expr::Field(base, _) | Expr::Index(base, _) => mutated_base(base),
        Expr::Var(name) => Some(name),
        _ => None,
    }
}

/// (C) on-ramp for #3 (RFC-0012, decided (A)-via-(C)): reject a `+`/`-`/`*` whose operands are
/// compile-time CONSTANTS and whose mathematical result overflows i64. Only PROVABLE overflows
/// are rejected, a strict subset of full checked arithmetic, non-breaking (the corpus has no
/// such constant; out-of-range decimal literals already fail at lexing; hex `i64::MIN` is a
/// single token, not an operation). Shifts and division are excluded (bit patterns / handled by
/// the divisor obligations).
pub fn check_constant_overflow(funcs: &[Function]) -> Result<(), String> {
    for f in funcs {
        // A `wrapping fn` opts its body OUT of overflow-checking: its integer arithmetic is
        // DEFINED to wrap mod 2^N (the crypto substrate), so an overflow there is the intended
        // semantics, not an error to reject.
        if f.is_wrapping {
            continue;
        }
        overflow_block(&f.body, &f.name)?;
    }
    Ok(())
}

fn overflow_block(b: &Block, fname: &str) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                overflow_expr(value, fname)?
            }
            Stmt::Assign { target, value } => {
                overflow_expr(target, fname)?;
                overflow_expr(value, fname)?;
            }
            Stmt::For {
                cond,
                invariant,
                body,
                ..
            } => {
                if let Some(c) = cond {
                    overflow_expr(c, fname)?;
                }
                for inv in invariant {
                    overflow_expr(inv, fname)?;
                }
                overflow_block(body, fname)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        overflow_expr(t, fname)?;
    }
    Ok(())
}

fn overflow_expr(e: &Expr, fname: &str) -> Result<(), String> {
    if let Expr::Binary(op, _, _) = e
        && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
    {
        const_fold(e, fname)?; // rejects a constant subtree that overflows
    }
    match e {
        Expr::Binary(_, l, r) => {
            overflow_expr(l, fname)?;
            overflow_expr(r, fname)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => overflow_expr(x, fname),
        Expr::Index(a, b) => {
            overflow_expr(a, fname)?;
            overflow_expr(b, fname)
        }
        Expr::Call(c, args) => {
            overflow_expr(c, fname)?;
            for a in args {
                overflow_expr(&a.value, fname)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                overflow_expr(it, fname)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                overflow_expr(v, fname)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            overflow_expr(cond, fname)?;
            overflow_block(then_branch, fname)?;
            if let Some(eb) = else_branch {
                overflow_block(eb, fname)?;
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            overflow_expr(scrutinee, fname)?;
            for a in arms {
                overflow_block(&a.body, fname)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Folds a compile-time-constant integer expression: `Ok(Some(v))` if constant, `Ok(None)` if
/// not, `Err` if a `+`/`-`/`*` of constants overflows i64. Sub-fold errors propagate, so a
/// constant overflow nested inside a non-constant expression is still caught.
fn const_fold(e: &Expr, fname: &str) -> Result<Option<i64>, String> {
    match e {
        Expr::Int(n) => Ok(Some(*n)),
        Expr::Binary(op, l, r) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
            let lv = const_fold(l, fname)?;
            let rv = const_fold(r, fname)?;
            match (lv, rv) {
                (Some(a), Some(b)) => {
                    let (res, sym) = match op {
                        BinOp::Add => (a.checked_add(b), "+"),
                        BinOp::Sub => (a.checked_sub(b), "-"),
                        _ => (a.checked_mul(b), "*"),
                    };
                    res.map(Some).ok_or_else(|| {
                        format!(
                            "function `{fname}`: constant integer overflow in `{a} {sym} {b}`: \
                             the result does not fit in i64"
                        )
                    })
                }
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}
/// (C) on-ramp for #2 (unchecked `a[i]`, RFC-0012 §6): reject `a[K]` into a fixed-size array
/// `let a[N]` when K is a COMPILE-TIME CONSTANT outside `[0, N)`: a provable stack overrun
/// (an off-by-one, a wrong literal). Only PROVABLE out-of-bounds are rejected; a variable index
/// is left to the prover and the coming refinement types. Non-breaking: no valid program indexes
/// a known-size array with a constant out of range. Mirrors `check_constant_overflow`.
/// The flow-sensitive facts the bounds check carries as it walks a body: the sound propositions
/// that hold at the current program point. It NEVER trusts an unverified user `invariant`; the
/// counter floors below are PROVEN here from the loop's own shape.
#[derive(Clone, Default)]
struct Flow {
    /// Invalidatable facts (a precondition, a loop condition, a branch guard), dropped as soon as
    /// a variable they mention is reassigned, because they may no longer hold.
    facts: Vec<Expr>,
    /// Monotonic-counter floors: `v >= C`. A counter set to constant `C` before a loop and stepped
    /// only by `v = v + <positive const>` never drops below `C`, so this survives its increment
    /// (unlike the upper bound from the condition, which the increment invalidates).
    counters: HashMap<String, i64>,
    /// Monotonic-counter ceilings: `v <= C`. The mirror of `counters` for a DECREASING counter
    /// (stepped only by `v = v - <positive const>` from a constant entry), which never rises above
    /// its entry value, and for the derived upper bound of a masked/shifted value.
    ceilings: HashMap<String, i64>,
    /// Known exact constant values: `v == C`.
    consts: HashMap<String, i64>,
    /// Local views in scope, as `name -> the local/parameter naming its length` (`None` when the
    /// declaration gave no length). A view whose length is knowable another way — one taken
    /// directly over a measurable array — is recorded as a size instead, like the array itself.
    views: HashMap<String, Option<String>>,
    /// Local slices whose length is tracked by the `len` field (a `Field(s, "len")` expression)
    /// or the raw constant. A local slice must be bounded the same way a slice parameter is:
    /// against its `.len` at runtime, so the same prover that discharges `for i < s.len {}`
    /// also discharges `s[i]` inside that loop.
    slices: HashMap<String, Box<Expr>>,
}

impl Flow {
    /// The proposition set for discharging a bounds obligation at this point.
    fn assumptions(&self) -> Vec<Expr> {
        let mut a = self.facts.clone();
        for (v, &c) in &self.counters {
            a.push(cmp_const(BinOp::Ge, v, c));
        }
        for (v, &c) in &self.ceilings {
            a.push(cmp_const(BinOp::Le, v, c));
        }
        for (v, &c) in &self.consts {
            a.push(cmp_const(BinOp::Ge, v, c));
            a.push(cmp_const(BinOp::Le, v, c));
        }
        a
    }
    /// A variable was reassigned: drop every fact that mentions it and any const/counter status.
    fn invalidate(&mut self, v: &str) {
        self.facts.retain(|f| !mentions(f, v));
        self.counters.remove(v);
        self.ceilings.remove(v);
        self.consts.remove(v);
        self.slices.remove(v);
    }
}

fn cmp_const(op: BinOp, v: &str, c: i64) -> Expr {
    Expr::Binary(
        op,
        Box::new(Expr::Var(v.to_string())),
        Box::new(Expr::Int(c)),
    )
}

fn cmp_var(op: BinOp, v: &str, rhs: &Expr) -> Expr {
    Expr::Binary(
        op,
        Box::new(Expr::Var(v.to_string())),
        Box::new(rhs.clone()),
    )
}

/// Does `e` mention the variable `name`?
fn mentions(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Var(v) => v == name,
        Expr::Binary(_, l, r) | Expr::Index(l, r) => mentions(l, name) || mentions(r, name),
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => mentions(x, name),
        Expr::Call(c, args) => mentions(c, name) || args.iter().any(|a| mentions(&a.value, name)),
        _ => false,
    }
}

/// Collects every variable assigned (as a plain `v = ..` target) anywhere in a block.
fn assigned_vars(b: &Block, out: &mut HashSet<String>) {
    let push = |e: &Expr, out: &mut HashSet<String>| match e {
        Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            assigned_vars(then_branch, out);
            if let Some(eb) = else_branch {
                assigned_vars(eb, out);
            }
        }
        Expr::Match { arms, .. } => {
            for a in arms {
                assigned_vars(&a.body, out);
            }
        }
        _ => {}
    };
    for s in &b.stmts {
        match s {
            Stmt::Assign {
                target: Expr::Var(v),
                value,
            } => {
                out.insert(v.clone());
                push(value, out);
            }
            Stmt::Assign { value, .. } => push(value, out),
            Stmt::For { body, .. } => assigned_vars(body, out),
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                push(value, out)
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        push(t, out);
    }
}

/// Does the block `break` out of THIS loop? A `break` inside a NESTED `for` targets that inner
/// loop, so nested loop bodies are not descended into. Used to decide whether the negated loop
/// condition is a sound post-loop fact (it is not if the loop can exit early via `break`).
fn has_break(b: &Block) -> bool {
    fn in_expr(e: &Expr) -> bool {
        match e {
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => has_break(then_branch) || else_branch.as_ref().is_some_and(|eb| has_break(eb)),
            Expr::Match { arms, .. } => arms.iter().any(|a| has_break(&a.body)),
            _ => false,
        }
    }
    b.stmts.iter().any(|s| match s {
        Stmt::Break => true,
        Stmt::For { .. } => false,
        Stmt::Let { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Expr(value)
        | Stmt::Assign { value, .. } => in_expr(value),
        _ => false,
    }) || b.tail.as_ref().is_some_and(|t| in_expr(t))
}

/// Flattens a conjunction `a && b && ..` into its conjuncts; a non-`&&` expression is a single
/// conjunct. Each conjunct of a loop condition holds throughout the body, so each is usable
/// separately (the linear prover cannot consume a raw `&&`).
fn conjuncts(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Binary(BinOp::And, l, r) => {
            let mut v = conjuncts(l);
            v.extend(conjuncts(r));
            v
        }
        _ => vec![e],
    }
}

/// Records the interval bound a `v OP const` proposition implies into the flow's counter/ceiling
/// maps (a floor for `>=`/`>`, a ceiling for `<`/`<=`), so the interval analysis (`expr_range`)
/// can use it to bound a derived index like `v >> 3`. Other shapes contribute nothing.
fn seed_interval_bound(flow: &mut Flow, e: &Expr) {
    if let Expr::Binary(op, l, r) = e
        && let Expr::Var(v) = l.as_ref()
        // The bound `k` is a literal, or a variable resolved to its constant value (a global const
        // like `TABLE_SIZE`, or a prior `let`), so `for i < TABLE_SIZE` seeds `i`'s ceiling.
        && let Some(k) = const_index(r).or_else(|| match r.as_ref() {
            Expr::Var(rv) => flow.consts.get(rv).copied(),
            _ => None,
        })
    {
        match op {
            BinOp::Lt => {
                flow.ceilings.insert(v.clone(), k - 1);
            }
            BinOp::Le => {
                flow.ceilings.insert(v.clone(), k);
            }
            BinOp::Gt => {
                flow.counters.insert(v.clone(), k + 1);
            }
            BinOp::Ge => {
                flow.counters.insert(v.clone(), k);
            }
            _ => {}
        }
    }
}

/// If `e` is `v >= k` or `v > k` with `k` a constant, the monotonic floor it establishes for `v`:
/// `k` (resp. `k + 1`).
fn floor_from(e: &Expr) -> Option<(String, i64)> {
    let Expr::Binary(op, l, r) = e else {
        return None;
    };
    let Expr::Var(v) = l.as_ref() else {
        return None;
    };
    let k = const_index(r)?;
    match op {
        BinOp::Ge => Some((v.clone(), k)),
        BinOp::Gt => Some((v.clone(), k + 1)),
        _ => None,
    }
}

/// If `value` is `v + K` or `K + v` with `K` a positive constant, returns `K`: the shape of a
/// monotonic increasing counter step.
fn increment_of(v: &str, value: &Expr) -> Option<i64> {
    let Expr::Binary(BinOp::Add, l, r) = value else {
        return None;
    };
    let is_v = |e: &Expr| matches!(e, Expr::Var(n) if n == v);
    let k = if is_v(l) {
        const_index(r)?
    } else if is_v(r) {
        const_index(l)?
    } else {
        return None;
    };
    (k > 0).then_some(k)
}

/// If `value` is `v - K` with `K` a positive constant, returns `K`: a monotonic DECREASING step.
fn decrement_of(v: &str, value: &Expr) -> Option<i64> {
    let Expr::Binary(BinOp::Sub, l, r) = value else {
        return None;
    };
    if !matches!(l.as_ref(), Expr::Var(n) if n == v) {
        return None;
    }
    let k = const_index(r)?;
    (k > 0).then_some(k)
}

/// A conservative `[lo, hi]` interval for `e` under the current flow, or `None` if not bounded on
/// both sides. Sound interval arithmetic over the operators crypto uses to derive an index from a
/// counter: masking (`& (2^k-1)`), shifts (`>>`/`>>>`/`<<`), `/`, `%`, and `+`/`-`/`*`. This is
/// what lets `exp[bit >> 3]` and `a[i / 8]` be proven in bounds without a hand-written `where`.
fn expr_range(e: &Expr, flow: &Flow) -> Option<(i64, i64)> {
    match e {
        Expr::Int(n) => Some((*n, *n)),
        Expr::Var(v) => {
            if let Some(&c) = flow.consts.get(v) {
                return Some((c, c));
            }
            // A one-sided bound is still a SOUND interval against the type's extreme: a proven
            // floor `f` puts the value in `[f, i64::MAX]` (a nonneg counter with no upper bound),
            // a proven ceiling `c` in `[i64::MIN, c]`. This is what lets `head % 16` clamp when
            // only `head >= 0` is known, the modulo case reads just the dividend's floor.
            match (flow.counters.get(v).copied(), flow.ceilings.get(v).copied()) {
                (Some(lo), Some(hi)) => Some((lo, hi)),
                (Some(lo), None) => Some((lo, i64::MAX)),
                (None, Some(hi)) => Some((i64::MIN, hi)),
                (None, None) => None,
            }
        }
        Expr::Binary(op, l, r) => match op {
            BinOp::BitAnd => {
                // `e & (2^k - 1)` ∈ [0, mask] for any `e`: the mask keeps only the low bits.
                let m = const_index(l).or_else(|| const_index(r))?;
                (m >= 0 && (m & m.wrapping_add(1)) == 0).then_some((0, m))
            }
            BinOp::Shr | BinOp::LShr => {
                // `e >> k` = `e / 2^k` for non-negative `e`: shifts the interval down.
                let (llo, lhi) = expr_range(l, flow)?;
                let k = const_index(r)?;
                (llo >= 0 && (0..63).contains(&k)).then_some((llo >> k, lhi >> k))
            }
            BinOp::Shl => {
                let (llo, lhi) = expr_range(l, flow)?;
                let k = const_index(r)?;
                (llo >= 0 && (0..63).contains(&k))
                    .then_some((llo.wrapping_shl(k as u32), lhi.wrapping_shl(k as u32)))
            }
            BinOp::Div => {
                let d = const_index(r)?;
                let (llo, lhi) = expr_range(l, flow)?;
                (d > 0 && llo >= 0).then_some((llo / d, lhi / d))
            }
            BinOp::Mod => {
                let m = const_index(r)?;
                let (llo, _) = expr_range(l, flow)?;
                (m > 0 && llo >= 0).then_some((0, m - 1))
            }
            BinOp::Add => {
                let (a, b) = expr_range(l, flow)?;
                let (c, d) = expr_range(r, flow)?;
                Some((a.checked_add(c)?, b.checked_add(d)?))
            }
            BinOp::Sub => {
                let (a, b) = expr_range(l, flow)?;
                let (c, d) = expr_range(r, flow)?;
                Some((a.checked_sub(d)?, b.checked_sub(c)?))
            }
            BinOp::Mul => {
                let (a, b) = expr_range(l, flow)?;
                let (c, d) = expr_range(r, flow)?;
                (a >= 0 && c >= 0).then_some((a.checked_mul(c)?, b.checked_mul(d)?))
            }
            _ => None,
        },
        // The value of an `if` is one of its branch tails, so its range is the UNION of the two,
        // each computed under that branch's guard (`x < 8` refines the else-branch's `x`). Handles
        // the clamp idiom `if x >= 8 { 7 } else { x }`; a branch with statements, or a missing
        // else, bails (no single value to range).
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let else_branch = else_branch.as_ref()?;
            if !then_branch.stmts.is_empty() || !else_branch.stmts.is_empty() {
                return None;
            }
            let (then_tail, else_tail) = (then_branch.tail.as_ref()?, else_branch.tail.as_ref()?);
            let mut tf = flow.clone();
            for c in conjuncts(cond) {
                seed_interval_bound(&mut tf, c);
            }
            let mut ef = flow.clone();
            if let Some(neg) = negate_cond(cond) {
                seed_interval_bound(&mut ef, &neg);
            }
            let (tlo, thi) = expr_range(then_tail, &tf)?;
            let (elo, ehi) = expr_range(else_tail, &ef)?;
            Some((tlo.min(elo), thi.max(ehi)))
        }
        _ => None,
    }
}

/// Does EVERY assignment to `v` in the block satisfy `step` (and is there at least one)? Used to
/// recognise a monotonic counter, all increments (`is_monotonic_counter`) or all decrements
/// (`is_monotonic_decounter`).
fn all_steps<F: Fn(&str, &Expr) -> bool>(v: &str, b: &Block, step: &F) -> bool {
    fn walk<F: Fn(&str, &Expr) -> bool>(
        v: &str,
        b: &Block,
        step: &F,
        all_ok: &mut bool,
        any: &mut bool,
    ) {
        let in_expr = |e: &Expr, all_ok: &mut bool, any: &mut bool| match e {
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => {
                walk(v, then_branch, step, all_ok, any);
                if let Some(eb) = else_branch {
                    walk(v, eb, step, all_ok, any);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    walk(v, &a.body, step, all_ok, any);
                }
            }
            _ => {}
        };
        for s in &b.stmts {
            match s {
                Stmt::Assign {
                    target: Expr::Var(t),
                    value,
                } if t == v => {
                    *any = true;
                    if !step(v, value) {
                        *all_ok = false;
                    }
                }
                Stmt::For { body, .. } => walk(v, body, step, all_ok, any),
                Stmt::Let { value, .. }
                | Stmt::Return(Some(value))
                | Stmt::Expr(value)
                | Stmt::Assign { value, .. } => in_expr(value, all_ok, any),
                _ => {}
            }
        }
        if let Some(t) = &b.tail {
            in_expr(t, all_ok, any);
        }
    }
    let (mut all_ok, mut any) = (true, false);
    walk(v, b, step, &mut all_ok, &mut any);
    any && all_ok
}

/// `v` is stepped only by `v = v + <positive const>`: it only ever increases from its entry value.
fn is_monotonic_counter(v: &str, b: &Block) -> bool {
    all_steps(v, b, &|v, e| increment_of(v, e).is_some())
}

/// `v` is stepped only by `v = v - <positive const>`: it only ever decreases from its entry value.
fn is_monotonic_decounter(v: &str, b: &Block) -> bool {
    all_steps(v, b, &|v, e| decrement_of(v, e).is_some())
}

/// Does assigning `value` to a global known `>= 0` keep it `>= 0`? True for `g = c`, `g = g + c`,
/// `g = g * c` with a non-negative constant `c`: the shapes a counter/length ever takes.
fn rhs_preserves_nonneg(value: &Expr, g: &str) -> bool {
    if matches!(const_index(value), Some(v) if v >= 0) {
        return true;
    }
    let is_g = |e: &Expr| matches!(e, Expr::Var(v) if v == g);
    let nonneg_const = |e: &Expr| matches!(const_index(e), Some(v) if v >= 0);
    matches!(value, Expr::Binary(BinOp::Add | BinOp::Mul, l, r)
        if (is_g(l) && nonneg_const(r)) || (is_g(r) && nonneg_const(l)))
}

/// Does every assignment to `g` anywhere in `e`'s nested blocks preserve non-negativity? Walks
/// into the branch blocks of `if`/`match` expressions (the only expression forms that hold
/// statements); other expressions contain no assignments.
fn expr_assigns_preserve_nonneg(e: &Expr, g: &str) -> bool {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_assigns_preserve_nonneg(cond, g)
                && block_assigns_preserve_nonneg(then_branch, g)
                && else_branch
                    .as_ref()
                    .is_none_or(|eb| block_assigns_preserve_nonneg(eb, g))
        }
        Expr::Match { scrutinee, arms } => {
            expr_assigns_preserve_nonneg(scrutinee, g)
                && arms
                    .iter()
                    .all(|a| block_assigns_preserve_nonneg(&a.body, g))
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            expr_assigns_preserve_nonneg(l, g) && expr_assigns_preserve_nonneg(r, g)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            expr_assigns_preserve_nonneg(x, g)
        }
        Expr::Call(c, args) => {
            expr_assigns_preserve_nonneg(c, g)
                && args
                    .iter()
                    .all(|a| expr_assigns_preserve_nonneg(&a.value, g))
        }
        Expr::List(items) => items.iter().all(|i| expr_assigns_preserve_nonneg(i, g)),
        Expr::StructLit(_, fields) => fields
            .iter()
            .all(|(_, v)| expr_assigns_preserve_nonneg(v, g)),
        Expr::SliceRange(b, lo, hi) => {
            expr_assigns_preserve_nonneg(b, g)
                && lo
                    .as_ref()
                    .is_none_or(|x| expr_assigns_preserve_nonneg(x, g))
                && hi
                    .as_ref()
                    .is_none_or(|x| expr_assigns_preserve_nonneg(x, g))
        }
        _ => true,
    }
}

fn stmt_assigns_preserve_nonneg(s: &Stmt, g: &str) -> bool {
    match s {
        Stmt::Assign {
            target: Expr::Var(v),
            value,
        } if v == g => rhs_preserves_nonneg(value, g),
        Stmt::For { body, .. } => block_assigns_preserve_nonneg(body, g),
        Stmt::Let { value, .. }
        | Stmt::Return(Some(value))
        | Stmt::Expr(value)
        | Stmt::Assign { value, .. } => expr_assigns_preserve_nonneg(value, g),
        _ => true,
    }
}

fn block_assigns_preserve_nonneg(b: &Block, g: &str) -> bool {
    b.stmts.iter().all(|s| stmt_assigns_preserve_nonneg(s, g))
        && b.tail
            .as_ref()
            .is_none_or(|t| expr_assigns_preserve_nonneg(t, g))
}

/// Module-level mutable scalar globals PROVABLY `>= 0` in every function: initialised to a
/// non-negative constant and only ever assigned a non-negativity-preserving value across the
/// WHOLE program. A counter that starts at 0 and only increments is the canonical case, so a
/// later `t[count]` (or `let i = count; t[i]`) needs no hand-written `count >= 0`: the floor is
/// a whole-module invariant no per-function walk can see. Sound: an unproven mutation (a
/// decrement, an opaque RHS) drops the global from the set.
fn nonnegative_globals(funcs: &[Function], globals: &[Global]) -> HashSet<String> {
    let mut cands: HashSet<String> = globals
        .iter()
        .filter(|g| g.mutable && g.array_len.is_none())
        .filter(|g| matches!(const_index(&g.init), Some(v) if v >= 0))
        .map(|g| g.name.clone())
        .collect();
    cands.retain(|g| {
        funcs
            .iter()
            .all(|f| block_assigns_preserve_nonneg(&f.body, g))
    });
    cands
}

/// Folds `e` to a constant, resolving a bare global-const variable through `consts`.
fn resolve_const(e: &Expr, consts: &HashMap<String, i64>) -> Option<i64> {
    const_index(e).or_else(|| match e {
        Expr::Var(v) => consts.get(v).copied(),
        _ => None,
    })
}

/// If `cond` (or one of its `&&` conjuncts) is `g < L` / `g <= L` with `L` constant, the upper
/// bound it puts on `g` inside the guarded branch (`L-1` / `L`); `None` otherwise.
fn guard_upper(cond: &Expr, g: &str, consts: &HashMap<String, i64>) -> Option<i64> {
    for c in conjuncts(cond) {
        if let Expr::Binary(op, l, r) = c
            && let Expr::Var(v) = l.as_ref()
            && v == g
            && let Some(lv) = resolve_const(r, consts)
        {
            match op {
                BinOp::Lt => return Some(lv - 1),
                BinOp::Le => return Some(lv),
                _ => {}
            }
        }
    }
    None
}

/// Combines the enclosing guard with the one `cond` adds (the tighter of the two upper bounds).
fn tighten_guard(
    cur: Option<i64>,
    cond: &Expr,
    g: &str,
    consts: &HashMap<String, i64>,
) -> Option<i64> {
    match (cur, guard_upper(cond, g, consts)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (None, b) => b,
        (a, None) => a,
    }
}

/// Walks `e`'s nested `if`/`match` branches for assignments to `g` (the only expression forms that
/// hold statements). Returns false if any assignment leaves `g` unbounded.
fn walk_ceiling_expr(
    e: &Expr,
    g: &str,
    guard: Option<i64>,
    consts: &HashMap<String, i64>,
    ceil: &mut Option<i64>,
) -> bool {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let tg = tighten_guard(guard, cond, g, consts);
            walk_ceiling(then_branch, g, tg, consts, ceil)
                && else_branch
                    .as_ref()
                    .is_none_or(|eb| walk_ceiling(eb, g, guard, consts, ceil))
        }
        Expr::Match { arms, .. } => {
            for a in arms {
                if !walk_ceiling(&a.body, g, guard, consts, ceil) {
                    return false;
                }
            }
            true
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            walk_ceiling_expr(l, g, guard, consts, ceil)
                && walk_ceiling_expr(r, g, guard, consts, ceil)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            walk_ceiling_expr(x, g, guard, consts, ceil)
        }
        Expr::Call(c, args) => {
            walk_ceiling_expr(c, g, guard, consts, ceil)
                && args
                    .iter()
                    .all(|a| walk_ceiling_expr(&a.value, g, guard, consts, ceil))
        }
        _ => true,
    }
}

/// Walks a block for assignments to `g`, tracking the enclosing `if g < L` guard, and folds the
/// running ceiling. A constant assignment contributes its value; a decrement preserves the
/// ceiling; an increment `g = g + k` contributes `guard + k` and REQUIRES an enclosing guard
/// (else `g` is unbounded → false). After any mutation of `g` the guard no longer holds, and a
/// loop body is walked with no guard (it does not persist across iterations). Returns false to
/// signal "unbounded" (bail).
fn walk_ceiling(
    b: &Block,
    g: &str,
    guard: Option<i64>,
    consts: &HashMap<String, i64>,
    ceil: &mut Option<i64>,
) -> bool {
    let mut guard = guard;
    for s in &b.stmts {
        match s {
            Stmt::Assign {
                target: Expr::Var(v),
                value,
            } if v == g => {
                if let Some(c) = resolve_const(value, consts) {
                    *ceil = Some(ceil.map_or(c, |x| x.max(c)));
                } else if decrement_of(g, value).is_some() {
                    // decrement preserves any ceiling, no new contribution
                } else if let Some(k) = increment_of(g, value) {
                    match guard {
                        Some(gc) => {
                            let c = gc + k;
                            *ceil = Some(ceil.map_or(c, |x| x.max(c)));
                        }
                        None => return false, // unguarded increment, unbounded
                    }
                } else {
                    return false; // opaque assignment, unbounded
                }
                guard = None; // `g` changed: the `g < L` guard no longer holds below
            }
            Stmt::Expr(Expr::If {
                cond,
                then_branch,
                else_branch,
            }) => {
                let tg = tighten_guard(guard, cond, g, consts);
                if !walk_ceiling(then_branch, g, tg, consts, ceil) {
                    return false;
                }
                if let Some(eb) = else_branch
                    && !walk_ceiling(eb, g, guard, consts, ceil)
                {
                    return false;
                }
            }
            Stmt::For { body, .. } => {
                if !walk_ceiling(body, g, None, consts, ceil) {
                    return false;
                }
            }
            Stmt::Let { value, .. }
            | Stmt::Return(Some(value))
            | Stmt::Expr(value)
            | Stmt::Assign { value, .. }
                if !walk_ceiling_expr(value, g, guard, consts, ceil) =>
            {
                return false;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail
        && !walk_ceiling_expr(t, g, guard, consts, ceil)
    {
        return false;
    }
    true
}

/// Module-level mutable scalar globals with a PROVEN upper bound `g <= C` everywhere: the
/// initialiser is a constant and every assignment across the whole program keeps `g` bounded (a
/// constant, a decrement, or an increment made under an `if g < L` guard). A counter incremented
/// only inside `if cnt < N { .. }` is the canonical case, so `for i < cnt { a[i] }` proves without
/// a hand-written bound. Sound: one unguarded increment (or opaque write) drops the global.
fn bounded_globals(
    funcs: &[Function],
    globals: &[Global],
    consts: &HashMap<String, i64>,
) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for gd in globals {
        if !gd.mutable || gd.array_len.is_some() {
            continue;
        }
        let Some(init) = const_index(&gd.init) else {
            continue;
        };
        let mut ceil = Some(init);
        if funcs
            .iter()
            .all(|f| walk_ceiling(&f.body, &gd.name, None, consts, &mut ceil))
            && let Some(c) = ceil
        {
            out.insert(gd.name.clone(), c);
        }
    }
    out
}

pub fn check_constant_bounds(funcs: &[Function], globals: &[Global]) -> Result<(), String> {
    // Module-level arrays (`.data` tables, crypto S-boxes, round constants) are in scope in
    // every function, so seed their sizes once.
    let mut global_sizes: HashMap<String, i64> = HashMap::new();
    // Immutable module-level scalar constants (`let TABLE_SIZE = 64`) are in scope everywhere and
    // often bound a loop (`for i < TABLE_SIZE`) over an array of that size; resolve each to its
    // value so the interval analysis can use it exactly like a literal.
    let mut global_consts: HashMap<String, i64> = HashMap::new();
    for g in globals {
        if let Some(n) = g.array_len {
            global_sizes.insert(g.name.clone(), n);
        } else if let Some(Type::View(_, Some(len_str))) = &g.ty {
            // A module-level view-typed global (`let mut NAME: T[N]`) has its length in
            // bytes stored elsewhere, but the element count N is what bounds checks use:
            // `NAME[i]` must be PROVEN in `0..N`. This is the same path that local views
            // follow through `sizes` -> `flow.views` -> length name, except globals live
            // in `sizes` from entry so the lookup succeeds without a name indirection.
            if let Ok(n) = len_str.parse::<i64>() {
                global_sizes.insert(g.name.clone(), n);
            }
        } else if !g.mutable
            && let Some(v) = const_index(&g.init)
        {
            global_consts.insert(g.name.clone(), v);
        }
    }
    // Mutable scalar globals that never go negative anywhere (counters, lengths) carry a `>= 0`
    // floor into every function, the whole-module invariant a single function cannot establish.
    let nonneg = nonnegative_globals(funcs, globals);
    // Mutable scalar globals bounded above (a counter only incremented under `if g < N`) carry a
    // `g <= C` ceiling into every function, so `for i < g { a[i] }` proves in bounds.
    let bounded = bounded_globals(funcs, globals, &global_consts);
    for f in funcs {
        let mut sizes = global_sizes.clone();
        // A PARAMETER shadows a module-level array of the same name, as it does everywhere else
        // in the language. Without this, a name chosen in one program reached into another: a
        // caller declaring `let mut ptrs[8]` gave that size to the `ptrs` parameter of a library
        // function, and indexing it was reported as unprovable -- in a file the programmer had
        // not written, about a bound they could not add, and including the library was enough to
        // trigger it. The only cure available to them was to rename their own variable.
        for p in &f.params {
            sizes.remove(&p.name);
        }
        // The function's preconditions are the facts true on every path from entry.
        let mut flow = Flow {
            facts: f.requires.clone(),
            consts: global_consts.clone(),
            ..Default::default()
        };
        // A precondition `x >= 0` / `x < N` also seeds the interval maps, so `expr_range` knows a
        // parameter's floor/ceiling (not only the linear prover via facts), e.g. the branches of
        // an `if`-expression clamp are ranged under it.
        for req in &f.requires {
            for conj in conjuncts(req) {
                seed_interval_bound(&mut flow, conj);
            }
        }
        for g in &nonneg {
            flow.facts.push(Expr::Binary(
                BinOp::Ge,
                Box::new(Expr::Var(g.clone())),
                Box::new(Expr::Int(0)),
            ));
            // A floor of 0 in the interval maps too, so a non-negativity-preserving increment
            // (`count = count + 1`) keeps it, the fact alone is dropped when `count` is reassigned.
            flow.counters.insert(g.clone(), 0);
        }
        for (g, c) in &bounded {
            flow.facts.push(Expr::Binary(
                BinOp::Le,
                Box::new(Expr::Var(g.clone())),
                Box::new(Expr::Int(*c)),
            ));
            flow.ceilings.insert(g.clone(), *c);
        }
        bounds_block(&f.body, &f.name, &mut sizes, &mut flow, &f.params, funcs, 0)?;
    }
    Ok(())
}

/// How many BYTES a view taken over `init` can address: the whole array for `backing`, and what
/// remains past the offset for `backing + K` with a constant `K` (an offset beyond the end leaves
/// nothing, not a negative). `None` when the expression is not a measurable array at all, which
/// is the honest answer for an allocator's return or an offset computed at runtime.
fn view_bytes_available(init: &Expr, sizes: &HashMap<String, i64>) -> Option<i64> {
    match init {
        Expr::Var(backing) => sizes.get(backing).map(|words| words.saturating_mul(8)),
        Expr::Binary(BinOp::Add, l, r) => {
            let (Expr::Var(backing), Some(off)) = (l.as_ref(), const_index(r)) else {
                return None;
            };
            let total = sizes.get(backing)?.saturating_mul(8);
            (off >= 0).then(|| (total - off).max(0))
        }
        _ => None,
    }
}

/// The width in bytes of a view's element type. An unknown name is a 64-bit word, which is what
/// every untyped Sond value is; the widths themselves are the machine's, not a choice.
fn elem_bytes(elem: &Type) -> i64 {
    let Type::Named(n) = elem else {
        return 8;
    };
    match n.as_str() {
        "i8" | "u8" | "bool" => 1,
        "i16" | "u16" => 2,
        "i32" | "u32" | "f32" => 4,
        _ => 8,
    }
}

/// How many BYTES a raw `peek`/`poke` touches, and `None` for anything that is not one of them.
/// The widths are the machine's, not a choice: `peek8` reads a byte, `poke64` writes a word.
fn raw_access_bytes(name: &str) -> Option<i64> {
    Some(match name {
        "peek8" | "poke8" => 1,
        "peek16" | "poke16" => 2,
        "peek32" | "poke32" => 4,
        "peek64" | "poke64" => 8,
        _ => return None,
    })
}

/// Splits `a` or `a + K` into the buffer's name and the constant byte offset. Anything else — an
/// offset computed at runtime, an address that arrived from elsewhere — carries no constant to
/// decide with, and says so instead of guessing one.
fn const_offset_from_buffer(e: &Expr) -> Option<(String, i64)> {
    match e {
        Expr::Var(n) => Some((n.clone(), 0)),
        Expr::Binary(BinOp::Add, l, r) => match (l.as_ref(), const_index(r)) {
            (Expr::Var(n), Some(k)) => Some((n.clone(), k)),
            _ => None,
        },
        _ => None,
    }
}

/// The builtins that take a string LITERAL directly: the console primitives, which write the
/// bytes they are given and never hand the address to anyone. A literal is text there too, but
/// there is nothing it can decay into.
const LITERAL_BUILTINS: [&str; 7] = [
    "print",
    "serial_print",
    "print_kern",
    "write",
    "cstr",
    "literal",
    "include",
];

/// Text has a TYPE. A string literal binds to `string` (and to the console builtins, which take
/// one directly); anywhere else it would decay into a bare address, which is precisely what made
/// "walk it until a NUL turns up" the natural thing to write — with an address and no length
/// there is nothing else you can do with it.
///
pub fn check_string_literal_types(funcs: &[Function], globals: &[Global]) -> Result<(), String> {
    let sigs: HashMap<&str, &Function> = funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    // Module-level string globals are text, just like function-local variables.
    let base_strings: HashSet<String> = globals
        .iter()
        .filter(|g| g.ty.as_ref().is_some_and(holds_code_points))
        .map(|g| g.name.clone())
        .collect();
    for f in funcs {
        // A parameter declared `string` carries code points just like a local one, so the
        // body starts with them already known.
        let mut strings: HashSet<String> = base_strings.clone();
        for p in &f.params {
            if holds_code_points(&p.ty) {
                strings.insert(p.name.clone());
            }
        }
        literals_block(&f.body, &f.name, &sigs, &strings)?;
    }
    Ok(())
}

fn is_string_ty(ty: &Type) -> bool {
    match ty {
        Type::Named(n) => n == "string",
        // `let s: string = ".."` is lowered to a view of code points, and `s.utf8` to a view of
        // bytes; both are what a literal is allowed to become.
        Type::View(inner, _) => {
            matches!(inner.as_ref(), Type::Named(n) if n == "u32" || n == "u8")
        }
        _ => false,
    }
}

/// Whether a type carries TEXT — code points four bytes wide. Narrower than [`is_string_ty`],
/// and deliberately so: a byte view is a legitimate destination for `.utf8`, but it is also
/// how every plain byte buffer is declared, and mistaking one for text rejects correct code.
fn holds_code_points(ty: &Type) -> bool {
    match ty {
        Type::Named(n) => n == "string",
        Type::View(inner, _) => matches!(inner.as_ref(), Type::Named(n) if n == "u32"),
        _ => false,
    }
}

fn literals_block(
    b: &Block,
    fname: &str,
    sigs: &HashMap<&str, &Function>,
    outer_strings: &HashSet<String>,
) -> Result<(), String> {
    // Names bound in an inner block die with it, so each block works on its own copy.
    let mut strings: HashSet<String> = outer_strings.clone();
    for st in &b.stmts {
        match st {
            Stmt::Let {
                name, ty, value, ..
            } => {
                if matches!(value, Expr::Str(_)) && !ty.as_ref().is_some_and(is_string_ty) {
                    return Err(format!(
                        "function `{fname}`: `{name}` is given a string literal but is not \
                         declared `string`; text has a type — write `let {name}: string = ..`"
                    ));
                }
                // A variable declared `string` holds code points (UTF-32), not bytes.
                // Binding it to anything else decays that address into a bare pointer and
                // makes byte-level access look right at compile time when it is not.
                if let Expr::Var(src) = value
                    && strings.contains(src.as_str())
                    && !ty.as_ref().is_some_and(holds_code_points)
                {
                    return Err(format!(
                        "function `{fname}`: `{name}` is bound from `{src}`, which is a \
                         `string` — code points four bytes wide, not bytes; text has a type — \
                         write `let {name}: string = {src}` to keep the text, or \
                         `let {name}: i64 = {src}.utf8` to get its bytes"
                    ));
                }
                // Track what carries text. A bare `let q: string = l` arrives as
                // `Type::Named("string")`; with a literal attached the parser has already
                // lowered it to the view of code points. Rebinding the name to anything
                // else drops it from the set — the new binding is what the name means now.
                if ty.as_ref().is_some_and(holds_code_points) {
                    strings.insert(name.clone());
                } else {
                    strings.remove(name);
                }
                literals_expr(value, fname, sigs, &strings)?;
            }
            Stmt::Assign { target, value } => {
                literals_expr(target, fname, sigs, &strings)?;
                literals_expr(value, fname, sigs, &strings)?;
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Spawn(e) => {
                literals_expr(e, fname, sigs, &strings)?
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    literals_expr(c, fname, sigs, &strings)?;
                }
                literals_block(body, fname, sigs, &strings)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        literals_expr(t, fname, sigs, &strings)?;
    }
    Ok(())
}

/// Whether `e` is a *text* value (code points, not a bare byte pointer).
/// A string literal, or a variable bound to text (`string` param or local).
fn is_text_expr(e: &Expr, strings: &HashSet<String>) -> bool {
    matches!(e, Expr::Str(_)) || matches!(e, Expr::Var(v) if strings.contains(v.as_str()))
}

/// Whether `e` is an *integer* value (raw pointer / number, not text).
/// An integer literal, a non-string local, a `.utf8` field access, or an arithmetic chain.
fn is_int_expr(e: &Expr, strings: &HashSet<String>) -> bool {
    if let Expr::Field(base, name) = e {
        if name == "utf8" && is_text_expr(base, strings) {
            return true;
        }
        if name == "utf8" && is_int_expr(base, strings) {
            return true;
        }
        // field access on a known text var is also a pointer → int
        if name != "utf8" && is_text_expr(base, strings) {
            return true;
        }
        if name != "utf8" && is_int_expr(base, strings) {
            return true;
        }
        // field access on an unknown variable: do NOT assume int (silly struct fields)
        return false;
    }
    if let Expr::Binary(_, l, r) = e {
        return is_int_expr(l, strings) || is_int_expr(r, strings);
    }
    if let Expr::Unary(_, inner) = e {
        return is_int_expr(inner, strings);
    }
    if let Expr::Int(_) = e {
        return true;
    }
    if let Expr::Var(v) = e {
        return !strings.contains(v.as_str());
    }
    false
}

/// Whether a function parameter accepts text at the type level (`string`).
fn accepts_text(p: &Param) -> bool {
    // `holds_code_points`, not `is_string_ty`: the wider test counts a `u8[n]` view as text, and
    // that is how EVERY plain byte buffer is declared. `fn fill(b: u8[n], n: i64)` filling a
    // buffer with 0xFF was refused as "takes `b` as text, and `b` is a number", advice that
    // cannot be followed because `b` was never text. A parameter carries text when it is
    // `string` or a view of code points; a byte view is a destination for `.utf8`, not text.
    holds_code_points(&p.ty)
}

/// Whether a function parameter expects a *raw* integer (not a text-typed parameter).
fn expects_int(p: &Param) -> bool {
    !accepts_text(p)
}

/// How an argument was written, for a diagnostic that points at the caller's own text. Only the
/// forms a programmer can act on are named; anything else is described, never invented.
fn arg_as_written(e: &Expr) -> String {
    match e {
        Expr::Str(_) => "the literal".to_string(),
        Expr::Var(v) => format!("`{v}`"),
        Expr::Field(base, n) => match base.as_ref() {
            Expr::Var(v) => format!("`{v}.{n}`"),
            // A module-level string reaches here already replaced by its own literal, so there
            // is no name left to quote -- say what it is instead of inventing one.
            Expr::Str(_) => format!("the literal's `.{n}`"),
            _ => format!("the `.{n}` of that value"),
        },
        _ => "that value".to_string(),
    }
}

/// Check that every argument to a declared function matches the parameter's text/int role.
///
/// The two representations of a text are the point: a `string` carries the characters, and
/// `.utf8` hands over the address of its UTF-8 bytes. Which one a value holds used to depend on
/// where it came from, and the compiler said nothing either way -- so a signature that said
/// `string` was a comment, and a `.utf8` sneaked into it kept working until the day it did not.
fn check_call_type_roles(
    g: &Function,
    args: &[Arg],
    fname: &str,
    strings: &HashSet<String>,
) -> Result<(), String> {
    for (i, a) in args.iter().enumerate() {
        let Some(param) = g.params.get(i) else {
            continue;
        };
        let pname = param.name.as_str();
        let callee = g.name.as_str();
        let written = arg_as_written(&a.value);

        // Text where a number is expected. The way out is `.utf8`, spelled with the caller's
        // own name for the value -- naming the parameter would send them to edit the callee.
        if is_text_expr(&a.value, strings) && expects_int(param) {
            let bytes_of = match &a.value {
                Expr::Var(v) => format!("`{v}.utf8`"),
                _ => "`.utf8` on it".to_string(),
            };
            return Err(format!(
                "function `{fname}`: `{callee}` takes `{pname}` as a number, and {written} is \
                 text -- write {bytes_of} to hand over the bytes"
            ));
        }

        if is_int_expr(&a.value, strings) && accepts_text(param) {
            // A `.utf8` on something that IS text: the caller already has the text and took its
            // address on the way in. Passing the text itself is almost always what they meant.
            let stripped = if let Expr::Field(base, n) = &a.value {
                (n == "utf8" && is_text_expr(base, strings)).then(|| arg_as_written(base))
            } else {
                None
            };
            if let Some(text) = stripped {
                return Err(format!(
                    "function `{fname}`: `{callee}` takes `{pname}` as text, and {written} is the \
                     address of its bytes -- pass {text} itself"
                ));
            }
            return Err(format!(
                "function `{fname}`: `{callee}` takes `{pname}` as text, and {written} is a \
                 number -- a text parameter carries the characters, not an address. If it really \
                 holds bytes, call the pointer form of `{callee}` if the module has one, the way \
                 `bytes_from_cstr` sits next to `bytes_from_str`"
            ));
        }
    }
    Ok(())
}

fn literals_expr(
    e: &Expr,
    fname: &str,
    sigs: &HashMap<&str, &Function>,
    strings: &HashSet<String>,
) -> Result<(), String> {
    if let Expr::Call(callee, args) = e
        && let Expr::Var(name) = callee.as_ref()
    {
        if LITERAL_BUILTINS.contains(&name.as_str()) {
            // Still check type roles even for builtins that take raw ints.
            // Builtins that deal in text (println etc.) are not expected here
            // because they only accept a single `string` parameter.
        }
        if let Some(g) = sigs.get(name.as_str()) {
            for (i, a) in args.iter().enumerate() {
                if matches!(&a.value, Expr::Str(_)) {
                    let ok = g.params.get(i).is_some_and(|p| is_string_ty(&p.ty));
                    if !ok {
                        let pname = g.params.get(i).map_or("?", |p| p.name.as_str());
                        return Err(format!(
                            "function `{fname}`: a string literal is passed to `{name}`, whose \
                             parameter `{pname}` is not `string`; text has a type — declare it \
                             `{pname}: string`"
                        ));
                    }
                }
            }
            // Check ALL arguments (not just string literals) → text/int mismatch.
            check_call_type_roles(g, args, fname, strings)?;
        }
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            literals_expr(l, fname, sigs, strings)?;
            literals_expr(r, fname, sigs, strings)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            literals_expr(x, fname, sigs, strings)
        }
        Expr::Call(c, args) => {
            literals_expr(c, fname, sigs, strings)?;
            for a in args {
                literals_expr(&a.value, fname, sigs, strings)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            literals_expr(cond, fname, sigs, strings)?;
            literals_block(then_branch, fname, sigs, strings)?;
            if let Some(eb) = else_branch {
                literals_block(eb, fname, sigs, strings)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Is `name` a parameter of VIEW type, and if so, which parameter carries its length? Returns
/// `None` when `name` is not a view at all, `Some(None)` for a length-less `T[]` (nothing to
/// prove an index against), and `Some(Some(n))` for `T[n]`.
fn view_len_of(name: &str, params: &[Param]) -> Option<Option<String>> {
    params
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| match &p.ty {
            Type::View(_, len) => Some(len.clone()),
            _ => None,
        })
}

/// Is `name` a parameter of slice type? A slice's length bound (`s.len`) is a runtime value, so
/// its indices are checked against that rather than a static array size.
fn is_slice_param(name: &str, params: &[Param]) -> bool {
    params
        .iter()
        .any(|p| p.name == name && matches!(&p.ty, Type::Named(t) if t == "Slice"))
}

/// The negation of a comparison guard, for the else-branch of an `if` (`i < N` -> `i >= N`), or
/// `None` for a non-comparison condition (which contributes no usable fact).
fn negate_cond(e: &Expr) -> Option<Expr> {
    let Expr::Binary(op, l, r) = e else {
        return None;
    };
    let nop = match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        _ => return None,
    };
    Some(Expr::Binary(nop, l.clone(), r.clone()))
}

/// Replaces free `Var(name)` nodes per `map`: used to instantiate a callee's `ensures` at a
/// call site (its return name becomes the binding, its parameters become the arguments).
fn subst_vars(e: &Expr, map: &HashMap<String, Expr>) -> Expr {
    match e {
        Expr::Var(v) => map.get(v).cloned().unwrap_or_else(|| e.clone()),
        Expr::Binary(op, l, r) => Expr::Binary(
            op.clone(),
            Box::new(subst_vars(l, map)),
            Box::new(subst_vars(r, map)),
        ),
        Expr::Unary(op, x) => Expr::Unary(op.clone(), Box::new(subst_vars(x, map))),
        Expr::Field(x, f) => Expr::Field(Box::new(subst_vars(x, map)), f.clone()),
        Expr::Index(a, b) => {
            Expr::Index(Box::new(subst_vars(a, map)), Box::new(subst_vars(b, map)))
        }
        Expr::Cast(x, t) => Expr::Cast(Box::new(subst_vars(x, map)), t.clone()),
        Expr::Call(c, args) => Expr::Call(
            Box::new(subst_vars(c, map)),
            args.iter()
                .map(|a| Arg {
                    name: a.name.clone(),
                    value: subst_vars(&a.value, map),
                })
                .collect(),
        ),
        _ => e.clone(),
    }
}

/// Does `e` reference any of `names` as a free variable? Guards ensures-propagation: a clause
/// still mentioning a callee parameter (one not positionally mapped to an argument) must not be
/// assumed, since the bare name could alias an unrelated caller variable.
fn mentions_any(e: &Expr, names: &HashSet<String>) -> bool {
    match e {
        Expr::Var(v) => names.contains(v),
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            mentions_any(l, names) || mentions_any(r, names)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => mentions_any(x, names),
        Expr::Call(c, args) => {
            mentions_any(c, names) || args.iter().any(|a| mentions_any(&a.value, names))
        }
        _ => false,
    }
}

/// Instantiates a callee's `ensures` at a `let name = f(args)` binding and returns the sound
/// facts about `name`: the return name mapped to `name`, and (only for all-positional calls)
/// each parameter mapped to its argument. A clause still mentioning an unmapped parameter is
/// dropped (it could alias a caller variable). This is what carries a bounded allocator return
/// (`fu_alloc`/`fu_find`, `-> (r) ensures { r >= 0; r < N }`) to the index at the call site.
fn call_ensures_facts(name: &str, value: &Expr, funcs: &[Function]) -> Vec<Expr> {
    let Expr::Call(callee, cargs) = value else {
        return Vec::new();
    };
    let Expr::Var(fnname) = callee.as_ref() else {
        return Vec::new();
    };
    let Some(f) = funcs
        .iter()
        .find(|g| &g.name == fnname && g.ret_name.is_some())
    else {
        return Vec::new();
    };
    let positional = cargs.len() == f.params.len() && cargs.iter().all(|a| a.name.is_none());
    let mut map: HashMap<String, Expr> = HashMap::new();
    map.insert(f.ret_name.clone().unwrap(), Expr::Var(name.to_string()));
    if positional {
        for (p, a) in f.params.iter().zip(cargs) {
            map.insert(p.name.clone(), a.value.clone());
        }
    }
    let param_names: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    f.ensures
        .iter()
        .map(|ens| subst_vars(ens, &map))
        .filter(|fact| !mentions_any(fact, &param_names))
        .collect()
}

/// Does `b` never fall through, does every path end in `return`/`break`/`continue`? A block
/// diverges when its last statement is such a jump, or its tail is an if/match whose branches all
/// diverge. Used to refine the code AFTER a guard clause: reaching it means the guard was false.
fn block_diverges(b: &Block) -> bool {
    if let Some(tail) = &b.tail {
        return match tail.as_ref() {
            Expr::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => block_diverges(then_branch) && block_diverges(eb),
            Expr::Match { arms, .. } => arms.iter().all(|a| block_diverges(&a.body)),
            _ => false,
        };
    }
    match b.stmts.last() {
        Some(Stmt::Return(_) | Stmt::Break | Stmt::Continue) => true,
        Some(Stmt::Expr(Expr::If {
            then_branch,
            else_branch: Some(eb),
            ..
        })) => block_diverges(then_branch) && block_diverges(eb),
        Some(Stmt::Expr(Expr::Match { arms, .. })) => arms.iter().all(|a| block_diverges(&a.body)),
        _ => false,
    }
}

fn bounds_block(
    b: &Block,
    fname: &str,
    sizes: &mut HashMap<String, i64>,
    flow: &mut Flow,
    params: &[Param],
    funcs: &[Function],
    line: u32,
) -> Result<(), String> {
    for (i, s) in b.stmts.iter().enumerate() {
        let stmt_line = b.stmt_lines.get(i).copied().unwrap_or(line);
        match s {
            Stmt::Let {
                name,
                value,
                array_len,
                ty,
                ..
            } => {
                bounds_expr(value, fname, sizes, flow, params, funcs, stmt_line)?;
                // Rebinding the name drops any prior fact/const/counter about it.
                flow.invalidate(name);
                // `let a: u32[] = backing` over an array whose size is known: the view's length is
                // not missing, it is the backing store's capacity IN ELEMENTS (4 words of backing
                // hold 8 `u32`s). Recording it means a local view is checked exactly like the array
                // it views, instead of being the one spelling that escapes the check.
                // What a view can hold comes from the EXPRESSION it is taken over, not from the
                // name alone: the whole backing store, or what is LEFT of it past a constant
                // offset. An offset the compiler cannot reduce (`backing + i`, a pointer from
                // elsewhere) leaves no capacity at all, and then the view has to declare its
                // length or go unindexed — silence is the one answer that is not allowed.
                let view_capacity = match ty.as_ref() {
                    Some(Type::View(elem, _)) => {
                        view_bytes_available(value, sizes).map(|bytes| bytes / elem_bytes(elem))
                    }
                    _ => None,
                };
                // Rebinding a name drops whatever it used to be, view or not; then record it if
                // this binding IS a view, so an index into it is never checked by accident against
                // a size that belonged to something else.
                flow.views.remove(name);
                if let Some(Type::View(_, len)) = ty.as_ref() {
                    flow.views.insert(name.clone(), len.clone());
                }
                match array_len {
                    // `let a[N]` records the known size; a scalar `let a = ..` reusing the
                    // name drops any prior size so a later index is not checked against it.
                    Some(n) => {
                        sizes.insert(name.clone(), *n);
                    }
                    None => {
                        sizes.remove(name);
                        if let Some(c) = const_index(value) {
                            flow.consts.insert(name.clone(), c);
                        } else {
                            // Record the definitional equality `name == value` so the prover can
                            // substitute (`let t = 16 + j` reduces `W[t - 16]` to `W[j]`). It is
                            // dropped as soon as `name` or any variable in `value` is reassigned,
                            // since both invalidate facts mentioning them.
                            flow.facts.push(cmp_var(BinOp::Ge, name, value));
                            flow.facts.push(cmp_var(BinOp::Le, name, value));
                            // And its derived interval, for a masked/shifted offset the linear
                            // prover cannot express (`let byte_idx = bit >> 3`).
                            if let Some((lo, hi)) = expr_range(value, flow) {
                                flow.counters.insert(name.clone(), lo);
                                flow.ceilings.insert(name.clone(), hi);
                            }
                            // A call with a postcondition carries its `ensures` to the binding, so
                            // a bounded allocator return (`let i = alloc()`) bounds `t[i]` without
                            // restating the bound at the call site.
                            for fact in call_ensures_facts(name, value, funcs) {
                                seed_interval_bound(flow, &fact);
                                flow.facts.push(fact);
                            }
                        }
                        // Recorded AFTER the `remove` above, which drops the size of any name being
                        // rebound: a view over a measurable array is one of the few non-array
                        // bindings that DOES have a size worth keeping.
                        if let Some(cap) = view_capacity {
                            sizes.insert(name.clone(), cap);
                        }
                    }
                }
                // Register local slices AFTER the match array_len block (which calls sizes.remove(name)
                // for the None arm). Use BOTH maps: `sizes` for constant-index fast-path checks,
                // and `flow.slices` with a Field(s, "len") expression so the prover can match the
                // loop condition i < s.len against it.
                let is_slice = ty.as_ref().is_some_and(|t| matches!(t, Type::Named(n) if n == "Slice"))
                    || matches!(&value, Expr::StructLit(n, _) if n == "Slice");
                if is_slice {
                    if let Expr::StructLit(_, fields) = &value
                        && let Some(c) = fields
                            .iter()
                            .find_map(|(k, v)| if k == "len" { const_index(v) } else { None })
                    {
                        sizes.insert(name.clone(), c);
                    }
                    flow.slices.insert(
                        name.clone(),
                        Box::new(Expr::Field(
                            Box::new(Expr::Var(name.clone())),
                            "len".to_string(),
                        )),
                    );
                }
            }
            Stmt::Assign {
                target: Expr::Var(v),
                value,
            } => {
                bounds_expr(value, fname, sizes, flow, params, funcs, stmt_line)?;
                // Classify the reassignment against `v`'s bounds captured BEFORE invalidation:
                // an increment `v = v + k` keeps the floor (drops the ceiling), a decrement keeps
                // the ceiling (drops the floor), a fresh constant is exact, and any other value
                // takes whatever interval can be derived for it (a mask/shift/mod bounds it).
                let floor = flow.consts.get(v).or(flow.counters.get(v)).copied();
                let ceil = flow.consts.get(v).or(flow.ceilings.get(v)).copied();
                let is_inc = increment_of(v, value).is_some();
                let is_dec = decrement_of(v, value).is_some();
                let rng = expr_range(value, flow);
                flow.invalidate(v);
                if is_inc {
                    if let Some(c) = floor {
                        flow.counters.insert(v.clone(), c);
                    }
                } else if is_dec {
                    if let Some(c) = ceil {
                        flow.ceilings.insert(v.clone(), c);
                    }
                } else if let Some(c) = const_index(value) {
                    flow.consts.insert(v.clone(), c);
                } else if let Some((lo, hi)) = rng {
                    flow.counters.insert(v.clone(), lo);
                    flow.ceilings.insert(v.clone(), hi);
                }
            }
            Stmt::Assign { target, value } => {
                // Assigning through an index/field: a memory write, no scalar rebinding.
                bounds_expr(target, fname, sizes, flow, params, funcs, stmt_line)?;
                bounds_expr(value, fname, sizes, flow, params, funcs, stmt_line)?;
            }
            Stmt::Expr(e @ Expr::If { .. }) => {
                // Check the condition and both branches (delegated to bounds_expr, which also
                // invalidates variables a branch reassigns). Then refine the CONTINUATION: a guard
                // clause whose then-branch never falls through means reaching the next statement
                // implies the condition was false, so `!cond` holds afterwards. This is what lets
                // `if fi < 0 { return }` establish `fi >= 0` for a later `used[fi]`. A single
                // comparison negates cleanly; a conjunctive guard's negation is a disjunction we
                // simply do not assume (sound, weaker). Symmetrically a diverging `else` with a
                // fall-through then-branch contributes `cond` (which the prover splits on `&&`).
                bounds_expr(e, fname, sizes, flow, params, funcs, stmt_line)?;
                if let Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                } = e
                {
                    if else_branch.is_none() && block_diverges(then_branch) {
                        if let Some(neg) = negate_cond(cond) {
                            seed_interval_bound(flow, &neg);
                            flow.facts.push(neg);
                        }
                    } else if let Some(eb) = else_branch
                        && block_diverges(eb)
                        && !block_diverges(then_branch)
                    {
                        let c = cond.as_ref().clone();
                        seed_interval_bound(flow, &c);
                        flow.facts.push(c);
                    }
                }
            }
            Stmt::Return(Some(v)) | Stmt::Expr(v) => {
                bounds_expr(v, fname, sizes, flow, params, funcs, stmt_line)?
            }
            Stmt::For {
                cond,
                invariant,
                body,
                ..
            } => {
                // The loop's SOUND facts are derived below (counter floors/ceilings, the
                // condition). A user `invariant` clause is trusted ONLY if the prover VERIFIES it
                // (init + preservation), trusting a verified invariant is sound, unlike an
                // unverified one; an unverified/unverifiable clause is simply ignored.
                if let Some(c) = cond {
                    bounds_expr(c, fname, sizes, flow, params, funcs, stmt_line)?;
                }
                let mut modified = HashSet::new();
                assigned_vars(body, &mut modified);
                let mut body_flow = flow.clone();
                // Facts about a variable the body reassigns do not survive across iterations.
                for v in &modified {
                    body_flow.invalidate(v);
                }
                // A variable stepped only by `v = v + <positive const>` from a constant entry never
                // drops below that entry value, a PROVEN monotonic floor; its mirror, a decrement,
                // never rises above the entry value, a monotonic ceiling.
                for v in &modified {
                    if is_monotonic_counter(v, body)
                        && let Some(&c) = flow.consts.get(v).or_else(|| flow.counters.get(v))
                    {
                        body_flow.counters.insert(v.clone(), c);
                    }
                    if is_monotonic_decounter(v, body)
                        && let Some(&c) = flow.consts.get(v).or_else(|| flow.ceilings.get(v))
                    {
                        body_flow.ceilings.insert(v.clone(), c);
                    }
                }
                // The condition holds at the top of each iteration (invalidated inside the body as
                // soon as the counter steps). A conjunctive condition (`val > 0 && tmp_len < 20`)
                // is split so EACH conjunct is a usable fact and interval bound, the second bounds
                // the digit buffer's index. The bound it names feeds the interval maps too, so a
                // derived index (`bit >> 3`) can be reasoned about.
                if let Some(c) = cond {
                    for conj in conjuncts(c) {
                        body_flow.facts.push(conj.clone());
                        seed_interval_bound(&mut body_flow, conj);
                    }
                }
                // A VERIFIED invariant holds at the top of every iteration, so its propositions are
                // seeded as facts, flow-sensitively, like the condition, so they drop as soon as a
                // variable they mention is reassigned (matching where the invariant is guaranteed:
                // at the iteration top, before the body's mutations). This is what lets the crypto
                // down-counting loops prove `exp[bit >> 3]` from `invariant { bit >= 0; bit < 256 }`.
                let inv_verified = !invariant.is_empty()
                    && crate::verify::loop_invariant_verified(
                        funcs,
                        params,
                        &flow.assumptions(),
                        cond.as_ref(),
                        invariant,
                        body,
                    );
                if inv_verified {
                    for inv in invariant {
                        body_flow.facts.push(inv.clone());
                        // Also feed the interval maps (`expr_range` reads them, not `facts`) so a
                        // derived index such as `bit >> 3` can be bounded.
                        seed_interval_bound(&mut body_flow, inv);
                    }
                }
                // A declaration inside the loop does not escape it.
                let mut inner = sizes.clone();
                bounds_block(body, fname, &mut inner, &mut body_flow, params, funcs, line)?;
                // After the loop, every reassigned variable is unknown.
                for v in &modified {
                    flow.invalidate(v);
                }
                // A VERIFIED invariant of a break-free loop holds AFTER the loop as well (it held at
                // every iteration top and the loop exits only by the condition failing), so seed it
                // into the continuation, this carries `tmp_len <= 20` from a digit-building loop to
                // the reversal loop that reads `digits[tmp_len - 1 - j]`.
                if inv_verified && !has_break(body) {
                    for inv in invariant {
                        flow.facts.push(inv.clone());
                        seed_interval_bound(flow, inv);
                    }
                }
                // A break-free `for cond {}` exits only when `cond` is false, so `!cond` holds
                // afterward, a sound fact even about the (modified) counter. When it bounds the
                // counter below (`v >= k`), that floor carries into a later loop reusing `v` (the
                // SHA message-schedule idiom, where one `t` runs 0..16 then 16..80).
                if let Some(c) = cond
                    && !has_break(body)
                    && let Some(neg) = negate_cond(c)
                {
                    if let Some((v, k)) = floor_from(&neg) {
                        flow.counters.insert(v, k);
                    }
                    flow.facts.push(neg);
                }
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        let tail_line = b.stmt_lines.last().copied().unwrap_or(line);
        bounds_expr(t, fname, sizes, flow, params, funcs, tail_line)?;
    }
    Ok(())
}

fn bounds_expr(
    e: &Expr,
    fname: &str,
    sizes: &HashMap<String, i64>,
    flow: &mut Flow,
    params: &[Param],
    funcs: &[Function],
    line: u32,
) -> Result<(), String> {
    // A raw `peek`/`poke` at a CONSTANT offset from a buffer whose length the compiler knows is an
    // access it can already decide: `a + 16` on two words reaches past the end however it is
    // spelled, and `a[2]` and `poke64(a + 16, ..)` are the same bytes with only one of them
    // watched. This refuses ONLY the provable overrun, so nothing that compiles today stops
    // compiling; a variable offset (`a + i`) and a bare pointer stay outside, where there is no
    // declared length to check against and pretending otherwise would invent a bound.
    if let Expr::Call(callee, args) = e
        && let Expr::Var(cname) = callee.as_ref()
        && let Some(width) = raw_access_bytes(cname)
        && let Some(first) = args.first()
        && let Some((buf, off)) = const_offset_from_buffer(&first.value)
        && let Some(&words) = sizes.get(&buf)
    {
        let total = words.saturating_mul(8);
        if off < 0 || off.saturating_add(width) > total {
            let last = (total - width).max(0);
            return Err(format!(
                "function `{fname}`: `{cname}` at offset {off} touches {width} byte(s) of `{buf}`, \
                 which is {total} bytes long, so this access is out of bounds (offsets 0..={last} \
                 reach inside it)"
            ));
        }
    }
    if let Expr::Index(base, idx) = e
        && let Expr::Var(a) = base.as_ref()
    {
        if let Some(&n) = sizes.get(a) {
            match const_index(idx) {
                // A compile-time-constant index outside [0, N) is a static stack overrun.
                Some(k) if k < 0 || k >= n => {
                    return Err(format!(
                        "function `{fname}`: index `{}` (= {k}) is out of bounds for `{a}` of size {n} (valid range 0..{n}) at line {line}",
                        expr_to_string(idx)
                    ));
                }
                // A constant in [0, N) is statically safe.
                Some(_) => {}
                // Checked-by-default: a VARIABLE index must be PROVEN in `[0, N)` (from a
                // `requires`, a `where`, branch guards, or a loop condition + counter floor).
                None => {
                    // VARIABLE index: prove it in bounds.
                    // For a local slice, try the field-based bound first (matches `for i < s.len`).
                    // Then always try the constant-sized bound (from `sizes`), which works for both
                    // constant-len slices and regular arrays. If either proves it, accept it.
                    let mut proved = false;
                    if let Some(bound) = flow.slices.get(a) {
                        // Slice: try field-based bound first (canonical `for i < s.len` form).
                        if crate::verify::index_proven_in_bounds(
                            idx, bound, &flow.assumptions(), params,
                        ) {
                            proved = true;
                        }
                    }
                    if crate::verify::index_proven_in_bounds(
                        idx, &Expr::Int(n), &flow.assumptions(), params,
                    ) {
                        proved = true;
                    }
                    // A derived interval confines the index (bit >> 3, i / 8, x & 7) — check via
                    // expr_range as an early exit.
                    if !proved {
                        let in_range =
                            matches!(expr_range(idx, flow), Some((lo, hi)) if lo >= 0 && hi < n);
                        if in_range {
                            proved = true;
                        }
                    }
                    if !proved {
                        return Err(format!(
                            "function `{fname}`: cannot prove `{}` is in bounds for `{a}` of size {n} at line {line}; bound it with a `where`/`requires`/loop condition to 0..{n}",
                            expr_to_string(idx)
                        ));
                    }
                }
            }
        } else if is_slice_param(a, params) {
            // A slice has no static size; its bound is the runtime `s.len`. Every index (constant
            // or variable) must be PROVEN in `[0, s.len)`.
            let bound = Expr::Field(Box::new(Expr::Var(a.clone())), "len".to_string());
            let asm = flow.assumptions();
            if !crate::verify::index_proven_in_bounds(idx, &bound, &asm, params) {
                return Err(format!(
                    "function `{fname}`: cannot prove `{}` is in bounds for slice `{a}` at line {line}; bound it with a `where`/`requires`/loop condition to 0..{a}.len",
                    expr_to_string(idx)
                ));
            }
        } else if let Some(bound) = flow.slices.get(a) {
            // A locally-bound slice: same treatment as a slice parameter, but the length expression
            // comes from the Flow map (stored when the `let` was processed), not from the params list.
            let asm = flow.assumptions();
            if !crate::verify::index_proven_in_bounds(idx, bound, &asm, params) {
                return Err(format!(
                    "function `{fname}`: cannot prove `{}` is in bounds for slice `{a}` at line {line}; bound it with a `where`/`requires`/loop condition to 0..{a}.len",
                    expr_to_string(idx)
                ));
            }
        } else if let Some(len) = view_len_of(a, params).or_else(|| flow.views.get(a).cloned()) {
            // A view is one word: a base address whose length lives elsewhere. `u32[n]` names the
            // parameter holding it, and the index is proven against THAT name, so bounding it by
            // another buffer's length proves nothing and is rejected — which is the mistake worth
            // catching, since both are just an `i64` at the call site.
            let Some(len) = len else {
                return Err(format!(
                    "function `{fname}`: view `{a}` has no declared length at line {line}, so an index into it \
                     cannot be checked; declare it as `T[n]`, naming the parameter or local that \
                     carries its length"
                ));
            };
            // A literal length is a constant bound; a name is the variable carrying it.
            let bound = match len.parse::<i64>() {
                Ok(n) => Expr::Int(n),
                Err(_) => Expr::Var(len.clone()),
            };
            let asm = flow.assumptions();
            if !crate::verify::index_proven_in_bounds(idx, &bound, &asm, params) {
                return Err(format!(
                    "function `{fname}`: cannot prove `{}` is in bounds for view `{a}` at line {line}; bound it with a `where`/`requires`/loop condition to 0..{len}",
                    expr_to_string(idx)
                ));
            }
        }
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            bounds_expr(l, fname, sizes, flow, params, funcs, line)?;
            bounds_expr(r, fname, sizes, flow, params, funcs, line)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            bounds_expr(x, fname, sizes, flow, params, funcs, line)
        }
        Expr::Call(c, args) => {
            bounds_expr(c, fname, sizes, flow, params, funcs, line)?;
            for a in args {
                bounds_expr(&a.value, fname, sizes, flow, params, funcs, line)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                bounds_expr(it, fname, sizes, flow, params, funcs, line)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            // A `Slice` that promises more elements than its backing array holds is refused
            // HERE, where it is built, because after this point nothing looks at the promise
            // again: the checker clears `s[i]` against the `len` field, and a `len` of 9 over
            // four words reads -- and writes -- somebody else's stack in silence.
            //
            // Only the case both halves of which are known: `ptr` is the name of an array whose
            // size was recorded by `let a[N]`, and `len` is a constant. A length that comes from
            // a variable, or a pointer with no known size, is not decided either way.
            let mut apuntado: Option<&str> = None;
            let mut prometido: Option<i64> = None;
            for (campo, valor) in fields {
                if campo == "ptr" {
                    if let Expr::Var(v) = valor.as_ref() {
                        apuntado = Some(v.as_str());
                    }
                } else if campo == "len" {
                    prometido = const_index(valor);
                }
            }
            if let (Some(ptr), Some(len)) = (apuntado, prometido)
                && let Some(&n) = sizes.get(ptr)
                && len > n
            {
                return Err(format!(
                    "function `{fname}`: slice length {len} is more than `{ptr}` holds (size {n}) at line {line}"
                ));
            }
            for (_, v) in fields {
                bounds_expr(v, fname, sizes, flow, params, funcs, line)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            bounds_expr(cond, fname, sizes, flow, params, funcs, line)?;
            // Each branch reasons under its own guard on a clone; after the `if`, any variable a
            // branch reassigned is unknown.
            let mut then_flow = flow.clone();
            then_flow.facts.push(cond.as_ref().clone());
            let mut t = sizes.clone();
            bounds_block(then_branch, fname, &mut t, &mut then_flow, params, funcs, line)?;
            if let Some(eb) = else_branch {
                let mut else_flow = flow.clone();
                if let Some(neg) = negate_cond(cond) {
                    else_flow.facts.push(neg);
                }
                let mut e2 = sizes.clone();
                bounds_block(eb, fname, &mut e2, &mut else_flow, params, funcs, line)?;
            }
            let mut modified = HashSet::new();
            // Only consider assignments from branches that actually fall through: a branch
            // ending in `continue`/`break`/`return` diverges, so its writes are local to that
            // path and do NOT escape to the continuation after the `if`.
            if block_diverges(then_branch) {
                // then-branch diverges: only consider else-branch assignments
                if let Some(eb) = else_branch {
                    assigned_vars(eb, &mut modified);
                }
            } else {
                assigned_vars(then_branch, &mut modified);
                if let Some(eb) = else_branch {
                    assigned_vars(eb, &mut modified);
                }
            }
            for v in &modified {
                flow.invalidate(v);
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            bounds_expr(scrutinee, fname, sizes, flow, params, funcs, line)?;
            for a in arms {
                let mut m = sizes.clone();
                let mut arm_flow = flow.clone();
                bounds_block(&a.body, fname, &mut m, &mut arm_flow, params, funcs, line)?;
            }
            let mut modified = HashSet::new();
            for a in arms {
                assigned_vars(&a.body, &mut modified);
            }
            for v in &modified {
                flow.invalidate(v);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Renders an index expression into a short human-readable string for error messages.
/// Covers the common cases (`Var`, `Int`, `Binary`); falls back to the `Debug` output for
/// anything more complex.
fn expr_to_string(e: &Expr) -> String {
    match e {
        Expr::Var(v) => v.clone(),
        Expr::Int(n) => n.to_string(),
        Expr::Unary(UnOp::Neg, x) => format!("-{}", expr_to_string(x)),
        Expr::Binary(op, l, r) => {
            let sym = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
                BinOp::LShr => ">>>",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                BinOp::Implies => "implies",
                BinOp::In => "in",
            };
            format!("{} {sym} {}", expr_to_string(l), expr_to_string(r))
        }
        other => format!("{other:?}"),
    }
}

/// Folds a compile-time-constant integer index (`Int`, unary negation, and `+`/`-`/`*` of
/// constants), or `None` if it is not constant. Overflow is not our concern (it is caught by
/// `check_constant_overflow`), so folding wraps and always yields a value.
fn const_index(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::Unary(UnOp::Neg, x) => Some(const_index(x)?.wrapping_neg()),
        Expr::Binary(op, l, r) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
            let (a, b) = (const_index(l)?, const_index(r)?);
            Some(match op {
                BinOp::Add => a.wrapping_add(b),
                BinOp::Sub => a.wrapping_sub(b),
                _ => a.wrapping_mul(b),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize, tokenize_located};
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let structs = p.take_structs();
        check_struct_mutability(&funcs, &structs)
    }

    fn check_match(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let enums = p.take_enums();
        check_enum_matches(&funcs, &enums)
    }

    fn literals(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_string_literal_types(&funcs, &globals)
    }

    fn bounds(src: &str) -> Result<(), String> {
        let (toks, lines, _cols) = tokenize_located(src);
        let mut p = Parser::with_lines(toks, lines);
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_constant_bounds(&funcs, &globals)
    }

    // ---------------------------------------------------------------------------
    // Bounds-check errors carry the exact source line of the index expression.
    // The line is the one where the index operator `[` appears, for every flavour
    // of statement (tail, let-value, assign-target).
    // ---------------------------------------------------------------------------

    #[test]
    fn bounds_error_tail_expr_in_function_body_reports_line_of_access() {
        // Lines:
        //   1  fn f(i: i64) -> i64 {
        //   2      let a[4]
        //   3      a[i]           ← tail of function body, at line 3
        //   4  }
        // `a[i]` is the tail of the function body → its line IS the block expression tail.
        let err = bounds(
            "fn f(i: i64) -> i64 {\n\
             let a[4]\n\
             a[i]\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("is in bounds")
                && err.contains("at line 3"),
            "tail expression must report its own line, got: {err}"
        );
    }

    #[test]
    fn bounds_error_tail_expr_explicit_multiline_reports_true_line() {
        // Lines:
        //   1  fn f(i: i64) -> i64 {
        //   2      let mut a[4]
        //   3      a[f]    ← the failing index (param `f` is unconstrained)
        //   4  }
        let err = bounds(
            "fn f(i: i64) -> i64 {\n\
             let mut a[4]\n\
             a[f]\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("is in bounds")
                && err.contains("at line 3"),
            "multi-line tail block must report the actual expression line, got: {err}"
        );
    }

    #[test]
    fn bounds_error_in_let_value_reports_line_of_value_expression() {
        // src:
        //   1  fn f(i: i64) -> i64 {
        //   2      let a[4]
        //   3      let b: i64 = a[i]   ← index in let-value, at line 3
        //   4      0
        //   5  }
        let err = bounds(
            "fn f(i: i64) -> i64 {\n\
             let a[4]\n\
             let b: i64 = a[i]\n\
             0\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("is in bounds")
                && err.contains("at line 3"),
            "let-value index should report line 3, got: {err}"
        );
    }

    #[test]
    fn bounds_error_in_assign_target_reports_line_of_target_expression() {
        // Lines:
        //   1  fn f(i: i64) -> i64 {
        //   2      let a[4]
        //   3      a[i] = 0       ← index in assign-target, at line 3
        //   4      a[0]
        //   5  }
        let err = bounds(
            "fn f(i: i64) -> i64 {\n\
             let a[4]\n\
             a[i] = 0\n\
             a[0]\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("is in bounds")
                && err.contains("at line 3"),
            "assign target should report line 3, got: {err}"
        );
    }

    #[test]
    fn bounds_error_in_for_body_tail_reports_actual_line() {
        // Verify that the for-body tail expression's line is correct in stmt_lines.
        // src:
        //   1  let mut a[4]
        //   2  fn f(i: i64) -> i64 {
        //   3      let mut j = 0
        //   4      for j < 10 { j = j + 1
        //   5      a[j] }           ← tail of for body, at line 5
        //   6      0
        //   7  }
        let err = bounds(
            "let mut a[4]\n\
             fn f(i: i64) -> i64 {\n\
             let mut j = 0\n\
             for j < 10 { j = j + 1\n\
             a[j] }\n\
             0\n\
             }",
        )
        .unwrap_err();
        // With the fix, the for body's tail `a[j]` is tracked with line 5 in stmt_lines.
        // a[j] is out of bounds for size 4 (j can be 4..9 from the loop condition),
        // so we get "cannot prove is in bounds at line 5".
        let found_line = extract_line(&err);
        assert!(found_line.is_some(), "expected a line number in error: {err}");
        assert!(found_line == Some(5), "for-body tail should report line 5, got: {err}");
    }

    fn extract_line(err: &str) -> Option<u32> {
        err.split("at line ")
            .nth(1)
            .and_then(|s| s.split(';').next())
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.trim().parse().ok())
    }

    #[test]
    fn a_parameter_hides_a_module_array_that_happens_to_share_its_name() {
        // A program declared `let mut ptrs[8]` and a LIBRARY function it called had a parameter
        // named `ptrs`. The checker keys array sizes by name and seeded them from the globals,
        // so inside that function the parameter inherited the caller's size of 8, and indexing
        // it was reported as unprovable -- in a file the programmer had not written, about a
        // bound they could not add, suggesting a `where` on somebody else's signature.
        //
        // The only fix available was to rename their own variable, and merely INCLUDING the
        // library was enough to trigger it. A parameter shadows a global in every other part of
        // the language; it has to here too, or a name chosen in one program silently changes
        // what can be proved in another.
        let src = "let mut ptrs[8]\n\
                   fn cargar(ptrs: i64, n: i64) -> i64 {\n\
                       let mut i = 0\n\
                       for i < n { ptrs[i] = i  i = i + 1 }\n\
                       0\n\
                   }";
        assert!(
            bounds(src).is_ok(),
            "a parameter is not the module array of the same name"
        );
    }

    #[test]
    fn a_module_array_is_still_bounded_where_it_is_not_shadowed() {
        // The other half: shadowing must not become a way to lose the check. A function with no
        // parameter of that name still indexes the real array, and an index past its end is
        // still an error.
        let src = "let mut ptrs[8]\n\
                   fn escribir() -> i64 { ptrs[9] = 1  0 }";
        assert!(
            bounds(src).is_err(),
            "the real module array keeps its size where nothing shadows it"
        );
    }

    #[test]
    fn rejects_constant_index_past_a_known_array_size() {
        // `a[5]` into a 4-element array is a provable out-of-bounds (a stack overrun), reject.
        let err = bounds("fn f() -> i64 { let a[4]\na[5] = 0\na[0] }").unwrap_err();
        assert!(
            err.contains("out of bounds") && err.contains("5"),
            "should name the constant index and the size; got: {err}"
        );
    }

    #[test]
    fn rejects_a_negative_constant_index() {
        let err = bounds("fn f() -> i64 { let a[4]\na[0 - 1] }").unwrap_err();
        assert!(err.contains("out of bounds"), "got: {err}");
    }

    #[test]
    fn rejects_index_equal_to_the_size() {
        // Off-by-one: valid indices are 0..N, so `a[N]` is out of range.
        assert!(bounds("fn f() -> i64 { let a[8]\na[8] }").is_err());
    }

    #[test]
    fn accepts_a_constant_index_in_bounds() {
        assert!(bounds("fn f() -> i64 { let a[4]\na[3] = 7\na[0] }").is_ok());
    }

    #[test]
    fn accepts_a_constant_index_from_arithmetic_in_bounds() {
        assert!(bounds("fn f() -> i64 { let a[4]\na[2 + 1] }").is_ok());
    }

    #[test]
    fn propagates_a_callees_ensures_to_bound_an_indexed_return() {
        // `alloc()` guarantees `0 <= r < 4` of its return; after `let i = alloc()` the index
        // `t[i]` must prove in bounds FROM that postcondition, the whole point of a return
        // contract is that the caller may assume it, so a bounded allocator index need not be
        // re-stated at every call site. (This is the `fu_alloc`/`fu_find` pattern.)
        let src = "let mut t[4]\n\
                   fn alloc() -> (r: i64) ensures { r >= 0; r < 4 } { 2 }\n\
                   fn use_it() { let i = alloc()\nt[i] = 1 }\n\
                   fn main() -> i64 { use_it()\n0 }";
        assert!(
            bounds(src).is_ok(),
            "callee ensures should bound the return"
        );
    }

    #[test]
    fn a_nonnegative_global_counter_bounds_an_index_below() {
        // `count` starts at 0 and is only ever incremented, so it is `>= 0` everywhere, a
        // whole-module invariant the per-function walk cannot see. With the upper bound from the
        // `if count < 4` guard, `t[count]` proves in bounds with NO hand-written `count >= 0`.
        let src = "let mut count = 0\n\
                   let mut t[4]\n\
                   fn bump() { if count < 4 { t[count] = 1\ncount = count + 1 } }\n\
                   fn main() -> i64 { bump()\n0 }";
        assert!(
            bounds(src).is_ok(),
            "nonneg global counter should give the lower bound"
        );
    }

    #[test]
    fn an_if_expression_clamp_bounds_an_index() {
        // `let i = if x >= 8 { 7 } else { x }` with `x >= 0` clamps `i` to [0, 7]: the then-branch
        // is the constant 7, the else-branch is `x` under `x < 8` (and `x >= 0` from requires). The
        // index range is the UNION of the two branch tails, each computed under its own guard.
        let src = "let mut t[8]\n\
                   fn at(x: i64) requires { x >= 0 } { let i = if x >= 8 { 7 } else { x }\nt[i] = 1 }\n\
                   fn main() -> i64 { at(3)\n0 }";
        assert!(
            bounds(src).is_ok(),
            "if-expression clamp should bound the index"
        );
    }

    #[test]
    fn a_modulo_of_a_nonnegative_global_bounds_a_ring_index() {
        // `head` is a nonneg counter (0, only incremented), so `head % 16` is in [0, 16), the
        // ring-buffer index. The modulo clamps however large `head` grows, so no upper bound on
        // `head` is needed; only its floor (>= 0), which the nonneg-global analysis supplies.
        let src = "let mut head = 0\n\
                   let mut ring[16]\n\
                   fn push(v: i64) { let i = head % 16\nring[i] = v\nhead = head + 1 }\n\
                   fn main() -> i64 { push(7)\n0 }";
        assert!(
            bounds(src).is_ok(),
            "modulo of a nonneg global is a valid ring index"
        );
    }

    #[test]
    fn a_global_that_can_go_negative_is_not_assumed_nonnegative() {
        // `count` is decremented in one function, so it is NOT provably `>= 0`; even under the
        // `count < 4` guard the index must be rejected (a negative index is a real overrun).
        let src = "let mut count = 0\n\
                   let mut t[4]\n\
                   fn dec() { count = count - 1 }\n\
                   fn use_it() { if count < 4 { t[count] = 1 } }\n\
                   fn main() -> i64 { use_it()\n0 }";
        assert!(
            bounds(src).is_err(),
            "a decremented global is not provably nonneg"
        );
    }

    #[test]
    fn a_guard_bounded_global_counter_bounds_a_loop_index() {
        // `cnt` is only ever incremented UNDER the guard `if cnt < 8`, so it never exceeds 8, a
        // whole-program ceiling. Combined with its non-negative floor, the loop `for i < cnt`
        // indexes `buf[8]` in range (`i < cnt <= 8`) with NO hand-written bound.
        let src = "let mut cnt = 0\n\
                   let mut buf[8]\n\
                   fn push(v: i64) { if cnt < 8 { buf[cnt] = v\ncnt = cnt + 1 } }\n\
                   fn sum() -> i64 { let mut i = 0\nlet mut s = 0\nfor i < cnt { s = s + buf[i]\ni = i + 1 }\ns }\n\
                   fn main() -> i64 { push(1)\nsum() }";
        assert!(
            bounds(src).is_ok(),
            "a guard-bounded global counter should bound the loop"
        );
    }

    #[test]
    fn a_counting_range_bounds_the_index_of_a_view_with_no_guard() {
        // `for i in 0..n` over a view declared `u32[n]` needs NO hand-written guard: the loop
        // condition the range desugars to (`i < n`) is what the prover reads, and it is exactly
        // the view's length. This is the canonical way to walk a view, so it is pinned here.
        let src = "fn main() -> i64 { let n = 8\n\
                   let px: u32[n] = heap_alloc(n * 4)\n\
                   for i in 0..n { px[i] = i }\n0 }\n\
                   fn heap_alloc(n: i64) -> i64 { n }";
        assert!(
            bounds(src).is_ok(),
            "the range itself should bound the index"
        );
    }

    #[test]
    fn a_counting_range_past_the_length_is_still_refused() {
        // The other half, and the one that gives the test above its meaning: if `0..n+3` were
        // accepted too, "no guard needed" would only mean the prover reads nothing. It must
        // reject the overrun for the acceptance above to be evidence of anything.
        let src = "fn main() -> i64 { let n = 8\n\
                   let px: u32[n] = heap_alloc(n * 4)\n\
                   for i in 0..n+3 { px[i] = i }\n0 }\n\
                   fn heap_alloc(n: i64) -> i64 { n }";
        assert!(
            bounds(src).is_err(),
            "walking past the declared length must not compile"
        );
    }

    #[test]
    fn an_unguarded_global_increment_has_no_ceiling() {
        // SOUNDNESS: one UNGUARDED `cnt = cnt + 1` anywhere means `cnt` can grow without bound, so
        // `for i < cnt { buf[i] }` could overrun `buf[8]` and MUST be rejected, no ceiling is
        // inferred when any increment escapes a guard.
        let src = "let mut cnt = 0\n\
                   let mut buf[8]\n\
                   fn bump() { cnt = cnt + 1 }\n\
                   fn sum() -> i64 { let mut i = 0\nlet mut s = 0\nfor i < cnt { s = s + buf[i]\ni = i + 1 }\ns }\n\
                   fn main() -> i64 { bump()\nsum() }";
        assert!(
            bounds(src).is_err(),
            "an unguarded increment leaves the counter unbounded"
        );
    }

    #[test]
    fn a_ceiling_larger_than_the_array_is_still_rejected() {
        // SOUNDNESS: the guard is `cnt < 100`, so the inferred ceiling is 100, larger than the
        // 8-element array, so `for i < cnt { buf[i] }` is still an overrun and must be rejected.
        let src = "let mut cnt = 0\n\
                   let mut buf[8]\n\
                   fn push(v: i64) { if cnt < 100 { cnt = cnt + 1 } }\n\
                   fn sum() -> i64 { let mut i = 0\nlet mut s = 0\nfor i < cnt { s = s + buf[i]\ni = i + 1 }\ns }\n\
                   fn main() -> i64 { push(1)\nsum() }";
        assert!(
            bounds(src).is_err(),
            "a ceiling above the array size is not in bounds"
        );
    }

    #[test]
    fn a_guard_clause_early_return_refines_the_continuation() {
        // `if fi < 0 { return }` means any code AFTER it runs only when `fi >= 0`. Combined with
        // an upper bound from the callee's `ensures`, that lower bound proves the index. This is
        // the `let fi = fu_alloc(); if fi < 0 { return } used[fi]` sentinel-allocator pattern.
        let src = "let mut used[64]\n\
                   fn alloc() -> (r: i64) ensures { r < 64 } { 5 }\n\
                   fn open() { let fi = alloc()\nif fi < 0 { return }\nused[fi] = 1 }\n\
                   fn main() -> i64 { open()\n0 }";
        assert!(
            bounds(src).is_ok(),
            "early-return guard should establish fi >= 0"
        );
    }

    #[test]
    fn without_the_guard_the_lower_bound_stays_unproven() {
        // Same as above but WITHOUT the `if fi < 0 { return }` guard: the callee only promises
        // `r < 64`, so `fi` could be negative and the index is correctly rejected.
        let src = "let mut used[64]\n\
                   fn alloc() -> (r: i64) ensures { r < 64 } { 5 }\n\
                   fn open() { let fi = alloc()\nused[fi] = 1 }\n\
                   fn main() -> i64 { open()\n0 }";
        assert!(bounds(src).is_err(), "no lower bound without the guard");
    }

    #[test]
    fn does_not_propagate_an_ensures_mentioning_an_unmapped_parameter() {
        // With a KEYWORD argument the parameters are not positionally mapped, so an `ensures`
        // that mentions a parameter must NOT be assumed (the bare param name could alias an
        // unrelated caller variable), the index stays unproven. Soundness guard.
        let src = "let mut t[4]\n\
                   fn pick(n: i64) -> (r: i64) ensures { r < n } { 0 }\n\
                   fn use_it() { let i = pick(n=4)\nt[i] = 1 }\n\
                   fn main() -> i64 { use_it()\n0 }";
        assert!(
            bounds(src).is_err(),
            "must not assume an unmapped-param ensures"
        );
    }

    #[test]
    fn a_loop_bounded_by_a_global_constant_proves() {
        // A module-level `let N = 64` used as a loop bound over an array of the same size (the
        // idiom `for i < TABLE_SIZE { table[i] }`) must prove: the global constant is resolved to
        // its value, so `i < 64` bounds `table[64]`.
        let src = "let N = 8\nfn f() -> i64 { let a[8]\nlet mut i = 0\nfor i < N { a[i] = 0\ni = i + 1 }\n0 }";
        assert!(
            bounds(src).is_ok(),
            "a loop bounded by a global const should prove"
        );
    }

    #[test]
    fn rejects_an_unproven_variable_index() {
        // Checked-by-default: an index that is not PROVEN in range is a compile error.
        let err = bounds("fn f(i: i64) -> i64 { let a[4]\na[i] }").unwrap_err();
        // The WHOLE message, not two loose fragments: it is the message that has to name the
        // index (`i`), the array, its size and the line, and a test that only checks the two ends
        // would stay green if the middle went away.
        assert!(
            err.contains(
                "function `f`: cannot prove `i` is in bounds for `a` of size 4 at line 2"
            ),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_a_variable_index_a_precondition_forces_out_of_bounds() {
        // `requires { i >= 4 }` makes `a[i]` into a size-4 array unprovable (in fact disproven).
        let err = bounds("fn f(i: i64) -> i64 requires { i >= 4 } { let a[4]\na[i] }").unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn rejects_a_variable_index_inside_a_branch_that_does_not_bound_it() {
        // The `i >= 4` guard does not prove `i < 4`, so `a[i]` in that branch is rejected.
        let err =
            bounds("fn f(i: i64) -> i64 { let a[4]\nif i >= 4 { a[i] } else { 0 } }").unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn accepts_a_variable_index_proven_in_bounds() {
        // `0 <= i < 4`: the access is provably safe.
        assert!(
            bounds("fn f(i: i64) -> i64 requires { i >= 0; i < 4 } { let a[4]\na[i] }").is_ok()
        );
    }

    #[test]
    fn accepts_a_variable_index_proven_by_a_loop() {
        // A counting loop bounds the index: the condition gives `i < 4`, the invariant `i >= 0`.
        assert!(bounds(
            "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 4 invariant { i >= 0 } { a[i] = 0\ni = i + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn accepts_a_counting_loop_with_no_invariant_clause() {
        // The floor comes from the constant init `i = 0` (a proven monotonic counter), the upper
        // bound from the loop condition. No user `invariant` is needed or trusted.
        assert!(
            bounds(
                "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 4 { a[i] = 0\ni = i + 1 }\n0 }"
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_a_counting_loop_from_a_nonzero_constant_start() {
        // `i` starts at 2 and only steps up, so `i >= 2`; the condition gives `i < 6`. Both bound
        // `a[i]` in a size-6 array.
        assert!(
            bounds(
                "fn f() -> i64 { let a[6]\nlet mut i = 2\nfor i < 6 { a[i] = 0\ni = i + 1 }\n0 }"
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_a_loop_whose_invariant_lies_about_the_counter() {
        // SOUNDNESS: a user `invariant { i < 4 }` on a loop that actually runs `i` up to 10 must NOT
        // be trusted, writing `a[i]` into a size-4 array is out of bounds for i in 4..10. The
        // condition only proves `i < 10`, which does not bound a size-4 array, so this is rejected.
        let err = bounds(
            "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 10 invariant { i >= 0; i < 4 } { a[i] = i\ni = i + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn rejects_an_index_after_the_counter_is_incremented() {
        // Flow-sensitive: after `i = i + 1` the condition `i < 4` no longer holds (i could be 4), so
        // the access is rejected even though the loop condition is `i < 4`.
        let err = bounds(
            "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 4 { i = i + 1\na[i] = 0 }\n0 }",
        )
        .unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn accepts_a_shift_index_from_a_decrementing_counter() {
        // The x25519 / ed25519 idiom: `bit` counts DOWN from a constant under `for bit >= 0`, and
        // the array is indexed by `bit >> 3` (= bit/8). The ceiling (bit <= 255) and the condition
        // (bit >= 0) confine `bit >> 3` to [0, 31], in bounds for a size-32 array.
        assert!(bounds(
            "fn f() -> i64 { let exp[32]\nlet mut acc = 0\nlet mut bit = 255\nfor bit >= 0 { acc = acc + exp[bit >> 3]\nif bit == 0 { acc = acc }\nbit = bit - 1 }\nacc }"
        )
        .is_ok());
    }

    #[test]
    fn accepts_a_division_index_bounded_by_the_counter() {
        // `a[i / 8]` with `i` a counter in [0, 64) lands in [0, 8), the hexdump idiom.
        assert!(bounds(
            "fn f() -> i64 { let a[8]\nlet mut i = 0\nfor i < 64 { a[i / 8] = 0\ni = i + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn accepts_an_index_from_a_masked_variable() {
        // `j = (j + 1) & 0xFF` confines `j` to [0, 255]; `S[j]` is then in bounds for a size-256
        // array, the RC4 permutation idiom, where the index lives in a masked variable.
        assert!(bounds(
            "fn f() -> i64 { let mut S[256]\nlet mut j = 0\nlet mut i = 0\nfor i < 10 { j = (j + 1) & 0xFF\nS[j] = i\ni = i + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn rejects_a_division_index_that_can_exceed_the_array() {
        // `a[i / 2]` with `i` in [0, 64) reaches 31, out of bounds for a size-8 array.
        let err = bounds(
            "fn f() -> i64 { let a[8]\nlet mut i = 0\nfor i < 64 { a[i / 2] = 0\ni = i + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn accepts_an_index_through_a_linear_let_binding() {
        // `let t = 16 + j` lets `W[t - 16]` reduce to `W[j]` and `W[t]` to `W[16 + j]` (j the
        // counter): the SHA-256 message schedule, which indexes via a derived offset.
        assert!(bounds(
            "fn f() -> i64 { let W[64]\nlet mut j = 0\nfor j < 48 { let t = 16 + j\nW[t] = W[t - 16]\nj = j + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn accepts_a_counter_carried_across_sequential_loops() {
        // A break-free `for t < 16` exits only when `t >= 16`, so that floor carries into a second
        // loop reusing `t`; `W[t - 16]` is then provable, the SHA message-schedule idiom.
        assert!(bounds(
            "fn f() -> i64 { let W[80]\nlet mut t = 0\nfor t < 16 { W[t] = 0\nt = t + 1 }\nfor t < 80 { W[t] = W[t - 16]\nt = t + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn does_not_assume_the_negated_condition_after_a_loop_with_break() {
        // SOUNDNESS: a `break` may exit the loop while `t < 16` still holds, so `t >= 16` is NOT a
        // sound post-loop fact, the second loop's `W[t - 16]` (t could be small) is rejected.
        let err = bounds(
            "fn f() -> i64 { let W[80]\nlet mut t = 0\nfor t < 16 { if t > 5 { break }\nt = t + 1 }\nfor t < 80 { W[t - 16] = 0\nt = t + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn accepts_a_modulo_index_confined_to_the_array() {
        // `key[i % 3]` into a size-3 array: `i % 3` ∈ [0, 3) since the counter gives `i >= 0`.
        assert!(bounds(
            "fn f() -> i64 { let key[3]\nkey[0] = 1\nlet mut i = 0\nfor i < 256 { key[i % 3] = 0\ni = i + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn rejects_a_modulo_index_whose_modulus_exceeds_the_array() {
        // `a[i % 8]` into a size-4 array: `i % 8` can reach 7, out of bounds.
        let err = bounds(
            "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 256 { a[i % 8] = 0\ni = i + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn accepts_a_bitmask_index_confined_to_the_array() {
        // `a[i & 7]` ∈ [0, 8) for any `i`: a power-of-two-minus-one mask, no sign obligation.
        assert!(bounds("fn f(i: i64) -> i64 { let a[8]\na[i & 7] = 0\na[0] }").is_ok());
    }

    #[test]
    fn rejects_a_bitmask_index_wider_than_the_array() {
        // `a[i & 15]` can reach 15, out of bounds for a size-8 array.
        let err = bounds("fn f(i: i64) -> i64 { let a[8]\na[i & 15] = 0\na[0] }").unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn rejects_a_modulo_index_without_a_nonnegative_dividend() {
        // `a[i % 3]` with an unconstrained `i` (could be negative → `i % 3` negative) is rejected.
        let err = bounds("fn f(i: i64) -> i64 { let a[4]\na[i % 3] = 0\na[0] }").unwrap_err();
        assert!(err.contains("cannot prove"), "got: {err}");
    }

    #[test]
    fn rejects_a_slice_index_not_proven_within_the_length() {
        let err = bounds("fn f(s: Slice, i: i64) -> i64 { s[i] }").unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("slice"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_a_slice_index_proven_within_the_length() {
        assert!(bounds("fn f(s: Slice, i: i64 where 0 <= i && i < s.len) -> i64 { s[i] }").is_ok());
    }

    #[test]
    fn rejects_indexing_a_view_that_never_says_how_long_it_is() {
        // A view is one word: a base address over memory the caller owns. With no length there is
        // nothing to prove an index against, and the compiler used to accept ANY index and emit a
        // program that walked off the buffer. Rejecting is the whole point of the language; the
        // message must name what is MISSING (the length), not blame the index.
        let err = bounds("fn w(v: u32[], i: i64) { v[i] = 1 }").unwrap_err();
        assert!(
            err.contains("length") && err.contains("v"),
            "the error must say the view has no declared length, got: {err}"
        );
    }

    #[test]
    fn accepts_the_canonical_loop_over_a_view_with_no_annotation_at_all() {
        // `for i < n { px[i] = .. }` over a view declared `u32[n]` is THE loop every image pass
        // writes. If it needed a `requires` on top, nobody would use the checked form, and an
        // unused guarantee protects nothing.
        assert!(
            bounds(
                "fn fill(px: u32[n], n: i64) { let mut i = 0\nfor i < n { px[i] = 0\ni = i + 1 } }"
            )
            .is_ok(),
            "the canonical bounded loop must pass with no annotations"
        );
    }

    #[test]
    fn accepts_a_view_index_proven_against_its_own_declared_length() {
        assert!(
            bounds("fn get(px: u32[n], n: i64, i: i64 where 0 <= i && i < n) -> i64 { px[i] }")
                .is_ok()
        );
    }

    #[test]
    fn rejects_a_view_index_bounded_by_the_wrong_buffers_length() {
        // The real mistake, and the one a length-less view could never catch: two buffers, each
        // with its own length, and the index bounded against the OTHER one. `src` may be longer
        // than `dst`, so this write runs off the end of `dst`.
        let err = bounds(
            "fn copy(dst: u32[n], n: i64, src: u32[m], m: i64, i: i64 where 0 <= i && i < m) \
             { dst[i] = src[i] }",
        )
        .unwrap_err();
        assert!(
            err.contains("dst"),
            "an index bounded by another buffer's length must be rejected, got: {err}"
        );
    }

    #[test]
    fn rejects_a_variable_index_into_a_local_view_over_a_known_array() {
        // A view declared as a LOCAL over an array the compiler can measure: the length is not
        // missing at all, it is right there. `let backing[4]` is 32 bytes, so a `u32` view over it
        // holds 8 elements, and a loop to a million must be rejected exactly as it would be on the
        // array itself. Checking parameters but not locals would leave the same hole one keyword
        // away.
        let err = bounds(
            "fn f() -> i64 { let backing[4]\nlet a: u32[] = backing\n\
             let mut i = 0\nfor i < 1000000 { a[i] = 1\ni = i + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(
            err.contains("a") && err.contains("bounds"),
            "an unbounded index into a local view must be rejected, got: {err}"
        );
    }

    #[test]
    fn accepts_a_local_view_index_within_the_backing_arrays_size() {
        // The other side of it, and the one that decides whether anyone uses this: 4 words of
        // backing hold 8 `u32`s, so a loop to 8 is in bounds and must pass with no annotation.
        assert!(
            bounds(
                "fn f() -> i64 { let backing[4]\nlet a: u32[] = backing\n\
                 let mut i = 0\nfor i < 8 { a[i] = 1\ni = i + 1 }\n0 }"
            )
            .is_ok(),
            "a loop within the backing array's real capacity must pass"
        );
    }

    #[test]
    fn an_eight_bit_view_is_bounds_checked_in_bytes_not_words() {
        // The capacity of a view is counted in ITS elements: one word of backing holds eight
        // `u8`s, so a loop to 8 is in bounds and a loop to 9 is not. Counting in words would
        // reject seven eighths of a legitimate byte buffer; counting in bytes for a word view
        // would accept eight times too much.
        assert!(
            bounds(
                "fn f() -> i64 { let backing[1]\nlet b: u8[] = backing\n\
                 let mut i = 0\nfor i < 8 { b[i] = 1\ni = i + 1 }\n0 }"
            )
            .is_ok(),
            "eight bytes fit in one word of backing"
        );
        let err = bounds(
            "fn f() -> i64 { let backing[1]\nlet b: u8[] = backing\n\
             let mut i = 0\nfor i < 9 { b[i] = 1\ni = i + 1 }\n0 }",
        )
        .unwrap_err();
        assert!(
            err.contains("size 8"),
            "the bound is the byte capacity, got: {err}"
        );
    }

    #[test]
    fn rejects_indexing_a_local_view_whose_length_nobody_knows() {
        // A view over an address the compiler cannot reduce — an offset computed at runtime, an
        // allocator's return, a pointer from elsewhere — has no length to check against. (A
        // CONSTANT offset does: that case has its own test.) Silence here was worse than the
        // parameter case it mirrors — `w[3000]` compiled without a word — so the rule is the same
        // one: no known length, no indexing, and say which is missing.
        let err = bounds(
            "fn f(k: i64) -> i64 { let backing[2]\nlet w: u32[] = backing + k\nw[3000] = 1\n0 }",
        )
        .unwrap_err();
        assert!(
            err.contains("length") && err.contains("w"),
            "an unmeasurable local view must be refused, got: {err}"
        );
    }

    #[test]
    fn a_view_over_a_constant_offset_into_an_array_gets_the_capacity_that_is_left() {
        // Refusing everything the compiler could not measure was too blunt: `backing + 8` is
        // measurable. Two words of backing are 16 bytes, so a `u32` view starting 8 bytes in has
        // exactly 2 elements left — index 1 is fine and index 2 runs past the end. Rejecting the
        // good half would push people back to peek/poke, where nothing is checked at all.
        assert!(
            bounds("fn f() -> i64 { let backing[2]\nlet w: u32[] = backing + 8\nw[1] = 1\n0 }")
                .is_ok(),
            "the elements that fit after the offset must still be usable"
        );
        let err =
            bounds("fn f() -> i64 { let backing[2]\nlet w: u32[] = backing + 8\nw[2] = 1\n0 }")
                .unwrap_err();
        assert!(
            err.contains("size 2"),
            "the capacity is what is LEFT after the offset, got: {err}"
        );
    }

    #[test]
    fn a_literal_length_in_a_view_type_is_a_constant_bound() {
        // `u32[16]` writes the length down instead of naming a variable that carries it, and the
        // checker has to read it as the NUMBER 16 — treating it as a variable called "16" would
        // leave the prover with nothing to compare against, so the canonical loop would be
        // rejected and the spelling would be useless.
        // Over a raw address, so the ONLY length available is the one written in the type: a view
        // over a measurable array would be checked against that array instead and prove nothing
        // about this.
        assert!(
            bounds(
                "fn f(raw: i64) -> i64 { let buf: u32[16] = raw\n\
                 let mut i = 0\nfor i < 16 { buf[i] = 1\ni = i + 1 }\n0 }"
            )
            .is_ok(),
            "a loop to the written-down length must prove itself"
        );
        assert!(
            bounds(
                "fn f(raw: i64) -> i64 { let buf: u32[16] = raw\n\
                 let mut i = 0\nfor i < 17 { buf[i] = 1\ni = i + 1 }\n0 }"
            )
            .is_err(),
            "and one past it must not"
        );
    }

    #[test]
    fn rebinding_a_name_stops_treating_it_as_a_view() {
        // The mirror of the rule, and the kind of false positive that would make people avoid the
        // checked form: once a name is bound to something else, it is no longer a view, and
        // indexing it is an ordinary word index again. Remembering the old view would reject
        // perfectly good code with a message about a length that no longer applies to anything.
        assert!(
            bounds(
                "fn f() -> i64 { let backing[4]\nlet v: u32[] = backing + 8\n\
                 let v = backing\nv[1] = 1\n0 }"
            )
            .is_ok(),
            "a name rebound away from a view must index like any other address"
        );
    }

    #[test]
    fn accepts_a_local_view_that_declares_its_length() {
        // And the way out, which is the shape a heap buffer actually takes: name the local holding
        // the length, `u32[n]`, exactly as a parameter does. The canonical loop then proves itself.
        assert!(
            bounds(
                "fn f(n: i64) -> i64 { let raw = 0\nlet px: u32[n] = raw\n\
                 let mut i = 0\nfor i < n { px[i] = 1\ni = i + 1 }\n0 }"
            )
            .is_ok(),
            "a local view that names its length must be checkable, not refused"
        );
    }

    #[test]
    fn a_string_literal_cannot_be_taken_as_a_plain_integer() {
        // A literal is TEXT, and text has a type. Letting one decay into `i64` is what made
        // "walk it until a NUL turns up" the natural thing to write: with an address and no
        // length there is nothing else to do. Binding one to anything but `string` is refused,
        // and the message says which type it wanted.
        let err = literals("fn main() -> i64 { let p: i64 = \"hola\"\np }").unwrap_err();
        assert!(
            err.contains("string"),
            "the error must name the type a literal belongs to, got: {err}"
        );
        // The same through a call: a parameter that says `i64` does not accept text either.
        let err = literals("fn takes(p: i64) -> i64 { p }\nfn main() -> i64 { takes(\"hola\") }")
            .unwrap_err();
        assert!(
            err.contains("string"),
            "a literal argument needs a `string` parameter: {err}"
        );
    }

    #[test]
    fn a_string_literal_is_fine_where_a_string_is_expected() {
        // The other half, and the one that decides whether the rule is usable: a `string`
        // binding, a `string` parameter, and the console builtins that take a literal directly
        // all keep working.
        assert!(literals("fn main() -> i64 { let s: string = \"hola\"\ns.len }").is_ok());
        assert!(
            literals("fn takes(s: string) -> i64 { s.len }\nfn main() -> i64 { takes(\"hola\") }")
                .is_ok()
        );
        assert!(literals("fn main() -> i64 { print(\"hola\")\n0 }").is_ok());
    }

    #[test]
    fn a_string_variable_cannot_be_rebound_as_an_integer() {
        // The hole this closes: `l` is a string (code points, not bytes). Assigning it
        // to `i64` makes byte-level access look correct at compile time when it's actually
        // reading UTF-32 code points. The literal-level check already rejects `let p: i64 = "hola"`;
        // this rejects `let p: i64 = l` where `l` was declared `string`.
        let err = literals(
            r#"fn main() -> i64 {
                let l: string = "hello world"
                let p: i64 = l
                p
            }"#,
        )
        .unwrap_err();
        assert!(
            err.contains("string"),
            "a string variable cannot become a plain integer; error should mention 'string', got: {err}"
        );
    }

    #[test]
    fn a_string_variable_can_be_bound_to_another_string() {
        // Copying the reference while keeping the text type is fine.
        assert!(
            literals(
                r#"fn main() -> i64 {
                    let l: string = "hello"
                    let q: string = l
                    q.len
                }"#,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_string_variable_can_be_bound_as_utf8_bytes() {
        // `.utf8` asks explicitly for bytes; the parser substitutes the variable with the
        // original literal, so the checker sees a Field(Str, "utf8"), not a plain Var.
        assert!(
            literals(
                r#"fn main() -> i64 {
                    let l: string = "hola"
                    let p: i64 = l.utf8
                    p
                }"#,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_literal_utf8_is_fine_on_an_integer() {
        // Same mechanism on a bare literal — the parser substitutes the underlying bytes.
        assert!(literals("fn main() -> i64 { let p: i64 = \"hola\".utf8\np }").is_ok());
    }

    #[test]
    fn a_string_variable_is_fine_as_a_string_parameter() {
        // Passing a string variable to a function whose parameter is declared `string`.
        assert!(
            literals(
                r#"fn takes(s: string) -> i64 { s.len }
                fn main() -> i64 {
                    let l: string = "hello"
                    takes(l)
                }"#,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_byte_buffer_is_not_text_and_can_become_a_pointer() {
        // `u8[N]` is a buffer of bytes, not text: handing its base address to `i64` is the
        // ordinary way to pass it to a syscall. The rule above must not mistake it for a
        // `string` just because `.utf8` is also allowed to land in a byte view.
        assert!(
            literals(
                r#"fn main() -> i64 {
                    let mut buf: u8[16]
                    let p: i64 = buf
                    p
                }"#,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_string_variable_cannot_be_rebound_as_a_byte_view() {
        // The same lie in the other direction: `l` holds code points four bytes wide, so
        // reading it through a byte view yields UTF-32 read as UTF-8.
        assert!(
            literals(
                r#"fn main() -> i64 {
                    let l: string = "hello"
                    let b: u8[4] = l
                    b[0]
                }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn a_string_parameter_cannot_be_rebound_as_an_integer() {
        // A parameter declared `string` holds code points exactly like a local one does.
        assert!(
            literals(
                r#"fn first(s: string) -> i64 {
                    let p: i64 = s
                    p
                }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_a_constant_index_from_arithmetic_out_of_bounds() {
        assert!(bounds("fn f() -> i64 { let a[4]\na[2 + 3] }").is_err());
    }

    #[test]
    fn rejects_constant_index_past_a_module_level_array() {
        // A `.data` table (crypto S-box etc.) indexed by a constant out of range is caught too.
        let err = bounds("let K[4] = [1, 2, 3, 4]\nfn f() -> i64 { K[9] }").unwrap_err();
        assert!(
            err.contains("out of bounds") && err.contains("K"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_constant_index_in_a_module_level_array() {
        assert!(bounds("let K[4] = [1, 2, 3, 4]\nfn f() -> i64 { K[3] }").is_ok());
    }

    #[test]
    fn exhaustive_match_is_accepted_non_exhaustive_is_rejected() {
        let en = "enum Shape { Circle, Square, Triangle }\n";
        // All three variants handled: OK.
        assert!(
            check_match(&format!(
                "{en}fn f(s: Shape) -> i64 {{ match s {{ Circle => 0, Square => 4, Triangle => 3 }} }}"
            ))
            .is_ok()
        );
        // Missing `Triangle`: rejected, the guarantee.
        let err = check_match(&format!(
            "{en}fn f(s: Shape) -> i64 {{ match s {{ Circle => 0, Square => 4 }} }}"
        ))
        .unwrap_err();
        assert!(
            err.contains("non-exhaustive") && err.contains("Triangle"),
            "{err}"
        );
        // A repeated arm is rejected too.
        assert!(
            check_match(&format!(
                "{en}fn f(s: Shape) -> i64 {{ match s {{ Circle => 0, Circle => 1, Square => 4, Triangle => 3 }} }}"
            ))
            .is_err()
        );
    }

    #[test]
    fn mutating_a_value_struct_parameter_is_rejected() {
        // The footgun: `p` is passed by value, so mutating its field is a mistake.
        let err = check(
            "struct Point { x: i64, y: i64 }\n\
             fn bad(p: Point) { p.x = 5 }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap_err();
        assert!(
            err.contains("value struct `p`"),
            "names the value struct: {err}"
        );
        assert!(err.contains("*Point"), "hints the pointer fix: {err}");
    }

    #[test]
    fn mutating_a_pointer_struct_parameter_is_allowed() {
        // Declared `*Point` up front: a mutable, shared handle, mutation is fine.
        check(
            "struct Point { x: i64, y: i64 }\n\
             fn ok(p: *Point) { p.x = 5 }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap();
    }

    #[test]
    fn mutating_a_local_struct_is_allowed() {
        // The function CREATED `p`, so it may set its fields.
        check(
            "struct Point { x: i64, y: i64 }\n\
             fn main() -> i64 { let p: Point  p.x = 5  p.x }",
        )
        .unwrap();
    }

    fn check_kinds(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        check_receiver_kinds(&funcs)
    }

    #[test]
    fn a_struct_mixing_value_and_pointer_methods_is_rejected() {
        // `area` is a value method, `scale` a pointer method, not allowed.
        let err = check_kinds(
            "struct Rect { w: i64, h: i64 }\n\
             impl Rect {\n\
             fn area(self) -> i64 { self.w * self.h }\n\
             fn scale(self: *Rect) { self.w = self.w * 2 }\n\
             }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap_err();
        assert!(
            err.contains("value XOR pointer"),
            "explains the rule: {err}"
        );
    }

    #[test]
    fn a_struct_with_uniform_pointer_methods_is_accepted() {
        // All pointer receivers: consistent, so accepted.
        check_kinds(
            "struct Cnt { n: i64 }\n\
             impl Cnt {\n\
             fn inc(self: *Cnt) { self.n = self.n + 1 }\n\
             fn add(self: *Cnt, k: i64) { self.n = self.n + k }\n\
             }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap();
    }

    fn check_decls(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        check_declaration_kinds(&funcs)
    }

    #[test]
    fn a_value_method_type_cannot_be_declared_as_a_pointer() {
        // Rect has a value method, so `*Rect` is not allowed.
        let err = check_decls(
            "struct Rect { w: i64, h: i64 }\n\
             impl Rect { fn area(self) -> i64 { self.w * self.h } }\n\
             fn main() -> i64 { let r: *Rect = Rect { w: 1, h: 2 }  0 }",
        )
        .unwrap_err();
        assert!(err.contains("value methods"), "explains the kind: {err}");
    }

    #[test]
    fn a_pointer_method_type_cannot_be_declared_as_a_value() {
        // Cnt has a pointer method, so a bare `Cnt` parameter is not allowed.
        let err = check_decls(
            "struct Cnt { n: i64 }\n\
             impl Cnt { fn inc(self: *Cnt) { self.n = self.n + 1 } }\n\
             fn use_it(c: Cnt) -> i64 { c.n }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap_err();
        assert!(err.contains("pointer methods"), "explains the kind: {err}");
    }

    #[test]
    fn declarations_matching_the_type_kind_are_accepted() {
        // Pointer-method type declared `*Cnt` everywhere: consistent.
        check_decls(
            "struct Cnt { n: i64 }\n\
             impl Cnt { fn inc(self: *Cnt) { self.n = self.n + 1 } }\n\
             fn bump(c: *Cnt) { c.inc() }\n\
             fn main() -> i64 { let c: *Cnt = Cnt { n: 0 }  bump(c)  c.n }",
        )
        .unwrap();
    }

    #[test]
    fn a_pointer_receiver_method_may_mutate_self() {
        // `self: *Cnt` is a mutable receiver, so setting a field is allowed.
        check(
            "struct Cnt { n: i64 }\n\
             fn Cnt_inc(self: *Cnt) { self.n = self.n + 1 }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap();
    }

    #[test]
    fn a_value_receiver_method_cannot_mutate_self() {
        // Value-receiver method: `self` is a value struct, so it is read-only.
        let err = check(
            "struct Point { x: i64, y: i64 }\n\
             fn Point_set(self: Point) { self.x = 5 }\n\
             fn main() -> i64 { 0 }",
        )
        .unwrap_err();
        assert!(err.contains("value struct `self`"), "self read-only: {err}");
    }

    #[test]
    fn rejects_constant_integer_overflow() {
        let funcs = Parser::new(tokenize(
            "fn f() -> i64 { 9000000000000000000 + 9000000000000000000 }",
        ))
        .parse_program();
        assert!(
            check_constant_overflow(&funcs).is_err(),
            "a constant `+` past i64::MAX must be rejected"
        );
    }

    #[test]
    fn accepts_constant_arithmetic_that_fits() {
        let funcs =
            Parser::new(tokenize("fn f() -> i64 { 1000000 * 1000 + 40 - 2 }")).parse_program();
        assert!(
            check_constant_overflow(&funcs).is_ok(),
            "in-range constant arithmetic is fine"
        );
    }

    #[test]
    fn a_wrapping_function_exempts_overflow_checking() {
        // Inside a `wrapping fn` the machine's mod-2^N arithmetic is the intended semantics (the
        // crypto substrate), so an overflow that would be a compile error in an ordinary function
        // is allowed, the region marker that lets #3 flip to checked-by-default elsewhere.
        let over = "9000000000000000000 + 9000000000000000000";
        let ordinary =
            Parser::new(tokenize(&format!("fn f() -> i64 {{ {over} }}"))).parse_program();
        assert!(
            check_constant_overflow(&ordinary).is_err(),
            "an ordinary function still rejects the overflow"
        );
        let wrapping =
            Parser::new(tokenize(&format!("wrapping fn f() -> i64 {{ {over} }}"))).parse_program();
        assert!(
            check_constant_overflow(&wrapping).is_ok(),
            "a `wrapping fn` allows the intended wrap"
        );
    }

    #[test]
    fn wrapping_exempts_only_overflow_not_bounds() {
        // `wrapping` narrows to arithmetic overflow, a `wrapping fn` still gets bounds-checked, so
        // a provable out-of-bounds index is rejected exactly as in an ordinary function.
        let funcs =
            Parser::new(tokenize("wrapping fn f() -> i64 { let a[4]\na[9] }")).parse_program();
        let err = check_constant_bounds(&funcs, &[]).unwrap_err();
        assert!(
            err.contains("out of bounds"),
            "wrapping must not exempt bounds: {err}"
        );
    }

    #[test]
    fn pub_wrapping_fn_parses() {
        // Visibility composes with the modifier.
        let funcs = Parser::new(tokenize("pub wrapping fn f() -> i64 { 1 }")).parse_program();
        assert!(funcs[0].is_wrapping && matches!(funcs[0].vis, crate::ast::Visibility::Pub));
    }

    #[test]
    fn rejects_an_index_into_a_module_view_without_a_proof() {
        // A module-level `let mut g: u8[8]` must be index-checked exactly like its local
        // counterpart. `g[i]` with a variable index is unprovable and must error.
        let err = bounds("let mut g: u8[8]\nfn main(i: i64) -> i64 { g[i] }").unwrap_err();
        assert!(
            err.contains(
                "function `main`: cannot prove `i` is in bounds for `g` of size 8 at line 2"
            ) && err.contains("0..8"),
            "module view index must be checked like local view; got: {err}"
        );
    }

    #[test]
    fn a_for_loop_over_a_module_view_is_in_bounds() {
        // Using a bounded loop to prove the index must still compile cleanly: the loop
        // condition proves 0 <= i < 8, which is exactly what the bounds checker needs.
        let src = "let mut g: u8[8]\n\
                   fn main() -> i64 {\n\
                       let mut i = 0\n\
                       for i < 8 { g[i] = 0\ni = i + 1 }\n\
                       0\n\
                   }";
        assert!(
            bounds(src).is_ok(),
            "for i < 8 {{ g[i] }} must compile without where/requires; got: {:?}",
            bounds(src).err()
        );
    }

    // --- Continue-related bounds tests (Bug 2: continue before break loses bounds) ---

    #[test]
    fn range_for_with_continue_still_binds_index_bounds() {
        let src = "let mut a[4]\n\
                   fn main() -> i64 {\n\
                   let mut t = 0\n\
                   for i in 0..4 {\n\
                       if a[i] == 9 { continue }\n\
                       t = t + a[i]\n\
                   }\n\
                   print_int(t)\n\
                   0\n\
                   }";
        assert!(bounds(src).is_ok(),
            "continue inside range-for must not kill index bounds; got: {}",
            bounds(src).unwrap_err());
    }

    #[test]
    fn cstyle_for_with_continue_still_binds_index_bounds() {
        let src = "let mut a[4]\n\
                   fn main() -> i64 {\n\
                   let mut t = 0\n\
                   for let mut i = 0; i < 4; i = i + 1 {\n\
                       if a[i] == 9 { continue }\n\
                       t = t + a[i]\n\
                   }\n\
                   print_int(t)\n\
                   0\n\
                   }";
        assert!(bounds(src).is_ok(),
            "continue inside C-style-for must not kill index bounds; got: {}",
            bounds(src).unwrap_err());
    }

    #[test]
    fn while_for_with_continue_still_binds_index_bounds() {
        let src = "let mut a[4]\n\
                   fn main() -> i64 {\n\
                   let mut i = 0\n\
                   let mut t = 0\n\
                   for i < 4 {\n\
                       if a[i] == 9 { continue }\n\
                       t = t + a[i]\n\
                       i = i + 1\n\
                   }\n\
                   print_int(t)\n\
                   0\n\
                   }";
        assert!(bounds(src).is_ok(),
            "continue inside while-for must not kill index bounds; got: {}",
            bounds(src).unwrap_err());
    }

    // ---------------------------------------------------------------------------
    // Slice promise > backing array size — must be a compile error
    // ---------------------------------------------------------------------------

    #[test]
    fn slice_promises_more_than_its_backing_array_has() {
        let err = bounds(
            "fn main() -> i64 {\n\
             let a[4]\n\
             let s = Slice { ptr: a, len: 9 }\n\
             s[0]\n\
             0\n\
             }",
        ).unwrap_err();
        assert!(
            err.contains("out of bounds") || err.contains("slice") || err.contains("size"),
            "must reject Slice with len > array size, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // Negative controls — these MUST keep compiling
    // ---------------------------------------------------------------------------

    #[test]
    fn slice_exact_len_compiles() {
        assert!(bounds(
            "fn main() -> i64 {\n\
             let a[4]\n\
             let s = Slice { ptr: a, len: 4 }\n\
             0\n\
             }"
        ).is_ok(), "exact len must compile");
    }

    #[test]
    fn slice_less_than_array_compiles() {
        assert!(bounds(
            "fn main() -> i64 {\n\
             let a[4]\n\
             let s = Slice { ptr: a, len: 2 }\n\
             0\n\
             }"
        ).is_ok(), "len < array size must compile");
    }

    #[test]
    fn slice_zero_len_compiles() {
        assert!(bounds(
            "fn main() -> i64 {\n\
             let a[4]\n\
             let s = Slice { ptr: a, len: 0 }\n\
             0\n\
             }"
        ).is_ok(), "len = 0 must compile");
    }

    #[test]
    fn slice_with_variable_len_compiles() {
        assert!(bounds(
            "fn main(n: i64) -> i64 {\n\
             let a[4]\n\
             let s = Slice { ptr: a, len: n }\n\
             0\n\
             }"
        ).is_ok(), "variable len cannot be checked at compile time");
    }

    #[test]
    fn slice_param_compiles() {
        assert!(bounds(
            "fn f(s: Slice) -> i64 {\n\
             0\n\
             }"
        ).is_ok(), "Slice parameter must still compile");
    }

    #[test]
    fn slice_call_with_variable_len_compiles() {
        assert!(bounds(
            "fn takes_slice(s: Slice) -> i64 { 0 }\n\
             fn main(n: i64) -> i64 {\n\
             let a[4]\n\
             takes_slice(Slice { ptr: a, len: n })\n\
             0\n\
             }"
        ).is_ok(), "len from variable cannot be checked at compile time");
    }

    // ---------------------------------------------------------------------------
    // Local Slice bounds checking (un-slice-local-no-comprueba-ni-un-indice)
    // ---------------------------------------------------------------------------

    #[test]
    fn rejects_const_index_out_of_local_slice_range() {
        let err = bounds(
            "fn f() -> i64 { let a[4]\nlet s = Slice { ptr: a, len: 4 }\ns[7] }",
        )
        .unwrap_err();
        assert!(
            err.contains("out of bounds") || (err.contains("cannot prove") && err.contains("slice")),
            "constant out-of-bounds index into a local slice must be rejected, got: {err}"
        );
    }

    #[test]
    fn accepts_const_index_in_local_slice_range() {
        let result = bounds("fn f() -> i64 { let a[4]\nlet s = Slice { ptr: a, len: 4 }\ns[3] }");
        assert!(result.is_ok(), "constant index within slice length must pass");
    }

    #[test]
    fn accepts_loop_over_local_slice_with_bounded_index() {
        // Using `for i < s.len` matches the field expression in flow.slices, so the prover
        // discharges i < s.len from the loop condition itself.
        let result = bounds(
            "fn f() -> i64 { let mut t[4]\nlet s = Slice { ptr: t, len: 4 }\n\
             let mut i = 0\nfor i < s.len { let _t = s[i]; i = i + 1 }\n0 }",
        );
        assert!(result.is_ok(), "canonical loop form for i < s.len");
    }

    #[test]
    fn rejects_out_of_range_index_on_empty_local_slice() {
        let err = bounds(
            "fn f() -> i64 { let a[4]\nlet s = Slice { ptr: a, len: 0 }\ns[0] }",
        )
        .unwrap_err();
        assert!(
            err.contains("out of bounds") || err.contains("cannot prove"),
            "any index on empty slice must be rejected, got: {err}"
        );
    }

    #[test]
    fn slice_parameter_still_works_as_before() {
        assert!(
            bounds("fn f(s: Slice, i: i64 where 0 <= i && i < s.len) -> i64 { s[i] }").is_ok(),
            "slice parameter with bound must still work"
        );
    }

    #[test]
    fn rejects_slice_param_index_without_bound() {
        let err = bounds("fn f(s: Slice, i: i64) -> i64 { s[i] }").unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("slice"),
            "unbounded slice param index must be rejected, got: {err}"
        );
    }

    #[test]
    fn a_slice_that_arrives_from_a_call_is_not_tracked_yet() {
        // `let s = make()` binds a slice the checker does not follow: the length lives in the
        // callee's `ensures`, and nothing here reads it, so `s[3]` passes unexamined. The
        // `ensures` below does not even bound the index (`s.len < 8` says nothing about
        // `3 < s.len`), which is exactly why this must not be read as a proof.
        let result = bounds(
            "fn make() -> (s: Slice) ensures { s.len >= 0; s.len < 8 } { 0 }\n\
             fn f() -> i64 { let s = make()\ns[3] }",
        );
        assert!(result.is_ok(), "the call-returned slice is unchecked today");
    }

    #[test]
    fn a_name_rebound_to_an_array_stops_being_a_slice() {
        // The second `let s` is a different variable that happens to reuse the name; from there
        // on `s` is the array, so its own size bounds the index and the slice's `len` does not.
        let result = bounds(
            "fn f() -> i64 { let a[4]\na[3] = 1\nlet s = Slice { ptr: a, len: 4 }\nlet s = a\ns[3] }",
        );
        assert!(result.is_ok(), "after rebinding the name is no longer a slice");
    }

    #[test]
    fn uses_the_local_slice_len_not_the_backing_array_size() {
        let err = bounds(
            "fn f() -> i64 { let a[8]\na[0] = 0\nlet s = Slice { ptr: a, len: 2 }\ns[3] }",
        )
        .unwrap_err();
        assert!(
            err.contains("s") && (err.contains("out of bounds") || err.contains("cannot prove")),
            "the slice's declared len, not the backing array size, must bound the index, got: {err}"
        );
    }

    #[test]
    fn lets_explicit_type_annotation_as_slice() {
        assert!(
            bounds("fn f() -> i64 { let a[4]\nlet s: Slice = Slice { ptr: a, len: 4 }\ns[3] }")
                .is_ok(),
            "an explicit Slice type annotation should register the local as a slice"
        );
    }
}

#[cfg(test)]
mod undeclared_tests {
    use super::*;
    use crate::lexer::tokenize_located;
    use crate::parser::Parser;

    fn parse_located(src: &str) -> (Vec<Function>, Vec<Global>, Vec<EnumDef>) {
        let (toks, lines, _cols) = tokenize_located(src);
        let mut p = Parser::with_lines(toks, lines);
        let funcs = p.parse_program();
        let globals = p.take_globals();
        let enums = p.take_enums();
        (funcs, globals, enums)
    }

    fn extract_line(err: &str) -> Option<u32> {
        err.split("at line ")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.split(';').next())
            .and_then(|s| s.trim().parse().ok())
    }

    // ---------------------------------------------------------------------------
    // The two programs from the prompt — measured with `tokenize_located` so
    // `stmt_lines` is populated and the reported line is the exact source line.
    // ---------------------------------------------------------------------------

    #[test]
    fn undeclared_in_tail_expr_reports_line() {
        // fn main() -> i64 {        // 1
        //  let a = 1                // 2
        //  let b = 2                // 3
        //  a + zz                   // 4  ← here
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n let a = 1\n let b = 2\n a + zz\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("zz") && err.contains("main"),
            "error must name the variable and function, got: {err}"
        );
        assert!(
            err.contains("at line 4"),
            "error must report line 4, got: {err}"
        );
        assert_eq!(extract_line(&err), Some(4), "line extraction must yield 4");
    }

    #[test]
    fn undeclared_in_if_body_reports_line() {
        // fn main() -> i64 {       // 1
        //  let q = 3               // 2
        //  if 1 == 1 {             // 3
        //   q = ww                 // 4  ← here
        //  }
        //  q
        // }
        // The `if` body is a parsed block with its own stmt_lines[0] = 4.
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n let q = 3\n if 1 == 1 {\n  q = ww\n }\n q\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("ww") && err.contains("main"),
            "error must name the variable and function, got: {err}"
        );
        assert!(
            err.contains("at line 4"),
            "error must report the if-body line where the undeclared var appears, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // Line numbers for the different statement forms where an undeclared name
    // can appear.
    // ---------------------------------------------------------------------------

    #[test]
    fn undeclared_in_let_value_reports_line() {
        // fn f() -> i64 {       // 1
        //  let x = missing      // 2  ← here
        //  x                   // 3
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n let x = missing\n x\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("at line 2"),
            "undeclared in let value must report line 2, got: {err}"
        );
    }

    #[test]
    fn undeclared_in_assign_target_reports_line() {
        // fn f() -> i64 {       // 1
        //  let mut x = 0        // 2
        //  missing = 1          // 3  ← here
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n let mut x = 0\n missing = 1\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("at line 3"),
            "undeclared in assign target must report line 3, got: {err}"
        );
    }

    #[test]
    fn undeclared_in_assign_value_reports_line() {
        // fn f() -> i64 {       // 1
        //  let mut x = 0        // 2
        //  x = missing          // 3  ← here
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n let mut x = 0\n x = missing\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("at line 3"),
            "undeclared in assign value must report line 3, got: {err}"
        );
    }

    #[test]
    fn undeclared_in_for_condition_reports_line() {
        // fn f() -> i64 {       // 1
        //  let mut i = 0        // 2
        //  for missing < 10 {}  // 3  ← here
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n let mut i = 0\n for missing < 10 {}\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("at line 3"),
            "undeclared in for condition must report line 3, got: {err}"
        );
    }

    #[test]
    fn undeclared_in_return_reports_line() {
        // fn f() -> i64 {       // 1
        //  return missing       // 2  ← here
        // }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n return missing\n }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("at line 2"),
            "undeclared in return must report line 2, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // Existing tests — updated to use tokenize_located so line info is present.
    // They assert on message content (variable name, function name), not line.
    // ---------------------------------------------------------------------------

    #[test]
    fn rejects_undeclared_variable() {
        let (funcs, globals, _enums) = parse_located("fn f() -> i64 { undeclared_name }");
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("undeclared_name"),
            "error must name the variable, got: {err}"
        );
    }

    #[test]
    fn accepts_declared_variables() {
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n let x = 42\n x\n }",
        );
        check_undeclared_variables(&funcs, &globals, &_enums).expect("declared var must be accepted");
    }

    #[test]
    fn accepts_function_parameters() {
        let (funcs, globals, _enums) = parse_located("fn f(x: i64) -> i64 { x }");
        check_undeclared_variables(&funcs, &globals, &_enums).expect("param must be accepted");
    }

    #[test]
    fn accepts_module_level_globals() {
        let (funcs, globals, _enums) = parse_located(
            "let SHARED = 42\nfn f() -> i64 { SHARED }",
        );
        check_undeclared_variables(&funcs, &globals, &_enums).expect("global must be accepted");
    }

    #[test]
    fn function_call_targets_are_not_checked_as_variables() {
        let (funcs, globals, _enums) = parse_located(
            "fn callee() -> i64 { 0 }\nfn caller() -> i64 { callee() }",
        );
        check_undeclared_variables(&funcs, &globals, &_enums).expect("function callee must be accepted");
    }

    #[test]
    fn function_call_arguments_are_checked_against_scope() {
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 { print_int(not_declared) }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("not_declared"),
            "undeclared call arg must be rejected, got: {err}"
        );
    }

    #[test]
    fn reports_error_in_function_name() {
        let (funcs, globals, _enums) = parse_located("fn test_field_ops() -> i64 { missing }");
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("test_field_ops"),
            "error must name the function, got: {err}"
        );
    }

    #[test]
    fn enum_variant_constructors_are_not_rejected() {
        let (funcs, globals, _enums) = parse_located(
            "enum Opt { Some(i64), None }\nfn f() -> Opt { None }",
        );
        check_undeclared_variables(&funcs, &globals, &_enums)
            .expect("enum variant `None` must not be rejected");
    }

    // ---------------------------------------------------------------------------
    // BUG FIX: foreach `for x in a` desugaring must keep stmt_lines in sync.
    // Before the fix, stmt_lines was not updated when synthetic statements
    // were inserted, causing error line numbers to be wrong.
    // ---------------------------------------------------------------------------

    #[test]
    fn undeclared_in_foreach_body_stmt_reports_correct_line() {
        // Lines:
        //   1  fn main() -> i64 {
        //   2      let a[3]
        //   3      a[0] = 1
        //   4      let mut s = 0
        //   5      for x in a {
        //   6          s = s + x
        //   7          s = s + noexiste     ← here
        //   8      }
        //   9      return s
        //  10  }
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n\
             let a[3]\n\
             a[0] = 1\n\
             let mut s = 0\n\
             for x in a {\n\
             s = s + x\n\
             s = s + noexiste\n\
             }\n\
             return s\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 7"),
            "foreach body stmt error must report line 7, got: {err}"
        );
        assert_eq!(extract_line(&err), Some(7), "line extraction must yield 7");
    }

    #[test]
    fn undeclared_in_foreach_body_first_stmt_reports_correct_line() {
        // Lines:
        //   1  fn main() -> i64 {
        //   2      let a[3]
        //   3      let mut s = 0
        //   4      for x in a {
        //   5          noexiste               ← here
        //   6          s = s + x
        //   7      }
        //   8      return s
        //   9  }
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n\
             let a[3]\n\
             let mut s = 0\n\
             for x in a {\n\
             noexiste\n\
             s = s + x\n\
             }\n\
             return s\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 5"),
            "foreach first stmt error must report line 5, got: {err}"
        );
        assert_eq!(extract_line(&err), Some(5), "line extraction must yield 5");
    }

    #[test]
    fn undeclared_in_foreach_body_tail_reports_correct_line() {
        // Lines:
        //   1  fn main() -> i64 {
        //   2      let a[3]
        //   3      let mut s = 0
        //   4      for x in a {
        //   5          s = s + x
        //   6          noexiste               ← tail, at line 6
        //   7      }
        //   8      return s
        //   9  }
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n\
             let a[3]\n\
             let mut s = 0\n\
             for x in a {\n\
             s = s + x\n\
             noexiste\n\
             }\n\
             return s\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 6"),
            "foreach tail error must report line 6, got: {err}"
        );
        assert_eq!(extract_line(&err), Some(6), "line extraction must yield 6");
    }

    #[test]
    fn undeclared_in_nested_foreach_reports_outer_for_line() {
        // Lines:
        //   1  fn main() -> i64 {
        //   2      let a[3]
        //   3      let b[3]
        //   4      for x in a {
        //   5          for y in b {
        //   6              let mut s = 0
        //   7              s = s + noexiste   ← here
        //   8          }
        //   9      }
        //  10      return 0
        //  11  }
        let (funcs, globals, _enums) = parse_located(
            "fn main() -> i64 {\n\
             let a[3]\n\
             let b[3]\n\
             for x in a {\n\
             for y in b {\n\
             let mut s = 0\n\
             s = s + noexiste\n\
             }\n\
             }\n\
             return 0\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 7"),
            "nested foreach error must report line 7, got: {err}"
        );
        assert_eq!(extract_line(&err), Some(7), "line extraction must yield 7");
    }

    #[test]
    fn undeclared_in_for_i_in_range_with_direct_continue_reports_correct_line() {
        // Lines:
        //   1  fn f() -> i64 {
        //   2      for i in 0..3 {
        //   3          continue
        //   4          let mut x = 0
        //   5          x = missing            ← undeclared at line 5
        //   6      }
        //   7      0
        //   8  }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n\
             for i in 0..3 {\n\
             continue\n\
             let mut x = 0\n\
             x = missing\n\
             }\n\
             0\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("missing") && err.contains("at line 5"),
            "for i in range with direct continue, error after must report line 5, got: {err}"
        );
    }

    // --- Regression: for forms that already work must keep working ---

    #[test]
    fn undeclared_in_for_i_lt_3_reports_for_line() {
        // Lines:
        //   1  fn f() -> i64 {
        //   2      let mut i = 0
        //   3      for i < 3 {
        //   4        noexiste                 ← here
        //   5      }
        //   6      0
        //   7  }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n\
             let mut i = 0\n\
             for i < 3 {\n\
             noexiste\n\
             }\n\
             0\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 4"),
            "for i < 3 body error must report line 4, got: {err}"
        );
    }

    #[test]
    fn undeclared_in_for_c_style_reports_correct_line() {
        // Lines:
        //   1  fn f() -> i64 {
        //   2      for let mut i = 0; i < 3; i = i + 1 {
        //   3        noexiste                 ← here
        //   4      }
        //   5      0
        //   6  }
        let (funcs, globals, _enums) = parse_located(
            "fn f() -> i64 {\n\
             for let mut i = 0; i < 3; i = i + 1 {\n\
             noexiste\n\
             }\n\
             0\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &globals, &_enums).unwrap_err();
        assert!(
            err.contains("noexiste") && err.contains("at line 3"),
            "for C-style body error must report line 3, got: {err}"
        );
    }
}

#[cfg(test)]
mod syscall_retry_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_syscall_retry_has_an_exit(&funcs)
    }

    #[test]
    fn rejects_a_fill_loop_that_never_looks_at_the_error() {
        // The exact shape that shipped twice: `got` only advances on success, nothing breaks or
        // returns, so a syscall that FAILS (not short) spins at syscall speed. Verified in the
        // real thing before it was fixed: a null buffer made getrandom fail every call and the
        // test binary hung there burning a core.
        let e = check(
            "let SYS_getrandom = 318\n\
             fn fill(buf: i64, n: i64) { let mut got = 0\n\
             for got < n { let r = syscall(SYS_getrandom, buf + got, n - got, 0)\n\
             if r > 0 { got = got + r } } }",
        );
        assert!(
            e.is_err(),
            "a retry loop with no error branch must be rejected"
        );
        let msg = e.unwrap_err();
        assert!(
            msg.contains("got") && msg.contains("r"),
            "name the counter and the result: {msg}"
        );
        assert!(
            msg.contains("EINTR"),
            "the error should say what the fix is: {msg}"
        );
    }

    #[test]
    fn accepts_a_fill_loop_that_returns_on_a_fatal_error() {
        // The fixed shape: retry the transient one, report the rest.
        let ok = check(
            "let SYS_getrandom = 318\n\
             fn fill(buf: i64, n: i64) -> i64 { let mut got = 0\n\
             for got < n { let r = syscall(SYS_getrandom, buf + got, n - got, 0)\n\
             if r > 0 { got = got + r } else { if r != 0 - 4 { return 0 } } }\n\
             1 }",
        );
        assert!(
            ok.is_ok(),
            "a loop that reports the failure is correct, got {ok:?}"
        );
    }

    #[test]
    fn accepts_a_fill_loop_that_breaks_on_a_fatal_error() {
        let ok = check(
            "let SYS_read = 0\n\
             fn fill(fd: i64, buf: i64, n: i64) { let mut got = 0\n\
             for got < n { let r = syscall(SYS_read, fd, buf + got, n - got)\n\
             if r <= 0 { break }\n\
             got = got + r } }",
        );
        assert!(ok.is_ok(), "a loop that breaks out is correct, got {ok:?}");
    }

    #[test]
    fn says_nothing_about_a_counted_loop_that_ignores_the_result() {
        // `i` advances unconditionally, so this terminates whatever the syscall does. Retrying
        // is not what makes a loop dangerous; retrying WITHOUT A WAY OUT is.
        let ok = check(
            "let SYS_write = 1\n\
             fn shout(fd: i64, buf: i64) { let mut i = 0\n\
             for i < 3 { let r = syscall(SYS_write, fd, buf, 1)\n\
             i = i + 1 } }",
        );
        assert!(ok.is_ok(), "a bounded loop is fine, got {ok:?}");
    }
}

#[cfg(test)]
mod call_arity_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        check_call_arity(&Parser::new(tokenize(src)).parse_program())
    }

    #[test]
    fn rejects_a_call_with_too_many_arguments() {
        // This compiled and ran, dropping the extra. The cost is worst in the ordinary red-first
        // step: a test written against a wider signature BEFORE widening it came out green, so
        // the failure that was supposed to drive the change never arrived.
        let e = check("fn f(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { f(1, 2, 99) }");
        assert!(e.is_err(), "an extra argument must be an error");
        let msg = e.unwrap_err();
        assert!(
            msg.contains("2") && msg.contains("3"),
            "it says both counts, got: {msg}"
        );
    }

    #[test]
    fn rejects_a_call_with_too_few_arguments() {
        // Worse than the extra: the callee reads a register nobody set, so it works on whatever
        // the previous call left there -- repeatable enough to look deliberate.
        let e = check("fn f(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { f(1) }");
        assert!(e.is_err(), "a missing argument must be an error");
    }

    #[test]
    fn accepts_a_call_that_matches() {
        let ok = check("fn f(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { f(1, 2) }");
        assert!(ok.is_ok(), "got {ok:?}");
    }

    #[test]
    fn looks_inside_the_places_a_call_hides() {
        // A call is rarely a whole statement: it sits in a condition, in another call's
        // arguments, in a branch, in a loop body. A checker that only walked statements would
        // pass every one of these.
        for src in [
            "fn f(a: i64) -> i64 { a }\nfn main() -> i64 { if f(1, 2) > 0 { 1 } else { 0 } }",
            "fn f(a: i64) -> i64 { a }\nfn g(x: i64) -> i64 { x }\nfn main() -> i64 { g(f(1, 2)) }",
            "fn f(a: i64) -> i64 { a }\nfn main() -> i64 { for i in 0..2 { f(i, i) }\n0 }",
            "fn f(a: i64) -> i64 { a }\nfn main() -> i64 { let x = f(1, 2)\nx }",
        ] {
            assert!(check(src).is_err(), "not caught in: {src}");
        }
    }

    #[test]
    fn leaves_builtins_alone() {
        // `syscall` and friends are not declared functions and take what they are given; a
        // checker that treated every name as a function would reject the standard library.
        let ok = check("fn main() -> i64 { syscall(60, 0) }");
        assert!(ok.is_ok(), "got {ok:?}");
    }
}

#[cfg(test)]
mod spawn_loop_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_spawn_not_in_loop(&funcs)
    }

    // These used to pin the REJECTION of a `spawn` inside a loop, in each shape an accept loop is
    // written: the plain body, an `if` inside it, an `else`, and the tail conditional -- three of
    // which had slipped past an earlier version of the walk. The rejection existed because a
    // spawn site owned ONE join cell, so a second turn overwrote a live thread's tid and lost it.
    //
    // A looping site now owns a ring of slots and joins the one it is about to reuse, so all four
    // shapes compile. What they guaranteed -- that no thread is left unjoined -- is now a
    // behavioural guarantee and is tested as one, by running the threads and counting them:
    // codegen::tests::a_spawn_in_a_loop_runs_every_iteration_and_joins_them_all and
    // a_loop_with_more_turns_than_slots_reuses_them_without_losing_a_thread. They stay here,
    // flipped, so that anybody who reads the old error message finds where it went.

    #[test]
    fn accepts_spawn_inside_a_loop() {
        let ok = check("fn w() -> i64 { 0 }\nfn f() -> i64 { for i in 0..3 { spawn w() } 0 }");
        assert!(ok.is_ok(), "a spawn in a loop is supported now, got {ok:?}");
    }

    #[test]
    fn accepts_spawn_inside_an_if_inside_a_loop() {
        // The shape every accept loop has: `for { let c = accept(); if c >= 0 { spawn h(c) } }`.
        let ok = check(
            "fn w() -> i64 { 0 }\nfn f() -> i64 { for i in 0..3 { if i > 0 { spawn w() } } 0 }",
        );
        assert!(ok.is_ok(), "got {ok:?}");
    }

    #[test]
    fn accepts_spawn_inside_an_else_inside_a_loop() {
        let ok = check(
            "fn w() -> i64 { 0 }\n\
             fn f() -> i64 { for i in 0..3 { if i > 0 { i } else { spawn w() 0 } } 0 }",
        );
        assert!(ok.is_ok(), "got {ok:?}");
    }

    #[test]
    fn accepts_spawn_outside_a_loop() {
        let ok = check("fn w() -> i64 { 0 }\nfn f() -> i64 { spawn w() spawn w() 0 }");
        assert!(ok.is_ok(), "two straight-line spawns are fine, got {ok:?}");
    }

    #[test]
    fn accepts_spawn_inside_an_if_outside_any_loop() {
        let ok = check("fn w() -> i64 { 0 }\nfn f(c: i64) -> i64 { if c > 0 { spawn w() } 0 }");
        assert!(
            ok.is_ok(),
            "a conditional spawn that runs once is fine, got {ok:?}"
        );
    }
}

#[cfg(test)]
mod par_let_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_par_let_arity(&funcs)
    }

    #[test]
    fn rejects_a_par_binding_with_mismatched_counts() {
        // Two names, three peers: one peer's result would be silently dropped. Rejected.
        let e = check(
            "fn f() -> i64 { 1 }\nfn g() -> i64 { 2 }\nfn h() -> i64 { 3 }\n\
             fn run() -> i64 { let a ~~ b = par f() ~~ g() ~~ h()\n a + b }",
        );
        assert!(e.is_err(), "mismatched name/peer counts must be rejected");
        let msg = e.unwrap_err();
        assert!(
            msg.contains("2 name") && msg.contains("3 peer"),
            "names the counts: {msg}"
        );
    }

    #[test]
    fn accepts_a_matched_par_binding() {
        let ok = check(
            "fn f() -> i64 { 1 }\nfn g() -> i64 { 2 }\n\
             fn run() -> i64 { let a ~~ b = par f() ~~ g()\n a + b }",
        );
        assert!(ok.is_ok(), "equal counts are fine, got {ok:?}");
    }

    #[test]
    fn rejects_a_non_commutative_par_reduce() {
        // `par(-)`: the peers finish in an unspecified order, so a non-commutative fold would give
        // an order-dependent result. Rejected, with the fix (a positional binding) named.
        let e = check(
            "fn f() -> i64 { 1 }\nfn g() -> i64 { 2 }\n\
             fn run() -> i64 { let s = par(-) f() ~~ g()\n s }",
        );
        assert!(e.is_err(), "a non-commutative reducer must be rejected");
        assert!(
            e.unwrap_err().contains("COMMUTATIVE"),
            "the error explains the commutativity requirement"
        );
    }

    #[test]
    fn accepts_commutative_par_reduces() {
        for op in ["+", "*", "&", "|", "^"] {
            let src = format!(
                "fn f() -> i64 {{ 1 }}\nfn g() -> i64 {{ 2 }}\n\
                 fn run() -> i64 {{ let s = par({op}) f() ~~ g()\n s }}"
            );
            assert!(check(&src).is_ok(), "`par({op})` is a commutative monoid");
        }
    }

    fn confluence(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_par_confluence(&funcs, &globals)
    }

    #[test]
    fn rejects_two_peers_writing_the_same_global() {
        // Both peers write `G`: a write-write race, the result depends on which finishes last.
        let e = confluence(
            "let mut G = 0\n\
             fn a() { G = 1 }\nfn b() { G = 2 }\n\
             fn run() { par a() ~~ b() }",
        );
        assert!(
            e.is_err(),
            "a write-write race on a global must be rejected"
        );
        assert!(
            e.unwrap_err().contains("G"),
            "the error names the raced global"
        );
    }

    #[test]
    fn rejects_a_write_read_race() {
        // One peer writes `G`, the other reads it: a read-write race.
        let e = confluence(
            "let mut G = 0\nlet mut H = 0\n\
             fn writer() { G = 1 }\nfn reader() { H = G }\n\
             fn run() { par writer() ~~ reader() }",
        );
        assert!(e.is_err(), "a write-read race must be rejected");
    }

    #[test]
    fn accepts_peers_on_disjoint_globals() {
        // Each peer owns its own global: no shared location, so they are confluent.
        let ok = confluence(
            "let mut R0 = 0\nlet mut R1 = 0\n\
             fn a() { R0 = 1 }\nfn b() { R1 = 2 }\n\
             fn run() { par a() ~~ b() }",
        );
        assert!(ok.is_ok(), "disjoint globals do not race, got {ok:?}");
    }

    #[test]
    fn detects_a_race_through_the_call_graph() {
        // Neither peer writes `G` directly, but both reach a helper that does: the transitive
        // footprint catches it.
        let e = confluence(
            "let mut G = 0\n\
             fn bump() { G = G + 1 }\n\
             fn a() { bump() }\nfn b() { bump() }\n\
             fn run() { par a() ~~ b() }",
        );
        assert!(
            e.is_err(),
            "a race reached transitively must still be rejected"
        );
    }

    #[test]
    fn accepts_pure_peers() {
        let ok = confluence(
            "let mut G = 0\n\
             fn a() -> i64 { 1 }\nfn b() -> i64 { 2 }\n\
             fn run() { par a() ~~ b() }",
        );
        assert!(
            ok.is_ok(),
            "peers that touch no global are confluent, got {ok:?}"
        );
    }

    fn acyclic(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_graph_acyclic(&funcs)
    }

    #[test]
    fn rejects_a_cyclic_graph() {
        let e = acyclic(
            "fn id(x: i64) -> i64 { x }\n\
             fn run() -> i64 { let r = graph { node a = id(b)\n node b = id(a)\n yield a }\n r }",
        );
        assert!(e.is_err(), "a dependency cycle in a graph must be rejected");
        assert!(
            e.unwrap_err().contains("cycle"),
            "the error explains the cycle"
        );
    }

    #[test]
    fn accepts_an_acyclic_graph() {
        let ok = acyclic(
            "fn id(x: i64) -> i64 { x }\nfn add(a: i64, b: i64) -> i64 { a + b }\n\
             fn run() -> i64 { let r = graph { node c = add(a, b)\n node a = id(1)\n node b = id(2)\n yield c }\n r }",
        );
        assert!(
            ok.is_ok(),
            "a DAG is fine even out of source order, got {ok:?}"
        );
    }

    #[test]
    fn rejects_two_functions_with_the_same_name() {
        // Two same-named free functions (the flattened result of a private helper defined in two
        // files) is a link collision, caught early with a clear message.
        let funcs = Parser::new(tokenize(
            "priv fn helper() -> i64 { 1 }\npriv fn helper() -> i64 { 2 }\nfn main() -> i64 { helper() }",
        ))
        .parse_program();
        let e = check_no_duplicate_functions(&funcs);
        assert!(e.is_err(), "a duplicate function name must be rejected");
        assert!(
            e.unwrap_err().contains("helper"),
            "the error names the function"
        );
    }

    #[test]
    fn allows_distinct_function_names() {
        let funcs = Parser::new(tokenize(
            "fn a() -> i64 { 1 }\nfn b() -> i64 { 2 }\nfn main() -> i64 { a() + b() }",
        ))
        .parse_program();
        assert!(
            check_no_duplicate_functions(&funcs).is_ok(),
            "distinct names are fine"
        );
    }

    #[test]
    fn checks_a_par_nested_inside_an_if() {
        // A `par` in an `if` branch (an expression) is still checked: the walk descends into
        // nested blocks, so a race there is caught, not skipped.
        let e = confluence(
            "let mut G = 0\n\
             fn a() { G = 1 }\nfn b() { G = 2 }\n\
             fn run(c: i64) { if c > 0 { par a() ~~ b() } }",
        );
        assert!(
            e.is_err(),
            "a race inside an `if` branch must still be rejected"
        );
    }
}

#[cfg(test)]
mod close_fd_after_spawn_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_no_close_fd_after_spawn(&funcs)
    }

    #[test]
    fn rejects_close_fd_in_the_continuation_of_a_spawn_that_took_it() {
        // `spawn pump(fd); close_fd(fd)`: the continuation runs concurrently with the thread,
        // so it closes the descriptor out from under it. The reported relay corruption.
        let e = check(
            "fn pump(fd: i64) -> i64 { 0 }\n\
             fn f(fd: i64) -> i64 { spawn pump(fd) close_fd(fd) 0 }",
        );
        assert!(
            e.is_err(),
            "close_fd after moving fd into a spawn must be rejected"
        );
        assert!(
            e.unwrap_err().contains("close_fd"),
            "the error should name the close"
        );
    }

    #[test]
    fn accepts_close_fd_with_no_spawn() {
        let ok = check("fn f(fd: i64) -> i64 { close_fd(fd) 0 }");
        assert!(ok.is_ok(), "close_fd without a spawn is fine, got {ok:?}");
    }

    #[test]
    fn accepts_closing_a_different_descriptor_than_the_spawned_one() {
        let ok = check(
            "fn pump(fd: i64) -> i64 { 0 }\n\
             fn f(a: i64, b: i64) -> i64 { spawn pump(a) close_fd(b) 0 }",
        );
        assert!(
            ok.is_ok(),
            "closing a descriptor the thread does not hold is fine, got {ok:?}"
        );
    }

    #[test]
    fn accepts_close_fd_after_the_variable_is_reassigned() {
        // Reassigning `fd` after the spawn makes it a fresh descriptor the thread never saw,
        // so closing it is legitimate: the move applied to the old value, not the new one.
        let ok = check(
            "fn pump(fd: i64) -> i64 { 0 }\n\
             fn open_one() -> i64 { 3 }\n\
             fn f(fd: i64) -> i64 { spawn pump(fd) fd = open_one() close_fd(fd) 0 }",
        );
        assert!(ok.is_ok(), "close of a reassigned fd is fine, got {ok:?}");
    }
}

#[cfg(test)]
mod exit_in_spawn_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_no_process_exit_in_spawn(&funcs)
    }

    // RFC-0011 footgun #3: `syscall(SYS_exit, N)` ends the calling THREAD under process
    // semantics but the whole PROCESS inside a spawned body, so one connection's error path
    // would take down the relay and every other connection with it, silently.

    #[test]
    fn rejects_exit_directly_in_a_spawned_body() {
        let e = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { syscall(SYS_exit, 2) 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(e.is_err(), "exit inside a spawned body must be rejected");
        let msg = e.unwrap_err();
        assert!(
            msg.contains("worker"),
            "error should name the offending function: {msg}"
        );
        assert!(
            msg.contains("task_return") || msg.contains("whole process"),
            "error should explain the process-termination effect: {msg}"
        );
    }

    #[test]
    fn rejects_exit_through_a_call_chain_from_a_spawned_body() {
        // The effect propagates up the call graph: `worker` is clean itself but calls `bail`,
        // which exits. A spawned body must be free of the process-exit effect transitively.
        let e = check(
            "let SYS_exit = 60\n\
             fn bail() -> i64 { syscall(SYS_exit, 3) 0 }\n\
             fn worker() -> i64 { bail() 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(
            e.is_err(),
            "exit reached through a call chain must be rejected"
        );
    }

    #[test]
    fn rejects_exit_in_a_body_spawned_from_inside_an_if() {
        // `if` is an expression, so a seed walk that only looks at top-level statements and `for`
        // bodies never sees this spawn and the effect check silently does not apply to `worker`.
        let e = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { syscall(SYS_exit, 2) 0 }\n\
             fn f(c: i64) -> i64 { if c > 0 { spawn worker() } 0 }",
        );
        assert!(
            e.is_err(),
            "a spawn inside an if must still carry the effect check"
        );
    }

    #[test]
    fn rejects_exit_in_a_par_let_peer() {
        // `let a ~~ b = par f() ~~ g()` runs its peers on spawned threads exactly like a bare
        // `par`, so the same effect must be forbidden in them.
        let e = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { syscall(SYS_exit, 2) 0 }\n\
             fn other() -> i64 { 1 }\n\
             fn f() -> i64 { let a ~~ b = par worker() ~~ other()\na + b }",
        );
        assert!(
            e.is_err(),
            "a par-let peer runs on a thread, so exit must be rejected there too"
        );
    }

    #[test]
    fn rejects_exit_in_a_body_spawned_from_a_tail_conditional() {
        // Same blind spot on the effect check: a spawn written as the last thing in a function
        // lives in the block's tail, so the seed walk never saw it and the worker was treated as
        // if it never ran on a thread.
        let e = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { syscall(SYS_exit, 2) 0 }\n\
             fn f(c: i64) -> i64 { if c > 0 { spawn worker() } }",
        );
        assert!(
            e.is_err(),
            "a spawn in a tail conditional must still carry the effect check"
        );
    }

    #[test]
    fn rejects_exit_via_integer_literal_syscall_number() {
        // The syscall number may be written as the raw literal 60 instead of the SYS_exit alias.
        let e = check(
            "fn worker() -> i64 { syscall(60, 1) 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(
            e.is_err(),
            "exit written as syscall(60, ..) must be rejected too"
        );
    }

    #[test]
    fn accepts_exit_outside_any_spawned_body() {
        // The same call is perfectly fine in code that is never spawned: a normal program
        // exits its own process on purpose. The effect is only forbidden under `spawn`.
        let ok = check(
            "let SYS_exit = 60\n\
             fn main() -> i64 { syscall(SYS_exit, 0) 0 }",
        );
        assert!(
            ok.is_ok(),
            "exit in non-spawned code is legitimate, got {ok:?}"
        );
    }

    #[test]
    fn rejects_exit_reached_through_a_nested_spawn() {
        // A spawned body may itself spawn: `outer` runs on a thread and spawns `inner`, which
        // exits. `inner` also runs on a thread, so the process-exit effect must be followed
        // across the nested spawn, not only across plain calls.
        let e = check(
            "let SYS_exit = 60\n\
             fn inner() -> i64 { syscall(SYS_exit, 9) 0 }\n\
             fn outer() -> i64 { spawn inner() 0 }\n\
             fn f() -> i64 { spawn outer() 0 }",
        );
        assert!(
            e.is_err(),
            "exit inside a nested spawned body must be rejected"
        );
        assert!(
            e.unwrap_err().contains("inner"),
            "the error should name the nested body"
        );
    }

    #[test]
    fn accepts_exit_in_a_helper_reached_only_off_the_spawn_path() {
        // `helper` exits, but it is reached only from the non-spawned continuation, never from
        // the spawned body. The effect is scoped to what runs on a thread, so this stays legal.
        let ok = check(
            "let SYS_exit = 60\n\
             fn helper() -> i64 { syscall(SYS_exit, 0) 0 }\n\
             fn worker() -> i64 { 0 }\n\
             fn f() -> i64 { spawn worker() helper() 0 }",
        );
        assert!(
            ok.is_ok(),
            "exit off the spawn path is legitimate, got {ok:?}"
        );
    }

    #[test]
    fn accepts_a_spawned_body_that_does_not_exit() {
        let ok = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(
            ok.is_ok(),
            "a clean spawned body must be accepted, got {ok:?}"
        );
    }
}

/// Rejects raw `peek`/`poke` on a buffer that was DECLARED as an array or a view.
///
/// A declared buffer knows how long it is, so `b[i]` is checked and `peek64(b + i * 8)` is not:
/// the two spellings reach the same bytes and only one of them is watched. Leaving both available
/// means the bounds check is opt-out by accident, and the way out is a byte shorter to type than
/// the way in. So the raw form is refused where the checked form exists.
///
/// `peek`/`poke` stay legal on a RAW pointer: memory from an allocator, a string in rodata, a
/// buffer a syscall filled. That is the boundary where there is no declared length to check
/// against, and pretending otherwise would only push the same hole one call deeper.
///
/// What a classic array does get is the half that costs nothing: a raw access at a CONSTANT
/// offset past its end is refused by `check_constant_bounds`, since the compiler can decide that
/// one without a migration. What stays legal here is the raw access it cannot decide.
///
/// Scoped to VIEWS for now, not to classic `let a[N]` arrays. Measured, not assumed: extending it
/// to arrays refuses 35 sites across the standard library's own tests (`pad`, `one`, `buf`, `tmp`
/// and friends), so landing it would break the library and every program built on it until each
/// one is migrated. Views are new, nothing reaches into them raw yet, and the code being written
/// against them right now is exactly the code this protects. The array half is a separate,
/// announced migration, not a side effect of this.
pub fn check_no_raw_access_to_declared_buffers(
    funcs: &[Function],
    globals: &[Global],
) -> Result<(), String> {
    fn is_raw_access(name: &str) -> bool {
        matches!(
            name,
            "peek8" | "peek16" | "peek32" | "peek64" | "poke8" | "poke16" | "poke32" | "poke64"
        )
    }
    // Every variable named anywhere in an expression.
    fn names_in(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Var(n) => out.push(n.clone()),
            Expr::Field(b, _) | Expr::Unary(_, b) | Expr::Cast(b, _) => names_in(b, out),
            Expr::Binary(_, a, b) | Expr::Index(a, b) => {
                names_in(a, out);
                names_in(b, out);
            }
            Expr::Call(c, args) => {
                names_in(c, out);
                for a in args {
                    names_in(&a.value, out);
                }
            }
            Expr::List(xs) => {
                for x in xs {
                    names_in(x, out);
                }
            }
            _ => {}
        }
    }
    fn walk_expr(e: &Expr, declared: &HashSet<String>, fname: &str) -> Result<(), String> {
        if let Expr::Call(callee, args) = e
            && let Expr::Var(cname) = callee.as_ref()
            && is_raw_access(cname)
            && let Some(first) = args.first()
        {
            let mut names = Vec::new();
            names_in(&first.value, &mut names);
            if let Some(hit) = names.iter().find(|n| declared.contains(*n)) {
                return Err(format!(
                    "function `{fname}`: `{cname}` reaches into `{hit}`, which is declared with a \
                     length, so the compiler knows how long it is and cannot check this access. \
                     Index it instead: `{hit}[i]`, which is checked. Raw `peek`/`poke` are for \
                     pointers with no declared length (an allocation, a string, a buffer a syscall \
                     filled), not for buffers that could be checked and are not."
                ));
            }
        }
        let mut inner = Vec::new();
        nested_blocks(e, &mut inner);
        for b in inner {
            walk_block(b, declared, fname)?;
        }
        if let Expr::Call(_, args) = e {
            for a in args {
                walk_expr(&a.value, declared, fname)?;
            }
        }
        if let Expr::Binary(_, l, r) = e {
            walk_expr(l, declared, fname)?;
            walk_expr(r, declared, fname)?;
        }
        Ok(())
    }
    fn walk_block(b: &Block, declared: &HashSet<String>, fname: &str) -> Result<(), String> {
        let mut declared = declared.clone();
        for s in &b.stmts {
            // A name is only protected from the point it is declared with a length.
            if let Stmt::Let {
                name, ty, value, ..
            } = s
            {
                walk_expr(value, &declared, fname)?;
                let is_view = matches!(ty.as_ref(), Some(Type::View(..)));
                if is_view {
                    declared.insert(name.clone());
                } else {
                    declared.remove(name); // rebinding drops the protection with the length
                }
                continue;
            }
            match s {
                Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Spawn(e) => {
                    walk_expr(e, &declared, fname)?
                }
                Stmt::Assign { target, value } => {
                    walk_expr(target, &declared, fname)?;
                    walk_expr(value, &declared, fname)?;
                }
                Stmt::For { body, .. } => walk_block(body, &declared, fname)?,
                _ => {}
            }
            let mut inner = Vec::new();
            nested_blocks_of_stmt(s, &mut inner);
            for blk in inner {
                walk_block(blk, &declared, fname)?;
            }
        }
        if let Some(t) = &b.tail {
            walk_expr(t, &declared, fname)?;
        }
        Ok(())
    }
    for f in funcs {
        // A parameter declared as a view carries its length too, so it is protected from entry.
        let mut declared: HashSet<String> = HashSet::new();
        for p in &f.params {
            if matches!(&p.ty, Type::View(..)) {
                declared.insert(p.name.clone());
            }
        }
        // Module-level view-typed globals are declared with a length everywhere, so they
        // are protected from raw peek/poke across all functions.
        //
        // A string literal is the exception, and not a special case: `let S: string = "..."`
        // lowers to the same `Type::View` shape, but at module level it holds the ADDRESS of
        // its UTF-8 bytes rather than storage of its own. It is a pointer with no declared
        // length, which is exactly what raw `peek`/`poke` are for. Protecting it would reject
        // working code and point the caller at `S[i]`, which walks code-point-wide slots over
        // a byte pointer and answers garbage without saying so.
        for g in globals {
            if matches!(&g.ty, Some(Type::View(..))) && !matches!(&g.init, Expr::Str(_)) {
                declared.insert(g.name.clone());
            }
        }
        walk_block(&f.body, &declared, &f.name)?;
    }
    Ok(())
}

/// Checks all range-based `for` loops (`for i in a..b`) for inverted/empty ranges.
///
/// Two cases:
/// 1. Both bounds are constants → if `start > end`, compile error (inverted range).
///    `start == end` (e.g. `5..5`) compiles: it is the idiomatic zero-element range,
///    not an inversion.
/// 2. At least one bound is variable → require that `start > end` is NOT provable.
///
/// Wrapping functions are skipped: their arithmetic wraps by specification (crypto
/// substrate), so overflow-based range checks are meaningless there.
pub fn check_for_range_bounds(funcs: &[Function]) -> Result<(), String> {
    for f in funcs {
        let mut flow = Vec::new();
        flow.extend(f.requires.iter().cloned());
        if f.is_wrapping {
            continue;
        }
        range_bounds_walk_block(&f.body, &f.name, &f.params, &flow)?;
    }
    Ok(())
}

/// Find the range start value for a loop variable: the `let mut i = ...` statement
/// that precedes a range-based for loop. Returns `Some(value)` where `value` is the
/// expression assigned to the counter, or `None` if not found.
///
/// `loop_idx` is the index of the `Stmt::For` statement that triggered this lookup.
/// We scan backwards only from `loop_idx` to avoid finding a later `let mut i = ...`
/// that belongs to a different loop reusing the same counter variable.
fn find_range_start<'a>(block: &'a Block, var: &str, loop_idx: usize) -> Option<&'a Expr> {
    for i in (0..loop_idx).rev() {
        if let Stmt::Let {
            mutable: true,
            name,
            value,
            ..
        } = &block.stmts[i]
            && name == var
        {
            return Some(value);
        }
    }
    None
}

fn range_bounds_const_fold(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::Unary(UnOp::Neg, inner) => {
            range_bounds_const_fold(inner).and_then(|v| v.checked_neg())
        }
        Expr::Binary(op, l, r) => {
            let a = range_bounds_const_fold(l)?;
            let b = range_bounds_const_fold(r)?;
            match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                _ => None,
            }
        }
        _ => None,
    }
}

#[allow(clippy::collapsible_if)]
fn range_bounds_walk_block(
    b: &Block,
    fname: &str,
    params: &[Param],
    flow: &[Expr],
) -> Result<(), String> {
    for (stmt_idx, stmt) in b.stmts.iter().enumerate() {
        match stmt {
            // Range-based for loop: detected by the `is_range_for` flag set by the parser.
            Stmt::For {
                cond: Some(cond_expr),
                body,
                is_range_for: true,
                ..
            } => {
                // `cond_expr` is `i < bound` where `i` is the loop variable.
                // The lower bound is the value from the preceding `let mut i = <value>`
                // desugaring (always a mutable binding since the parser created it).
                if let Expr::Binary(BinOp::Lt, lhs, rhs) = cond_expr {
                    if let Expr::Var(var) = lhs.as_ref() {
                        // Find the preceding `let mut` that sets this variable.
                        // Since the parser always emits `let mut i = value` immediately before
                        // `for i < bound`, scan backwards from the position of the current stmt.
                        let start_expr = find_range_start(b, var, stmt_idx);

                        if let Some(start) = start_expr {
                            if let (Some(start_val), Some(end_val)) = (
                                range_bounds_const_fold(start),
                                range_bounds_const_fold(rhs.as_ref()),
                            ) {
                                // Both bounds constant: reject if inverted.
                                // 5..5 (start == end) is accepted: it is the normal zero-element range.
                                if start_val > end_val {
                                    return Err(format!(
                                        "function `{fname}`: the range \
                                         `{s}..{e}` \
                                         is inverted (goes from greater to lesser), so the loop body \
                                         never executes. Did you mean `{e}..{s}`?",
                                        s = start_val,
                                        e = end_val,
                                    ));
                                }
                            } else {
                                // At least one bound is variable: check if `start > end` is provable
                                // (rejection) or if `start <= end` is provable (acceptance).
                                // If BOTH bounds are constants and not provably inverted: accept.
                                // If only one bound is variable: accept the range (the programmer
                                // using `0..n` or `n..10` doesn't need to write a contract).
                                // If BOTH bounds are variable: require a contract.
                                let start_has_var = !matches!(start, Expr::Int(_));
                                let end_has_var = !matches!(rhs.as_ref(), Expr::Int(_));
                                let start_expr_str = range_bounds_expr_short(start);
                                let end_expr_str = range_bounds_expr_short(rhs.as_ref());
                                if start_has_var && end_has_var {
                                    if crate::verify::proves_gt_for_bounds(
                                        start,
                                        rhs.as_ref(),
                                        flow,
                                        params,
                                    ) {
                                        // `start > end` is provably true: reject.
                                        return Err(format!(
                                            "function `{fname}`: the range \
                                             `{start_expr_str}..{end_expr_str}` \
                                             is provably empty: `{start_expr_str}` is provably greater \
                                             than `{end_expr_str}`, so the loop body never executes. \
                                             Provide a precondition `requires {{ {start_expr_str} <= {end_expr_str} }}`.",
                                        ));
                                    }
                                    // Check if a contract/guard guarantees `start <= end`.
                                    if crate::verify::proves_le_for_bounds(
                                        start,
                                        rhs.as_ref(),
                                        flow,
                                        params,
                                    ) {
                                        // Flow guarantees the range is non-empty.
                                    } else {
                                        // No guarantee found: reject.
                                        return Err(format!(
                                            "function `{fname}`: the range \
                                             `{start_expr_str}..{end_expr_str}` \
                                             has variable bounds that are not provably ordered. \
                                             Provide a precondition `requires {{ {start_expr_str} <= {end_expr_str} }}` \
                                             or guard the loop body with `if {start_expr_str} <= {end_expr_str}`.",
                                        ));
                                    }
                                }
                                // Only one bound is variable: accept (common idiom like `0..n`).
                            }
                        }
                        // Walk inside loop body, extending flow with `var < bound` fact.
                        let mut block_flow = flow.to_vec();
                        if let Some(vval) = range_bounds_const_fold(rhs.as_ref()) {
                            block_flow.push(Expr::Binary(
                                BinOp::Lt,
                                Box::new(Expr::Var(var.clone())),
                                Box::new(Expr::Int(vval)),
                            ));
                        }
                        range_bounds_walk_block(body, fname, params, &block_flow)?;
                    }
                }
            }
            // Condition-based or foreach for loop: always visit the body.
            Stmt::For { cond, body, .. } => {
                // Propagate flow into the body: if the loop body is entered,
                // it means `cond` was true on this iteration.
                let mut block_flow = flow.to_vec();
                if let Some(c) = cond {
                    block_flow.push((*c).clone());
                }
                range_bounds_walk_block(body, fname, params, &block_flow)?;
            }
            // Walk expressions within this statement (for variable assignments that might
            // become range bounds in a following for loop).
            _ => {
                range_bounds_walk_expr_for_stmt(stmt, fname, params, flow)?;
            }
        }
    }
    if let Some(tail) = &b.tail {
        range_bounds_walk_expr(tail, fname, params, flow)?;
    }
    Ok(())
}

/// Extract and walk all expressions from a statement.
fn range_bounds_walk_expr_for_stmt(
    s: &Stmt,
    fname: &str,
    params: &[Param],
    flow: &[Expr],
) -> Result<(), String> {
    match s {
        Stmt::Let { value, .. } => range_bounds_walk_expr(value, fname, params, flow),
        Stmt::Expr(v) => range_bounds_walk_expr(v, fname, params, flow),
        Stmt::Return(Some(v)) => range_bounds_walk_expr(v, fname, params, flow),
        Stmt::Spawn(v) => range_bounds_walk_expr(v, fname, params, flow),
        Stmt::Assign { target, value } => {
            range_bounds_walk_expr(target, fname, params, flow)?;
            range_bounds_walk_expr(value, fname, params, flow)
        }
        Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
            let mut ok = Ok(());
            for p in peers {
                ok = range_bounds_walk_expr(p, fname, params, flow);
                if ok.is_err() {
                    break;
                }
            }
            ok
        }
        Stmt::GraphLet {
            nodes, yield_expr, ..
        } => {
            for (_, e) in nodes {
                range_bounds_walk_expr(e, fname, params, flow)?;
            }
            range_bounds_walk_expr(yield_expr, fname, params, flow)
        }
        _ => Ok(()),
    }
}

fn range_bounds_walk_expr(
    e: &Expr,
    fname: &str,
    params: &[Param],
    flow: &[Expr],
) -> Result<(), String> {
    match e {
        Expr::Binary(_, l, r) => {
            range_bounds_walk_expr(l, fname, params, flow)?;
            range_bounds_walk_expr(r, fname, params, flow)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            range_bounds_walk_expr(x, fname, params, flow)
        }
        Expr::Index(a, b) => {
            range_bounds_walk_expr(a, fname, params, flow)?;
            range_bounds_walk_expr(b, fname, params, flow)
        }
        Expr::Call(c, args) => {
            range_bounds_walk_expr(c, fname, params, flow)?;
            for a in args {
                range_bounds_walk_expr(&a.value, fname, params, flow)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                range_bounds_walk_expr(it, fname, params, flow)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                range_bounds_walk_expr(v, fname, params, flow)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            range_bounds_walk_expr(cond, fname, params, flow)?;
            let mut then_flow = flow.to_vec();
            then_flow.push((**cond).clone());
            range_bounds_walk_block(then_branch, fname, params, &then_flow)?;
            if let Some(eb) = else_branch {
                range_bounds_walk_block(eb, fname, params, flow)?;
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            range_bounds_walk_expr(scrutinee, fname, params, flow)?;
            for a in arms {
                range_bounds_walk_block(&a.body, fname, params, flow)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Render an expression to a short string.
fn range_bounds_expr_short(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Var(n) => n.clone(),
        Expr::Unary(UnOp::Neg, inner) => format!("-({})", range_bounds_expr_short(inner)),
        Expr::Binary(op, l, r) => {
            let sym = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                _ => "?",
            };
            format!(
                "{} {} {}",
                range_bounds_expr_short(l),
                sym,
                range_bounds_expr_short(r)
            )
        }
        Expr::Field(base, field) => format!("{}.{}", range_bounds_expr_short(base), field),
        Expr::Index(base, idx) => format!(
            "{}[{}]",
            range_bounds_expr_short(base),
            range_bounds_expr_short(idx)
        ),
        _ => "<expr>".to_string(),
    }
}

#[cfg(test)]
mod raw_access_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_no_raw_access_to_declared_buffers(&funcs, &globals)
    }

    #[test]
    fn a_string_is_not_refused_while_it_cannot_be_indexed() {
        // A TRIPWIRE, not a guarantee. The rule keys on `Type::View(..)`, so the day `string`
        // is represented as a length-carrying view this test goes RED.
        //
        // That is the point. The rule exists to push every declared buffer onto `buf[i]`, and it
        // is only fair once `buf[i]` EXISTS. A `string` today carries no length to index with, so
        // refusing `peek8` on it would take away the only access there is and leave the stdlib
        // between two rules, each right on its own — which is exactly what a full migration of 47
        // files hit: everything compiled except the 8 files where a parameter had become `string`.
        //
        // So when this goes red, the question to answer is NOT "how do I exempt string". It is
        // "did `string` become indexable in the same change?" If yes, flip this test to assert
        // refusal and migrate the `peek8` sites to `s[i]`. If no, the type change landed too
        // early and the prohibition must not be wired yet.
        let src = "fn largo(s: string) -> i64 { peek8(s + 0) }\n\
                   fn main() -> i64 { largo(\"hola\") }";
        let funcs = Parser::new(tokenize(src)).parse_program();
        assert!(
            check_no_raw_access_to_declared_buffers(&funcs, &[]).is_ok(),
            "a string cannot be indexed yet, so raw access is the only access it has"
        );
    }

    #[test]
    fn a_classic_array_is_not_refused_yet_and_that_is_deliberate() {
        // Not an oversight: extending this to `let a[N]` refuses 35 sites in the standard
        // library's own tests, so it would break the library and everything built on it until
        // each site is migrated. Pinned here so the scope is a decision on the record rather
        // than something that drifts.
        let ok = check("fn f() -> i64 { let a[4]\npoke64(a + 8, 7)\n0 }");
        assert!(
            ok.is_ok(),
            "the array half is a separate, announced migration, got {ok:?}"
        );
    }

    #[test]
    fn a_declared_view_cannot_be_reached_with_raw_peek() {
        let e = check("fn f(px: u32[n], n: i64) -> i64 { peek32(px + 4) }");
        assert!(e.is_err(), "raw peek into a declared view must be refused");
    }

    #[test]
    fn a_raw_pointer_is_still_free_to_peek_and_poke() {
        // The boundary where there IS no declared length: an allocation, a string, a buffer a
        // syscall filled. Refusing here would push the same hole one call deeper, not close it.
        let ok = check("fn f(p: i64) -> i64 { poke8(p, 65)\npeek8(p + 1) }");
        assert!(
            ok.is_ok(),
            "a raw pointer has no length to check against, got {ok:?}"
        );
    }

    #[test]
    fn a_name_rebound_to_a_scalar_loses_the_protection_with_the_length() {
        // Rebinding drops the length, so it must also drop the prohibition: refusing here would
        // reject correct code, and a check that rejects correct code gets switched off.
        let ok = check("fn f(p: i64) -> i64 { let a[4]\nlet a = p\npeek8(a) }");
        assert!(
            ok.is_ok(),
            "after rebinding, `a` is a plain pointer again, got {ok:?}"
        );
    }

    #[test]
    fn the_prohibition_starts_where_the_declaration_does() {
        // A name that is not a declared buffer YET must not be caught by a later declaration.
        let ok = check("fn f(a: i64) -> i64 { let x = peek8(a)\nlet a[4]\nx }");
        assert!(
            ok.is_ok(),
            "the parameter `a` is a raw pointer at that point, got {ok:?}"
        );
    }

    #[test]
    fn a_module_view_is_protected_from_raw_peek8() {
        // A module-level view must not be accessible via peek/poke: the same rule that protects
        // a local declared buffer (or parameter) applies here. `peek8(g + 100)` into a
        // `let mut g: u8[8]` must be refused with the same message as the local version.
        let e = check("let mut g: u8[8]\nfn f() -> i64 { peek8(g + 100) }");
        assert!(e.is_err(), "raw peek into a module view must be refused");
        let err = e.unwrap_err();
        assert!(
            err.contains("peek8") && err.contains("declared with a length") && err.contains("g"),
            "the error must name the function, the accessor, and the buffer; got: {err}"
        );
    }

    #[test]
    fn a_module_view_is_protected_from_raw_poke8() {
        let e = check("let mut g: u8[16]\nfn f() -> i64 { poke8(g + 0, 65)\n0 }");
        assert!(e.is_err(), "raw poke into a module view must be refused");
    }

    #[test]
    fn peek8_on_other_view_accessors_is_protected() {
        // peek16, peek32, peek64, poke16, poke32, poke64 on module views must also be refused.
        let e1 = check("let mut g: u16[4]\nfn f() -> i64 { peek16(g + 0) }");
        assert!(e1.is_err(), "peek16 into a u16 module view must be refused");
        let e2 = check("let mut g: u32[8]\nfn f() -> i64 { poke32(g + 0, 42)\n0 }");
        assert!(e2.is_err(), "poke32 into a u32 module view must be refused");
    }

    #[test]
    fn a_module_string_literal_is_still_a_raw_pointer() {
        // `let S: string = "..."` at module level is NOT a declared buffer: it lowers to the
        // ADDRESS of its UTF-8 bytes in `.rodata`, exactly like a literal handed to a function.
        // Reading those bytes with `peek8` is the only access it has today, so protecting it
        // here would reject working code and send the caller to `S[i]`, which reads
        // code-point-wide slots off a byte pointer and quietly answers garbage.
        let ok = check("let S: string = \"anb\"\nfn f() -> i64 { peek8(S + 1) }");
        assert!(
            ok.is_ok(),
            "a module string literal is a pointer, not a buffer with a length; got {ok:?}"
        );
    }
}

/// Tests for inverted/empty range bounds in range-based for loops.
mod range_bounds_checks {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    #[allow(dead_code)]
    fn check_for(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        check_for_range_bounds(&funcs)
    }

    #[test]
    fn constant_inverted_range_is_rejected() {
        // `3..0` is inverted and must be rejected with a clear message.
        let err = check_for("fn main() -> i64 { let mut n = 0\nfor i in 3..0 { n = n + 1 }\nn }")
            .unwrap_err();
        assert!(
            err.contains("3..0"),
            "error should mention the inverted range"
        );
        assert!(err.contains("inverted"), "error should say it is inverted");
        assert!(err.contains("0..3"), "error should suggest the fix");
    }

    #[test]
    fn constant_empty_range_55_compiles() {
        // `5..5` is not inverted (start == end), it is the idiomatic zero-element range.
        let ok = check_for("fn main() -> i64 { let mut n = 0\nfor i in 5..5 { n = n + 1 }\nn }");
        assert!(
            ok.is_ok(),
            "5..5 (empty zero-element range) should compile: {ok:?}"
        );
    }

    #[test]
    fn both_bounds_variable_requires_contract() {
        // `a..b` where both are parameters requires a contract or guard.
        let err = check_for(
            "fn cuenta(a: i64, b: i64) { let mut n = 0\nfor i in a..b { n = n + 1 }\nn }",
        )
        .unwrap_err();
        assert!(
            err.contains("a..b"),
            "error should mention the variable bounds: {err}"
        );
        assert!(
            err.contains("requires"),
            "error should suggest a precondition: {err}"
        );
    }

    #[test]
    fn one_constant_bound_compiles_freely() {
        // `0..n` where only `n` is variable: accepted as common idiom.
        let ok = check_for(
            "fn main() -> i64 { let n = 5\nlet mut i = 0\nfor i in 0..n { i = i + 1 }\ni }",
        );
        assert!(ok.is_ok(), "0..n should compile without contract: {ok:?}");
    }

    #[test]
    fn provable_empty_range_is_rejected() {
        // `n+1..n` is provably empty (n+1 > n always).
        let err = check_for(
            "fn main() -> i64 { let n = 5\nlet mut i = 0\nfor i in n + 1..n { i = i + 1 }\ni }",
        )
        .unwrap_err();
        assert!(
            err.contains("provably"),
            "provable empty range should say provably: {err}"
        );
    }

    #[test]
    fn requires_guarantee_allows_variable_bounds() {
        // `requires { a <= b }` discharges the obligation.
        let ok = check_for(
            "fn cuenta(a: i64, b: i64) { let mut n = 0\nfor i in a..b { n = n + 1 }\nn }",
        );
        // Without requires, this should fail.
        assert!(ok.is_err(), "a..b without requires should fail");

        let ok_with_contract = check_for(
            "fn cuenta(a: i64, b: i64) requires { a <= b } { let mut n = 0\nfor i in a..b { n = n + 1 }\nn }",
        );
        assert!(
            ok_with_contract.is_ok(),
            "a..b with requires should compile: {ok_with_contract:?}"
        );
    }

    #[test]
    fn nested_inverted_range_is_detected() {
        // An inverted range inside another loop should still be caught.
        let err = check_for(
            "fn main() -> i64 { let mut n = 0\nfor i in 0..5 { let mut j = 0\nfor j in 3..0 { n = n + 1 } }\nn }",
        )
        .unwrap_err();
        assert!(
            err.contains("3..0"),
            "nested inverted range should be caught: {err}"
        );
    }

    #[test]
    fn wrapping_function_is_skipped() {
        // Wrapping functions are skipped (crypto substrate).
        let ok = check_for(
            "wrapping fn crypto() -> i64 { let mut n = 0\nfor i in 3..0 { n = n + 1 }\nn }",
        );
        assert!(ok.is_ok(), "wrapping functions should be skipped");
    }

    #[test]
    fn two_consecutive_loops_same_var_both_valid() {
        // Two loops in the same block reusing `i`, both with valid ranges.
        let ok = check_for(
            "fn main() -> i64 {\n\
             let mut s = 0\n\
             for i in 0..3 { s = s + 1 }\n\
             for i in 5..9 { s = s + 1 }\n\
             s\n\
             }",
        );
        assert!(
            ok.is_ok(),
            "two consecutive valid loops should compile: {ok:?}"
        );
    }

    #[test]
    fn two_loops_second_inverted() {
        // Two loops, the second one has an inverted range.
        let err = check_for(
            "fn main() -> i64 {\n\
             let mut s = 0\n\
             for i in 0..3 { s = s + 1 }\n\
             for i in 9..5 { s = s + 1 }\n\
             s\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("9..5"),
            "error should mention the second loop's range: {err}"
        );
        assert!(
            !err.contains("0..3"),
            "error must not mention the first loop's range: {err}"
        );
    }

    #[test]
    fn three_loops_middle_inverted() {
        // Three loops with the same variable, the middle one is inverted.
        let err = check_for(
            "fn main() -> i64 {\n\
             let mut s = 0\n\
             for i in 0..3 { s = s + 1 }\n\
             for i in 5..2 { s = s + 1 }\n\
             for i in 7..10 { s = s + 1 }\n\
             s\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("5..2"),
            "error should mention the middle loop: {err}"
        );
        assert!(!err.contains("0..3"), "must not mention first loop: {err}");
        assert!(!err.contains("7..10"), "must not mention third loop: {err}");
    }

    #[test]
    fn nested_loop_reused_var_inverted_inner() {
        // A loop inside another loop, both reusing `i`, the inner one is inverted.
        let err = check_for(
            "fn main() -> i64 {\n\
             let mut s = 0\n\
             for i in 0..5 {\n\
                 for i in 3..0 { s = s + 1 }\n\
             }\n\
             s\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("3..0"),
            "nested inverted range should be caught: {err}"
        );
    }

    #[test]
    fn hand_written_while_does_not_trigger_range_check() {
        // A hand-written while loop using `let mut i = 0` + `for i < 3`
        // does NOT have the `is_range_for` flag, so it should not enter
        // the range-bounds check.
        let ok = check_for(
            "fn main() -> i64 {\n\
             let mut i = 0\n\
             for i < 3 { i = i + 1 }\n\
             i\n\
             }",
        );
        assert!(
            ok.is_ok(),
            "hand-written while should not trigger range check: {ok:?}"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Module-level string indexing → compile error
// ──────────────────────────────────────────────────────────────────────────────

/// Indexing a module-level `string` variable is a compile error.
pub fn check_module_string_indexing(
    funcs: &[Function],
    string_globals: &HashSet<String>,
) -> Result<(), String> {
    for f in funcs {
        let mut alive: HashSet<&str> = string_globals.iter().map(|s| s.as_str()).collect();
        walk_stmt_list(&f.body.stmts, &f.name, &mut alive)?;
        if let Some(ref t) = f.body.tail {
            expr_string_index(t, &f.name, &alive)?;
        }
    }
    Ok(())
}

fn walk_stmt_list(stmts: &[Stmt], fname: &str, alive: &mut HashSet<&str>) -> Result<(), String> {
    for st in stmts {
        match st {
            Stmt::Let { name, value, .. } => {
                // Shadowing: remove from alive.
                alive.remove(name.as_str());
                // Check the initializer.
                expr_string_index(value, fname, alive)?;
            }
            Stmt::Assign { target, value } => {
                // S = ... drops S.
                if let Expr::Var(ref n) = *target {
                    alive.remove(n.as_str());
                }
                expr_string_index(target, fname, alive)?;
                expr_string_index(value, fname, alive)?;
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => {
                expr_string_index(e, fname, alive)?
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    expr_string_index(c, fname, alive)?
                }
                walk_stmt_list(&body.stmts, fname, alive)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn expr_string_index_with_name<'a>(
    e: &'a Expr,
    alive: &'a HashSet<&'a str>,
    rebased: Option<&str>,
    fname: &str,
) -> Result<(), String> {
    let is_alive_string = |name: &str| (rebased != Some(name)) && alive.contains(name);

    match e {
        Expr::Index(base, idx) => {
            if let Expr::Var(ref name) = **base {
                if is_alive_string(name) {
                    if let Some(k) = const_index(idx) {
                        return Err(format!(
                            "function `{fname}`: indexing `{name}` with `{k}` — `{name}` is a \
                             `string` declared at module level, which holds the ADDRESS of its \
                             UTF-8 bytes, not a code-point view; the compiler multiplies by 4 and \
                             reads garbage. Use `peek8({name} + {k})` to read the raw UTF-8 byte \
                             at offset {k} (those are UTF-8 bytes, not characters)."
                        ));
                    }
                    return Err(format!(
                        "function `{fname}`: indexing `{name}` — `{name}` is a `string` \
                         declared at module level, which holds the ADDRESS of its UTF-8 bytes, \
                         not a code-point view; the compiler multiplies the index by 4 and reads \
                         garbage. Use `peek8({name} + i)` to read the raw UTF-8 byte at offset \
                         `i` (those are UTF-8 bytes, not characters)."
                    ));
                }
                return expr_string_index_with_name(base, alive, rebased, fname);
            }
            expr_string_index_with_name(base, alive, rebased, fname)
        }
        Expr::Binary(_, l, r) => {
            expr_string_index_with_name(l, alive, rebased, fname)?;
            expr_string_index_with_name(r, alive, rebased, fname)
        }
        Expr::Unary(_, x) | Expr::Cast(x, _) | Expr::Field(x, _) => {
            expr_string_index_with_name(x, alive, rebased, fname)
        }
        Expr::Call(c, args) => {
            expr_string_index_with_name(c, alive, rebased, fname)?;
            for a in args {
                expr_string_index_with_name(&a.value, alive, rebased, fname)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_string_index_with_name(cond, alive, rebased, fname)?;
            let mut alive2 = alive.clone();
            walk_stmt_list(&then_branch.stmts, fname, &mut alive2)?;
            if let Some(eb) = else_branch {
                let mut alive3 = alive.clone();
                walk_stmt_list(&eb.stmts, fname, &mut alive3)?;
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            expr_string_index_with_name(scrutinee, alive, rebased, fname)?;
            // Clone alive since match arms don't shadow module strings (they're just blocks) but
            // walk_stmt_list needs &mut. Match arms are independent so the clone is safe.
            for a in arms {
                let mut _a = alive.clone();
                walk_stmt_list(&a.body.stmts, fname, &mut _a)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                expr_string_index_with_name(it, alive, rebased, fname)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                expr_string_index_with_name(v, alive, rebased, fname)?;
            }
            Ok(())
        }
        Expr::SliceRange(b, lo, hi) => {
            expr_string_index_with_name(b, alive, rebased, fname)?;
            if let Some(l) = lo {
                expr_string_index_with_name(l, alive, rebased, fname)?;
            }
            if let Some(h) = hi {
                expr_string_index_with_name(h, alive, rebased, fname)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn expr_string_index(e: &Expr, fname: &str, alive: &HashSet<&str>) -> Result<(), String> {
    expr_string_index_with_name(e, alive, None, fname)
}

#[cfg(test)]
mod string_index_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn string_globals(_funcs: &[Function], globals: &[Global]) -> HashSet<String> {
        // A module-level string is declared as `string` and its initializer is a string literal.
        // The parser lowers `string` to `View(u32, N)` and stores `Expr::Str(...)` as init.
        // A byte buffer like `u8[8]` is NOT a string even if its type is `View(u8, ...)`:
        // it is a real byte array. Distinguish by checking that the init is a Str expression.
        globals
            .iter()
            .filter(|g| matches!(g.init, Expr::Str(_)))
            .map(|g| g.name.clone())
            .collect()
    }

    fn check(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        let sg = string_globals(&funcs, &globals);
        check_module_string_indexing(&funcs, &sg)
    }

    #[test]
    fn index_a_module_string_constant_index_is_refused() {
        let err = check("let S: string = \"anb\"\nfn main() -> i64 { S[1] }").unwrap_err();
        assert!(
            err.contains("indexing") && err.contains("S") && err.contains("peek8"),
            "should name S and suggest peek8; got: {err}"
        );
    }

    #[test]
    fn index_a_module_string_variable_index_is_refused() {
        let err = check("let S: string = \"anb\"\nfn main(i: i64) -> i64 { S[i] }").unwrap_err();
        assert!(
            err.contains("indexing") && err.contains("S") && err.contains("peek8"),
            "should name S and suggest peek8; got: {err}"
        );
    }

    #[test]
    fn peek8_on_module_string_compiles_and_is_not_caught() {
        let ok = check("let S: string = \"anb\"\nfn main() -> i64 { peek8(S + 1) }");
        assert!(
            ok.is_ok(),
            "peek8 on a module string must not be caught; got {ok:?}"
        );
    }

    #[test]
    fn module_view_buffer_still_indexes() {
        let ok = check("let mut g: u8[8]\nfn main() -> i64 { g[1] = 65\n0 }");
        assert!(
            ok.is_ok(),
            "a u8[8] module buffer must not be caught by the string check; got {ok:?}"
        );
    }

    #[test]
    fn local_string_still_indexes() {
        let ok = check("fn main() -> i64 { let l: string = \"anb\"\nl[1] }");
        assert!(
            ok.is_ok(),
            "a local string must still be indexable; got {ok:?}"
        );
    }
}

/// Checks PIC16-specific constraints: the backend is an 8-bit accumulator machine where every
/// local/parameter occupies exactly one byte of RAM and only `movlw`/`movwf` are emitted.
/// A value that does not fit in one byte would be silently truncated by the codegen, so the
/// check rejects it as a compile error.
pub fn check_pic16_constraints(funcs: &[Function]) -> Result<(), String> {
    for f in funcs {
        // 1) Multi-byte types: every local/parameter is one byte, so u16/i16/u32/i32/i64/...
        //    are impossible to store. Reject them in declarations so the author knows why.
        for p in &f.params {
            let Type::Named(ref n) = p.ty else { continue };
            if pic16_too_wide(&p.ty) {
                return Err(format!(
                    "function `{}`: `{}` is annotated as `{}` but the PIC16 backend stores \
                     each parameter in a single byte; a {} is two or more bytes. Use \
                     separate `u8` variables to hold the high and low bytes.",
                    f.name, p.name, n, n
                ));
            }
        }
        // Walk every statement and expression in the function body, checking let bindings
        // that have an explicit multi-byte type annotation OR a constant value > 255.
        pic16_block(&f.body, &f.name)?;
    }
    Ok(())
}

/// Whether a type someone wrote down is wider than the one byte the PIC16 backend gives to every
/// local and every parameter. The widths come from `elem_bytes`, the table this file already keeps.
///
/// `i64` is the exception, and not an oversight: it is what Sond gives to everything the programmer
/// did not annotate, so every firmware in the tree is full of i64 values the backend already stores
/// as bytes. Refusing it would refuse code that works today. A width written on purpose -- `u16`,
/// `i32` -- is a different statement: the programmer asked for two bytes, and there is one.
///
/// One function for the two places that ask, so the exception cannot be remembered in one and
/// forgotten in the other. It was: a parameter `x: i64` compiled and a local `a: i64` did not.
fn pic16_too_wide(t: &Type) -> bool {
    let Type::Named(n) = t else { return false };
    n != "i64" && elem_bytes(t) > 1
}

fn pic16_block(b: &Block, fname: &str) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            // `let name: Ty = expr`: reject multi-byte types (u16/i16/u32/i32/u64/i64
            // on anything wider than 8 bits) and check the initializer value for PIC16
            // compatibility.  The type width comes from `elem_bytes` — the machine's
            // definition, not a string heuristic.
            Stmt::Let {
                name, ty, value, ..
            } => {
                if let Some(t) = &ty
                    && let Type::Named(ref n) = *t
                    && pic16_too_wide(t)
                {
                    return Err(format!(
                        "function `{fname}`: `{name}` is declared as `{n}` but the PIC16 \
                         backend stores each local in a single byte; a {n} is two or more bytes. \
                         Use separate `u8` variables for the high and low bytes."
                    ));
                }
                check_pic16_expr(value, fname)?;
            }
            Stmt::Assign { target, value } => {
                check_pic16_expr(value, fname)?;
                check_pic16_expr(target, fname)?;
            }
            Stmt::Return(Some(value)) | Stmt::Expr(value) | Stmt::Spawn(value) => {
                check_pic16_expr(value, fname)?;
            }
            Stmt::For {
                cond,
                invariant,
                body,
                ..
            } => {
                if let Some(c) = cond {
                    check_pic16_expr(c, fname)?;
                }
                for inv in invariant {
                    check_pic16_expr(inv, fname)?;
                }
                pic16_block(body, fname)?;
            }
            Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                for p in peers {
                    check_pic16_expr(p, fname)?;
                }
            }
            Stmt::GraphLet {
                nodes, yield_expr, ..
            } => {
                for (_, e) in nodes {
                    check_pic16_expr(e, fname)?;
                }
                check_pic16_expr(yield_expr, fname)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        check_pic16_expr(t, fname)?;
    }
    Ok(())
}

/// Descend an expression tree, checking that all integer literal values fit in 8 bits
/// (0..255 unsigned, -128..127 signed for signed types). This catches `poke8(0x20, 1000)`
/// where 1000 = 0x03E8 would be truncated to 232 = 0xE8.
///
/// Also checks that constant expressions (binary ops on literals) that evaluate to a value
/// outside 8 bits are rejected: `let a = 200 + 100` → 300, which cannot be emitted.
fn check_pic16_expr(e: &Expr, fname: &str) -> Result<(), String> {
    match e {
        Expr::Int(n) => {
            // An integer literal used in PIC16 must fit in 8 bits.
            // The PIC16 backend is an 8-bit machine: locals are one byte, and immediates
            // are loaded with `movlw` which takes an 8-bit value.
            if *n > 255 || *n < -128 {
                return Err(format!(
                    "function `{fname}`: integer literal `{n}` does not fit in 8 bits; \
                     the PIC16 backend only emits 8-bit values (range 0..255). \
                     Divide the value or express it as a constant expression that the \
                     backend can break down."
                ));
            }
        }
        Expr::Binary(_, l, r) => {
            // Also fold the expression to catch `200 + 100` = 300 from a binary op.
            if let Some(v) = const_fold_pic16(e)
                && !(-128..=255).contains(&v)
            {
                return Err(format!(
                    "function `{fname}`: constant expression evaluates to `{v}` which \
                     does not fit in 8 bits; the PIC16 backend only emits 8-bit values \
                     (range 0..255). Split the value or use separate `u8` variables."
                ));
            }
            check_pic16_expr(l, fname)?;
            check_pic16_expr(r, fname)?;
        }
        Expr::Unary(UnOp::Neg, x) => {
            check_pic16_expr(x, fname)?;
            // Negation of -128 would be 128 which overflows i8. Check the inner value.
            if let Expr::Int(n) = x.as_ref()
                && *n == -128
            {
                return Err(format!(
                    "function `{fname}`: integer literal `-(-128)` is 128 which does not \
                     fit in a signed i8 type's range (-128..127) on the PIC16 backend."
                ));
            }
        }
        Expr::Field(x, _) | Expr::Cast(x, _) => {
            check_pic16_expr(x, fname)?;
        }
        Expr::Index(a, b) => {
            check_pic16_expr(a, fname)?;
            check_pic16_expr(b, fname)?;
        }
        Expr::Call(c, args) => {
            check_pic16_expr(c, fname)?;
            for a in args {
                check_pic16_expr(&a.value, fname)?;
            }
        }
        Expr::List(items) => {
            for it in items {
                check_pic16_expr(it, fname)?;
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                check_pic16_expr(v, fname)?;
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            check_pic16_expr(cond, fname)?;
            pic16_block(then_branch, fname)?;
            if let Some(eb) = else_branch {
                pic16_block(eb, fname)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            check_pic16_expr(scrutinee, fname)?;
            for a in arms {
                pic16_block(&a.body, fname)?;
            }
        }
        Expr::SliceRange(b, lo, hi) => {
            check_pic16_expr(b, fname)?;
            if let Some(l) = lo {
                check_pic16_expr(l, fname)?;
            }
            if let Some(h) = hi {
                check_pic16_expr(h, fname)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Folds a constant integer expression to its value, or `None` if not a compile-time
/// constant.  The value is returned as-is (unmasked); callers that need an 8-bit literal
/// must apply the `& 0xFF` mask themselves.  Used by the PIC16 checker and by the PIC16
/// codegen so there is exactly one place where constant folding lives.
pub fn const_fold_pic16(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::Unary(UnOp::Neg, x) => Some(const_fold_pic16(x)?.wrapping_neg()),
        Expr::Binary(op, l, r) => {
            let (a, b) = (const_fold_pic16(l)?, const_fold_pic16(r)?);
            Some(match op {
                BinOp::Add => a.wrapping_add(b),
                BinOp::Sub => a.wrapping_sub(b),
                BinOp::Mul => a.wrapping_mul(b),
                BinOp::BitOr => a | b,
                BinOp::BitAnd => a & b,
                BinOp::BitXor => a ^ b,
                BinOp::Shl => a << b,
                BinOp::Shr => a >> b,
                _ => return None,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod text_vs_pointer_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_string_literal_types(&funcs, &globals)
    }

    // A `string` parameter used to accept any number at all, so the whole cstr-to-string
    // migration was a comment with the shape of a signature. These three are what it buys.

    #[test]
    fn text_where_a_number_is_expected_names_the_callers_own_value() {
        let err = check(
            "let S: string = \"hola\"\nfn quiere(p: i64) -> i64 { peek8(p) }\nfn main() -> i64 { quiere(S) }",
        )
        .unwrap_err();
        assert!(
            err.contains("`S.utf8`"),
            "the way out has to be spelled with the caller's own name, not the parameter's; got: {err}"
        );
    }

    #[test]
    fn a_utf8_where_text_is_expected_says_to_pass_the_text() {
        let err = check(
            "let S: string = \"hola\"\nfn quiere(s: string) -> i64 { peek8(s) }\nfn main() -> i64 { quiere(S.utf8) }",
        )
        .unwrap_err();
        assert!(
            err.contains("address of its bytes"),
            "should say the value is the address, not the text; got: {err}"
        );
    }

    #[test]
    fn a_plain_number_where_text_is_expected_is_refused() {
        let err = check(
            "fn quiere(s: string) -> i64 { peek8(s) }\nfn main() -> i64 { let p: i64 = 12345\nquiere(p) }",
        )
        .unwrap_err();
        assert!(
            err.contains("carries the characters"),
            "should explain what a text parameter holds; got: {err}"
        );
    }

    // And the four shapes of text that have to keep compiling. Without these, the check above
    // is one keystroke away from refusing everything and still passing its own tests.

    #[test]
    fn a_literal_is_text() {
        check("fn q(s: string) -> i64 { peek8(s) }\nfn main() -> i64 { q(\"hola\") }").unwrap();
    }

    #[test]
    fn a_module_string_is_text() {
        check(
            "let S: string = \"h\"\nfn q(s: string) -> i64 { peek8(s) }\nfn main() -> i64 { q(S) }",
        )
        .unwrap();
    }

    #[test]
    fn a_local_string_is_text() {
        check(
            "fn q(s: string) -> i64 { peek8(s) }\nfn main() -> i64 { let t: string = \"h\"\nq(t) }",
        )
        .unwrap();
    }

    #[test]
    fn a_string_parameter_forwarded_to_another_one_is_text() {
        check("fn o(s: string) -> i64 { peek8(s) }\nfn q(s: string) -> i64 { o(s) }\nfn main() -> i64 { q(\"h\") }")
            .unwrap();
    }
}

#[cfg(test)]
mod pic16_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn pic16_check(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        check_pic16_constraints(&funcs)
    }

    // CASE 1: u16 local — backend stores each local in one byte
    #[test]
    fn u16_local_is_rejected() {
        let err =
            pic16_check("fn main() -> i64 { let b: u16 = 1000\npoke8(0x20, b)\n0 }").unwrap_err();
        assert!(
            err.contains("`b` is declared as `u16`") && err.contains("single byte"),
            "u16 local should be rejected; got: {err}"
        );
    }

    // u16 parameter — one byte per param
    #[test]
    fn u16_parameter_is_rejected() {
        let err = pic16_check("fn main(x: u16) -> i64 { poke8(0x20, x) 0 }").unwrap_err();
        assert!(
            err.contains("`x` is annotated as `u16`") && err.contains("single byte"),
            "u16 parameter should be rejected; got: {err}"
        );
    }

    // CASE 2: literal 1000 — caught by the literal-width check
    #[test]
    fn literal_1000_is_rejected() {
        let err = pic16_check("fn main() -> i64 { poke8(0x20, 1000) 0 }").unwrap_err();
        assert!(
            err.contains("integer literal") && err.contains("1000") && err.contains("8 bits"),
            "literal 1000 should be rejected; got: {err}"
        );
    }

    // CASE 3: u8 = 300 (value out of declared type range)
    // The literal 300 is caught by the literal-width check before reaching the type annotation.
    // The test asserts the literal is named, not the type.
    #[test]
    fn const_out_of_declared_range_is_rejected() {
        let err =
            pic16_check("fn main() -> i64 { let a: u8 = 300\npoke8(0x20, a)\n0 }").unwrap_err();
        assert!(
            err.contains("300") && err.contains("8 bits"),
            "`let a: u8 = 300` should be rejected naming the value and its width; got: {err}"
        );
    }

    // `i64` is what Sond gives to everything the programmer did not annotate, so the tree is
    // full of it and the backend already treats it as a byte. A parameter written `x: i64` is
    // accepted; a local written `a: i64` is the same declaration and has to be accepted too.
    #[test]
    fn an_i64_written_out_is_accepted_in_a_local_as_it_is_in_a_parameter() {
        assert!(
            pic16_check("fn f(x: i64) { poke8(0x20, x) }").is_ok(),
            "a parameter written `i64` must compile"
        );
        assert!(
            pic16_check("fn main() -> i64 { let a: i64 = 5\npoke8(0x20, a)\n0 }").is_ok(),
            "a local written `i64` must compile too"
        );
    }

    // POSITIVE: u8 = 255 should compile
    #[test]
    fn u8_255_is_accepted() {
        assert!(
            pic16_check("fn main() -> i64 { let a: u8 = 255\npoke8(0x20, a)\n0 }").is_ok(),
            "u8 = 255 must still compile"
        );
    }

    // POSITIVE: unnamed constant 255 should compile
    #[test]
    fn unnamed_255_is_accepted() {
        assert!(
            pic16_check("fn main() -> i64 { let a = 255\npoke8(0x20, a)\n0 }").is_ok(),
            "= 255 must still compile"
        );
    }

    // POSITIVE: zero should compile
    #[test]
    fn zero_is_accepted() {
        assert!(
            pic16_check("fn main() -> i64 { let a = 0\npoke8(0x20, a)\n0 }").is_ok(),
            "= 0 must still compile"
        );
    }

    // CASE 1: u16 local with a VARIABLE value (serial_getc returns one byte, stored in u16)
    // This was the original bug case: the multi-byte check required a constant value,
    // so `let b: u16 = x` where x is a variable silently compiled.
    #[test]
    fn u16_local_with_variable_is_rejected() {
        let err =
            pic16_check("fn main() { let x = serial_getc()\nlet b: u16 = x\npoke8(0x20, b)\n}")
                .unwrap_err();
        assert!(
            err.contains("`b` is declared as `u16`") && err.contains("single byte"),
            "u16 local with variable value should be rejected; got: {err}"
        );
    }

    // CASE 2: constant from binary operation > 255
    #[test]
    fn binary_const_200_plus_100_is_rejected() {
        let err =
            pic16_check("fn main() -> i64 { let a = 200 + 100\npoke8(0x20, a)\n0 }").unwrap_err();
        assert!(
            err.contains("constant expression") && err.contains("300") && err.contains("8 bits"),
            "200+100 should be rejected; got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// check_undeclared_variables — invariant typos and ..-blind spots
// ---------------------------------------------------------------------------

#[cfg(test)]
mod invariant_undeclared {
    use super::*;
    use crate::lexer::tokenize_located;
    use crate::parser::Parser;

    fn check_undeclared(src: &str) -> Result<(), String> {
        let (toks, lines, _cols) = tokenize_located(src);
        let mut p = Parser::with_lines(toks, lines);
        let funcs = p.parse_program();
        let globals = p.take_globals();
        let enums = p.take_enums();
        check_undeclared_variables(&funcs, &globals, &enums)
    }

    /// An identifier that does not exist, used inside an `invariant` clause,
    /// must be rejected with its exact source line.
    #[test]
    fn undeclared_in_invariant_is_rejected_with_line() {
        // Line numbers:
        //   1  fn main() -> i64 {
        //   2      let mut arr[8]
        //   3      let mut idx = 0
        //   4      let mut t = 0
        //   5      for idx < 8 invariant { indx >= 0 && indx <= 8 } {
        //   6          t = t + arr[idx]
        //   7          idx = idx + 1
        //   8      }
        //   9      0
        //  10  }
        let err = check_undeclared(
            "fn main() -> i64 {\n\
             let mut arr[8]\n\
             let mut idx = 0\n\
             let mut t = 0\n\
             for idx < 8 invariant { indx >= 0 && indx <= 8 } {\n\
                 t = t + arr[idx]\n\
                 idx = idx + 1\n\
             }\n\
             0\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("indx") && err.contains("at line 5"),
            "must name the bad variable and its line, got: {err}"
        );
    }

    /// A variable declared inside the loop body must NOT be visible in the
    /// invariant: the invariant fires at the loop boundary, before the body
    /// runs, so the body-local does not exist yet.
    #[test]
    fn body_local_not_visible_in_invariant() {
        let err = check_undeclared(
            "fn main() -> i64 {\n\
             let mut arr[8]\n\
             let mut idx = 0\n\
             for idx < 8 invariant { val >= 0 } {\n\
                 let val = arr[idx]\n\
                 idx = idx + 1\n\
             }\n\
             0\n\
             }",
        )
        .unwrap_err();
        assert!(
            err.contains("val") && err.contains("at line 4"),
            "body-local `val` must not be in scope in the invariant, got: {err}"
        );
    }

    /// A function parameter IS visible in the invariant — it is the normal
    /// case for pre-loop guards.
    #[test]
    fn function_param_visible_in_invariant() {
        assert!(
            check_undeclared(
                "fn main(n: i64) -> i64 {\n\
                 let mut idx = 0\n\
                 for idx < n invariant { n >= 0 } {\n\
                     idx = idx + 1\n\
                 }\n\
                 0\n\
                 }",
            )
            .is_ok(),
            "parameter `n` must be visible in the invariant"
        );
    }

    /// A variable declared BEFORE the loop IS visible in the invariant —
    /// this is the common case for loop counters and bounds.
    #[test]
    fn var_declared_before_loop_visible_in_invariant() {
        assert!(
            check_undeclared(
                "fn main() -> i64 {\n\
                 let mut arr[8]\n\
                 let mut idx = 0\n\
                 let mut t = 0\n\
                 for idx < 8 invariant { idx >= 0 && idx <= 8 } {\n\
                     t = t + arr[idx]\n\
                     idx = idx + 1\n\
                 }\n\
                 0\n\
                 }",
            )
            .is_ok(),
            "variable `idx` declared before the loop must be visible in the invariant"
        );
    }

    /// Regression: undeclared variable in the function body is rejected.
    #[test]
    fn undeclared_in_body_is_rejected() {
        let err = check_undeclared("fn main() -> i64 { noexistenada }").unwrap_err();
        assert!(
            err.contains("noexistenada"),
            "body must reject undeclared vars, got: {err}"
        );
    }

    /// Regression: undeclared variable in `requires` is rejected.
    #[test]
    fn undeclared_in_requires_is_rejected() {
        let err =
            check_undeclared("fn f(x: i64) -> i64 requires { noexistenada > 0 } { 0 }")
                .unwrap_err();
        assert!(
            err.contains("noexistenada"),
            "requires must reject undeclared vars, got: {err}"
        );
    }

    /// Regression: undeclared variable in `ensures` is rejected.
    #[test]
    fn undeclared_in_ensures_is_rejected() {
        let err =
            check_undeclared("fn f(x: i64) -> (r: i64) ensures { noexistenada > 0 } { 0 }")
                .unwrap_err();
        assert!(
            err.contains("noexistenada"),
            "ensures must reject undeclared vars, got: {err}"
        );
    }

    /// `check_undeclared_stmt` must name every field of every `Stmt` arm, with no bare `..`.
    ///
    /// This is the arrangement that made the bug possible: `Stmt::For { cond, body, .. }`
    /// swallowed the `invariant` field, so the clause was never walked and a typo in it compiled
    /// in silence. With every field named, adding a field to the enum breaks THIS function at
    /// compile time and whoever adds it has to decide what checking it means -- instead of the
    /// new field being born unchecked.
    ///
    /// The whole body is searched, not one line at a time: a four-field pattern is written across
    /// several lines, so `..` and the `Stmt::` it belongs to usually do NOT share a line. A guard
    /// that only reads single lines passes a saboteur written the normal way -- measured.
    #[test]
    fn no_ellipsis_in_check_undeclared_stmt() {
        let source = include_str!("check.rs");
        let desde = source
            .find("fn check_undeclared_stmt(")
            .expect("the function this guard protects must exist");
        let abre = source[desde..]
            .find(") -> Result<(), String> {")
            .map(|i| desde + i + ") -> Result<(), String> {".len() - 1)
            .expect("the function signature must end in an opening brace");

        let mut profundidad = 0usize;
        let mut fin = abre;
        for (i, c) in source[abre..].char_indices() {
            match c {
                '{' => profundidad += 1,
                '}' => {
                    profundidad -= 1;
                    if profundidad == 0 {
                        fin = abre + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(fin > abre, "the function body must be delimited");

        let cuerpo: String = source[abre..fin]
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !cuerpo.contains(".."),
            "check_undeclared_stmt must name every field of every Stmt arm: a bare `..` there \
             lets a field be added to the enum and never checked, which is how the `invariant` \
             clause went unwalked"
        );
    }
}

#[cfg(test)]
mod coverage_harness_globals {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    // The `sond test --coverage` harness is generated as Sond SOURCE and parsed with the
    // ordinary parser, so its `main` refers to the coverage bookkeeping symbols
    // (`__cov_total`, `__cov_ptr`, `__cov_nfns`, `__cov_fn_table`) by name. Those are reserved
    // compiler-internal globals published by the coverage code generator; they are never
    // user-declared `let`s, so the undeclared-variable check must treat them as predeclared —
    // but ONLY in the harness, which is marked `is_harness` by `testing::parse_main`.
    // Without the mark (and the predeclaration it unlocks), every `sond test` on a coverage
    // build dies with "variable `__cov_total` is not declared in function `main`".
    #[test]
    fn a_coverage_harness_referencing_the_cov_symbols_passes() {
        // The real harness body is exercised: it reads the total point count, mmaps a shared
        // buffer, points __cov_ptr at it, and later indexes __cov_fn_table and reads
        // __cov_nfns. None of the four __cov_* names is a user `let`. The mark is what the
        // driver's `parse_main` sets on the synthesized harness; this test parses the same
        // source directly, so it sets it by hand.
        let src = "fn main() -> i64 {\n\
             \x20   let __n = __cov_total\n\
             \x20   let __buf = 7\n\
             \x20   __cov_ptr = __buf\n\
             \x20   let __base = __cov_fn_table + __n * 32\n\
             \x20   __base + __cov_nfns\n\
             }";
        let mut funcs = Parser::new(tokenize(src)).parse_program();
        for f in &mut funcs {
            f.is_harness = true;
        }
        assert_eq!(
            check_undeclared_variables(&funcs, &[], &[]),
            Ok(()),
            "a synthesized coverage harness must not trip the undeclared-variable check"
        );
    }

    #[test]
    fn an_unrelated_unknown_name_is_still_rejected() {
        let mut p = Parser::new(tokenize("fn f() -> i64 { __cov_totals }"));
        let funcs = p.parse_program();
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(
            err.contains("reserved"),
            "a name that merely resembles the reserved ones still wears the reserved prefix, \
             and the error says so rather than suggesting to declare it: {err}"
        );
    }

    #[test]
    fn the_same_harness_source_without_the_harness_mark_is_rejected() {
        // The exemption belongs to the synthesized harness, not to the source text: the
        // very same body written by an author is refused, or the mark would be decorative.
        let src = "fn main() -> i64 {\n\
             \x20   let __n = __cov_total\n\
             \x20   __n\n\
             }";
        let funcs = Parser::new(tokenize(src)).parse_program();
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(
            err.contains("reserved"),
            "unmarked code reading the symbols must be refused as reserved: {err}"
        );
    }

    // ── the `__cov_` reservation is ENFORCED, not just documented ────────────
    //
    // Predeclaring the four symbols in EVERY function of EVERY program meant a user
    // program could read one (silently reaching into compiler internals: the names
    // resolved as ordinary variables) or declare its own (colliding with the `.data`
    // symbols the coverage build emits, as an opaque link error). The reservation the
    // comment promised has to exist in code: user code may neither USE nor DECLARE a
    // `__cov_` identifier, at any nesting level or program position.

    #[test]
    fn a_user_function_reading_an_undeclared_cov_name_is_rejected() {
        let funcs = parse_fns("fn f() -> i64 { __cov_ptr }");
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(
            err.contains("reserved"),
            "the error must name the reservation, not just say undeclared: {err}"
        );
    }

    #[test]
    fn a_user_function_declaring_a_cov_name_is_rejected() {
        let funcs = parse_fns("fn f() -> i64 {\n    let __cov_ptr = 5\n    __cov_ptr\n}");
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(
            err.contains("reserved"),
            "declaring the name must be refused, a local must not shadow the symbol: {err}"
        );
    }

    #[test]
    fn a_cov_name_declared_inside_a_nested_block_is_rejected() {
        // The reservation is not a top-level formality: a `__cov_` `let` buried in an
        // `if` inside a `for` is just as refused, or the prefix is only mostly reserved.
        // (The loop counter is declared before the loop, as Sond requires.)
        let funcs = parse_fns(
            "fn f() -> i64 {\n\
             \x20   let mut n = 0\n\
             \x20   let mut i = 0\n\
             \x20   for i < 3 {\n\
             \x20       if i == 1 {\n\
             \x20           let __cov_total = 7\n\
             \x20           n = __cov_total\n\
             \x20       }\n\
             \x20       n = n + i\n\
             \x20   }\n\
             \x20   n\n\
             }",
        );
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(err.contains("reserved"), "nested bind must be refused: {err}");
    }

    #[test]
    fn a_cov_parameter_name_is_rejected() {
        let funcs = parse_fns("fn f(__cov_ptr: i64) -> i64 { __cov_ptr }");
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(err.contains("reserved"), "a parameter is a bind too: {err}");
    }

    #[test]
    fn a_cov_named_global_is_rejected() {
        let g = Global {
            vis: crate::ast::Visibility::Priv,
            mutable: false,
            name: "__cov_ptr".to_string(),
            array_len: None,
            ty: None,
            init: Expr::Int(0),
        };
        let funcs = parse_fns("fn f() -> i64 { 0 }");
        let err = check_undeclared_variables(&funcs, &[g], &[]).unwrap_err();
        assert!(
            err.contains("reserved"),
            "a global of that name would collide with the emitted `.data` symbol: {err}"
        );
    }

    #[test]
    fn a_cov_named_function_is_rejected() {
        // `fn __cov_total` would emit an assembler label equal to a coverage symbol.
        let funcs = parse_fns("fn __cov_total() -> i64 { 0 }");
        let err = check_undeclared_variables(&funcs, &[], &[]).unwrap_err();
        assert!(err.contains("reserved"), "the prefix covers fn names too: {err}");
    }

    #[test]
    fn a_cov_named_enum_variant_is_rejected() {
        // A variant constructor is an identifier in expression position; it must not
        // wear the reserved prefix either.
        let e = EnumDef {
            vis: crate::ast::Visibility::Priv,
            name: "E".to_string(),
            variants: vec![crate::ast::EnumVariant {
                name: "__cov_ptr".to_string(),
                payload: None,
            }],
        };
        let funcs = parse_fns("fn f() -> i64 { 0 }");
        let err = check_undeclared_variables(&funcs, &[], &[e]).unwrap_err();
        assert!(err.contains("reserved"), "variant names are identifiers: {err}");
    }

    #[test]
    fn the_real_synth_coverage_harness_passes_the_check() {
        // End to end over the seam the driver uses: `sond test --coverage` synthesizes
        // the harness and runs it through this very check. It must keep passing. The
        // `SYS_*` names are globals the included `syscall-nums.snd` declares (spliced in
        // before the harness is built), so this fixture carries them; every other name
        // the harness touches (`syscall`, `peek64`, `print`, `__test_report`) is a call
        // target, which this check does not police.
        let tests = parse_fns("test \"a\" { let x = 1 }\ntest \"b\" { let y = 2 }");
        let main = crate::testing::synth_coverage_harness(&tests, 80);
        let sys_globals: Vec<Global> = ["SYS_mmap", "SYS_fork", "SYS_exit", "SYS_wait4", "SYS_write"]
            .iter()
            .map(|n| Global {
                vis: crate::ast::Visibility::Priv,
                mutable: false,
                name: n.to_string(),
                array_len: None,
                ty: None,
                init: Expr::Int(0),
            })
            .collect();
        assert_eq!(
            check_undeclared_variables(&[main], &sys_globals, &[]),
            Ok(()),
            "the harness the driver actually ships must pass the check it is subject to"
        );
    }

    /// Parses a whole program (functions, `test` blocks) the way the driver does.
    fn parse_fns(src: &str) -> Vec<Function> {
        Parser::new(tokenize(src)).parse_program()
    }

mod raw_constant_offset_bounds_tests {
    use super::*;
    use crate::lexer::tokenize_located;
    use crate::parser::Parser;

    fn raw_bounds(src: &str) -> Result<(), String> {
        let (toks, lines, _cols) = tokenize_located(src);
        let mut p = Parser::with_lines(toks, lines);
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_constant_bounds(&funcs, &globals)
    }

    #[test]
    fn peek8_past_end_of_known_array_is_a_compile_error() {
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 let x = peek8(a + 100)\n                 0\n                 }",
        )
        .unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "peek8 past end must error; got: {err}");
    }

    #[test]
    fn poke8_past_end_of_known_array_is_a_compile_error() {
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke8(a + 100, 66)\n                 0\n                 }",
        )
        .unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "poke8 past end must error; got: {err}");
    }

    #[test]
    fn peek64_past_end_of_known_array_is_a_compile_error() {
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 let x = peek64(a + 4096)\n                 0\n                 }",
        )
        .unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "peek64 past end must error; got: {err}");
    }

    #[test]
    fn poke64_past_end_of_known_array_is_a_compile_error() {
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke64(a + 4096, 42)\n                 0\n                 }",
        )
        .unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "poke64 past end must error; got: {err}");
    }

    #[test]
    fn poke8_first_and_last_byte_compile() {
        assert!(raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke8(a + 0, 65)\n                 poke8(a + 15, 77)\n                 0\n                 }",
        ).is_ok(), "poke8(a+0) and poke8(a+15) must compile");
    }

    #[test]
    fn poke64_within_array_bounds_compiles() {
        assert!(raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke64(a + 8, 1)\n                 0\n                 }",
        ).is_ok(), "poke64(a+8) must compile");
    }

    #[test]
    fn poke8_variable_offset_compiles() {
        assert!(raw_bounds(
            "fn f(i: i64) -> i64 {\n                 let a[2]\n                 poke8(a + i, 1)\n                 0\n                 }",
        ).is_ok(), "poke8 with variable offset must compile");
    }

    #[test]
    fn poke8_pointer_param_compiles() {
        assert!(raw_bounds(
            "fn f(p: i64) -> i64 {\n                 peek8(p + 100)\n                 0\n                 }",
        ).is_ok(), "pointer param must compile");
    }

    #[test]
    fn peek64_last_word_compiles_for_a2() {
        assert!(raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 let x = peek64(a + 8)\n                 0\n                 }",
        ).is_ok(), "peek64(a+8) must compile");
    }

    #[test]
    fn poke8_at_exact_bound_compiles_plus_one_fails() {
        assert!(raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke8(a + 15, 99)\n                 0\n                 }",
        ).is_ok(), "poke8(a+15) must compile");
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[2]\n                 poke8(a + 16, 99)\n                 0\n                 }",
        ).unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "poke8(a+16) must error; got: {err}");
    }

    #[test]
    fn peek64_at_exact_end_compiles_plus_eight_fails() {
        assert!(raw_bounds(
            "fn f() -> i64 {\n                 let a[3]\n                 let x = peek64(a + 16)\n                 0\n                 }",
        ).is_ok(), "peek64(a+16) must compile");
        let err = raw_bounds(
            "fn f() -> i64 {\n                 let a[3]\n                 peek64(a + 24)\n                 0\n                 }",
        ).unwrap_err();
        assert!(err.contains("out of bounds") || err.contains("a") || err.contains("bounds"),
            "peek64(a+24) must error; got: {err}");
    }
}
}

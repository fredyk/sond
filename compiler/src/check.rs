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
            let owner = enums
                .iter()
                .find(|en| seen.iter().all(|v| en.variants.iter().any(|ev| ev.name == **v)));
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
        Stmt::Let { value, .. } | Stmt::Expr(value) | Stmt::Spawn(value) | Stmt::Return(Some(value)) => {
            nested_blocks(value, out)
        }
        Stmt::Assign { target, value } => {
            nested_blocks(target, out);
            nested_blocks(value, out);
        }
        Stmt::For { cond, invariant, .. } => {
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
            if let Stmt::Expr(Expr::If { cond, then_branch, else_branch }) = s {
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
            && let Expr::If { cond, then_branch, else_branch } = t.as_ref()
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
            if let Stmt::For { cond: Some(cond), body, .. } = s {
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

/// Rejects a `spawn` inside a loop (RFC-0010 Layer B). Each spawn site owns one static join
/// cell that holds the child TID until the structured join at function exit clears it. Spawning
/// in a loop would overwrite that cell on the next iteration, so the earlier thread would never
/// be joined: a silent thread leak. Per-iteration join state (a growable set of live threads)
/// is a later increment; until then this is a compile error, not a footgun.
pub fn check_spawn_not_in_loop(funcs: &[Function]) -> Result<(), String> {
    fn walk(b: &Block, in_loop: bool, fname: &str) -> Result<(), String> {
        for s in &b.stmts {
            match s {
                Stmt::Spawn(_) if in_loop => {
                    return Err(format!(
                        "function `{fname}`: `spawn` inside a loop is not supported yet. Each \
                         spawn is joined at the function's return through a single per-site cell, \
                         so a loop would leave every thread but the last one unjoined. Spawn a \
                         fixed set of threads outside the loop for now."
                    ));
                }
                Stmt::For { body, .. } => walk(body, true, fname)?,
                _ => {}
            }
            // Conditionals and match arms carry the loop nesting into their own blocks; without
            // this the guard is bypassed by the one line every accept loop is written with.
            let mut inner = Vec::new();
            nested_blocks_of_stmt(s, &mut inner);
            for blk in inner {
                walk(blk, in_loop, fname)?;
            }
        }
        // And the block's TAIL, which is where a conditional written last actually lives.
        if let Some(t) = &b.tail {
            let mut inner = Vec::new();
            nested_blocks(t, &mut inner);
            for blk in inner {
                walk(blk, in_loop, fname)?;
            }
        }
        Ok(())
    }
    for f in funcs {
        walk(&f.body, false, &f.name)?;
    }
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
            Expr::Int(_)
            | Expr::Decimal(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::Var(_) => {}
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
                Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => {
                    walk_expr(e, fname, eff)?
                }
                Stmt::For { cond, body, .. } => {
                    if let Some(c) = cond {
                        walk_expr(c, fname, eff)?;
                    }
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
            Expr::If { cond, then_branch, else_branch } => {
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
        let Expr::Var(name) = callee else { return false };
        if name != "syscall" {
            return false;
        }
        let Some(first) = args.first() else { return false };
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
                Stmt::Assign { target: Expr::Var(name), .. } => {
                    moved.retain(|m| *m != name.as_str())
                }
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
    Expr::Binary(op, Box::new(Expr::Var(v.to_string())), Box::new(rhs.clone()))
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
                && arms.iter().all(|a| block_assigns_preserve_nonneg(&a.body, g))
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            expr_assigns_preserve_nonneg(l, g) && expr_assigns_preserve_nonneg(r, g)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            expr_assigns_preserve_nonneg(x, g)
        }
        Expr::Call(c, args) => {
            expr_assigns_preserve_nonneg(c, g)
                && args.iter().all(|a| expr_assigns_preserve_nonneg(&a.value, g))
        }
        Expr::List(items) => items.iter().all(|i| expr_assigns_preserve_nonneg(i, g)),
        Expr::StructLit(_, fields) => {
            fields.iter().all(|(_, v)| expr_assigns_preserve_nonneg(v, g))
        }
        Expr::SliceRange(b, lo, hi) => {
            expr_assigns_preserve_nonneg(b, g)
                && lo.as_ref().is_none_or(|x| expr_assigns_preserve_nonneg(x, g))
                && hi.as_ref().is_none_or(|x| expr_assigns_preserve_nonneg(x, g))
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
    cands.retain(|g| funcs.iter().all(|f| block_assigns_preserve_nonneg(&f.body, g)));
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
fn tighten_guard(cur: Option<i64>, cond: &Expr, g: &str, consts: &HashMap<String, i64>) -> Option<i64> {
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
            walk_ceiling_expr(l, g, guard, consts, ceil) && walk_ceiling_expr(r, g, guard, consts, ceil)
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
                if !walk_ceiling_expr(value, g, guard, consts, ceil) => {
                    return false;
                }
            _ => {}
        }
    }
    if let Some(t) = &b.tail
        && !walk_ceiling_expr(t, g, guard, consts, ceil) {
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
        bounds_block(&f.body, &f.name, &mut sizes, &mut flow, &f.params, funcs)?;
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
/// NOT WIRED IN YET, and the reason is worth writing down. Turning it on forces every function
/// that takes text to declare `string`, and the standard library then hits the OTHER rule: raw
/// `peek`/`poke` into a buffer the compiler knows the length of is refused, so those bodies must
/// index instead — and a `string` today carries no length to index against. Measured: 47 files
/// need the signature change, and 8 of them then need their bodies rewritten, which cannot be
/// done until `string` carries its own length (a pointer AND a count, the way every language
/// that got this right does it). The rule below is implemented and tested so that work has a
/// finished target to land on; it starts rejecting the day `string` can answer `s.len` for a
/// value it did not see created.
#[allow(dead_code)]
pub fn check_string_literal_types(funcs: &[Function]) -> Result<(), String> {
    let sigs: HashMap<&str, &Function> = funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    for f in funcs {
        literals_block(&f.body, &f.name, &sigs)?;
    }
    Ok(())
}

fn is_string_ty(ty: &Type) -> bool {
    match ty {
        Type::Named(n) => n == "string",
        // `let s: string = ".."` is lowered to a byte view by the parser, so the lowered form
        // counts as the same type.
        Type::View(inner, _) => matches!(inner.as_ref(), Type::Named(n) if n == "u8"),
        _ => false,
    }
}

fn literals_block(
    b: &Block,
    fname: &str,
    sigs: &HashMap<&str, &Function>,
) -> Result<(), String> {
    for st in &b.stmts {
        match st {
            Stmt::Let { name, ty, value, .. } => {
                if matches!(value, Expr::Str(_))
                    && !ty.as_ref().is_some_and(is_string_ty)
                {
                    return Err(format!(
                        "function `{fname}`: `{name}` is given a string literal but is not \
                         declared `string`; text has a type — write `let {name}: string = ..`"
                    ));
                }
                literals_expr(value, fname, sigs)?;
            }
            Stmt::Assign { target, value } => {
                literals_expr(target, fname, sigs)?;
                literals_expr(value, fname, sigs)?;
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::Spawn(e) => {
                literals_expr(e, fname, sigs)?
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    literals_expr(c, fname, sigs)?;
                }
                literals_block(body, fname, sigs)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        literals_expr(t, fname, sigs)?;
    }
    Ok(())
}

fn literals_expr(e: &Expr, fname: &str, sigs: &HashMap<&str, &Function>) -> Result<(), String> {
    if let Expr::Call(callee, args) = e
        && let Expr::Var(name) = callee.as_ref()
    {
        if LITERAL_BUILTINS.contains(&name.as_str()) {
            return Ok(());
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
        }
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            literals_expr(l, fname, sigs)?;
            literals_expr(r, fname, sigs)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => literals_expr(x, fname, sigs),
        Expr::Call(c, args) => {
            literals_expr(c, fname, sigs)?;
            for a in args {
                literals_expr(&a.value, fname, sigs)?;
            }
            Ok(())
        }
        Expr::If { cond, then_branch, else_branch } => {
            literals_expr(cond, fname, sigs)?;
            literals_block(then_branch, fname, sigs)?;
            if let Some(eb) = else_branch {
                literals_block(eb, fname, sigs)?;
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
        Expr::Index(a, b) => Expr::Index(Box::new(subst_vars(a, map)), Box::new(subst_vars(b, map))),
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
) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let {
                name,
                value,
                array_len,
                ty,
                ..
            } => {
                bounds_expr(value, fname, sizes, flow, params, funcs)?;
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
                    Some(Type::View(elem, _)) => view_bytes_available(value, sizes)
                        .map(|bytes| bytes / elem_bytes(elem)),
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
            }
            Stmt::Assign {
                target: Expr::Var(v),
                value,
            } => {
                bounds_expr(value, fname, sizes, flow, params, funcs)?;
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
                bounds_expr(target, fname, sizes, flow, params, funcs)?;
                bounds_expr(value, fname, sizes, flow, params, funcs)?;
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
                bounds_expr(e, fname, sizes, flow, params, funcs)?;
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
            Stmt::Return(Some(v)) | Stmt::Expr(v) => bounds_expr(v, fname, sizes, flow, params, funcs)?,
            Stmt::For {
                cond,
                invariant,
                body,
            } => {
                // The loop's SOUND facts are derived below (counter floors/ceilings, the
                // condition). A user `invariant` clause is trusted ONLY if the prover VERIFIES it
                // (init + preservation), trusting a verified invariant is sound, unlike an
                // unverified one; an unverified/unverifiable clause is simply ignored.
                if let Some(c) = cond {
                    bounds_expr(c, fname, sizes, flow, params, funcs)?;
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
                bounds_block(body, fname, &mut inner, &mut body_flow, params, funcs)?;
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
        bounds_expr(t, fname, sizes, flow, params, funcs)?;
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
) -> Result<(), String> {
    if let Expr::Index(base, idx) = e
        && let Expr::Var(a) = base.as_ref()
    {
        if let Some(&n) = sizes.get(a) {
            match const_index(idx) {
                // A compile-time-constant index outside [0, N) is a static stack overrun.
                Some(k) if k < 0 || k >= n => {
                    return Err(format!(
                        "function `{fname}`: index {k} is out of bounds for `{a}` of size {n} (valid range 0..{n})"
                    ));
                }
                // A constant in [0, N) is statically safe.
                Some(_) => {}
                // Checked-by-default: a VARIABLE index must be PROVEN in `[0, N)` (from a
                // `requires`, a `where`, branch guards, or a loop condition + counter floor).
                None => {
                    // Either a derived interval confines the index (`bit >> 3`, `i / 8`, `x & 7`),
                    // or the linear prover discharges it (`i`, `i - 16` from a counter + condition).
                    let in_range =
                        matches!(expr_range(idx, flow), Some((lo, hi)) if lo >= 0 && hi < n);
                    if !in_range
                        && !crate::verify::index_proven_in_bounds(
                            idx,
                            &Expr::Int(n),
                            &flow.assumptions(),
                            params,
                        )
                    {
                        return Err(format!(
                            "function `{fname}`: cannot prove this index is in bounds for `{a}` of size {n}; bound it with a `where`/`requires`/loop condition to 0..{n}"
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
                    "function `{fname}`: cannot prove this index is in bounds for slice `{a}`; bound it with a `where`/`requires`/loop condition to 0..{a}.len"
                ));
            }
        } else if let Some(len) = view_len_of(a, params).or_else(|| flow.views.get(a).cloned()) {
            // A view is one word: a base address whose length lives elsewhere. `u32[n]` names the
            // parameter holding it, and the index is proven against THAT name, so bounding it by
            // another buffer's length proves nothing and is rejected — which is the mistake worth
            // catching, since both are just an `i64` at the call site.
            let Some(len) = len else {
                return Err(format!(
                    "function `{fname}`: view `{a}` has no declared length, so an index into it \
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
                    "function `{fname}`: cannot prove this index is in bounds for view `{a}`; bound it with a `where`/`requires`/loop condition to 0..{len}"
                ));
            }
        }
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            bounds_expr(l, fname, sizes, flow, params, funcs)?;
            bounds_expr(r, fname, sizes, flow, params, funcs)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            bounds_expr(x, fname, sizes, flow, params, funcs)
        }
        Expr::Call(c, args) => {
            bounds_expr(c, fname, sizes, flow, params, funcs)?;
            for a in args {
                bounds_expr(&a.value, fname, sizes, flow, params, funcs)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                bounds_expr(it, fname, sizes, flow, params, funcs)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                bounds_expr(v, fname, sizes, flow, params, funcs)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            bounds_expr(cond, fname, sizes, flow, params, funcs)?;
            // Each branch reasons under its own guard on a clone; after the `if`, any variable a
            // branch reassigned is unknown.
            let mut then_flow = flow.clone();
            then_flow.facts.push(cond.as_ref().clone());
            let mut t = sizes.clone();
            bounds_block(then_branch, fname, &mut t, &mut then_flow, params, funcs)?;
            if let Some(eb) = else_branch {
                let mut else_flow = flow.clone();
                if let Some(neg) = negate_cond(cond) {
                    else_flow.facts.push(neg);
                }
                let mut e2 = sizes.clone();
                bounds_block(eb, fname, &mut e2, &mut else_flow, params, funcs)?;
            }
            let mut modified = HashSet::new();
            assigned_vars(then_branch, &mut modified);
            if let Some(eb) = else_branch {
                assigned_vars(eb, &mut modified);
            }
            for v in &modified {
                flow.invalidate(v);
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            bounds_expr(scrutinee, fname, sizes, flow, params, funcs)?;
            for a in arms {
                let mut m = sizes.clone();
                let mut arm_flow = flow.clone();
                bounds_block(&a.body, fname, &mut m, &mut arm_flow, params, funcs)?;
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
    use crate::lexer::tokenize;
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
        check_string_literal_types(&funcs)
    }

    fn bounds(src: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let globals = p.take_globals();
        check_constant_bounds(&funcs, &globals)
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
        assert!(bounds(src).is_ok(), "callee ensures should bound the return");
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
        assert!(bounds(src).is_ok(), "nonneg global counter should give the lower bound");
    }

    #[test]
    fn an_if_expression_clamp_bounds_an_index() {
        // `let i = if x >= 8 { 7 } else { x }` with `x >= 0` clamps `i` to [0, 7]: the then-branch
        // is the constant 7, the else-branch is `x` under `x < 8` (and `x >= 0` from requires). The
        // index range is the UNION of the two branch tails, each computed under its own guard.
        let src = "let mut t[8]\n\
                   fn at(x: i64) requires { x >= 0 } { let i = if x >= 8 { 7 } else { x }\nt[i] = 1 }\n\
                   fn main() -> i64 { at(3)\n0 }";
        assert!(bounds(src).is_ok(), "if-expression clamp should bound the index");
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
        assert!(bounds(src).is_ok(), "modulo of a nonneg global is a valid ring index");
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
        assert!(bounds(src).is_err(), "a decremented global is not provably nonneg");
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
        assert!(bounds(src).is_ok(), "a guard-bounded global counter should bound the loop");
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
        assert!(bounds(src).is_ok(), "the range itself should bound the index");
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
        assert!(bounds(src).is_err(), "walking past the declared length must not compile");
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
        assert!(bounds(src).is_err(), "an unguarded increment leaves the counter unbounded");
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
        assert!(bounds(src).is_err(), "a ceiling above the array size is not in bounds");
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
        assert!(bounds(src).is_ok(), "early-return guard should establish fi >= 0");
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
        assert!(bounds(src).is_err(), "must not assume an unmapped-param ensures");
    }

    #[test]
    fn a_loop_bounded_by_a_global_constant_proves() {
        // A module-level `let N = 64` used as a loop bound over an array of the same size (the
        // idiom `for i < TABLE_SIZE { table[i] }`) must prove: the global constant is resolved to
        // its value, so `i < 64` bounds `table[64]`.
        let src = "let N = 8\nfn f() -> i64 { let a[8]\nlet mut i = 0\nfor i < N { a[i] = 0\ni = i + 1 }\n0 }";
        assert!(bounds(src).is_ok(), "a loop bounded by a global const should prove");
    }

    #[test]
    fn rejects_an_unproven_variable_index() {
        // Checked-by-default: an index that is not PROVEN in range is a compile error.
        let err = bounds("fn f(i: i64) -> i64 { let a[4]\na[i] }").unwrap_err();
        assert!(err.contains("cannot prove this index is in bounds"), "got: {err}");
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
        let err = bounds("fn f(i: i64) -> i64 { let a[4]\nif i >= 4 { a[i] } else { 0 } }")
            .unwrap_err();
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
        assert!(bounds(
            "fn f() -> i64 { let a[4]\nlet mut i = 0\nfor i < 4 { a[i] = 0\ni = i + 1 }\n0 }"
        )
        .is_ok());
    }

    #[test]
    fn accepts_a_counting_loop_from_a_nonzero_constant_start() {
        // `i` starts at 2 and only steps up, so `i >= 2`; the condition gives `i < 6`. Both bound
        // `a[i]` in a size-6 array.
        assert!(bounds(
            "fn f() -> i64 { let a[6]\nlet mut i = 2\nfor i < 6 { a[i] = 0\ni = i + 1 }\n0 }"
        )
        .is_ok());
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
        assert!(bounds(
            "fn f(i: i64) -> i64 { let a[8]\na[i & 7] = 0\na[0] }"
        )
        .is_ok());
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
        let err =
            bounds("fn f(s: Slice, i: i64) -> i64 { s[i] }").unwrap_err();
        assert!(
            err.contains("cannot prove") && err.contains("slice"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_a_slice_index_proven_within_the_length() {
        assert!(
            bounds("fn f(s: Slice, i: i64 where 0 <= i && i < s.len) -> i64 { s[i] }").is_ok()
        );
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
        assert!(err.contains("size 8"), "the bound is the byte capacity, got: {err}");
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
        let err = literals(
            "fn takes(p: i64) -> i64 { p }\nfn main() -> i64 { takes(\"hola\") }",
        )
        .unwrap_err();
        assert!(err.contains("string"), "a literal argument needs a `string` parameter: {err}");
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
    fn rejects_a_constant_index_from_arithmetic_out_of_bounds() {
        assert!(bounds("fn f() -> i64 { let a[4]\na[2 + 3] }").is_err());
    }

    #[test]
    fn rejects_constant_index_past_a_module_level_array() {
        // A `.data` table (crypto S-box etc.) indexed by a constant out of range is caught too.
        let err = bounds("let K[4] = [1, 2, 3, 4]\nfn f() -> i64 { K[9] }").unwrap_err();
        assert!(err.contains("out of bounds") && err.contains("K"), "got: {err}");
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
        let ordinary = Parser::new(tokenize(&format!("fn f() -> i64 {{ {over} }}"))).parse_program();
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
        let funcs = Parser::new(tokenize(
            "wrapping fn f() -> i64 { let a[4]\na[9] }",
        ))
        .parse_program();
        let err = check_constant_bounds(&funcs, &[]).unwrap_err();
        assert!(err.contains("out of bounds"), "wrapping must not exempt bounds: {err}");
    }

    #[test]
    fn pub_wrapping_fn_parses() {
        // Visibility composes with the modifier.
        let funcs = Parser::new(tokenize("pub wrapping fn f() -> i64 { 1 }")).parse_program();
        assert!(funcs[0].is_wrapping && matches!(funcs[0].vis, crate::ast::Visibility::Pub));
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
        assert!(e.is_err(), "a retry loop with no error branch must be rejected");
        let msg = e.unwrap_err();
        assert!(msg.contains("got") && msg.contains("r"), "name the counter and the result: {msg}");
        assert!(msg.contains("EINTR"), "the error should say what the fix is: {msg}");
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
        assert!(ok.is_ok(), "a loop that reports the failure is correct, got {ok:?}");
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
mod spawn_loop_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_spawn_not_in_loop(&funcs)
    }

    #[test]
    fn rejects_spawn_inside_a_loop() {
        // Each spawn site has one static join cell, so spawning in a loop would overwrite the
        // previous thread's tid and leave it unjoined. Until per-iteration join state exists,
        // a spawn in a loop is a compile error rather than a silent thread leak.
        let e = check("fn w() -> i64 { 0 }\nfn f() -> i64 { for i in 0..3 { spawn w() } 0 }");
        assert!(e.is_err(), "spawn inside a loop must be rejected");
        assert!(e.unwrap_err().contains("loop"), "the error should mention the loop");
    }

    #[test]
    fn rejects_spawn_inside_an_if_inside_a_loop() {
        // The shape every accept loop has: `for { let c = accept(); if c >= 0 { spawn h(c) } }`.
        // An `if` is an EXPRESSION in Sond, so a walk that only descends through `for` bodies
        // walks straight past it and the loop guard never fires -- which is worse than having no
        // guard, because the author reads the clean compile as permission.
        let e = check("fn w() -> i64 { 0 }\nfn f() -> i64 { for i in 0..3 { if i > 0 { spawn w() } } 0 }");
        assert!(e.is_err(), "spawn inside an if inside a loop must be rejected");
        assert!(e.unwrap_err().contains("loop"), "the error should mention the loop");
    }

    #[test]
    fn rejects_spawn_inside_an_else_inside_a_loop() {
        let e = check(
            "fn w() -> i64 { 0 }\n\
             fn f() -> i64 { for i in 0..3 { if i > 0 { i } else { spawn w() 0 } } 0 }",
        );
        assert!(e.is_err(), "spawn inside an else inside a loop must be rejected");
    }

    #[test]
    fn rejects_a_spawn_in_the_tail_conditional_of_a_loop() {
        // The `if` written LAST in a block is its tail expression, not a statement, so a walk over
        // `stmts` alone skips it. That is the ordinary way to write an accept loop: the spawn is
        // the last thing in the body, so the guard was still being bypassed after the first fix.
        let e = check("fn w() -> i64 { 0 }\nfn f() -> i64 { for i in 0..3 { if i > 0 { spawn w() } } 0 }");
        assert!(e.is_err(), "a spawn in a loop body's tail conditional must be rejected");
    }

    #[test]
    fn accepts_spawn_outside_a_loop() {
        let ok = check("fn w() -> i64 { 0 }\nfn f() -> i64 { spawn w() spawn w() 0 }");
        assert!(ok.is_ok(), "two straight-line spawns are fine, got {ok:?}");
    }

    #[test]
    fn accepts_spawn_inside_an_if_outside_any_loop() {
        let ok = check("fn w() -> i64 { 0 }\nfn f(c: i64) -> i64 { if c > 0 { spawn w() } 0 }");
        assert!(ok.is_ok(), "a conditional spawn that runs once is fine, got {ok:?}");
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
        assert!(msg.contains("2 name") && msg.contains("3 peer"), "names the counts: {msg}");
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
        assert!(e.is_err(), "a write-write race on a global must be rejected");
        assert!(e.unwrap_err().contains("G"), "the error names the raced global");
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
        assert!(e.is_err(), "a race reached transitively must still be rejected");
    }

    #[test]
    fn accepts_pure_peers() {
        let ok = confluence(
            "let mut G = 0\n\
             fn a() -> i64 { 1 }\nfn b() -> i64 { 2 }\n\
             fn run() { par a() ~~ b() }",
        );
        assert!(ok.is_ok(), "peers that touch no global are confluent, got {ok:?}");
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
        assert!(e.unwrap_err().contains("cycle"), "the error explains the cycle");
    }

    #[test]
    fn accepts_an_acyclic_graph() {
        let ok = acyclic(
            "fn id(x: i64) -> i64 { x }\nfn add(a: i64, b: i64) -> i64 { a + b }\n\
             fn run() -> i64 { let r = graph { node c = add(a, b)\n node a = id(1)\n node b = id(2)\n yield c }\n r }",
        );
        assert!(ok.is_ok(), "a DAG is fine even out of source order, got {ok:?}");
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
        assert!(e.unwrap_err().contains("helper"), "the error names the function");
    }

    #[test]
    fn allows_distinct_function_names() {
        let funcs = Parser::new(tokenize(
            "fn a() -> i64 { 1 }\nfn b() -> i64 { 2 }\nfn main() -> i64 { a() + b() }",
        ))
        .parse_program();
        assert!(check_no_duplicate_functions(&funcs).is_ok(), "distinct names are fine");
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
        assert!(e.is_err(), "a race inside an `if` branch must still be rejected");
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
        assert!(e.is_err(), "close_fd after moving fd into a spawn must be rejected");
        assert!(e.unwrap_err().contains("close_fd"), "the error should name the close");
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
        assert!(ok.is_ok(), "closing a descriptor the thread does not hold is fine, got {ok:?}");
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
        assert!(msg.contains("worker"), "error should name the offending function: {msg}");
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
        assert!(e.is_err(), "exit reached through a call chain must be rejected");
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
        assert!(e.is_err(), "a spawn inside an if must still carry the effect check");
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
        assert!(e.is_err(), "a par-let peer runs on a thread, so exit must be rejected there too");
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
        assert!(e.is_err(), "a spawn in a tail conditional must still carry the effect check");
    }

    #[test]
    fn rejects_exit_via_integer_literal_syscall_number() {
        // The syscall number may be written as the raw literal 60 instead of the SYS_exit alias.
        let e = check(
            "fn worker() -> i64 { syscall(60, 1) 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(e.is_err(), "exit written as syscall(60, ..) must be rejected too");
    }

    #[test]
    fn accepts_exit_outside_any_spawned_body() {
        // The same call is perfectly fine in code that is never spawned: a normal program
        // exits its own process on purpose. The effect is only forbidden under `spawn`.
        let ok = check(
            "let SYS_exit = 60\n\
             fn main() -> i64 { syscall(SYS_exit, 0) 0 }",
        );
        assert!(ok.is_ok(), "exit in non-spawned code is legitimate, got {ok:?}");
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
        assert!(e.is_err(), "exit inside a nested spawned body must be rejected");
        assert!(e.unwrap_err().contains("inner"), "the error should name the nested body");
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
        assert!(ok.is_ok(), "exit off the spawn path is legitimate, got {ok:?}");
    }

    #[test]
    fn accepts_a_spawned_body_that_does_not_exit() {
        let ok = check(
            "let SYS_exit = 60\n\
             fn worker() -> i64 { 0 }\n\
             fn f() -> i64 { spawn worker() 0 }",
        );
        assert!(ok.is_ok(), "a clean spawned body must be accepted, got {ok:?}");
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
/// Scoped to VIEWS for now, not to classic `let a[N]` arrays. Measured, not assumed: extending it
/// to arrays refuses 35 sites across the standard library's own tests (`pad`, `one`, `buf`, `tmp`
/// and friends), so landing it would break the library and every program built on it until each
/// one is migrated. Views are new, nothing reaches into them raw yet, and the code being written
/// against them right now is exactly the code this protects. The array half is a separate,
/// announced migration, not a side effect of this.
pub fn check_no_raw_access_to_declared_buffers(funcs: &[Function]) -> Result<(), String> {
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
            if let Stmt::Let { name, array_len, ty, value, .. } = s {
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
        walk_block(&f.body, &declared, &f.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod raw_access_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_no_raw_access_to_declared_buffers(&funcs)
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
            check_no_raw_access_to_declared_buffers(&funcs).is_ok(),
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
        assert!(ok.is_ok(), "the array half is a separate, announced migration, got {ok:?}");
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
        assert!(ok.is_ok(), "a raw pointer has no length to check against, got {ok:?}");
    }

    #[test]
    fn a_name_rebound_to_a_scalar_loses_the_protection_with_the_length() {
        // Rebinding drops the length, so it must also drop the prohibition: refusing here would
        // reject correct code, and a check that rejects correct code gets switched off.
        let ok = check("fn f(p: i64) -> i64 { let a[4]\nlet a = p\npeek8(a) }");
        assert!(ok.is_ok(), "after rebinding, `a` is a plain pointer again, got {ok:?}");
    }

    #[test]
    fn the_prohibition_starts_where_the_declaration_does() {
        // A name that is not a declared buffer YET must not be caught by a later declaration.
        let ok = check("fn f(a: i64) -> i64 { let x = peek8(a)\nlet a[4]\nx }");
        assert!(ok.is_ok(), "the parameter `a` is a raw pointer at that point, got {ok:?}");
    }
}

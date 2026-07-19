//! Static struct-mutability checks, the value/pointer rules.
//!
//! A struct passed BY VALUE (`p: Point`) is immutable in a function that RECEIVED
//! it: only the function that CREATED it (where it is a local) may set its fields.
//! To share a struct for mutation you declare it a POINTER (`p: *Point`) up front -
//! Sond has no `&` address-of. So assigning to `p.field` is a COMPILE ERROR when
//! `p` is a value-struct parameter. This kills the Go footgun (silently mutating a
//! value you thought was shared, then debugging for hours) at compile time.
//!
//! Runs BEFORE struct desugaring, so it sees the original `p.field` form.

use crate::ast::{BinOp, Block, EnumDef, Expr, Function, Global, Param, Stmt, StructDef, Type, UnOp};

/// Exhaustiveness: every `match` on a sum type must cover ALL of its enum's variants,
/// use only variants that exist, and not repeat one. A missing case is a COMPILE ERROR
///, the guarantee that makes sum types "illegal states unrepresentable" (the thing an
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
                    "function `{fname}`: non-exhaustive match on `{}`, missing {missing:?}. \
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
        Stmt::Expr(e) | Stmt::Return(Some(e)) => check_expr(e, immutable, fname),
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
                            "function `{fname}`: constant integer overflow in `{a} {sym} {b}`, \
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
/// `let a[N]` when K is a COMPILE-TIME CONSTANT outside `[0, N)`, a provable stack overrun
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
/// maps, a floor for `>=`/`>`, a ceiling for `<`/`<=`, so the interval analysis (`expr_range`)
/// can use it to bound a derived index like `v >> 3`. Other shapes contribute nothing.
fn seed_interval_bound(flow: &mut Flow, e: &Expr) {
    if let Expr::Binary(op, l, r) = e
        && let Expr::Var(v) = l.as_ref()
        && let Some(k) = const_index(r)
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

/// If `value` is `v + K` or `K + v` with `K` a positive constant, returns `K`, the shape of a
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

/// If `value` is `v - K` with `K` a positive constant, returns `K`, a monotonic DECREASING step.
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
            Some((
                flow.counters.get(v).copied()?,
                flow.ceilings.get(v).copied()?,
            ))
        }
        Expr::Binary(op, l, r) => match op {
            BinOp::BitAnd => {
                // `e & (2^k - 1)` ∈ [0, mask] for any `e`, the mask keeps only the low bits.
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

pub fn check_constant_bounds(funcs: &[Function], globals: &[Global]) -> Result<(), String> {
    // Module-level arrays (`.data` tables, crypto S-boxes, round constants) are in scope in
    // every function, so seed their sizes once.
    let mut global_sizes: HashMap<String, i64> = HashMap::new();
    for g in globals {
        if let Some(n) = g.array_len {
            global_sizes.insert(g.name.clone(), n);
        }
    }
    for f in funcs {
        let mut sizes = global_sizes.clone();
        // The function's preconditions are the facts true on every path from entry.
        let mut flow = Flow {
            facts: f.requires.clone(),
            ..Default::default()
        };
        bounds_block(&f.body, &f.name, &mut sizes, &mut flow, &f.params, funcs)?;
    }
    Ok(())
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
                ..
            } => {
                bounds_expr(value, fname, sizes, flow, params, funcs)?;
                // Rebinding the name drops any prior fact/const/counter about it.
                flow.invalidate(name);
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
                            "function `{fname}`: cannot prove this index is in bounds for `{a}` of size {n}, bound it with a `where`/`requires`/loop condition to 0..{n}"
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
                    "function `{fname}`: cannot prove this index is in bounds for slice `{a}`, bound it with a `where`/`requires`/loop condition to 0..{a}.len"
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

/// Folds a compile-time-constant integer index, `Int`, unary negation, and `+`/`-`/`*` of
/// constants, or `None` if it is not constant. Overflow is not our concern (it is caught by
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
        // `0 <= i < 4`, the access is provably safe.
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
        // `a[i & 7]` ∈ [0, 8) for any `i`, a power-of-two-minus-one mask, no sign obligation.
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
}

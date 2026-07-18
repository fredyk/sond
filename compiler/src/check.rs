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

use crate::ast::{BinOp, Block, EnumDef, Expr, Function, Global, Stmt, StructDef, Type, UnOp};

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
        bounds_block(&f.body, &f.name, &mut sizes)?;
    }
    Ok(())
}

fn bounds_block(b: &Block, fname: &str, sizes: &mut HashMap<String, i64>) -> Result<(), String> {
    for s in &b.stmts {
        match s {
            Stmt::Let {
                name,
                value,
                array_len,
                ..
            } => {
                bounds_expr(value, fname, sizes)?;
                match array_len {
                    // `let a[N]` records the known size; a scalar `let a = ..` reusing the
                    // name drops any prior size so a later index is not checked against it.
                    Some(n) => {
                        sizes.insert(name.clone(), *n);
                    }
                    None => {
                        sizes.remove(name);
                    }
                }
            }
            Stmt::Assign { target, value } => {
                bounds_expr(target, fname, sizes)?;
                bounds_expr(value, fname, sizes)?;
            }
            Stmt::Return(Some(v)) | Stmt::Expr(v) => bounds_expr(v, fname, sizes)?,
            Stmt::For {
                cond,
                invariant,
                body,
            } => {
                if let Some(c) = cond {
                    bounds_expr(c, fname, sizes)?;
                }
                for inv in invariant {
                    bounds_expr(inv, fname, sizes)?;
                }
                // A declaration inside the loop does not escape it.
                let mut inner = sizes.clone();
                bounds_block(body, fname, &mut inner)?;
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        bounds_expr(t, fname, sizes)?;
    }
    Ok(())
}

fn bounds_expr(e: &Expr, fname: &str, sizes: &HashMap<String, i64>) -> Result<(), String> {
    if let Expr::Index(base, idx) = e
        && let Expr::Var(a) = base.as_ref()
        && let Some(&n) = sizes.get(a)
        && let Some(k) = const_index(idx)
        && (k < 0 || k >= n)
    {
        return Err(format!(
            "function `{fname}`: index {k} is out of bounds for `{a}` of size {n} (valid range 0..{n})"
        ));
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            bounds_expr(l, fname, sizes)?;
            bounds_expr(r, fname, sizes)
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => bounds_expr(x, fname, sizes),
        Expr::Call(c, args) => {
            bounds_expr(c, fname, sizes)?;
            for a in args {
                bounds_expr(&a.value, fname, sizes)?;
            }
            Ok(())
        }
        Expr::List(items) => {
            for it in items {
                bounds_expr(it, fname, sizes)?;
            }
            Ok(())
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields {
                bounds_expr(v, fname, sizes)?;
            }
            Ok(())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            bounds_expr(cond, fname, sizes)?;
            let mut t = sizes.clone();
            bounds_block(then_branch, fname, &mut t)?;
            if let Some(eb) = else_branch {
                let mut e2 = sizes.clone();
                bounds_block(eb, fname, &mut e2)?;
            }
            Ok(())
        }
        Expr::Match { scrutinee, arms } => {
            bounds_expr(scrutinee, fname, sizes)?;
            for a in arms {
                let mut m = sizes.clone();
                bounds_block(&a.body, fname, &mut m)?;
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
    fn rejects_a_constant_index_from_arithmetic_out_of_bounds() {
        assert!(bounds("fn f() -> i64 { let a[4]\na[2 + 3] }").is_err());
    }

    #[test]
    fn accepts_a_variable_index_unchecked() {
        // A variable index is left to the prover/refinements, the (C) on-ramp rejects only
        // PROVABLE (constant) out-of-bounds, so this must NOT be rejected here.
        assert!(bounds("fn f(i: i64) -> i64 { let a[4]\na[i] }").is_ok());
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
}

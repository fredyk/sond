//! A minimal type checker for Sond.
//!
//! Infers the type of expressions and checks a function's body against its
//! declared return type. This is the first layer of static checking before
//! evaluation/codegen; effect and capability checking come later. Grows via TDD.

use crate::ast::{BinOp, Block, Expr, Function, Stmt, Type as AstType, UnOp};
use std::collections::HashMap;

/// A checked type. `Named` covers user types not yet modelled further.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Decimal,
    Bool,
    Str,
    Unit,
    Named(String),
}

/// A type error found statically.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub message: String,
}

fn err<T>(message: String) -> Result<T, TypeError> {
    Err(TypeError { message })
}

/// Maps a declared AST type to a checked `Ty`.
fn ty_of(decl: &AstType) -> Ty {
    let name = match decl {
        AstType::Named(name) => name,
        // A `*Point` checks as its pointee struct type; value vs pointer is a
        // separate static (mutability) distinction, not a different `Ty` here.
        AstType::Ptr(inner) => return ty_of(inner),
        // A view is held as its base address, so it checks as an integer here; the ELEMENT
        // type is what the codegen needs, and it reads it from the declaration itself.
        AstType::View(..) => return Ty::Int,
    };
    match name.as_str() {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => Ty::Int,
        "f32" | "f64" | "Money" | "Percent" => Ty::Decimal,
        "bool" => Ty::Bool,
        "String" => Ty::Str,
        _ => Ty::Named(name.clone()),
    }
}

type Env = HashMap<String, Ty>;

/// Type-checks a function: builds the parameter environment, checks the body,
/// and verifies the body's type matches the declared return type.
pub fn typecheck_function(func: &Function) -> Result<(), TypeError> {
    let mut env: Env = HashMap::new();
    for p in &func.params {
        env.insert(p.name.clone(), ty_of(&p.ty));
    }

    let body_ty = check_block(&func.body, &mut env)?;

    if let Some(ret) = &func.ret {
        let expected = ty_of(ret);
        if body_ty != expected {
            return err(format!(
                "function `{}` returns {:?} but its body has type {:?}",
                func.name, expected, body_ty
            ));
        }
    }
    Ok(())
}

/// Checks a block and returns the type of its value (tail expression, or `Unit`).
fn check_block(block: &Block, env: &mut Env) -> Result<Ty, TypeError> {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let t = infer(value, env)?;
                env.insert(name.clone(), t);
            }
            Stmt::Assign { target, value } => {
                let vt = infer(value, env)?;
                let tt = infer(target, env)?;
                if vt != tt {
                    return err(format!("cannot assign {vt:?} to a target of type {tt:?}"));
                }
            }
            Stmt::Expr(e) | Stmt::Spawn(e) => {
                infer(e, env)?;
            }
            Stmt::Par(peers) => {
                for p in peers {
                    infer(p, env)?;
                }
            }
            Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                for p in peers {
                    infer(p, env)?;
                }
            }
            Stmt::GraphLet {
                nodes, yield_expr, ..
            } => {
                for (_, e) in nodes {
                    infer(e, env)?;
                }
                infer(yield_expr, env)?;
            }
            Stmt::Return(Some(e)) => {
                infer(e, env)?;
            }
            Stmt::Return(None) => {}
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    let ct = infer(c, env)?;
                    if ct != Ty::Bool {
                        return err(format!("for condition must be bool, got {ct:?}"));
                    }
                }
                check_block(body, env)?;
            }
            Stmt::Lock { body, .. } => {
                check_block(body, env)?;
            }
            Stmt::Break | Stmt::Continue => {}
        }
    }
    match &block.tail {
        Some(tail) => infer(tail, env),
        None => Ok(Ty::Unit),
    }
}

/// Infers the type of an expression.
fn infer(expr: &Expr, env: &mut Env) -> Result<Ty, TypeError> {
    match expr {
        Expr::Int(_) => Ok(Ty::Int),
        Expr::Decimal(_) => Ok(Ty::Decimal),
        Expr::Bool(_) => Ok(Ty::Bool),
        // A string literal is the address of static NUL-terminated bytes, so its type is
        // the integer/address type, the same as what indexing or `peek8` expects.
        Expr::Str(_) => Ok(Ty::Int),
        Expr::Var(name) => env.get(name).cloned().ok_or_else(|| TypeError {
            message: format!("unbound variable `{name}`"),
        }),
        Expr::Unary(op, e) => {
            let t = infer(e, env)?;
            match (op, &t) {
                (UnOp::Not, Ty::Bool) => Ok(Ty::Bool),
                (UnOp::Neg, Ty::Int) => Ok(Ty::Int),
                (UnOp::BitNot, Ty::Int) => Ok(Ty::Int),
                (UnOp::Neg, Ty::Decimal) => Ok(Ty::Decimal),
                _ => err(format!("operator {op:?} does not apply to {t:?}")),
            }
        }
        Expr::Binary(op, l, r) => infer_binary(op, l, r, env),
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let ct = infer(cond, env)?;
            if ct != Ty::Bool {
                return err(format!("if condition must be bool, got {ct:?}"));
            }
            let then_ty = check_block(then_branch, env)?;
            match else_branch {
                Some(eb) => {
                    let else_ty = check_block(eb, env)?;
                    if then_ty != else_ty {
                        return err(format!(
                            "if branches have different types: {then_ty:?} vs {else_ty:?}"
                        ));
                    }
                    Ok(then_ty)
                }
                None => Ok(Ty::Unit),
            }
        }
        // List/Call/Field inference will be added via TDD.
        other => err(format!("type inference not implemented for {other:?}")),
    }
}

fn infer_binary(op: &BinOp, l: &Expr, r: &Expr, env: &mut Env) -> Result<Ty, TypeError> {
    match op {
        // Logical: both operands bool, result bool.
        BinOp::And | BinOp::Or | BinOp::Implies => {
            expect(l, Ty::Bool, env)?;
            expect(r, Ty::Bool, env)?;
            Ok(Ty::Bool)
        }
        // Arithmetic: both operands the same numeric type, result that type.
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
            let lt = infer(l, env)?;
            let rt = infer(r, env)?;
            if lt != rt {
                return err(format!("arithmetic on mismatched types {lt:?} and {rt:?}"));
            }
            if lt != Ty::Int && lt != Ty::Decimal {
                return err(format!("arithmetic requires a numeric type, got {lt:?}"));
            }
            Ok(lt)
        }
        // Bitwise/shift: integers only, result integer.
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr | BinOp::LShr => {
            expect(l, Ty::Int, env)?;
            expect(r, Ty::Int, env)?;
            Ok(Ty::Int)
        }
        // Comparisons: operands same type, result bool.
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
            let lt = infer(l, env)?;
            let rt = infer(r, env)?;
            if lt != rt {
                return err(format!("comparison on mismatched types {lt:?} and {rt:?}"));
            }
            Ok(Ty::Bool)
        }
        // Membership: element type must match the list's element type; result bool.
        BinOp::In => {
            let _ = infer(l, env)?;
            let _ = infer(r, env)?;
            Ok(Ty::Bool)
        }
    }
}

fn expect(expr: &Expr, want: Ty, env: &mut Env) -> Result<(), TypeError> {
    let got = infer(expr, env)?;
    if got == want {
        Ok(())
    } else {
        err(format!("expected {want:?}, got {got:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), TypeError> {
        typecheck_function(&Parser::new(tokenize(src)).parse_function())
    }

    #[test]
    fn accepts_a_well_typed_function() {
        assert!(check("fn f(x: i64) -> i64 { x + 1 }").is_ok());
        assert!(check("fn g() -> bool { 3 > 2 }").is_ok());
        assert!(check("fn h(b: bool) -> i64 { if b { 1 } else { 2 } }").is_ok());
    }

    #[test]
    fn rejects_return_type_mismatch() {
        // Body is bool, declared return is i64.
        let e = check("fn f() -> i64 { true }").unwrap_err();
        assert!(e.message.contains("body has type Bool"));
    }

    #[test]
    fn rejects_arithmetic_on_bool() {
        assert!(check("fn f() -> i64 { true + 1 }").is_err());
    }

    #[test]
    fn rejects_non_bool_if_condition() {
        let e = check("fn f() -> i64 { if 5 { 1 } else { 2 } }").unwrap_err();
        assert!(e.message.contains("condition must be bool"));
    }

    #[test]
    fn rejects_mismatched_if_branches() {
        let e = check("fn f(b: bool) -> i64 { if b { 1 } else { true } }").unwrap_err();
        assert!(e.message.contains("different types"));
    }

    #[test]
    fn rejects_unbound_variable() {
        assert!(check("fn f() -> i64 { y }").is_err());
    }

    #[test]
    fn accepts_the_money_contract_shape() {
        // Decimal params and body.
        assert!(check("fn id(base: Money) -> Money { base }").is_ok());
    }
}

#[cfg(test)]
mod str_literal_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    #[test]
    fn a_string_literal_is_an_address() {
        // `"x"` is a pointer to NUL-terminated bytes, so it types as the address
        // type and composes with pointer arithmetic (`"x" + 1`), unlike a distinct
        // Str type that would reject it.
        let e = Parser::new(tokenize("\"hello\"")).parse_expr();
        let mut env = Env::new();
        assert_eq!(infer(&e, &mut env).unwrap(), Ty::Int);
    }
}

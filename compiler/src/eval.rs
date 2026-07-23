//! A tree-walking evaluator for Sond.
//!
//! This is the first, dynamic form of the contract oracle: it runs a function
//! and checks its `requires`/`ensures` at runtime. A static verifier (SMT /
//! symbolic execution) will come later; runtime checking is the cheap first
//! step that already lets us exercise programs end to end.

use crate::ast::{Arg, BinOp, Block, Expr, Function, Stmt, UnOp};
use crate::decimal::Decimal;
use std::collections::HashMap;

/// A runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    /// Fixed-point decimal (`Money`, `Percent`), exact, never `f64`.
    /// Decimal carries its own scale so that `Money` and `Percent` types
    /// have the correct precision built in.
    Decimal(Decimal),
    Bool(bool),
    Str(String),
    Unit,
    List(Vec<Value>),
}

/// Variable bindings in scope.
pub type Env = HashMap<String, Value>;

/// A contract failure detected at runtime, the dynamic form of the oracle.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractViolation {
    /// Which contract failed: `"requires"` or `"ensures"`.
    pub kind: &'static str,
    /// The 0-based index of the failing proposition within that clause.
    pub index: usize,
}

/// Calls a function with the given arguments, checking its `requires` before
/// and its `ensures` after. This is the oracle: a body that does not satisfy
/// its postconditions is rejected, no matter how it was written.
pub fn call_function(func: &Function, args: &[Value]) -> Result<Value, ContractViolation> {
    let mut env = Env::new();
    for (param, arg) in func.params.iter().zip(args) {
        env.insert(param.name.clone(), arg.clone());
    }

    for (i, req) in func.requires.iter().enumerate() {
        if !as_bool(eval_expr(req, &mut env)) {
            return Err(ContractViolation {
                kind: "requires",
                index: i,
            });
        }
    }

    let result = eval_block(&func.body, &mut env);

    // Bind the named return value so postconditions can refer to it.
    if let Some(name) = &func.ret_name {
        env.insert(name.clone(), result.clone());
    }

    for (i, ens) in func.ensures.iter().enumerate() {
        if !as_bool(eval_expr(ens, &mut env)) {
            return Err(ContractViolation {
                kind: "ensures",
                index: i,
            });
        }
    }

    Ok(result)
}

/// Evaluates a block: runs its statements, then yields its tail expression
/// (or `Unit`). Note: block scoping and early `return` propagation across
/// nested blocks are simplified for now (a known limitation to revisit).
pub fn eval_block(block: &Block, env: &mut Env) -> Value {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = eval_expr(value, env);
                env.insert(name.clone(), v);
            }
            Stmt::Assign {
                target: Expr::Var(name),
                value,
            } => {
                let v = eval_expr(value, env);
                env.insert(name.clone(), v);
            }
            Stmt::Assign { .. } => todo!("assignment to non-variable target"),
            Stmt::Expr(e) | Stmt::Spawn(e) => {
                eval_expr(e, env);
            }
            Stmt::Par(peers) => {
                for p in peers {
                    eval_expr(p, env);
                }
            }
            Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
                for p in peers {
                    eval_expr(p, env);
                }
            }
            Stmt::Return(e) => {
                return e.as_ref().map_or(Value::Unit, |e| eval_expr(e, env));
            }
            // Loop evaluation in the interpreter needs proper break/continue
            // control flow (a Signal refactor); the codegen backend supports
            // loops fully. Contracts do not loop yet, so this is deferred.
            Stmt::For { .. } | Stmt::Break | Stmt::Continue => {
                todo!("interpreter loop support (use the codegen backend for loops)")
            }
        }
    }
    block
        .tail
        .as_ref()
        .map_or(Value::Unit, |e| eval_expr(e, env))
}

/// Evaluates an expression against an environment.
pub fn eval_expr(expr: &Expr, env: &mut Env) -> Value {
    match expr {
        Expr::Int(n) => Value::Int(*n),
        Expr::Decimal(s) => Value::Decimal(Decimal::parse(s)),
        Expr::Bool(b) => Value::Bool(*b),
        Expr::Str(s) => Value::Str(s.clone()),
        Expr::Var(name) => env
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("unbound variable: {name}")),
        Expr::List(items) => Value::List(items.iter().map(|e| eval_expr(e, env)).collect()),
        Expr::Unary(op, e) => {
            let v = eval_expr(e, env);
            eval_unary(op, v)
        }
        Expr::Binary(op, l, r) => eval_binary(op, l, r, env),
        Expr::Call(callee, args) => eval_call(callee, args, env),
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            if as_bool(eval_expr(cond, env)) {
                eval_block(then_branch, env)
            } else if let Some(eb) = else_branch {
                eval_block(eb, env)
            } else {
                Value::Unit
            }
        }
        _ => todo!("eval of {expr:?} not implemented yet"),
    }
}

/// Evaluates a call. Currently supports the rounding builtins; user-defined
/// function calls will be added when the evaluator gains a function table.
fn eval_call(callee: &Expr, args: &[Arg], env: &mut Env) -> Value {
    let Expr::Var(name) = callee else {
        todo!("call of non-identifier callee {callee:?}");
    };
    match name.as_str() {
        // round_accounting(x, decimals=n) / round(x, decimals=n)
        "round_accounting" | "round" => {
            let mut value = None;
            let mut scale = 0u32;
            for arg in args {
                let v = eval_expr(&arg.value, env);
                match arg.name.as_deref() {
                    None => value = Some(v),
                    Some("decimals") => {
                        scale = match v {
                            Value::Int(n) => n as u32,
                            other => panic!("decimals must be an integer, got {other:?}"),
                        }
                    }
                    Some(other) => panic!("unknown keyword argument: {other}"),
                }
            }
            match value {
                Some(Value::Decimal(d)) => Value::Decimal(d.round(scale)),
                other => panic!("round expects a decimal, got {other:?}"),
            }
        }
        other => todo!("builtin or function `{other}` not implemented"),
    }
}

fn eval_unary(op: &UnOp, v: Value) -> Value {
    match (op, v) {
        (UnOp::Not, Value::Bool(b)) => Value::Bool(!b),
        (UnOp::Neg, Value::Int(n)) => Value::Int(-n),
        (op, v) => panic!("bad unary op {op:?} on {v:?}"),
    }
}

fn eval_binary(op: &BinOp, l: &Expr, r: &Expr, env: &mut Env) -> Value {
    // Short-circuiting logical operators.
    match op {
        BinOp::And => {
            return Value::Bool(as_bool(eval_expr(l, env)) && as_bool(eval_expr(r, env)));
        }
        BinOp::Or => {
            return Value::Bool(as_bool(eval_expr(l, env)) || as_bool(eval_expr(r, env)));
        }
        // `a implies b` ≡ `not a or b`.
        BinOp::Implies => {
            return Value::Bool(!as_bool(eval_expr(l, env)) || as_bool(eval_expr(r, env)));
        }
        _ => {}
    }

    let lv = eval_expr(l, env);
    let rv = eval_expr(r, env);
    use Value::{Decimal as D, Int as I};
    match (op, &lv, &rv) {
        // Arithmetic, Int and Decimal, never mixed.
        (BinOp::Add, I(a), I(b)) => I(a + b),
        (BinOp::Sub, I(a), I(b)) => I(a - b),
        (BinOp::Mul, I(a), I(b)) => I(a * b),
        (BinOp::Div, I(a), I(b)) => I(a / b),
        (BinOp::Add, D(a), D(b)) => D(a.add(b)),
        (BinOp::Sub, D(a), D(b)) => D(a.sub(b)),
        (BinOp::Mul, D(a), D(b)) => D(a.mul(b)),
        // Decimal `/` needs a result scale; the operator uses guard digits and
        // callers round afterwards (see Decimal::div, a design finding).
        (BinOp::Div, D(a), D(b)) => D(a.div(b, a.scale.max(b.scale) + 6)),
        // Ordering comparisons, Int and Decimal.
        (BinOp::Lt, I(a), I(b)) => Value::Bool(a < b),
        (BinOp::Gt, I(a), I(b)) => Value::Bool(a > b),
        (BinOp::Le, I(a), I(b)) => Value::Bool(a <= b),
        (BinOp::Ge, I(a), I(b)) => Value::Bool(a >= b),
        (BinOp::Lt, D(a), D(b)) => Value::Bool(a < b),
        (BinOp::Gt, D(a), D(b)) => Value::Bool(a > b),
        (BinOp::Le, D(a), D(b)) => Value::Bool(a <= b),
        (BinOp::Ge, D(a), D(b)) => Value::Bool(a >= b),
        // Equality works for every value type (Decimal compares numerically).
        (BinOp::Eq, _, _) => Value::Bool(lv == rv),
        (BinOp::Ne, _, _) => Value::Bool(lv != rv),
        // Membership.
        (BinOp::In, _, Value::List(items)) => Value::Bool(items.contains(&lv)),
        (BinOp::And | BinOp::Or | BinOp::Implies, _, _) => unreachable!("handled above"),
        (op, a, b) => panic!("bad binary op {op:?} on {a:?} and {b:?}"),
    }
}

fn as_bool(v: Value) -> bool {
    match v {
        Value::Bool(b) => b,
        other => panic!("expected Bool, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn eval(src: &str) -> Value {
        let expr = Parser::new(tokenize(src)).parse_expr();
        eval_expr(&expr, &mut Env::new())
    }

    fn eval_with(src: &str, env: &mut Env) -> Value {
        eval_expr(&Parser::new(tokenize(src)).parse_expr(), env)
    }

    fn parse_function(src: &str) -> Function {
        Parser::new(tokenize(src)).parse_function()
    }

    #[test]
    fn evaluates_arithmetic_with_precedence() {
        // `2 + 3 * 4` == 14
        assert_eq!(eval("2 + 3 * 4"), Value::Int(14));
    }

    #[test]
    fn evaluates_comparisons() {
        assert_eq!(eval("3 >= 2"), Value::Bool(true));
        assert_eq!(eval("1 == 2"), Value::Bool(false));
    }

    #[test]
    fn evaluates_logical_and_implies() {
        assert_eq!(eval("true and false"), Value::Bool(false));
        // `false implies X` is vacuously true.
        assert_eq!(eval("false implies false"), Value::Bool(true));
        assert_eq!(eval("true implies false"), Value::Bool(false));
    }

    #[test]
    fn evaluates_membership() {
        assert_eq!(eval("2 in [1, 2, 3]"), Value::Bool(true));
        assert_eq!(eval("9 in [1, 2, 3]"), Value::Bool(false));
    }

    #[test]
    fn evaluates_decimal_arithmetic_precision() {
        assert_eq!(eval("1.50 + 2.25"), Value::Decimal(Decimal::parse("3.75")));
        assert_eq!(eval("10.00 - 0.01"), Value::Decimal(Decimal::parse("9.99")));
        assert_eq!(eval("0.00 >= 0.00"), Value::Bool(true));
        // A membership test over decimals works as expected.
        assert_eq!(eval("21.0 in [0.0, 4.0, 10.0, 21.0]"), Value::Bool(true));
        assert_eq!(eval("15.0 in [0.0, 4.0, 10.0, 21.0]"), Value::Bool(false));
    }

    fn money(s: &str) -> Value {
        Value::Decimal(Decimal::parse(s))
    }

    #[test]
    fn executes_the_full_fiscal_function_and_it_self_verifies() {
        // The complete RFC-0002 §5.1 function: parsed, executed with exact
        // fixed-point money and accounting rounding, and self-checked against
        // its own requires/ensures. This is the whole loop, end to end.
        let src = concat!(
            "fn compute_vat(\n",
            "    tax_base: Money,\n",
            "    vat_rate: Percent,\n",
            "    intra_community: bool,\n",
            ") -> (vat_amount: Money)\n",
            "requires {\n",
            "    tax_base >= 0.00\n",
            "    vat_rate in [0.0, 4.0, 10.0, 21.0]\n",
            "    intra_community implies vat_rate == 0.0\n",
            "}\n",
            "ensures {\n",
            "    intra_community implies vat_amount == 0.00\n",
            "    vat_amount >= 0.00\n",
            "}\n",
            "effects [] {\n",
            "    if intra_community {\n",
            "        0.00\n",
            "    } else {\n",
            "        round_accounting(tax_base * vat_rate / 100.0, decimals=2)\n",
            "    }\n",
            "}",
        );
        let f = parse_function(src);

        // 1000.00 at 21% = 210.00
        assert_eq!(
            call_function(&f, &[money("1000.00"), money("21.0"), Value::Bool(false)]),
            Ok(money("210.00"))
        );
        // 33.33 at 21% = 6.9993 → rounds half-up to 7.00
        assert_eq!(
            call_function(&f, &[money("33.33"), money("21.0"), Value::Bool(false)]),
            Ok(money("7.00"))
        );
        // Intra-community (inversion of taxable person): vat_amount is 0.00
        assert_eq!(
            call_function(&f, &[money("1000.00"), money("0.0"), Value::Bool(true)]),
            Ok(money("0.00"))
        );
        // Negative base violates the precondition.
        assert_eq!(
            call_function(&f, &[money("-1.00"), money("21.0"), Value::Bool(false)])
                .unwrap_err()
                .kind,
            "requires"
        );
    }

    #[test]
    fn the_oracle_checks_a_money_contract() {
        // A `Money` contract enforced over exact fixed-point decimals.
        let f = parse_function(
            "fn id(base: Money) -> (r: Money) requires { base >= 0.00 } ensures { r == base } effects [] { base }",
        );
        // 1000.00 at 21% = 210.00
        assert_eq!(
            call_function(&f, &[Value::Decimal(Decimal::parse("100.00"))]),
            Ok(Value::Decimal(Decimal::parse("100.00")))
        );
        // Negative money violates the precondition.
        assert_eq!(
            call_function(&f, &[Value::Decimal(Decimal::parse("-1.00"))])
                .unwrap_err()
                .kind,
            "requires"
        );
    }

    #[test]
    fn evaluates_variables_using_the_environment() {
        let mut env = Env::new();
        env.insert("x".to_string(), Value::Int(10));
        assert_eq!(eval_with("x + 5", &mut env), Value::Int(15));
    }

    #[test]
    fn evaluates_an_if_else_expression() {
        let f = parse_function("fn pick(b: bool) -> (r: i64) effects [] { if b { 1 } else { 2 } }");
        assert_eq!(call_function(&f, &[Value::Bool(true)]), Ok(Value::Int(1)));
        assert_eq!(call_function(&f, &[Value::Bool(false)]), Ok(Value::Int(2)));
    }

    #[test]
    fn call_enforces_the_precondition() {
        let f = parse_function(
            "fn double(x: i64) -> (r: i64) requires { x >= 0 } ensures { r == x + x } effects [] { x + x }",
        );
        // Valid input: x >= 0 and body satisfies the postcondition.
        assert_eq!(call_function(&f, &[Value::Int(5)]), Ok(Value::Int(10)));
        // Precondition violated: x < 0.
        assert_eq!(
            call_function(&f, &[Value::Int(-1)]),
            Err(ContractViolation {
                kind: "requires",
                index: 0
            })
        );
    }

    #[test]
    fn the_oracle_catches_a_wrong_body_via_the_postcondition() {
        // The core of the whole thesis, in miniature: the contract promises
        // `r == x + x`, but the body computes `x + 1`. For x = 5 that yields 6,
        // not 10, so the `ensures` rejects it, exactly what must happen when a
        // weak model (qwen3.6) writes a subtly wrong body.
        let f = parse_function(
            "fn double(x: i64) -> (r: i64) ensures { r == x + x } effects [] { x + 1 }",
        );
        assert_eq!(
            call_function(&f, &[Value::Int(5)]),
            Err(ContractViolation {
                kind: "ensures",
                index: 0
            })
        );
    }
}

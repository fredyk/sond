//! The `sond test` harness: turns the `test "..." { .. }` blocks in a source file into a
//! runnable program whose `main` announces and runs each test in sequence.
//!
//! A failing `assert`/`assert_eq` aborts the process with a nonzero exit at the offending
//! test (see the testing library), so the last announced-but-unconfirmed test names the
//! failure and the process exit code is nonzero. Per-test isolation via `fork` (so the
//! suite continues past a failure and reports full pass/fail counts) is a later refinement.

use crate::ast::{Arg, Block, Expr, Function, Stmt, Type};

/// A positional call `name(args...)`.
fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call(
        Box::new(Expr::Var(name.to_string())),
        args.into_iter()
            .map(|value| Arg { name: None, value })
            .collect(),
    )
}

/// A `print("literal")` statement.
fn print_str(s: &str) -> Stmt {
    Stmt::Expr(call("print", vec![Expr::Str(s.to_string())]))
}

/// Builds the `main` that `sond test` runs: announce the count, then for each test print
/// its name, call it, and confirm `ok`. Returns 0 when every test's body ran to completion
/// (a failing assertion exits the process before its `ok`).
pub fn synth_harness(funcs: &[Function]) -> Function {
    let tests: Vec<&Function> = funcs.iter().filter(|f| f.is_test).collect();
    let n = tests.len();
    let mut stmts = vec![print_str(&format!("running {n} test(s)\n"))];
    for t in &tests {
        let name = t.test_name.clone().unwrap_or_else(|| t.name.clone());
        stmts.push(print_str(&format!("test {name} ... ")));
        stmts.push(Stmt::Expr(call(&t.name, vec![]))); // aborts here on assertion failure
        stmts.push(print_str("ok\n"));
    }
    stmts.push(print_str(&format!("\nall {n} test(s) passed\n")));
    Function {
        name: "main".to_string(),
        ret: Some(Type::Named("i64".to_string())),
        body: Block {
            stmts,
            tail: Some(Box::new(Expr::Int(0))),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn parse(src: &str) -> Vec<Function> {
        Parser::new(tokenize(src)).parse_program()
    }

    #[test]
    fn harness_calls_every_test_and_returns_zero() {
        let funcs = parse("test \"a\" { let x = 1 }\ntest \"b\" { let y = 2 }");
        let main = synth_harness(&funcs);
        assert_eq!(main.name, "main");
        // Both test functions are called from the harness body.
        let calls: Vec<String> = main
            .body
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Expr(Expr::Call(callee, _)) => match callee.as_ref() {
                    Expr::Var(n) if n.starts_with("__test_") => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(calls, vec!["__test_0", "__test_1"], "calls each test in order");
        assert!(matches!(main.body.tail.as_deref(), Some(Expr::Int(0))));
    }

    #[test]
    fn harness_with_no_tests_is_valid_and_returns_zero() {
        let main = synth_harness(&parse("fn f() -> i64 { 0 }"));
        assert_eq!(main.name, "main");
        assert!(matches!(main.body.tail.as_deref(), Some(Expr::Int(0))));
    }
}

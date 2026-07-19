use super::*;
use crate::ast::{Function, Param, StructDef, Type, Visibility};
use crate::lexer::tokenize;

fn parse_fn(src: &str) -> Function {
    Parser::new(tokenize(src)).parse_function()
}

fn named(t: &str) -> Type {
    Type::Named(t.to_string())
}

#[test]
fn parses_a_test_block() {
    // `test "name" { .. }` is a named test block, lowered to a niladic function marked
    // `is_test` with the human-readable name kept for the runner's report.
    let mut p = Parser::new(tokenize("test \"abs is non-negative\" { let x = 1 }"));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 1, "the test block is one top-level item");
    assert!(funcs[0].is_test, "should be marked as a test");
    assert_eq!(
        funcs[0].test_name.as_deref(),
        Some("abs is non-negative"),
        "keeps the readable test name"
    );
}

#[test]
fn test_is_a_contextual_keyword_usable_as_an_identifier() {
    // `test` is a keyword only in `test "name" { .. }` position (a test block). Elsewhere, a
    // function name, a variable, a parameter, it is an ordinary identifier. (Gate finding: an
    // independent AI hit "expected identifier, got Test" writing `fn test()`.)
    let f = parse_fn("fn test(test: i64) -> i64 { let test = 1\ntest }");
    assert_eq!(f.name, "test", "a function may be named `test`");
    assert_eq!(f.params[0].name, "test", "a parameter may be named `test`");
    // The top-level test-block form still parses (the keyword role is unaffected).
    let mut p = Parser::new(tokenize("test \"still works\" { let x = 1 }"));
    let funcs = p.parse_program();
    assert!(funcs[0].is_test);
}

#[test]
fn parses_multiple_tests_with_distinct_names() {
    let mut p = Parser::new(tokenize(
        "test \"one\" { let a = 1 }\ntest \"two\" { let b = 2 }",
    ));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 2);
    assert_ne!(funcs[0].name, funcs[1].name, "each test gets a distinct symbol");
    assert!(funcs.iter().all(|f| f.is_test));
}

#[test]
fn parses_pub_and_priv_visibility() {
    // `pub` exports a declaration; the default (unmarked) is private. This is the
    // surface a generated header will expose (only the `pub` items), and the
    // mechanism the module checker will enforce.
    let mut p = Parser::new(tokenize(
        "pub fn f() -> i64 { 0 }\n\
         fn g() -> i64 { 0 }\n\
         priv fn h() -> i64 { 0 }\n\
         pub struct P { x: i64 }\n\
         pub let N = 5",
    ));
    let funcs = p.parse_program();
    assert_eq!(funcs[0].vis, Visibility::Pub, "pub fn is public");
    assert_eq!(
        funcs[1].vis,
        Visibility::Priv,
        "unmarked fn is private by default"
    );
    assert_eq!(funcs[2].vis, Visibility::Priv, "priv fn is private");
    assert_eq!(p.take_structs()[0].vis, Visibility::Pub, "pub struct");
    assert_eq!(p.take_globals()[0].vis, Visibility::Pub, "pub global");
}

#[test]
fn parses_interrupt_handler_qualifier() {
    // `interrupt fn` marks an ISR (codegen saves all GP registers and returns with
    // `iretq`). It composes with visibility and is off for a plain `fn`.
    let plain = parse_fn("fn f() { 0 }");
    assert!(
        !plain.is_interrupt,
        "a plain fn is not an interrupt handler"
    );
    let isr = parse_fn("interrupt fn timer() { 0 }");
    assert!(isr.is_interrupt, "interrupt fn is an interrupt handler");
    assert_eq!(isr.name, "timer");
    let mut p = Parser::new(tokenize("pub interrupt fn h() { 0 }"));
    let funcs = p.parse_program();
    assert!(funcs[0].is_interrupt, "pub interrupt fn is a handler");
    assert_eq!(funcs[0].vis, Visibility::Pub, "and keeps its visibility");
}

#[test]
fn parses_a_struct_declaration() {
    // `struct Point { x: i64, y: i64 }`, a top-level record type, collected
    // separately from functions (like globals). Fields reuse `Param` (name: Type).
    let mut p = Parser::new(tokenize("struct Point { x: i64, y: i64 }"));
    let funcs = p.parse_program();
    assert!(funcs.is_empty(), "a struct declaration is not a function");
    assert_eq!(
        p.take_structs(),
        vec![StructDef {
            vis: Visibility::Priv,
            name: "Point".to_string(),
            fields: vec![
                Param {
                    name: "x".to_string(),
                    ty: named("i64"),
                },
                Param {
                    name: "y".to_string(),
                    ty: named("i64"),
                },
            ],
        }]
    );
}

#[test]
fn parses_the_simplest_function() {
    // `fn f() -> Money { }`, no params, a return type, empty body.
    assert_eq!(
        parse_fn("fn f() -> Money { }"),
        Function {
            name: "f".to_string(),
            params: vec![],
            ret: Some(named("Money")),
            ..Default::default()
        }
    );
}

#[test]
fn parses_a_function_with_params() {
    assert_eq!(
        parse_fn("fn compute_vat(base: Money, pct: Percent) -> Money { }"),
        Function {
            name: "compute_vat".to_string(),
            params: vec![
                Param {
                    name: "base".to_string(),
                    ty: named("Money")
                },
                Param {
                    name: "pct".to_string(),
                    ty: named("Percent")
                },
            ],
            ret: Some(named("Money")),
            ..Default::default()
        }
    );
}

#[test]
fn parses_the_fiscal_function_signature_and_requires() {
    // The real RFC-0002 §5.1 fiscal function: named return, three `requires`
    // propositions, empty effects. This is the design meeting the compiler.
    let src = "fn compute_vat(\n\
            \x20   tax_base: Money,\n\
            \x20   vat_rate: Percent,\n\
            \x20   intra_community: bool,\n\
            ) -> (vat_amount: Money)\n\
            requires {\n\
            \x20   tax_base >= 0.00\n\
            \x20   vat_rate in [0.0, 4.0, 10.0, 21.0]\n\
            \x20   intra_community implies vat_rate == 0.0\n\
            }\n\
            effects [] {\n\
            }";
    let f = parse_fn(src);
    assert_eq!(f.name, "compute_vat");
    assert_eq!(f.params.len(), 3);
    assert_eq!(f.ret_name, Some("vat_amount".to_string()));
    assert_eq!(f.ret, Some(named("Money")));
    assert_eq!(f.requires.len(), 3);
    assert_eq!(f.ensures.len(), 0);
    assert!(f.effects.is_empty());
    // The first precondition is `tax_base >= 0.00`.
    assert_eq!(
        f.requires[0],
        bin(
            BinOp::Ge,
            var("tax_base"),
            Expr::Decimal("0.00".to_string())
        )
    );
    // The third is an `implies` (loosest operator, parsed as the root).
    assert!(matches!(f.requires[2], Expr::Binary(BinOp::Implies, _, _)));
}

#[test]
fn parses_where_refinements_on_a_parameter_into_requires() {
    // `where 0 <= i && i < 10` on the parameter desugars to two preconditions.
    let f = parse_fn("fn get(i: i64 where 0 <= i && i < 10) -> i64 { i }");
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.requires.len(), 2);
    assert_eq!(f.requires[0], bin(BinOp::Le, Expr::Int(0), var("i")));
    assert_eq!(f.requires[1], bin(BinOp::Lt, var("i"), Expr::Int(10)));
}

// --- body / statement tests ---

#[test]
fn parses_a_let_statement_and_a_tail_expression() {
    let f = parse_fn("fn f() -> Money { let x = 42\nx }");
    assert_eq!(f.body.stmts.len(), 1);
    assert!(matches!(f.body.stmts[0], Stmt::Let { .. }));
    assert_eq!(f.body.tail, Some(Box::new(var("x"))));
}

#[test]
fn parses_an_assignment_statement() {
    // `source.balance = 0.00`, Field target, decimal value.
    let f = parse_fn("fn f() { source.balance = 0.00\n}");
    assert_eq!(f.body.stmts.len(), 1);
    assert!(matches!(f.body.stmts[0], Stmt::Assign { .. }));
    assert!(f.body.tail.is_none());
}

#[test]
fn parses_the_full_fiscal_function_including_its_body() {
    // The complete RFC-0002 §5.1 function: signature + contracts + an
    // if/else body whose else branch calls round_accounting with a
    // keyword argument. The whole designed syntax, meeting the compiler.
    let src = concat!(
        "fn compute_vat(\n",
        "    tax_base: Money,\n",
        "    vat_rate: Percent,\n",
        "    intra_community: bool,\n",
        ") -> (vat_amount: Money)\n",
        "requires {\n",
        "    tax_base >= 0.00\n",
        "}\n",
        "ensures {\n",
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
    let f = parse_fn(src);
    assert_eq!(f.name, "compute_vat");
    assert_eq!(f.params.len(), 3);
    assert_eq!(f.ret_name, Some("vat_amount".to_string()));
    assert_eq!(f.requires.len(), 1);
    assert_eq!(f.ensures.len(), 1);
    // The body has no statements; its value is the if/else expression.
    assert!(f.body.stmts.is_empty());
    assert!(matches!(f.body.tail.as_deref(), Some(Expr::If { .. })));
    // The else branch's tail is the round_accounting(...) call.
    let Some(Expr::If {
        else_branch: Some(eb),
        ..
    }) = f.body.tail.as_deref()
    else {
        panic!("expected an if/else tail expression");
    };
    assert!(matches!(eb.tail.as_deref(), Some(Expr::Call(..))));
}

#[test]
fn array_typed_parameters_are_accepted_as_i64_addresses() {
    // AI writers annotate array/pointer params as `[]` (or `[i64]`). Sond passes
    // an array as its i64 address, so `[]` is accepted as the type i64.
    let f = parse_fn("fn fe_copy(c: [], a: [i64]) -> i64 { poke8(c, peek8(a))\n0 }");
    assert_eq!(
        f.params,
        vec![
            Param {
                name: "c".to_string(),
                ty: named("i64")
            },
            Param {
                name: "a".to_string(),
                ty: named("i64")
            },
        ]
    );
}

#[test]
fn a_parameter_may_be_marked_mut() {
    // `mut` on a parameter is accepted, parameters are assignable locals, so a
    // mutable accumulator param (common in checksum/crypto code) is idiomatic.
    let f = parse_fn("fn cksum(mut s: i64) -> i64 { s = s + 1\n s }");
    assert_eq!(
        f.params,
        vec![Param {
            name: "s".to_string(),
            ty: named("i64")
        }]
    );
    assert!(matches!(f.body.stmts[0], Stmt::Assign { .. }));
}

#[test]
fn parses_params_with_trailing_comma() {
    // gofmt-style trailing comma (RFC-0002 §7.2, rule 7).
    assert_eq!(
        parse_fn("fn f(x: A,) -> B { }").params,
        vec![Param {
            name: "x".to_string(),
            ty: named("A")
        }]
    );
}

#[test]
fn parses_a_function_returning_unit() {
    // No `->`: the return type is None (unit).
    let f = parse_fn("fn f() { }");
    assert_eq!(f.ret, None);
}

// --- expression tests ---

fn parse_e(src: &str) -> Expr {
    Parser::new(tokenize(src)).parse_expr()
}

fn var(n: &str) -> Expr {
    Expr::Var(n.to_string())
}

fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    Expr::Binary(op, Box::new(l), Box::new(r))
}

#[test]
fn if_statement_without_else_is_not_called_by_the_next_paren_line() {
    // `if c { .. }` as a statement, then a line starting with `(`, must be TWO
    // statements, not Call(if-expr, ...). The implicit `;` after `}` terminates
    // the statement; the optional-`else` lookahead must not swallow it.
    let f = parse_fn("fn f() -> i64 { let mut s = 7\nif s > 5 { s = 1 }\n(s + 1) }");
    assert_eq!(f.body.stmts.len(), 2, "let + if, two statements");
    assert!(
        matches!(f.body.tail.as_deref(), Some(Expr::Binary(..))),
        "tail is (s + 1), not a call of the if"
    );
}

#[test]
fn list_literal_last_element_needs_no_trailing_comma() {
    // The last element may sit on its own line with NO trailing comma before `]`
    // (how a hand-written table ends). The implicit `;` at that newline, inserted
    // because the line ends in a value, must be skipped, not mistaken for the end.
    let e = parse_e("[\n  10,\n  20,\n  30\n]");
    assert_eq!(
        e,
        Expr::List(vec![Expr::Int(10), Expr::Int(20), Expr::Int(30)])
    );
}

#[test]
fn range_for_loop_desugars_to_a_counting_loop() {
    // `for i in a..b { body }` is a counting loop: it declares `i = a`, loops while
    // `i < b`, and auto-increments, what AI writers reach for. It desugars to a
    // `let mut i` plus a `for i < b` whose body ends with `i = i + 1`.
    let f = parse_fn("fn f() -> i64 { let mut s = 0\nfor i in 0..5 { s = s + i }\ns }");
    assert!(
        f.body
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::Let { name, .. } if name == "i")),
        "declares the loop counter i"
    );
    let Some(Stmt::For { body, .. }) = f.body.stmts.iter().find(|s| matches!(s, Stmt::For { .. }))
    else {
        panic!("a for-loop is produced");
    };
    assert!(
        matches!(body.stmts.last(), Some(Stmt::Assign { .. })),
        "the body ends with the i = i + 1 increment"
    );
}

#[test]
fn a_list_literal_may_span_multiple_lines() {
    // A crypto constant table `[c0, c1, ...]` is written across many lines; the
    // implicit semicolons at the newlines inside `[ ]` must be skipped.
    let e = parse_e("[\n  10,\n  20,\n  30,\n]");
    assert_eq!(
        e,
        Expr::List(vec![Expr::Int(10), Expr::Int(20), Expr::Int(30)])
    );
}

#[test]
fn parses_a_comparison() {
    // `tax_base >= 0.00`
    assert_eq!(
        parse_e("tax_base >= 0.00"),
        bin(
            BinOp::Ge,
            var("tax_base"),
            Expr::Decimal("0.00".to_string())
        )
    );
}

#[test]
fn arithmetic_precedence_multiplication_before_addition() {
    // `a + b * c` == `a + (b * c)`
    assert_eq!(
        parse_e("a + b * c"),
        bin(BinOp::Add, var("a"), bin(BinOp::Mul, var("b"), var("c")))
    );
}

#[test]
fn implies_is_loosest_and_right_associative() {
    // `a implies b == c` == `a implies (b == c)`
    assert_eq!(
        parse_e("a implies b == c"),
        bin(BinOp::Implies, var("a"), bin(BinOp::Eq, var("b"), var("c")))
    );
}

#[test]
fn parses_membership_with_a_list_literal() {
    // `vat_rate in [0.0, 21.0]`
    assert_eq!(
        parse_e("vat_rate in [0.0, 21.0]"),
        bin(
            BinOp::In,
            var("vat_rate"),
            Expr::List(vec![
                Expr::Decimal("0.0".to_string()),
                Expr::Decimal("21.0".to_string()),
            ])
        )
    );
}

#[test]
fn parses_prefix_not_tighter_than_and() {
    // `not a and b` == `(not a) and b`
    assert_eq!(
        parse_e("not a and b"),
        bin(
            BinOp::And,
            Expr::Unary(UnOp::Not, Box::new(var("a"))),
            var("b")
        )
    );
}

#[test]
fn parses_field_access_and_call() {
    // `items.len()` == Call(Field(items, "len"), [])
    assert_eq!(
        parse_e("items.len()"),
        Expr::Call(
            Box::new(Expr::Field(Box::new(var("items")), "len".to_string())),
            vec![]
        )
    );
}

#[test]
fn parses_a_call_with_positional_and_keyword_args() {
    // `round(x, decimals=2)`, RFC-0002 examples use keyword args.
    assert_eq!(
        parse_e("round(x, decimals=2)"),
        Expr::Call(
            Box::new(var("round")),
            vec![
                Arg {
                    name: None,
                    value: var("x")
                },
                Arg {
                    name: Some("decimals".to_string()),
                    value: Expr::Int(2)
                },
            ]
        )
    );
}

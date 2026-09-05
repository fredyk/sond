use super::*;
use crate::ast::{Block, Expr, Function, Param, Stmt, StructDef, Type, Visibility};
use crate::lexer::tokenize;

fn parse_fn(src: &str) -> Function {
    Parser::new(tokenize(src)).parse_function()
}

#[test]
fn a_rejected_token_carries_its_line_and_column() {
    // The parser refuses the `;` where the `if` wanted a `{`; the refusal must say EXACTLY
    // where, in the text that was parsed: line 2, the column of the `;` (column 13), and the
    // tokens written the way a person writes them.
    let src = "fn main() -> i64 {\n  if a == 1;\n}\n";
    let (toks, lines, cols) = crate::lexer::tokenize_located(src);
    let mut p = Parser::with_positions(toks, lines, cols);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.parse_program()))
        .expect_err("a `;` where a `{` belongs must be refused");
    std::panic::set_hook(prev);
    let p = err
        .downcast_ref::<ParsePanic>()
        .expect("a parse refusal must carry a structured position");
    assert_eq!(p.line, 2, "line of the `;`: {p:?}");
    assert_eq!(p.col as usize, src[src.find('\n').unwrap() + 1..].find(';').unwrap() + 1);
    assert_eq!(p.message, "expected `{`, got `;`");
}

#[test]
fn end_of_input_refuses_at_the_last_token_not_one_line_past_it() {
    // An unterminated block: the honest position is where the text stops -- the last token's
    // line -- never a line index the file does not have.
    let (toks, lines, cols) = crate::lexer::tokenize_located("fn main() -> i64 {\nif a == 1");
    let mut p = Parser::with_positions(toks, lines, cols);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.parse_program()))
        .expect_err("end of input is a parse error");
    std::panic::set_hook(prev);
    let p = err
        .downcast_ref::<ParsePanic>()
        .expect("a parse refusal must carry a structured position");
    assert_eq!(p.line, 2, "the file has two lines: {p:?}");
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
    // `test` is a keyword only in `test "name" { .. }` position (a test block). Elsewhere (a
    // function name, a variable, a parameter), it is an ordinary identifier. (Gate finding: an
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
    assert_ne!(
        funcs[0].name, funcs[1].name,
        "each test gets a distinct symbol"
    );
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
    // `struct Point { x: i64, y: i64 }`: a top-level record type, collected
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
    // `fn f() -> Money { }`: no params, a return type, empty body.
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
    // `source.balance = 0.00`: Field target, decimal value.
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
    // (how a hand-written table ends). The implicit `;` at that newline (inserted
    // because the line ends in a value) must be skipped, not mistaken for the end.
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
fn rejects_a_call_whose_callee_is_not_a_name() {
    // `let mut b = 2 (a + b) + (a + b)` on ONE line: with no semicolons the parser has
    // to decide where `2` ends, and it used to decide that `(a + b)` was a CALL of the
    // literal `2`. The resulting program was not the one written, no line survived,
    // and the tell only surfaced later when x64 codegen met `Int(2)` as a callee.
    // Sond has no invocable values: only a name (or a method on one) may be called.
    let mut p = Parser::new(tokenize(
        "fn main() {\n let mut a = 1\n let mut b = 2 (a + b) + (a + b)\n}",
    ));
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.parse_program()))
        .expect_err("a call of a literal must be refused");
    std::panic::set_hook(prev);
    let rejection = err
        .downcast_ref::<ParsePanic>()
        .expect("a parse refusal carries a position");
    assert!(
        rejection.message.contains("a call can only target a function name"),
        "{rejection:?}"
    );
}

#[test]
fn parses_a_call_with_positional_and_keyword_args() {
    // `round(x, decimals=2)`: RFC-0002 examples use keyword args.
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

#[test]
fn parses_a_spawn_statement() {
    // `spawn f(a)` is a statement whose inner expression is the call to run on a
    // new thread.
    let f = parse_fn("fn run(a: i64) -> i64 { spawn work(a) 0 }");
    let is_spawn_call = f
        .body
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::Spawn(Expr::Call(_, _))));
    assert!(
        is_spawn_call,
        "the body should hold a Spawn of a call, got {:?}",
        f.body
    );
}

#[test]
fn parses_a_par_statement_as_a_flat_peer_list() {
    // `par a() ~~ b() ~~ c()`: the `~~` separates a FLAT list of peer calls (no
    // associativity, no nesting). It parses into one `Stmt::Par` holding all three.
    let f = parse_fn("fn run() -> i64 { par a() ~~ b() ~~ c() 0 }");
    let par = f
        .body
        .stmts
        .iter()
        .find_map(|s| match s {
            Stmt::Par(peers) => Some(peers),
            _ => None,
        })
        .expect("the body should hold a Par statement");
    assert_eq!(par.len(), 3, "three flat peers, not a nested tree: {par:?}");
    assert!(
        par.iter().all(|p| matches!(p, Expr::Call(_, _))),
        "every peer is a call: {par:?}"
    );
}

#[test]
fn parses_a_positional_par_binding() {
    // `let a ~~ b ~~ c = par f() ~~ g() ~~ h()`: N names bound to N peers, both `~~`-separated.
    let f = parse_fn("fn run() -> i64 { let a ~~ b ~~ c = par f() ~~ g() ~~ h()\n a + b + c }");
    let (names, peers) = f
        .body
        .stmts
        .iter()
        .find_map(|s| match s {
            Stmt::ParLet { names, peers, .. } => Some((names, peers)),
            _ => None,
        })
        .expect("the body should hold a ParLet");
    assert_eq!(names, &["a", "b", "c"], "the three names, in source order");
    assert_eq!(peers.len(), 3, "one peer per name");
}

#[test]
fn parses_a_par_reduce() {
    // `let s = par(+) f() ~~ g() ~~ h()`: the reducer op in parens, then the peers.
    let f = parse_fn("fn run() -> i64 { let s = par(*) f() ~~ g() ~~ h()\n s }");
    let (op, peers) = f
        .body
        .stmts
        .iter()
        .find_map(|s| match s {
            Stmt::ParReduce { op, peers, .. } => Some((op, peers)),
            _ => None,
        })
        .expect("the body should hold a ParReduce");
    assert_eq!(*op, crate::ast::BinOp::Mul, "the reducer is `*`");
    assert_eq!(peers.len(), 3, "three peers folded into one value");
}

#[test]
fn parse_block_stamps_each_statement_with_its_source_line() {
    // Line 1: `fn f() {`; line 2: `let a = 1`; line 3: `let b = 2`; line 4: `a + b` (the tail).
    let (toks, lines, _) =
        crate::lexer::tokenize_located("fn f() -> i64 {\nlet a = 1\nlet b = 2\na + b\n}");
    let mut p = Parser::with_lines(toks, lines);
    let funcs = p.parse_program();
    let body = &funcs[0].body;
    assert_eq!(body.stmts.len(), 2, "two statements; `a + b` is the tail");
    assert_eq!(
        body.stmt_lines,
        vec![2, 3, 4],
        "each statement plus the tail stamped with its source line"
    );
}

#[test]
fn parses_a_graph_block() {
    // `let r = graph { node a = e  node b = e  yield e }`: `node`/`yield` are contextual keywords.
    let f = parse_fn(
        "fn run() -> i64 { let r = graph { node a = f(1)\n node b = g(a)\n yield b }\n r }",
    );
    let (nodes, _) = f
        .body
        .stmts
        .iter()
        .find_map(|s| match s {
            Stmt::GraphLet {
                nodes, yield_expr, ..
            } => Some((nodes, yield_expr)),
            _ => None,
        })
        .expect("the body should hold a GraphLet");
    let names: Vec<&str> = nodes.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, &["a", "b"], "two nodes in source order");
}

#[test]
fn node_is_a_free_identifier_outside_a_graph() {
    // `node` is contextual: it must still work as an ordinary parameter/variable name (json.snd
    // uses `fn json_type(node: i64)`), so making it a keyword only inside `graph` matters.
    let f = parse_fn("fn json_type(node: i64) -> i64 { node }");
    assert_eq!(
        f.params[0].name, "node",
        "`node` is a normal parameter name"
    );
}

// --- RFC-0014: ports (interfaces implemented per target) ---

#[test]
fn parses_a_port_with_bodyless_signatures() {
    // `iface Name { fn a() fn b() -> T }` is a named interface: a set of signatures
    // with no bodies. It is collected like structs/enums, off the function list.
    let mut p = Parser::new(tokenize(
        "iface Serial {\n\
           fn serial_init()\n\
           fn serial_getc() -> i64\n\
           fn serial_putc(b: i64)\n\
         }",
    ));
    let funcs = p.parse_program();
    assert!(
        funcs.is_empty(),
        "a port declares no functions, got {funcs:?}"
    );
    let ports = p.take_ports();
    assert_eq!(ports.len(), 1, "one port parsed");
    assert_eq!(ports[0].name, "Serial");
    let names: Vec<&str> = ports[0].methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["serial_init", "serial_getc", "serial_putc"]);
    assert_eq!(
        ports[0].methods[1].ret,
        Some(named("i64")),
        "keeps the return type"
    );
    assert_eq!(ports[0].methods[2].params[0].name, "b", "keeps the params");
}

#[test]
fn a_port_signature_keeps_its_contracts() {
    // A port method carries requires/ensures: the obligation every provider must meet
    // and the guarantee every caller may rely on.
    let mut p = Parser::new(tokenize(
        "iface Serial {\n\
           fn serial_getc() -> (b: i64) ensures { b >= -1 }\n\
         }",
    ));
    p.parse_program();
    let ports = p.take_ports();
    assert_eq!(ports[0].methods[0].ret_name.as_deref(), Some("b"));
    assert_eq!(
        ports[0].methods[0].ensures.len(),
        1,
        "the ensures clause is kept"
    );
}

#[test]
fn a_pub_port_is_public() {
    let mut p = Parser::new(tokenize("pub iface P { fn f() }"));
    p.parse_program();
    assert_eq!(p.take_ports()[0].vis, Visibility::Pub);
}

#[test]
fn a_program_mixes_a_port_with_ordinary_functions() {
    // A port coexists with functions in any order; the functions still land on the list.
    let mut p = Parser::new(tokenize(
        "fn main() -> i64 { 0 }\niface P { fn f() -> i64 }",
    ));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 1, "main is the only function");
    assert_eq!(funcs[0].name, "main");
    assert_eq!(p.take_ports().len(), 1, "the port is collected separately");
}

#[test]
fn parses_a_provider_with_target_and_bodied_functions() {
    // `impl Port for Target { fn .. { body } }` is one target's implementation.
    // The functions have bodies (unlike a port's signatures) and are collected off the
    // function list, like the port itself.
    let mut p = Parser::new(tokenize(
        "impl Serial for pic16 {\n\
           fn serial_init() { }\n\
           fn serial_getc() -> i64 { 0 }\n\
         }",
    ));
    let funcs = p.parse_program();
    assert!(
        funcs.is_empty(),
        "provider methods are not top-level functions, got {funcs:?}"
    );
    let provs = p.take_providers();
    assert_eq!(provs.len(), 1, "one provider parsed");
    assert_eq!(provs[0].port, "Serial", "names the port it implements");
    assert_eq!(
        provs[0].target, "pic16",
        "names the target it implements for"
    );
    let names: Vec<&str> = provs[0].methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["serial_init", "serial_getc"]);
}

#[test]
fn a_port_and_two_providers_coexist() {
    // The whole shape of RFC-0014: one interface, two target implementations, all in one
    // program, none of them polluting the ordinary function list.
    let mut p = Parser::new(tokenize(
        "iface Serial { fn putc(b: i64) }\n\
         impl Serial for x86_64_linux { fn putc(b: i64) { } }\n\
         impl Serial for pic16 { fn putc(b: i64) { } }\n\
         fn main() -> i64 { 0 }",
    ));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 1, "only main is a function");
    assert_eq!(p.take_ports().len(), 1, "one port");
    let provs = p.take_providers();
    assert_eq!(provs.len(), 2, "two providers");
    let targets: Vec<&str> = provs.iter().map(|pr| pr.target.as_str()).collect();
    assert_eq!(targets, vec!["x86_64_linux", "pic16"]);
}

#[test]
fn parses_a_use_directive() {
    // `use Port` names the interface the program depends on; it is collected off the
    // function list and resolved to a provider in a later pass.
    let mut p = Parser::new(tokenize("use Serial\nfn main() -> i64 { 0 }"));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 1, "main is the function; `use` is not one");
    let uses = p.take_uses();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].port, "Serial");
}

#[test]
fn rejects_an_unrecognised_top_level_declaration() {
    // A top-level declaration the parser does not know must be a syntax ERROR, never a
    // silent stop.
    //
    // Why this test exists, and why it is worth a panic: the top-level loop used to run
    // only `while` the next token was one it recognised, so anything else ended the loop
    // quietly and EVERYTHING AFTER IT WAS NEVER PARSED. A file using `const` (which is not
    // Sond — a named constant is `let NAME = ...`) lost every `test` block that followed,
    // and the compiler then complained that the file had no tests. The error pointed at the
    // wrong place entirely, and the real cause was three lines further up.
    //
    // This is the same failure already described in lexer.rs about a `#[no_main]` copied
    // from Rust: "every file including it lost its test blocks, and the compiler said
    // nothing". That one was closed on the lexer side, for characters that are not valid
    // Sond at all. This closes the other half: words made of ordinary letters, which the
    // lexer accepts happily and the parser then swallows.
    //
    // A compiler whose whole claim is that it rejects what it cannot prove cannot also
    // accept what it cannot read.
    let (toks, lines, cols) = crate::lexer::tokenize_located("const X = 1\ntest \"t\" { let y = 1 }");
    let mut p = Parser::with_positions(toks, lines, cols);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.parse_program()))
        .expect_err("an unknown top-level word must stop the parse, not end it quietly");
    std::panic::set_hook(prev);
    let rejection = err
        .downcast_ref::<ParsePanic>()
        .expect("a parse refusal carries a position");
    assert!(
        rejection.message.contains("unrecognised top-level declaration"),
        "{rejection:?}"
    );
    assert_eq!((rejection.line, rejection.col), (1, 1), "it points at `const`");
}

#[test]
fn a_program_that_ends_cleanly_still_parses() {
    // The guard above must not reject a perfectly normal file: the loop is allowed to end
    // when the tokens run out, which is every well-formed program.
    let mut p = Parser::new(tokenize("fn f() -> i64 { 1 }\ntest \"t\" { let y = 1 }"));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 2, "the function and the test block");
    assert!(funcs[1].is_test, "the test block survives");
}

#[test]
fn a_range_loop_accepts_a_parameter_as_its_bound() {
    // The sibling of the test below, with the bound coming in as a PARAMETER rather than a local:
    // the same missing restriction, reached by the shape a function that walks a caller's buffer
    // actually has.
    let funcs = Parser::new(tokenize(
        "fn f(n: i64) -> i64 { let mut s = 0\nfor i in 0..n { s = s + i }\ns }",
    ))
    .parse_program();
    assert_eq!(funcs.len(), 1, "the program parses at all");
}

#[test]
fn the_end_of_a_range_may_be_a_plain_variable() {
    // `for i in 0..n { ... }` — the natural way to walk a buffer whose length is only
    // known at run time, and the whole point of the counting loop over a typed view.
    //
    // It did not parse. The error was `expected Colon, got Eq`, which talks about STRUCT
    // FIELDS, and left everyone looking for a missing feature in ranges: two agents
    // concluded in turn that `for i in a..b` was unimplemented and asked for it to be
    // built. It was implemented all along. The end of the range was parsed WITHOUT the
    // `no_struct_lit` restriction that the start already sets, so `n { s = s + i }` read
    // as a struct literal `n` with a field `s`, and the loop body was swallowed as its
    // field list.
    //
    // That is why every existing test of this form passes: they all bound the range with a
    // LITERAL (`0..3`, `0..5`), and a literal cannot start a struct literal, so the missing
    // restriction was invisible. A variable is the first case that reaches it, and it is
    // also the only case anyone actually needs — nobody walks a buffer up to a constant.
    let mut p = Parser::new(tokenize(
        "fn main() -> i64 { let n = 5\nlet mut s = 0\nfor i in 0..n { s = s + i }\ns }",
    ));
    let funcs = p.parse_program();
    assert_eq!(funcs.len(), 1);
}

#[test]
fn a_struct_literal_still_parses_after_a_range_loop() {
    // The range loop suppresses struct literals while reading its bounds, because `0..n {` must
    // open the loop body. Restoring that flag to the WRONG value — an inner `saved` shadowing the
    // outer one, so it restored `true` instead of the original — left it stuck ON for the rest of
    // the parse, and every struct literal after a range loop failed with `unexpected token in
    // expression: LBrace`. The fix for one form broke another, which is what this pins.
    let funcs = Parser::new(tokenize(
        "struct P { a: i64, b: i64 }\n\
         fn f(n: i64) -> i64 { let mut s = 0\nfor i in 0..n { s = s + i }\n\
         let p = P { a: 1, b: 2 }\np.a + s }",
    ))
    .parse_program();
    assert_eq!(
        funcs.len(),
        1,
        "a struct literal after a range loop must still parse"
    );
}

// ---------------------------------------------------------------------------
// BUG FIX: stmt_lines must always match stmts after the parser runs.
// These tests establish that no statement ever enters stmts without a
// synchronized line number.  The Block methods (push_stmt, insert_stmt)
// are the only way the parser mutates these vectors.
// ---------------------------------------------------------------------------

#[test]
fn stmt_lines_len_equals_stmts_len_after_parse_function_body() {
    let src = "fn f() -> i64 {\n\
               let a[3]\n\
               a[0] = 1\n\
               for x in a {\n\
               s = s + x\n\
               }\n\
               for i in 0..3 {\n\
               if 1 == 1 { continue }\n\
               s = s + noexiste\n\
               }\n\
               0\n\
               }";
    let (toks, lines, _) = crate::lexer::tokenize_located(src);
    let mut p = Parser::with_lines(toks, lines);
    let funcs = p.parse_program();
    for (i, f) in funcs.iter().enumerate() {
        let tail_count = if f.body.tail.is_some() { 1 } else { 0 };
        let expected_lines = f.body.stmts.len() + tail_count;
        assert_eq!(
            f.body.stmt_lines.len(),
            expected_lines,
            "function #{i}: stmt_lines.len ({}) must equal stmts.len ({}) + tail ({}); got stmt_lines={:?}",
            f.body.stmt_lines.len(),
            f.body.stmts.len(),
            tail_count,
            f.body.stmt_lines
        );
        check_block_invariants(&f.body, i);
    }
}

fn check_block_invariants(b: &Block, ctx: usize) {
    let expected_lines = b.stmts.len() + if b.tail.is_some() { 1 } else { 0 };
    assert_eq!(
        b.stmt_lines.len(),
        expected_lines,
        "block #{ctx}: stmt_lines.len ({}) must equal stmts.len ({}) + tail ({}); got {:?}",
        b.stmt_lines.len(),
        b.stmts.len(),
        if b.tail.is_some() { 1 } else { 0 },
        b.stmt_lines
    );
    if b.stmt_lines.len() == b.stmts.len() {
        for (i, stmt) in b.stmts.iter().enumerate() {
            let _line = b.stmt_lines[i];
            if let Stmt::For { body, .. } = stmt {
                check_block_invariants(body, i);
            }
        }
    }
}

/// Nothing outside `ast.rs` may push or insert into a `Block`'s `stmts` directly.
///
/// `stmts` and `stmt_lines` are parallel BY INDEX. A direct `stmts.insert(0, ..)` in the `for`
/// desugaring shifted every line by one without touching `stmt_lines`, and the error for a bad
/// name inside a loop body came out pointing at the `for` line instead of at the offending one.
/// `push_stmt` / `insert_stmt` are the only entry points that keep the two in step.
///
/// The guard walks the source TREE rather than a list of files: a list is a thing to maintain,
/// and the file that reintroduces this will be one nobody remembered to add to it.
#[test]
fn nothing_outside_ast_touches_a_block_s_statements_directly() {
    // Built at run time so this very file does not contain the string it looks for: written out
    // in full, the guard found ITSELF and failed on a clean tree -- the same way `pgrep -f` finds
    // its own command line.
    let empujar = format!(".stmts{}", ".push(");
    let meter = format!(".stmts{}", ".insert(");
    fn recorrer(dir: &std::path::Path, culpables: &mut Vec<String>, empujar: &str, meter: &str) {
        for e in std::fs::read_dir(dir).expect("the source tree must be readable") {
            let p = e.expect("each entry must be readable").path();
            if p.is_dir() {
                recorrer(&p, culpables, empujar, meter);
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            if p.file_name().and_then(|s| s.to_str()) == Some("ast.rs") {
                continue; // where push_stmt / insert_stmt live
            }
            let texto = std::fs::read_to_string(&p).expect("each source file must be readable");
            for (n, l) in texto.lines().enumerate() {
                let code = l.split("//").next().unwrap_or("");
                if code.contains(empujar) || code.contains(meter) {
                    culpables.push(format!("{}:{}", p.display(), n + 1));
                }
            }
        }
    }
    let raiz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut culpables = Vec::new();
    recorrer(&raiz, &mut culpables, &empujar, &meter);
    assert!(
        culpables.is_empty(),
        "these push or insert into a Block's `stmts` without its line, which desynchronises \
         `stmt_lines` and makes error messages point at the wrong line -- use `push_stmt` / \
         `insert_stmt`: {culpables:?}"
    );
}

// ---------------------------------------------------------------------------
// for i in a..b: bounds that MUST be lifted (function call + body mutation)
// ---------------------------------------------------------------------------

/// The helper extracts the inner For loop from the desugared function body.
fn for_body_and_cond(f: &Function) -> Option<(&crate::ast::Block, &crate::ast::Expr)> {
    let for_stmt = f
        .body
        .stmts
        .iter()
        .find(|s| matches!(s, crate::ast::Stmt::For { .. }))?;
    match for_stmt {
        crate::ast::Stmt::For { body, cond, .. } => cond.as_ref().map(|c| (body, c)),
        _ => None,
    }
}

fn for_cond_right_var(f: &Function) -> Option<String> {
    let (_, cond) = for_body_and_cond(f)?;
    if let crate::ast::Expr::Binary(_, _, right) = cond
        && let crate::ast::Expr::Var(v) = right.as_ref() {
            return Some(v.clone());
        }
    None
}

/// Does a let-binding for the loop counter appear in the function body?
fn has_counter_let(f: &Function, var: &str) -> bool {
    f.body.stmts.iter().any(|s| {
        matches!(
            s,
            crate::ast::Stmt::Let { name, .. } if name == var
        )
    })
}

/// Returns the name of the lifted bound variable (`__endN`) if the bound
/// was materialised into a local, or `None` if the bound is used directly.
fn captured_end_name(f: &Function) -> Option<String> {
    for s in &f.body.stmts {
        if let crate::ast::Stmt::Let { name, .. } = s
            && name.starts_with("__end") {
                return Some(name.clone());
            }
    }
    None
}

#[test]
fn bound_with_function_call_is_lifted() {
    // `for i in 0..tope()` → a function call in the bound must be evaluated once.
    // The parser lifts it to `let mut __endN = tope()` and uses `__endN` in the
    // condition, so the call happens exactly once instead of once per iteration.
    let funcs = Parser::new(tokenize(
        "fn tope() -> i64 { 5 }\n\
         fn main() -> i64 { let mut s = 0\nfor i in 0..tope() { s = s + i }\ns }",
    ))
    .parse_program();
    let f = funcs.iter().find(|f| f.name == "main").expect("must find main");
    // Must produce: let mut i = 0; (counter let)
    assert!(has_counter_let(f, "i"), "counter must be declared");
    // Must produce: let mut __endN = tope();  (lifted bound)
    let end_name = captured_end_name(f);
    assert!(
        end_name.is_some(),
        "function-call bound must be lifted to a local variable"
    );
    // The for loop's condition must reference the lifted name, not the call.
    assert_eq!(
        for_cond_right_var(f),
        end_name,
        "for condition must reference the lifted bound"
    );
}

#[test]
fn bound_with_body_assignment_is_lifted() {
    // `for i in 0..n { n = n + 1 ... }` → the body assigns to a name that appears
    // in the bound. Without lifting, the bound would change each iteration, causing
    // an infinite loop. The parser lifts to `let mut __endN = n` before the loop.
    let funcs = Parser::new(tokenize(
        "fn main() -> i64 { let mut n = 5\nlet mut s = 0\nfor i in 0..n { n = n + 1; s = s + i }\ns }",
    ))
    .parse_program();
    let f = &funcs[0];
    assert!(has_counter_let(f, "i"), "counter must be declared");
    let end_name = captured_end_name(f);
    assert!(
        end_name.is_some(),
        "bound that references a body-assigned variable must be lifted"
    );
    // The condition must reference the lifted name, not the original var.
    assert_eq!(
        for_cond_right_var(f),
        end_name,
        "for condition must reference the lifted bound"
    );
}

#[test]
fn stable_variable_bound_is_not_lifted() {
    // `for i in 0..n { s = s + i }` → a bare variable `n` in the bound, with
    // no call in the bound and no body assignment to `n`. The bound should NOT
    // be lifted, so the prover can still reason about `n` (e.g. `i < n` for
    // bounds-checking).
    let funcs = Parser::new(tokenize(
        "fn main(n: i64) -> i64 { let mut s = 0\nfor i in 0..n { s = s + i }\ns }",
    ))
    .parse_program();
    let f = &funcs[0];
    assert!(has_counter_let(f, "i"), "counter must be declared");
    // No __endN let-binding — the bound stays as-is.
    let end_name = captured_end_name(f);
    assert!(
        end_name.is_none(),
        "stable variable bound must NOT be lifted"
    );
    // The condition is `i < n` directly.
    assert_eq!(
        for_cond_right_var(f),
        Some("n".to_string()),
        "stable bound must stay as the original variable"
    );
}

#[test]
fn a_bound_that_is_not_a_plain_name_is_never_stable() {
    // Anything with a call, or any computed expression, has to be lifted: the parser cannot know
    // that evaluating it twice gives the same answer, and the whole bug was evaluating it N+1
    // times. Only a literal or a plain name/field can stay in the condition, where the prover
    // still sees it by name.
    let vacio = Block::default();
    assert!(!end_is_stable(&parse_e("f(g())"), &vacio), "a nested call must be lifted");
    assert!(!end_is_stable(&parse_e("f(a) + 1"), &vacio), "a call inside a sum must be lifted");
    assert!(!end_is_stable(&parse_e("a + b"), &vacio), "a computed bound must be lifted");
    assert!(end_is_stable(&parse_e("4"), &vacio), "a literal never changes");
    assert!(end_is_stable(&parse_e("a.len"), &vacio), "a field nobody assigns to stays in place");
}

#[test]
fn a_bound_variable_the_body_assigns_to_is_seen_however_deep_the_assignment_is() {
    // The first fix only looked at the body's top-level statements, so moving the very same
    // assignment inside an `if` brought the infinite loop straight back: measured 51 turns where
    // 4 were due.
    let plano = parse_fn("fn f(n: i64) -> i64 { n = 5\nn }");
    assert!(block_assigns_path(&plano.body, "n"), "a top-level assignment must be seen");

    let anidado = parse_fn("fn f(n: i64) -> i64 { if n > 0 { n = 5 }\nn }");
    assert!(block_assigns_path(&anidado.body, "n"), "an assignment inside an `if` must be seen");

    let en_bucle = parse_fn("fn f(n: i64) -> i64 { for j in 0..2 { n = n + 1 }\nn }");
    assert!(block_assigns_path(&en_bucle.body, "n"), "an assignment inside a loop must be seen");

    let solo_lee = parse_fn("fn f(n: i64) -> i64 { let x = n\nx }");
    assert!(!block_assigns_path(&solo_lee.body, "n"), "reading a bound variable is not assigning");

    // A prefix of the bound's path counts: replacing the whole slice moves its `len` with it.
    let reemplaza = parse_fn("fn f(s: Slice, u: Slice) -> i64 { s = u\n0 }");
    assert!(block_assigns_path(&reemplaza.body, "s.len"), "assigning `s` disturbs `s.len`");
    // Writing one field leaves the others alone: a bound on `s.cap` survives `s.len = 1`.
    let un_campo = parse_fn("fn f(s: Slice) -> i64 { s.len = 1\n0 }");
    assert!(block_assigns_path(&un_campo.body, "s.len"), "the field written is disturbed");
    assert!(!block_assigns_path(&un_campo.body, "s.cap"), "a sibling field is not");
}

#[test]
fn writing_into_an_element_does_not_lift_the_length_bound() {
    // `for i in 0..s.len { s[i] = 0 }` is the single most common loop in the language, and the
    // prover clears the index by matching `i < s.len` BY NAME. Lifting that bound to `__end0`
    // would take the name away and start rejecting programs that compile today.
    //
    // Writing into an element is not writing to the name: `s[i] = 0` changes neither `s` nor
    // `s.len`. Only an assignment to the bound's own path -- or to a prefix of it, since `s = t`
    // moves `s.len` with it -- may lift.
    let escribe_elemento = Parser::new(tokenize(
        "fn main(s: Slice) -> i64 { let mut t = 0\nfor i in 0..s.len { s[i] = 0 }\nt }",
    ))
    .parse_program();
    assert!(
        captured_end_name(&escribe_elemento[0]).is_none(),
        "writing into an element must leave the `s.len` bound where the prover can see it"
    );

    let escribe_longitud = Parser::new(tokenize(
        "fn main(s: Slice) -> i64 { let mut t = 0\nfor i in 0..s.len { s.len = s.len + 1 }\nt }",
    ))
    .parse_program();
    assert!(
        captured_end_name(&escribe_longitud[0]).is_some(),
        "a body that raises the bound itself must have it evaluated once"
    );

    let reasigna_base = Parser::new(tokenize(
        "fn main(s: Slice, u: Slice) -> i64 { let mut t = 0\nfor i in 0..s.len { s = u }\nt }",
    ))
    .parse_program();
    assert!(
        captured_end_name(&reasigna_base[0]).is_some(),
        "assigning the whole slice moves its `len` with it, so the bound must be lifted"
    );
}

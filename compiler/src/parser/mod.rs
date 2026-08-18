//! The Sond parser: turns a token stream into an AST.
//!
//! Grown with strict TDD. Only the productions exercised by a test exist.

use crate::ast::{
    Arg, BinOp, Block, EnumDef, EnumVariant, Expr, Function, Global, MatchArm, Param, PortDef,
    ProviderDef, Signature, Stmt, StructDef, Type, UnOp, UseDecl, Visibility,
};
use crate::token::Token;
use std::collections::HashMap;

/// A recursive-descent parser over a flat token stream.
pub struct Parser {
    tokens: Vec<Token>,
    /// The source line of each token (parallel to `tokens`), or empty when the parser was built
    /// from tokens alone. Used to stamp each statement with its line for the `--debug` checkpoint.
    lines: Vec<u32>,
    pos: usize,
    /// Module-level `let` bindings collected while parsing the program.
    globals: Vec<Global>,
    /// Top-level `struct` declarations collected while parsing the program.
    structs: Vec<StructDef>,
    /// Top-level `enum` (sum type) declarations collected while parsing the program.
    enums: Vec<EnumDef>,
    /// Top-level `port` (interface) declarations collected while parsing the program.
    ports: Vec<PortDef>,
    /// `extern fn` declarations collected while parsing the program.
    externs: Vec<Signature>,
    /// The source text of each `string` binding, so `s.utf8` can hand over the very bytes the
    /// literal was written with instead of re-encoding the characters.
    string_bytes: HashMap<String, String>,
    /// Byte count of the `.utf8` just parsed, to type the binding it initializes.
    utf8_len: Option<usize>,
    /// Top-level `provides` (per-target implementations) collected while parsing.
    providers: Vec<ProviderDef>,
    /// Top-level `use Port` directives collected while parsing.
    uses: Vec<UseDecl>,
    /// While true, `Name { ... }` is NOT parsed as a struct literal, set when
    /// parsing an `if`/`for` condition so `if x { }` stays condition + block.
    no_struct_lit: bool,
}

/// Flattens a `&&` chain into its conjuncts, appending each to `out`; a non-`&&` expression
/// contributes itself. Lets a `where a && b` refinement enter `requires` as two single
/// comparisons, which is what the linear prover can reason about.
fn split_conjuncts(e: Expr, out: &mut Vec<Expr>) {
    if let Expr::Binary(BinOp::And, l, r) = e {
        split_conjuncts(*l, out);
        split_conjuncts(*r, out);
    } else {
        out.push(e);
    }
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser::with_lines(tokens, Vec::new())
    }

    /// Builds a parser that also knows each token's source line (from `tokenize_located`), so the
    /// blocks it parses carry per-statement line info for the `--debug` checkpoint.
    pub fn with_lines(tokens: Vec<Token>, lines: Vec<u32>) -> Self {
        Parser {
            tokens,
            lines,
            pos: 0,
            globals: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            ports: Vec::new(),
            externs: Vec::new(),
            string_bytes: HashMap::new(),
            utf8_len: None,
            providers: Vec::new(),
            uses: Vec::new(),
            no_struct_lit: false,
        }
    }

    /// The source line of the current token, or 0 if line info is absent.
    fn cur_line(&self) -> u32 {
        self.lines.get(self.pos).copied().unwrap_or(0)
    }

    /// Parses a whole program: top-level `fn` declarations, `let` globals and
    /// `struct` types in any order. Functions are returned; globals via
    /// `take_globals` and structs via `take_structs`.
    pub fn parse_program(&mut self) -> Vec<Function> {
        let mut funcs = Vec::new();
        self.skip_semicolons();
        while matches!(
            self.peek(),
            Some(
                Token::Fn
                    | Token::Interrupt
                    | Token::Wrapping
                    | Token::Let
                    | Token::Struct
                    | Token::Enum
                    | Token::Iface
                    | Token::Use
                    | Token::Pub
                    | Token::Priv
                    | Token::Impl
                    | Token::Test
            )
        ) || (matches!(self.peek(), Some(Token::Ident(k)) if k == "extern")
            && self.tokens.get(self.pos + 1) == Some(&Token::Fn))
        {
            // `impl` opens one of two blocks, told apart by a `for` after the name:
            //   `impl Iface for Target { fn .. }`  a per-target interface implementation
            //                                      (RFC-0014, the old `provides`), collected as
            //                                      a provider and resolved by the active target.
            //   `impl Type { fn m(self, ..) .. }`  inherent methods: each becomes a free function
            //                                      `Type_m` with `self` typed as the struct;
            //                                      `p.m(a)` lowers to `Type_m(p, a)` by desugar.
            // `extern fn name(params) -> T`: a function this program CALLS but does not define,
            // resolved by the linker. `extern` is a contextual keyword (a plain identifier
            // elsewhere), recognized only here, directly before `fn`.
            if matches!(self.peek(), Some(Token::Ident(k)) if k == "extern")
                && self.tokens.get(self.pos + 1) == Some(&Token::Fn)
            {
                self.advance(); // `extern`
                let sig = self.parse_signature();
                self.externs.push(sig);
                self.skip_semicolons();
                continue;
            }
            if self.peek() == Some(&Token::Impl) {
                if self.tokens.get(self.pos + 2) == Some(&Token::For) {
                    let p = self.parse_provides();
                    self.providers.push(p);
                } else {
                    for method in self.parse_impl() {
                        funcs.push(method);
                    }
                }
                self.skip_semicolons();
                continue;
            }
            // Determine the declaration kind, looking past an optional `pub`/`priv`.
            let kind = if matches!(self.peek(), Some(Token::Pub | Token::Priv)) {
                self.tokens.get(self.pos + 1)
            } else {
                self.peek()
                //
                // NOTE: the loop below this one ends when the next token is not a known
                // declaration. Falling out of it with tokens still pending is a SYNTAX
                // ERROR and is reported after the loop — see the check at its end. It used
                // to end quietly, and everything after the offending line was never parsed.
            };
            match kind {
                // `fn` and `struct` consume their own leading visibility.
                Some(Token::Fn | Token::Interrupt | Token::Wrapping) => {
                    funcs.push(self.parse_function())
                }
                Some(Token::Struct) => {
                    let s = self.parse_struct();
                    self.structs.push(s);
                }
                Some(Token::Enum) => {
                    let e = self.parse_enum();
                    self.enums.push(e);
                }
                Some(Token::Iface) => {
                    let p = self.parse_iface();
                    self.ports.push(p);
                }
                Some(Token::Use) => {
                    let u = self.parse_use();
                    self.uses.push(u);
                }
                Some(Token::Test) => {
                    let t = self.parse_test(funcs.len());
                    funcs.push(t);
                }
                // A global `let`: consume the visibility here, then the binding.
                _ => {
                    let vis = self.parse_visibility();
                    let Stmt::Let {
                        mutable,
                        name,
                        value,
                        array_len,
                        ty,
                    } = self.parse_let()
                    else {
                        unreachable!("parse_let returns a Let")
                    };
                    self.globals.push(Global {
                        vis,
                        mutable,
                        name,
                        array_len,
                        ty,
                        init: value,
                    });
                }
            }
            self.skip_semicolons();
        }
        // Falling out of the loop with tokens left means the next thing is not a
        // declaration this parser knows. That is a SYNTAX ERROR, and saying so is the whole
        // point: the loop used to end here quietly and EVERYTHING AFTER THIS POINT WAS
        // NEVER PARSED. A file with `const NAME = ...` (which is not Sond — a named constant
        // is `let NAME = ...`) silently lost every `test` block that followed it, and the
        // compiler then reported that the file had no tests at all. The message pointed
        // nowhere near the cause.
        //
        // Same failure as the `#[no_main]` case described in lexer.rs, on the other side of
        // the fence: that one was closed for characters that cannot be Sond, this one for
        // words made of ordinary letters that the lexer accepts and the parser cannot use.
        if let Some(tok) = self.peek() {
            panic!(
                "unrecognised top-level declaration at line {}: {:?}. \
                 Expected one of: fn, let, struct, enum, iface, use, impl, test \
                 (a named constant is `let NAME = ...`, Sond has no `const`).",
                self.cur_line(),
                tok
            );
        }
        funcs
    }

    /// Eats an optional `pub`/`priv` prefix and returns the visibility. Absent =
    /// `Priv` (private by default).
    fn parse_visibility(&mut self) -> Visibility {
        if self.eat(Token::Pub) {
            Visibility::Pub
        } else {
            self.eat(Token::Priv);
            Visibility::Priv
        }
    }

    /// The module-level globals collected during `parse_program`.
    pub fn take_globals(&mut self) -> Vec<Global> {
        std::mem::take(&mut self.globals)
    }

    /// The top-level struct declarations collected during `parse_program`.
    pub fn take_structs(&mut self) -> Vec<StructDef> {
        std::mem::take(&mut self.structs)
    }

    /// The top-level enum (sum type) declarations collected during `parse_program`.
    pub fn take_enums(&mut self) -> Vec<EnumDef> {
        std::mem::take(&mut self.enums)
    }

    /// The top-level `iface` (interface) declarations collected during `parse_program`.
    /// The `extern fn` declarations collected while parsing: functions the program calls but
    /// does not define, whose calls follow the platform's C ABI rather than Sond's own.
    pub fn take_externs(&mut self) -> Vec<Signature> {
        std::mem::take(&mut self.externs)
    }

    pub fn take_ports(&mut self) -> Vec<PortDef> {
        std::mem::take(&mut self.ports)
    }

    /// The top-level `impl Iface for Target` (per-target implementations) collected during
    /// `parse_program`.
    pub fn take_providers(&mut self) -> Vec<ProviderDef> {
        std::mem::take(&mut self.providers)
    }

    /// The top-level `use Iface` directives collected during `parse_program`.
    pub fn take_uses(&mut self) -> Vec<UseDecl> {
        std::mem::take(&mut self.uses)
    }

    /// Parses `enum Name { A, B(T), C }`: a sum type; each variant is a bare tag or
    /// carries a single payload type in parentheses (`Some(i64)`). Trailing comma allowed.
    fn parse_enum(&mut self) -> EnumDef {
        let vis = self.parse_visibility();
        self.expect(Token::Enum);
        let name = self.expect_ident();
        self.expect(Token::LBrace);
        let mut variants = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            let vname = self.expect_ident();
            // Optional single payload: `Some(i64)`. A bare variant has none.
            let payload = if self.eat(Token::LParen) {
                let ty = self.parse_type();
                self.expect(Token::RParen);
                Some(ty)
            } else {
                None
            };
            variants.push(EnumVariant {
                name: vname,
                payload,
            });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RBrace);
        EnumDef {
            vis,
            name,
            variants,
        }
    }

    /// Parses `struct Name { field: Type, ... }`: a record type. Trailing comma
    /// allowed; fields reuse the `name: Type` shape of a parameter.
    fn parse_struct(&mut self) -> StructDef {
        let vis = self.parse_visibility();
        self.expect(Token::Struct);
        let name = self.expect_ident();
        self.expect(Token::LBrace);
        let mut fields = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            let fname = self.expect_ident();
            self.expect(Token::Colon);
            let ty = self.parse_type();
            fields.push(Param { name: fname, ty });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RBrace);
        StructDef { vis, name, fields }
    }

    /// Parses a single function declaration, including its optional named
    /// return, `requires`/`ensures` contracts and `effects` clause.
    /// Parses `impl TypeName { fn m(self, ..) { .. } .. }`. Each method becomes a
    /// free function named `TypeName_m`, with the `self` receiver typed as the
    /// struct (passed by reference). Method CALLS `p.m(a)` are lowered to
    /// `TypeName_m(p, a)` during struct desugaring, a minimal OOP over the flat
    /// namespace, with no new codegen.
    fn parse_impl(&mut self) -> Vec<Function> {
        self.expect(Token::Impl);
        let ty = self.expect_ident();
        self.expect(Token::LBrace);
        self.skip_semicolons();
        let mut methods = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            let mut f = self.parse_function();
            // The receiver `self` takes the impl struct's type. A bare `self` is a
            // VALUE receiver (self: Point); `self: *Point` is a POINTER receiver
            // (may mutate). Normalize the pointee to the impl type either way.
            if f.params.first().is_some_and(|r| r.name == "self") {
                f.params[0].ty = match &f.params[0].ty {
                    Type::Ptr(_) => Type::Ptr(Box::new(Type::Named(ty.clone()))),
                    _ => Type::Named(ty.clone()),
                };
            }
            f.name = format!("{ty}_{}", f.name); // mangle into the flat namespace
            methods.push(f);
            self.skip_semicolons();
        }
        self.expect(Token::RBrace);
        methods
    }

    pub fn parse_function(&mut self) -> Function {
        let vis = self.parse_visibility();
        let is_wrapping = self.eat(Token::Wrapping);
        let is_interrupt = self.eat(Token::Interrupt);
        self.expect(Token::Fn);
        let name = self.expect_ident();
        let sig = self.parse_sig_tail(vis, name);
        let body = self.parse_block();

        Function {
            vis: sig.vis,
            name: sig.name,
            params: sig.params,
            ret_name: sig.ret_name,
            ret: sig.ret,
            requires: sig.requires,
            ensures: sig.ensures,
            effects: sig.effects,
            is_interrupt,
            is_wrapping,
            is_test: false,
            test_name: None,
            body,
        }
    }

    /// Parses everything of a function head AFTER `fn NAME`: the parameter list,
    /// return type, `requires`/`ensures` contracts and `effects` clause, into a
    /// [`Signature`]. Shared by `parse_function` (which then parses a body) and
    /// `parse_signature` (a port method, which has none), so the head is defined once.
    fn parse_sig_tail(&mut self, vis: Visibility, name: String) -> Signature {
        self.expect(Token::LParen);
        let (params, param_refinements) = self.parse_params();
        self.expect(Token::RParen);

        // Return type: `-> Type` or named `-> (name: Type)`.
        let (ret_name, ret) = if self.eat(Token::Arrow) {
            if self.eat(Token::LParen) {
                let n = self.expect_ident();
                self.expect(Token::Colon);
                let t = self.parse_type();
                self.expect(Token::RParen);
                (Some(n), Some(t))
            } else {
                (None, Some(self.parse_type()))
            }
        } else {
            (None, None)
        };

        // Implicit `;` may follow the signature line; the contract clauses and
        // the effects clause may each be separated by them.
        self.skip_semicolons();
        let mut requires = if self.eat(Token::Requires) {
            self.parse_prop_block()
        } else {
            vec![]
        };
        // Parameter `where` refinements are preconditions too, fold them in so verification and
        // call-site checking treat them identically to a `requires` clause.
        requires.extend(param_refinements);

        self.skip_semicolons();
        let ensures = if self.eat(Token::Ensures) {
            self.parse_prop_block()
        } else {
            vec![]
        };

        self.skip_semicolons();
        let effects = if self.eat(Token::Effects) {
            self.parse_effect_list()
        } else {
            vec![]
        };

        Signature {
            vis,
            name,
            params,
            ret_name,
            ret,
            requires,
            ensures,
            effects,
        }
    }

    /// Parses a single port-method signature: `(pub)? fn NAME(params) -> ret
    /// requires.. ensures.. effects..`, with NO body. Reuses `parse_sig_tail`.
    fn parse_signature(&mut self) -> Signature {
        let vis = self.parse_visibility();
        self.expect(Token::Fn);
        let name = self.expect_ident();
        self.parse_sig_tail(vis, name)
    }

    /// Parses `(pub)? port Name { <signatures> }`: a named interface (RFC-0014). The
    /// body is a sequence of bodyless `fn` signatures; implicit `;` between them is
    /// skipped. Providers and `use` resolution are later slices.
    fn parse_iface(&mut self) -> PortDef {
        let vis = self.parse_visibility();
        self.expect(Token::Iface);
        let name = self.expect_ident();
        self.expect(Token::LBrace);
        let mut methods = Vec::new();
        self.skip_semicolons();
        while self.peek() != Some(&Token::RBrace) {
            methods.push(self.parse_signature());
            self.skip_semicolons();
        }
        self.expect(Token::RBrace);
        PortDef { vis, name, methods }
    }

    /// Parses `impl Iface for Target { <functions> }`: one target's implementation of an
    /// interface (RFC-0014). The body is ordinary functions (with bodies); `Target` is a
    /// target-profile identifier. Told apart from an inherent `impl Type { .. }` by the `for`.
    /// Matching methods to the interface and resolving `use` happen later; this only parses
    /// the block. (Internally still a `ProviderDef`: the concept is a port and its provider.)
    fn parse_provides(&mut self) -> ProviderDef {
        self.expect(Token::Impl);
        let port = self.expect_ident();
        self.expect(Token::For);
        let target = self.expect_ident();
        self.expect(Token::LBrace);
        let mut methods = Vec::new();
        self.skip_semicolons();
        while self.peek() != Some(&Token::RBrace) {
            methods.push(self.parse_function());
            self.skip_semicolons();
        }
        self.expect(Token::RBrace);
        ProviderDef {
            port,
            target,
            methods,
        }
    }

    /// Parses `use Port`: a dependency on a port's interface (RFC-0014). Resolution
    /// to the active target's provider is a later slice; this parses the directive.
    fn parse_use(&mut self) -> UseDecl {
        self.expect(Token::Use);
        let port = self.expect_ident();
        UseDecl { port }
    }

    /// Parses a `{ ... }` block: a sequence of statements plus an optional tail
    /// expression (the block's value). See the design note on `ast::Block`: the
    /// last bare expression before `}` is the tail, despite the implicit `;`.
    /// Parses `test "name" { .. }` into a niladic function marked `is_test`, keeping the
    /// readable name for the runner. `idx` makes the generated symbol unique across a file.
    fn parse_test(&mut self, idx: usize) -> Function {
        self.expect(Token::Test);
        let name = match self.advance() {
            Token::Str(s) => s,
            other => panic!("expected a test name string after `test`, got {other:?}"),
        };
        let body = self.parse_block();
        Function {
            name: format!("__test_{idx}"),
            is_test: true,
            test_name: Some(name),
            body,
            ..Default::default()
        }
    }

    fn parse_block(&mut self) -> Block {
        self.expect(Token::LBrace);
        self.skip_semicolons();
        let mut stmts = Vec::new();
        let mut stmt_lines = Vec::new();
        let mut tail = None;

        while self.peek() != Some(&Token::RBrace) {
            // The line this statement starts on, stamped after it is pushed (an iteration parses at
            // most one statement, or the tail expression, which is not stamped).
            let stmt_line = self.cur_line();
            match self.peek() {
                Some(Token::Let) => {
                    stmts.push(self.parse_let());
                    self.skip_semicolons();
                }
                Some(Token::Return) => {
                    stmts.push(self.parse_return());
                    self.skip_semicolons();
                }
                Some(Token::For | Token::While) => {
                    // `while` is a synonym for the conditional `for`; bare `for {}` loops forever.
                    self.advance();
                    // Three-part form `for let mut i = 0; i < n; i++ { body }`: the shape every
                    // C-family writer reaches for. Sugar for what this parser already builds for
                    // the counting form — init before the loop, step at the end of the body — so
                    // it is the same loop by the time anything else sees it, bounds checker
                    // included.
                    if self.peek() == Some(&Token::Let) {
                        let init = self.parse_let();
                        self.expect(Token::Semicolon);
                        let saved = self.no_struct_lit;
                        self.no_struct_lit = true;
                        let cond = self.parse_expr();
                        self.no_struct_lit = saved;
                        self.expect(Token::Semicolon);
                        // The step: `i++` (two `+` tokens, no `++` in the lexer) or any
                        // assignment, so `i = i + 2` reads the same way.
                        let step = if matches!(self.peek(), Some(Token::Ident(_)))
                            && self.tokens.get(self.pos + 1) == Some(&Token::Plus)
                            && self.tokens.get(self.pos + 2) == Some(&Token::Plus)
                        {
                            let var = self.expect_ident();
                            self.advance();
                            self.advance();
                            assign_inc(&var)
                        } else {
                            // Any assignment, so `i = i + 2` reads the same way.
                            let target = self.parse_expr();
                            self.expect(Token::Eq);
                            let value = self.parse_expr();
                            Stmt::Assign { target, value }
                        };
                        let mut body = self.parse_block();
                        sink_tail(&mut body); // the step runs AFTER the body, not before
                        step_before_continues(&mut body, &step); // ...and before every `continue`
                        body.stmts.push(step);
                        stmts.push(init);
                        stmts.push(Stmt::For {
                            cond: Some(cond),
                            body,
                            invariant: Vec::new(),
                        });
                        self.skip_semicolons();
                        continue;
                    }
                    // Counting form `for i in a..b { body }`: declare `i = a`, loop
                    // while `i < b`, auto-increment. Desugar to a `let mut i` plus a
                    // `for i < b` whose body ends with `i = i + 1`.
                    if matches!(self.peek(), Some(Token::Ident(_)))
                        && self.tokens.get(self.pos + 1) == Some(&Token::In)
                    {
                        let var = self.expect_ident();
                        self.expect(Token::In);
                        // The range start / collection: a following `{` is the loop body,
                        // not a struct literal.
                        let saved = self.no_struct_lit;
                        self.no_struct_lit = true;
                        let first = self.parse_expr();
                        if self.eat(Token::DotDot) {
                            // Counting form `for i in a..b`: i = a; while i < b; i += 1.
                            // The end needs the same restriction as the start: in `for i in
                            // 0..n { ... }` the `{` opens the loop body, so a bare `n` must not
                            // read as the name of a struct literal whose fields are the body.
                            // Every existing test bound the range with a LITERAL, which cannot
                            // start a struct literal, so the gap was invisible until a variable
                            // reached it — and a variable is the only bound anyone really uses.
                            //
                            // The flag is ALREADY suppressed here (set before the start was
                            // parsed) and is restored once, below, from the OUTER `saved`. An
                            // inner `let saved` here shadowed it, captured `true`, and restored
                            // `true` — leaving struct literals suppressed for the rest of the
                            // file.
                            let end = self.parse_expr();
                            self.no_struct_lit = saved;
                            let mut body = self.parse_block();
                            stmts.push(Stmt::Let {
                                mutable: true,
                                name: var.clone(),
                                ty: None,
                                value: first,
                                array_len: None,
                            });
                            sink_tail(&mut body); // increment must run AFTER the body, not before
                            // ...and before every `continue`, which jumps past the end of the
                            // body: without this the iteration that continues never advances `i`
                            // and the loop spins for ever.
                            let inc = assign_inc(&var);
                            step_before_continues(&mut body, &inc);
                            body.stmts.push(inc);
                            stmts.push(Stmt::For {
                                cond: Some(Expr::Binary(
                                    BinOp::Lt,
                                    Box::new(Expr::Var(var)),
                                    Box::new(end),
                                )),
                                invariant: Vec::new(),
                                body,
                            });
                        } else {
                            // Foreach `for x in coll { body }`: iterate a slice/array's elements.
                            // `x` is bound to `coll[i]` each turn; `i` is a hidden counter,
                            // named by token position so nested loops never collide.
                            let idx = format!("__fe{}", self.pos);
                            let mut body = self.parse_block();
                            body.stmts.insert(
                                0,
                                Stmt::Let {
                                    mutable: false,
                                    name: var,
                                    ty: None,
                                    value: Expr::Index(
                                        Box::new(first.clone()),
                                        Box::new(Expr::Var(idx.clone())),
                                    ),
                                    array_len: None,
                                },
                            );
                            sink_tail(&mut body);
                            body.stmts.push(assign_inc(&idx));
                            stmts.push(Stmt::Let {
                                mutable: true,
                                name: idx.clone(),
                                ty: None,
                                value: Expr::Int(0),
                                array_len: None,
                            });
                            stmts.push(Stmt::For {
                                cond: Some(Expr::Binary(
                                    BinOp::Lt,
                                    Box::new(Expr::Var(idx)),
                                    Box::new(Expr::Field(Box::new(first), "len".to_string())),
                                )),
                                invariant: Vec::new(),
                                body,
                            });
                        }
                        self.skip_semicolons();
                    } else {
                        // No condition when the body (`{`) or an `invariant` clause follows `for`
                        // directly, an infinite loop, optionally carrying an invariant.
                        let cond = if matches!(self.peek(), Some(Token::LBrace | Token::Invariant))
                        {
                            None
                        } else {
                            let saved = self.no_struct_lit;
                            self.no_struct_lit = true;
                            let c = self.parse_expr();
                            self.no_struct_lit = saved;
                            Some(c)
                        };
                        // Optional `invariant { p1; p2 }` before the loop body, what the
                        // prover assumes holds before and after every iteration.
                        let invariant = if self.eat(Token::Invariant) {
                            self.parse_prop_block()
                        } else {
                            Vec::new()
                        };
                        let body = self.parse_block();
                        stmts.push(Stmt::For {
                            cond,
                            invariant,
                            body,
                        });
                        self.skip_semicolons();
                    }
                }
                Some(Token::Break) => {
                    self.advance();
                    stmts.push(Stmt::Break);
                    self.skip_semicolons();
                }
                Some(Token::Continue) => {
                    self.advance();
                    stmts.push(Stmt::Continue);
                    self.skip_semicolons();
                }
                Some(Token::Spawn) => {
                    // `spawn f(args)`: run the call on a new thread. The inner expression is
                    // parsed as an ordinary expression, but it must be a call, nothing else
                    // is meaningful to run on a thread.
                    self.advance();
                    let call = self.parse_expr();
                    stmts.push(Stmt::Spawn(call));
                    self.skip_semicolons();
                }
                Some(Token::Par) => {
                    // `par a() ~~ b() ~~ c()`: the peers, separated by the flat `~~` delimiter,
                    // run concurrently and join before the next statement. `~~` is not an
                    // expression operator, so `parse_expr` stops at it and we collect a flat list
                    // (no associativity, no left-before-right). Each peer must be a call.
                    self.advance();
                    let mut peers = vec![self.parse_expr()];
                    while self.eat(Token::TildeTilde) {
                        peers.push(self.parse_expr());
                    }
                    stmts.push(Stmt::Par(peers));
                    self.skip_semicolons();
                }
                _ => {
                    let e = self.parse_expr();
                    if self.eat(Token::Eq) {
                        // Assignment: `target = value`.
                        let value = self.parse_expr();
                        stmts.push(Stmt::Assign { target: e, value });
                        self.skip_semicolons();
                    } else {
                        self.skip_semicolons();
                        if self.peek() == Some(&Token::RBrace) {
                            // Last bare expression = the block's tail value.
                            tail = Some(Box::new(e));
                        } else {
                            stmts.push(Stmt::Expr(e));
                        }
                    }
                }
            }
            // An iteration parses at most one statement; attribute it to the line it began on.
            while stmt_lines.len() < stmts.len() {
                stmt_lines.push(stmt_line);
            }
        }

        self.expect(Token::RBrace);
        Block {
            stmts,
            tail,
            stmt_lines,
        }
    }

    fn parse_let(&mut self) -> Stmt {
        self.expect(Token::Let);
        let mutable = self.eat(Token::Ident("mut".to_string()));
        let name = self.expect_ident();
        // `let a ~~ b ~~ c = par f() ~~ g() ~~ h()`: the positional parallel binding. The `~~`
        // after the first name is the tell; collect the names, then the `par` peers (also `~~`
        // separated). Each name binds its peer's result by source position after the join.
        if self.peek() == Some(&Token::TildeTilde) {
            let mut names = vec![name];
            while self.eat(Token::TildeTilde) {
                names.push(self.expect_ident());
            }
            self.expect(Token::Eq);
            self.expect(Token::Par);
            let mut peers = vec![self.parse_expr()];
            while self.eat(Token::TildeTilde) {
                peers.push(self.parse_expr());
            }
            return Stmt::ParLet {
                mutable,
                names,
                peers,
            };
        }
        // `let name[N]`: a fixed-size stack array of N words (N a literal).
        let array_len = if self.eat(Token::LBracket) {
            let n = match self.advance() {
                Token::Int(n) => n,
                other => panic!("array size must be an integer literal, got {other:?}"),
            };
            self.expect(Token::RBracket);
            Some(n)
        } else {
            None
        };
        let ty = if self.eat(Token::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        // `let s = par(op) f() ~~ g() ~~ h()`: the parallel reduce. The peers run concurrently and
        // fold with `op` into `s`. Recognized before the general value parse because `par` is a
        // statement keyword, not an expression.
        if self.peek() == Some(&Token::Eq) && self.tokens.get(self.pos + 1) == Some(&Token::Par) {
            self.expect(Token::Eq);
            self.expect(Token::Par);
            self.expect(Token::LParen);
            let op = match self.advance() {
                Token::Plus => BinOp::Add,
                Token::Star => BinOp::Mul,
                Token::Amp => BinOp::BitAnd,
                Token::Pipe => BinOp::BitOr,
                Token::Caret => BinOp::BitXor,
                // Parsed so the checker can reject it with a clear message; not in the monoid set.
                Token::Minus => BinOp::Sub,
                Token::Slash => BinOp::Div,
                other => panic!("`par(op)` reducer must be an operator, got {other:?}"),
            };
            self.expect(Token::RParen);
            let mut peers = vec![self.parse_expr()];
            while self.eat(Token::TildeTilde) {
                peers.push(self.parse_expr());
            }
            return Stmt::ParReduce {
                mutable,
                name,
                op,
                peers,
            };
        }
        // `let r = graph { node a = e1  node b = e2  yield e3 }`: the dependency-graph block. Only
        // valid as a `let` initializer for now. `node` and `yield` are CONTEXTUAL keywords (plain
        // identifiers, recognized only here), so `node` stays a free identifier elsewhere.
        if self.peek() == Some(&Token::Eq) && self.tokens.get(self.pos + 1) == Some(&Token::Graph) {
            self.expect(Token::Eq);
            self.expect(Token::Graph);
            self.expect(Token::LBrace);
            let is_kw = |t: Option<&Token>, kw: &str| matches!(t, Some(Token::Ident(s)) if s == kw);
            let mut nodes = Vec::new();
            loop {
                self.skip_semicolons();
                if is_kw(self.peek(), "node") {
                    self.advance();
                    let nn = self.expect_ident();
                    self.expect(Token::Eq);
                    let e = self.parse_expr();
                    nodes.push((nn, e));
                } else if is_kw(self.peek(), "yield") {
                    self.advance();
                    let y = self.parse_expr();
                    self.skip_semicolons();
                    self.expect(Token::RBrace);
                    return Stmt::GraphLet {
                        mutable,
                        name,
                        nodes,
                        yield_expr: y,
                    };
                } else {
                    panic!(
                        "graph block: expected `node NAME = ..` or `yield ..`, got {:?}",
                        self.peek()
                    );
                }
            }
        }
        // `let s: string = "..."`: `string` IS a type — a view of bytes whose length the
        // compiler counted when it emitted the literal, so it indexes with the same bounds check
        // as any other view and never has to be walked until a NUL turns up. Lowered here to
        // exactly that view, which is where all the checking already lives.
        let mut string_chars: Option<Vec<char>> = None;
        let ty = match (&ty, self.peek()) {
            (Some(Type::Named(n)), Some(Token::Eq)) if n == "string" => {
                if let Some(Token::Str(text)) = self.tokens.get(self.pos + 1) {
                    // A `string` is CHARACTERS, not bytes: the compiler reads the source as UTF-8,
                    // so it can count them exactly and hand over the code points themselves. That
                    // keeps `s.len` and `s[i]` talking about the same thing — with bytes, `.len`
                    // of "año" would be 4 while it plainly has 3 characters, and the obvious loop
                    // `for i in 0..s.len { s[i] }` would read half of the ñ.
                    let chars: Vec<char> = text.chars().collect();
                    let len = chars.len().to_string();
                    string_chars = Some(chars);
                    Some(Type::View(
                        Box::new(Type::Named("u32".to_string())),
                        Some(len),
                    ))
                } else {
                    ty
                }
            }
            _ => ty,
        };
        // `let x = value`, or `let mut x` / `let a[N]` with no initializer (0),
        // the C-style "declare now, assign later" that AI writers reach for.
        // The literal becomes its code points, so the existing brace-initializer path reserves
        // the storage and writes them: one mechanism, not a second one for text.
        if let Some(chars) = string_chars {
            self.expect(Token::Eq);
            let Token::Str(text) = self.advance() else {
                unreachable!("peeked above")
            };
            // The characters are what `s` holds; the original bytes stay reachable through
            // `s.utf8`, which is the only way to get them and says so at the call site.
            self.string_bytes.insert(name.clone(), text.clone());
            // The value stays the LITERAL; the type says it is a view of code points. The codegen
            // interns those in `.rodata` — constant data emitted once, not rebuilt on every call
            // of the function it sits in.
            let _ = chars;
            return Stmt::Let {
                mutable,
                name,
                ty,
                value: Expr::Str(text),
                array_len,
            };
        }
        self.utf8_len = None;
        let value = if self.eat(Token::Eq) {
            // `let buf: T[n] = {a, b, c}`: a view declared WITH its contents. Braces rather than
            // brackets, which already mean an array literal for `let a[N] = [..]`, and read here
            // without ambiguity because only a view-typed binding accepts them in this position.
            if matches!(ty, Some(Type::View(..))) && self.peek() == Some(&Token::LBrace) {
                self.expect(Token::LBrace);
                let mut items = Vec::new();
                while self.peek() != Some(&Token::RBrace) {
                    items.push(self.parse_expr());
                    if !self.eat(Token::Comma) {
                        break;
                    }
                }
                self.expect(Token::RBrace);
                Expr::List(items)
            } else {
                self.parse_expr()
            }
        } else {
            Expr::Int(0)
        };
        // `let b = s.utf8` is a view of BYTES, and its length is their count — typed here so the
        // binding carries it, the same way any other view does.
        // `let b = s.utf8` is a view of BYTES — but only when the value IS that request. A
        // `.utf8` buried inside a larger expression (`let n = f(a, s.utf8)`) says nothing about
        // what `n` is, and typing it as a view made an ordinary integer look like a buffer.
        let is_utf8_value = matches!(&value, Expr::Field(inner, f)
            if f == "utf8" && matches!(inner.as_ref(), Expr::Str(_)));
        let ty = match (ty, self.utf8_len.take()) {
            (None, Some(n)) if is_utf8_value => Some(Type::View(
                Box::new(Type::Named("u8".to_string())),
                Some(n.to_string()),
            )),
            (t, _) => t,
        };
        Stmt::Let {
            mutable,
            name,
            ty,
            value,
            array_len,
        }
    }

    fn parse_return(&mut self) -> Stmt {
        self.expect(Token::Return);
        // A `return` with no value is followed by `;` or `}`.
        if matches!(self.peek(), Some(Token::Semicolon | Token::RBrace)) {
            Stmt::Return(None)
        } else {
            Stmt::Return(Some(self.parse_expr()))
        }
    }

    /// Parses a `{ <prop>+ }` block of contract propositions, each an
    /// expression separated by implicit `;` (end of line) or newlines.
    fn parse_prop_block(&mut self) -> Vec<Expr> {
        self.expect(Token::LBrace);
        self.skip_semicolons();
        let mut props = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            props.push(self.parse_expr());
            self.skip_semicolons();
        }
        self.expect(Token::RBrace);
        props
    }

    /// Parses an `effects [A, B]` list (the effect names).
    fn parse_effect_list(&mut self) -> Vec<String> {
        self.expect(Token::LBracket);
        let mut effects = Vec::new();
        while self.peek() != Some(&Token::RBracket) {
            effects.push(self.expect_ident());
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RBracket);
        effects
    }

    fn skip_semicolons(&mut self) {
        while self.eat(Token::Semicolon) {}
    }

    /// Parses a comma-separated parameter list, up to (but not consuming) the
    /// closing `)`. Allows a trailing comma (gofmt style).
    /// Parses the parameter list, returning the params and any refinement predicates written as
    /// `where` clauses on them. A `where` on a parameter (`i: i64 where 0 <= i && i < n`) is a
    /// precondition attached to the value; each is desugared into the function's `requires` so
    /// the prover uses it exactly like a hand-written precondition (and a caller must satisfy it).
    fn parse_params(&mut self) -> (Vec<Param>, Vec<Expr>) {
        let mut params = Vec::new();
        let mut refinements = Vec::new();
        while self.peek() != Some(&Token::RParen) {
            // Optional `mut`: params are already assignable locals, so just accept it.
            self.eat(Token::Ident("mut".to_string()));
            let name = self.expect_ident();
            // The type annotation is optional and defaults to i64, since every
            // value is i64, AI writers often omit it (`fn qr(a, b, c, d)`).
            let ty = if self.eat(Token::Colon) {
                self.parse_type()
            } else {
                Type::Named("i64".to_string())
            };
            // A `where <predicate>` refines the parameter. A `&&` chain contributes each conjunct
            // separately (so each is a single comparison the linear prover can use).
            if self.eat(Token::Where) {
                split_conjuncts(self.parse_expr(), &mut refinements);
            }
            params.push(Param { name, ty });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        (params, refinements)
    }

    /// Parses an expression (used in bodies and contract propositions).
    fn parse_type(&mut self) -> Type {
        // `*T` is a pointer to a struct: a mutable, shareable handle declared up
        // front (Sond has no `&`). Same representation as the value form; the
        // difference is the static mutability rules checked later.
        if self.eat(Token::Star) {
            return Type::Ptr(Box::new(self.parse_type()));
        }
        // `[]` or `[ElemType]` annotates an array/pointer parameter. Sond passes an
        // array as its i64 address, so such an annotation is just the type i64.
        if self.eat(Token::LBracket) {
            if self.peek() != Some(&Token::RBracket) {
                let _ = self.parse_type(); // element type, ignored
            }
            self.expect(Token::RBracket);
            return Type::Named("i64".to_string());
        }
        // `T[]` is a VIEW of T: the element type carries, the length does not. Postfix so it
        // reads as the type it is a view of, matching `let px: u32[1024]` for the sized form.
        // `T[n]` additionally NAMES the parameter holding the view's length, which is what the
        // bounds checker proves indices against (`u32[]` alone cannot be indexed).
        let base = Type::Named(self.expect_ident());
        if self.peek() == Some(&Token::LBracket) {
            match self.tokens.get(self.pos + 1) {
                Some(Token::RBracket) => {
                    self.expect(Token::LBracket);
                    self.expect(Token::RBracket);
                    return Type::View(Box::new(base), None);
                }
                // The length is either the NAME of the parameter/local carrying it, or a literal
                // for a view over a store whose size is written down at the declaration.
                Some(Token::Ident(len))
                    if self.tokens.get(self.pos + 2) == Some(&Token::RBracket) =>
                {
                    let len = len.clone();
                    self.expect(Token::LBracket);
                    self.advance();
                    self.expect(Token::RBracket);
                    return Type::View(Box::new(base), Some(len));
                }
                Some(Token::Int(len))
                    if self.tokens.get(self.pos + 2) == Some(&Token::RBracket) =>
                {
                    let len = len.to_string();
                    self.expect(Token::LBracket);
                    self.advance();
                    self.expect(Token::RBracket);
                    return Type::View(Box::new(base), Some(len));
                }
                _ => {}
            }
        }
        base
    }
}

/// `name = name + 1`: a loop-counter increment statement.
fn assign_inc(name: &str) -> Stmt {
    Stmt::Assign {
        target: Expr::Var(name.to_string()),
        value: Expr::Binary(
            BinOp::Add,
            Box::new(Expr::Var(name.to_string())),
            Box::new(Expr::Int(1)),
        ),
    }
}

/// Sinks a block's tail expression into its statement list (as a discarded expression), so a
/// loop-counter increment appended afterwards runs AFTER the body's last expression, not before
/// it (`for i in 0..3 { print(i) }` must print 0,1,2, the increment cannot precede the tail).
/// Inserts `step` before every `continue` that belongs to THIS loop.
///
/// The counting and three-part `for` forms both desugar to a `for <cond>` whose body ENDS with
/// the step (`i = i + 1`). That is right for a body that runs to its end, and wrong for one that
/// jumps: `continue` goes straight to the condition, past the step, so the iteration that took it
/// never advanced the counter and the loop spun for ever -- compiling clean, printing nothing,
/// at 100% of a core.
///
/// The desugar owns the rule "the step runs on every path that leaves the body", so it has to
/// fix up the paths that leave early too. Nested loops are skipped: a `continue` inside one
/// belongs to that loop and stepping the outer counter as well would silently skip an iteration,
/// which is a wrong answer rather than a hang, and harder to notice.
fn step_before_continues(block: &mut Block, step: &Stmt) {
    let mut i = 0;
    while i < block.stmts.len() {
        match &mut block.stmts[i] {
            Stmt::Continue => {
                block.stmts.insert(i, step.clone());
                i += 1; // past the step just inserted
            }
            Stmt::For { .. } => {} // a nested loop owns its own `continue`
            other => step_in_stmt(other, step),
        }
        i += 1;
    }
    if let Some(t) = block.tail.as_mut() {
        step_in_expr(t, step);
    }
}

/// Walks a statement's expressions looking for the blocks of an `if`/`match`, where a `continue`
/// of the enclosing loop can also sit.
fn step_in_stmt(s: &mut Stmt, step: &Stmt) {
    match s {
        Stmt::Let { value, .. } => step_in_expr(value, step),
        Stmt::Assign { target, value } => {
            step_in_expr(target, step);
            step_in_expr(value, step);
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => step_in_expr(e, step),
        Stmt::Par(peers) | Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
            for p in peers {
                step_in_expr(p, step);
            }
        }
        _ => {}
    }
}

/// The same, through an expression: `if` and `match` carry blocks, and any expression can hold
/// one deeper down (`f(if c { .. } else { .. })`).
fn step_in_expr(e: &mut Expr, step: &Stmt) {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            step_in_expr(cond, step);
            step_before_continues(then_branch, step);
            if let Some(b) = else_branch.as_mut() {
                step_before_continues(b, step);
            }
        }
        Expr::Match { scrutinee, arms } => {
            step_in_expr(scrutinee, step);
            for a in arms {
                step_before_continues(&mut a.body, step);
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            step_in_expr(l, step);
            step_in_expr(r, step);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => step_in_expr(x, step),
        Expr::Call(c, args) => {
            step_in_expr(c, step);
            for a in args {
                step_in_expr(&mut a.value, step);
            }
        }
        _ => {}
    }
}

fn sink_tail(body: &mut Block) {
    if let Some(t) = body.tail.take() {
        body.stmts.push(Stmt::Expr(*t));
    }
}

mod expr;
#[cfg(test)]
mod tests;

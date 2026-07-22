//! The Sond parser: turns a token stream into an AST.
//!
//! Grown with strict TDD. Only the productions exercised by a test exist.

use crate::ast::{
    Arg, BinOp, Block, EnumDef, EnumVariant, Expr, Function, Global, MatchArm, Param, PortDef,
    ProviderDef, Signature, Stmt, StructDef, Type, UnOp, UseDecl, Visibility,
};
use crate::token::Token;

/// A recursive-descent parser over a flat token stream.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Module-level `let` bindings collected while parsing the program.
    globals: Vec<Global>,
    /// Top-level `struct` declarations collected while parsing the program.
    structs: Vec<StructDef>,
    /// Top-level `enum` (sum type) declarations collected while parsing the program.
    enums: Vec<EnumDef>,
    /// Top-level `port` (interface) declarations collected while parsing the program.
    ports: Vec<PortDef>,
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
        Parser {
            tokens,
            pos: 0,
            globals: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            ports: Vec::new(),
            providers: Vec::new(),
            uses: Vec::new(),
            no_struct_lit: false,
        }
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
                    | Token::Port
                    | Token::Provides
                    | Token::Use
                    | Token::Pub
                    | Token::Priv
                    | Token::Impl
                    | Token::Test
            )
        ) {
            // `impl Type { fn m(self, ..) { .. } }`: methods become free functions
            // named `Type_m` with `self` typed as the struct; calls `p.m(a)` are
            // lowered to `Type_m(p, a)` by desugar.
            if self.peek() == Some(&Token::Impl) {
                for method in self.parse_impl() {
                    funcs.push(method);
                }
                self.skip_semicolons();
                continue;
            }
            // Determine the declaration kind, looking past an optional `pub`/`priv`.
            let kind = if matches!(self.peek(), Some(Token::Pub | Token::Priv)) {
                self.tokens.get(self.pos + 1)
            } else {
                self.peek()
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
                Some(Token::Port) => {
                    let p = self.parse_port();
                    self.ports.push(p);
                }
                Some(Token::Provides) => {
                    let p = self.parse_provides();
                    self.providers.push(p);
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
                        ..
                    } = self.parse_let()
                    else {
                        unreachable!("parse_let returns a Let")
                    };
                    self.globals.push(Global {
                        vis,
                        mutable,
                        name,
                        array_len,
                        init: value,
                    });
                }
            }
            self.skip_semicolons();
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

    /// The top-level `port` (interface) declarations collected during `parse_program`.
    pub fn take_ports(&mut self) -> Vec<PortDef> {
        std::mem::take(&mut self.ports)
    }

    /// The top-level `provides` (per-target implementations) collected during `parse_program`.
    pub fn take_providers(&mut self) -> Vec<ProviderDef> {
        std::mem::take(&mut self.providers)
    }

    /// The top-level `use Port` directives collected during `parse_program`.
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
    fn parse_port(&mut self) -> PortDef {
        let vis = self.parse_visibility();
        self.expect(Token::Port);
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

    /// Parses `provides Port for Target { <functions> }`: one target's implementation
    /// of a port (RFC-0014). The body is ordinary functions (with bodies); `Target` is
    /// a target-profile identifier. Matching methods to the port and resolving `use` are
    /// later slices; this one only parses the implementation block.
    fn parse_provides(&mut self) -> ProviderDef {
        self.expect(Token::Provides);
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
        let mut tail = None;

        while self.peek() != Some(&Token::RBrace) {
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
                        self.no_struct_lit = saved;
                        if self.eat(Token::DotDot) {
                            // Counting form `for i in a..b`: i = a; while i < b; i += 1.
                            let end = self.parse_expr();
                            let mut body = self.parse_block();
                            stmts.push(Stmt::Let {
                                mutable: true,
                                name: var.clone(),
                                ty: None,
                                value: first,
                                array_len: None,
                            });
                            sink_tail(&mut body); // increment must run AFTER the body, not before
                            body.stmts.push(assign_inc(&var));
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
                        let cond = if matches!(self.peek(), Some(Token::LBrace | Token::Invariant)) {
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
        }

        self.expect(Token::RBrace);
        Block { stmts, tail }
    }

    fn parse_let(&mut self) -> Stmt {
        self.expect(Token::Let);
        let mutable = self.eat(Token::Ident("mut".to_string()));
        let name = self.expect_ident();
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
        // `let x = value`, or `let mut x` / `let a[N]` with no initializer (0),
        // the C-style "declare now, assign later" that AI writers reach for.
        let value = if self.eat(Token::Eq) {
            self.parse_expr()
        } else {
            Expr::Int(0)
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
        Type::Named(self.expect_ident())
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
fn sink_tail(body: &mut Block) {
    if let Some(t) = body.tail.take() {
        body.stmts.push(Stmt::Expr(*t));
    }
}

mod expr;
#[cfg(test)]
mod tests;

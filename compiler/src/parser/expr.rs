//! Expression parsing: Pratt/precedence-climbing over the token stream.
//! Split from mod.rs to keep each parser file under the source-size cap.

use super::*;

impl Parser {
    pub fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0)
    }

    /// Precedence-climbing expression parser. `min_bp` is the minimum binding
    /// power an infix operator must have to be consumed here (RFC-0001 §4.16).
    fn parse_expr_bp(&mut self, min_bp: u8) -> Expr {
        // Prefix / atom.
        let mut lhs = match self.peek() {
            Some(Token::Not) => {
                self.advance();
                Expr::Unary(UnOp::Not, Box::new(self.parse_expr_bp(NOT_BP)))
            }
            Some(Token::Minus) => {
                self.advance();
                Expr::Unary(UnOp::Neg, Box::new(self.parse_expr_bp(NEG_BP)))
            }
            Some(Token::Tilde) => {
                self.advance();
                Expr::Unary(UnOp::BitNot, Box::new(self.parse_expr_bp(NEG_BP)))
            }
            _ => self.parse_atom(),
        };

        loop {
            // Postfix (field access, call) binds tighter than any infix.
            match self.peek() {
                Some(Token::Dot) => {
                    self.advance();
                    lhs = Expr::Field(Box::new(lhs), self.expect_ident());
                    continue;
                }
                Some(Token::LParen) => {
                    self.advance();
                    lhs = Expr::Call(Box::new(lhs), self.parse_arg_list());
                    continue;
                }
                // Word-array indexing `base[index]` (8-byte elements).
                Some(Token::LBracket) => {
                    self.advance();
                    let index = self.parse_expr();
                    self.expect(Token::RBracket);
                    lhs = Expr::Index(Box::new(lhs), Box::new(index));
                    continue;
                }
                // `expr as Type`, an explicit conversion; binds tighter than infix.
                Some(Token::As) => {
                    self.advance();
                    let ty = self.parse_type();
                    lhs = Expr::Cast(Box::new(lhs), ty);
                    continue;
                }
                _ => {}
            }

            // Infix.
            let Some((lbp, rbp, op)) = self.peek().and_then(infix_bp) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.advance(); // consume the operator
            let rhs = self.parse_expr_bp(rbp);
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }

        lhs
    }

    /// Parses `Name { field: value, field: value, ... }` after the name. The name
    /// is already consumed; the current token is `{`. Trailing comma allowed; the
    /// list may span lines (implicit `;` at end of line are skipped).
    fn parse_struct_literal(&mut self, name: String) -> Expr {
        self.expect(Token::LBrace);
        self.skip_semicolons();
        let mut fields = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            let field = self.expect_ident();
            self.expect(Token::Colon);
            let value = Box::new(self.parse_expr());
            fields.push((field, value));
            self.skip_semicolons();
            if !self.eat(Token::Comma) {
                break;
            }
            self.skip_semicolons();
        }
        self.expect(Token::RBrace);
        Expr::StructLit(name, fields)
    }

    fn parse_atom(&mut self) -> Expr {
        match self.advance() {
            Token::Int(n) => Expr::Int(n),
            Token::Decimal(s) => Expr::Decimal(s),
            Token::Str(s) => Expr::Str(s),
            Token::Bool(b) => Expr::Bool(b),
            Token::Ident(name) => {
                // `Name { field: value, ... }` is a struct literal, unless we are
                // parsing a condition (where `{` opens the body block).
                if !self.no_struct_lit && self.peek() == Some(&Token::LBrace) {
                    self.parse_struct_literal(name)
                } else {
                    Expr::Var(name)
                }
            }
            Token::LParen => {
                let e = self.parse_expr();
                self.expect(Token::RParen);
                e
            }
            Token::LBracket => {
                // A list may span many lines (a crypto constant table). Newlines
                // inside `[ ]` produce implicit `;` tokens, skip them so elements
                // separate by commas, and the last element needs no trailing comma.
                let mut items = Vec::new();
                self.skip_semicolons();
                while self.peek() != Some(&Token::RBracket) {
                    items.push(self.parse_expr());
                    self.skip_semicolons();
                    if !self.eat(Token::Comma) {
                        break;
                    }
                    self.skip_semicolons();
                }
                self.skip_semicolons();
                self.expect(Token::RBracket);
                Expr::List(items)
            }
            Token::If => {
                let saved = self.no_struct_lit;
                self.no_struct_lit = true;
                let cond = Box::new(self.parse_expr());
                self.no_struct_lit = saved;
                let then_branch = Box::new(self.parse_block());
                // Allow an implicit `;` between `}` and `else` (`} \n else`), but do
                // NOT swallow it when there is no else, otherwise a following
                // statement that starts with `(` would be mis-parsed as a call of
                // the whole `if`. Backtrack if no `else` follows the semicolons.
                let before_semis = self.pos;
                self.skip_semicolons();
                let else_branch = if self.eat(Token::Else) {
                    if self.peek() == Some(&Token::If) {
                        // `else if ...`: the else branch is a block whose tail is
                        // the nested `if` expression (an else-if chain).
                        let nested = self.parse_expr();
                        Some(Box::new(Block {
                            stmts: Vec::new(),
                            tail: Some(Box::new(nested)),
                        }))
                    } else {
                        Some(Box::new(self.parse_block()))
                    }
                } else {
                    self.pos = before_semis; // no else: keep the `;` to end the statement
                    None
                };
                Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                }
            }
            // `match scrutinee { Variant => body, ... }`, each arm's body is a block
            // `{ .. }` or a bare expression. Exhaustiveness is checked after parsing.
            Token::Match => {
                let saved = self.no_struct_lit;
                self.no_struct_lit = true;
                let scrutinee = Box::new(self.parse_expr());
                self.no_struct_lit = saved;
                self.expect(Token::LBrace);
                self.skip_semicolons();
                let mut arms = Vec::new();
                while self.peek() != Some(&Token::RBrace) {
                    let variant = self.expect_ident();
                    // Optional payload binding: `Some(x) => ..` binds the variant's
                    // carried value to `x` inside the arm. A bare `None => ..` has none.
                    let binding = if self.eat(Token::LParen) {
                        let b = self.expect_ident();
                        self.expect(Token::RParen);
                        Some(b)
                    } else {
                        None
                    };
                    self.expect(Token::FatArrow);
                    let body = if self.peek() == Some(&Token::LBrace) {
                        self.parse_block()
                    } else {
                        let e = self.parse_expr();
                        Block {
                            stmts: Vec::new(),
                            tail: Some(Box::new(e)),
                        }
                    };
                    arms.push(MatchArm {
                        variant,
                        binding,
                        body,
                    });
                    self.eat(Token::Comma);
                    self.skip_semicolons();
                }
                self.expect(Token::RBrace);
                Expr::Match { scrutinee, arms }
            }
            other => panic!("unexpected token in expression: {other:?}"),
        }
    }

    /// Parses a comma-separated argument list up to and including the `)`.
    /// Each argument is positional or keyword (`name = expr`).
    fn parse_arg_list(&mut self) -> Vec<Arg> {
        let mut args = Vec::new();
        while self.peek() != Some(&Token::RParen) {
            // Keyword argument: `ident = expr` (two-token lookahead to avoid
            // confusing it with a plain identifier expression).
            let name = if matches!(self.peek(), Some(Token::Ident(_)))
                && self.peek_at(1) == Some(&Token::Eq)
            {
                let n = self.expect_ident();
                self.expect(Token::Eq);
                Some(n)
            } else {
                None
            };
            let value = self.parse_expr();
            args.push(Arg { name, value });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RParen);
        args
    }

    // --- helpers ---

    pub(crate) fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    pub(crate) fn peek_at(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.pos + n)
    }

    pub(crate) fn advance(&mut self) -> Token {
        let tok = self
            .tokens
            .get(self.pos)
            .cloned()
            .expect("unexpected end of input");
        self.pos += 1;
        tok
    }

    /// Consumes the next token, asserting it equals `expected`.
    pub(crate) fn expect(&mut self, expected: Token) {
        let got = self.advance();
        if got != expected {
            // A plain panic (not assert_eq!) gives the qwen workers a readable
            // "expected X, got Y" instead of a left/right assertion dump.
            panic!("expected {expected:?}, got {got:?}");
        }
    }

    /// If the next token equals `tok`, consumes it and returns true.
    pub(crate) fn eat(&mut self, tok: Token) -> bool {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Consumes an identifier token and returns its name.
    pub(crate) fn expect_ident(&mut self) -> String {
        match self.advance() {
            Token::Ident(name) => name,
            other => panic!("expected identifier, got {other:?}"),
        }
    }
}

/// Binding power of prefix `not`, tighter than `and`/`or`, looser than `in`
/// and comparisons (RFC-0001 §4.16: `not` sits between `and` and `in`).
const NOT_BP: u8 = 8;
/// Binding power of prefix `-` (negation), tighter than `*`/`/`.
const NEG_BP: u8 = 17;

/// Infix binding powers `(left, right, op)` from the precedence table
/// (RFC-0001 §4.16). Left-associative ops have `left < right`; `implies` is
/// right-associative (`left > right`) and the loosest.
fn infix_bp(tok: &Token) -> Option<(u8, u8, BinOp)> {
    let (op, l, r) = match tok {
        Token::Implies => (BinOp::Implies, 2, 1), // right-assoc, loosest
        Token::Or | Token::PipePipe => (BinOp::Or, 3, 4),
        Token::And | Token::AmpAmp => (BinOp::And, 5, 6),
        Token::In => (BinOp::In, 9, 10),
        Token::EqEq => (BinOp::Eq, 11, 12),
        Token::Ne => (BinOp::Ne, 11, 12),
        Token::Lt => (BinOp::Lt, 11, 12),
        Token::Gt => (BinOp::Gt, 11, 12),
        Token::Le => (BinOp::Le, 11, 12),
        Token::Ge => (BinOp::Ge, 11, 12),
        Token::Plus => (BinOp::Add, 13, 14),
        Token::Minus => (BinOp::Sub, 13, 14),
        Token::Star => (BinOp::Mul, 15, 16),
        Token::Slash => (BinOp::Div, 15, 16),
        Token::Percent => (BinOp::Mod, 15, 16),
        // Bitwise/shift bind TIGHTER than arithmetic so the common driver idiom
        // `reg & 0x20 == 0x20` groups as `(reg & 0x20) == 0x20` (no C footgun).
        // Relative order matches C: `|` loosest, then `^`, `&`, then shifts.
        Token::Pipe => (BinOp::BitOr, 17, 18),
        Token::Caret => (BinOp::BitXor, 19, 20),
        Token::Amp => (BinOp::BitAnd, 21, 22),
        Token::Shl => (BinOp::Shl, 23, 24),
        Token::Shr => (BinOp::Shr, 23, 24),
        Token::LShr => (BinOp::LShr, 23, 24),
        _ => return None,
    };
    Some((l, r, op))
}

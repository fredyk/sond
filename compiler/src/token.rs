//! Lexer tokens for Sond.
//!
//! The set of variants grows ONLY when a test requires it (TDD).

/// A lexical token of Sond.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // --- Literals ---
    /// Integer literal, e.g. `42`.
    Int(i64),
    /// Decimal literal, stored as the raw lexeme (e.g. `"0.00"`) to preserve
    /// exact precision. `Money` is fixed-point, not `f64`, so the lexer must
    /// not lose digits by parsing into a float here.
    Decimal(String),
    /// String literal, e.g. `"hello"` (contents without the quotes).
    Str(String),
    /// Boolean literal, `true` or `false`.
    Bool(bool),

    // --- Identifiers and keywords ---
    /// Identifier, e.g. `tax_base`, `tâche`, `donnée`. Supports Unicode (accents and
    /// `ñ`) per RFC-0001 §3.4, Sond source may be written in any language.
    Ident(String),
    /// `let`
    Let,
    /// `fn`
    Fn,
    /// `interrupt`: qualifier for an interrupt handler function (ISR): saves all
    /// GP registers and returns with `iretq` instead of `ret`.
    Interrupt,
    /// `wrapping fn`: a function whose integer arithmetic is defined to wrap mod 2^N (the crypto
    /// substrate), exempt from the overflow-checking that is a compile error elsewhere.
    Wrapping,
    /// `struct`
    Struct,
    Test,
    /// `spawn`: run a function call on a new OS thread (RFC-0010 Layer B). The enclosing
    /// scope joins every thread it spawned before it exits (structured concurrency), so a
    /// spawned thread can never outlive the frame that started it.
    Spawn,
    /// `enum`: a sum type (closed set of variants).
    Enum,
    /// `port`: a named interface, a set of signatures implemented per target (RFC-0014).
    Port,
    /// `provides Port for Target`: one target's implementation of a port (RFC-0014).
    Provides,
    /// `as`: an explicit numeric conversion (`x as u8`, `n as f64`).
    As,
    /// `match`: pattern match on a sum type (exhaustive).
    Match,
    /// `=>`: a match arm separator.
    FatArrow,
    /// `impl`
    Impl,
    /// `pub`
    Pub,
    /// `priv`
    Priv,
    /// `requires`
    Requires,
    /// `where`: a refinement predicate on a parameter's type (`i: i64 where 0 <= i`)
    Where,
    /// `ensures`
    Ensures,
    /// `invariant`
    Invariant,
    /// `effects`
    Effects,
    /// `return`
    Return,
    /// `if`
    If,
    /// `else`
    Else,
    /// `for`
    For,
    /// `while` (a synonym for the conditional `for`)
    While,
    /// `break`
    Break,
    /// `continue`
    Continue,

    // --- Word operators (RFC-0001 §4.16) ---
    /// `and`
    And,
    /// `or`
    Or,
    /// `not`
    Not,
    /// `implies`
    Implies,
    /// `in`
    In,

    // --- Symbol operators and delimiters ---
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `~`
    Tilde,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `>>>` (logical / zero-fill right shift)
    LShr,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `..` (range, in `for i in a..b`)
    DotDot,
    /// `->`
    Arrow,
    /// `;`: statement terminator. May be written explicitly or inserted
    /// implicitly at end of line (RFC-0002 §3.5, Go-style).
    Semicolon,
}

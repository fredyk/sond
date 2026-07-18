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
    /// `interrupt`, qualifier for an interrupt handler function (ISR): saves all
    /// GP registers and returns with `iretq` instead of `ret`.
    Interrupt,
    /// `struct`
    Struct,
    Test,
    /// `enum`, a sum type (closed set of variants).
    Enum,
    /// `as`, an explicit numeric conversion (`x as u8`, `n as f64`).
    As,
    /// `match`, pattern match on a sum type (exhaustive).
    Match,
    /// `=>`, a match arm separator.
    FatArrow,
    /// `impl`
    Impl,
    /// `pub`
    Pub,
    /// `priv`
    Priv,
    /// `requires`
    Requires,
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
    /// `;`, statement terminator. May be written explicitly or inserted
    /// implicitly at end of line (RFC-0002 §3.5, Go-style).
    Semicolon,
}

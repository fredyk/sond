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
    /// `par`: the inline concurrent-peers form, `par a() ~~ b() ~~ c()` (RFC-0008 §5.1a).
    /// Runs the peers in parallel over Layer B threads and joins them before the next statement.
    Par,
    /// `graph`: opens the dependency-graph block (RFC-0008 §5.1). `graph { node a = .. node b = ..
    /// yield e }` binds nodes whose only order is their data dependency; the block evaluates to the
    /// `yield` expression. `node` and `yield` are CONTEXTUAL keywords, recognized only inside the
    /// block, so `node` stays a free identifier everywhere else (a common variable name).
    Graph,
    /// `enum`: a sum type (closed set of variants).
    Enum,
    /// `iface`: a named interface, a set of signatures implemented per target (RFC-0014). The
    /// concept is a port (ports & adapters); the surface keyword is `iface` so `port` stays a
    /// free identifier (a network port is a very common variable name).
    Iface,
    /// `use Iface`: depend on an interface; resolved to the active target's `impl` (RFC-0014).
    /// A target's implementation is written `impl Iface for Target { .. }`, reusing `Impl`.
    Use,
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
    /// `~~`: the flat peer delimiter of a `par` block (RFC-0008 §5.1a). Its own two-character
    /// token, so it is never bitwise-NOT (`~`), and it does not enter the expression precedence
    /// table: it only separates peers inside `par`.
    TildeTilde,
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

impl Token {
    /// The token written the way a person writes it: `` `{` ``, `` `;` ``, `` `+` ``,
    /// an identifier by its name, a literal by its value. Its own comment above each variant
    /// is the source of truth for the spelling.
    ///
    /// Error messages go through here and never through `Debug`, whose names (`LBrace`,
    /// `Semicolon`) are lexer internals: an author told "expected LBrace, got Semicolon" has to
    /// look up a vocabulary they never used.
    pub fn describe(&self) -> String {
        let text: String = match self {
            Token::Int(v) => v.to_string(),
            Token::Decimal(raw) => raw.clone(),
            Token::Str(s) => format!("\"{s}\""),
            Token::Bool(b) => b.to_string(),
            Token::Ident(name) => name.clone(),
            Token::Let => "let".into(),
            Token::Fn => "fn".into(),
            Token::Interrupt => "interrupt".into(),
            Token::Wrapping => "wrapping".into(),
            Token::Struct => "struct".into(),
            Token::Test => "test".into(),
            Token::Spawn => "spawn".into(),
            Token::Par => "par".into(),
            Token::Graph => "graph".into(),
            Token::Enum => "enum".into(),
            Token::Iface => "iface".into(),
            Token::Use => "use".into(),
            Token::As => "as".into(),
            Token::Match => "match".into(),
            Token::FatArrow => "=>".into(),
            Token::Impl => "impl".into(),
            Token::Pub => "pub".into(),
            Token::Priv => "priv".into(),
            Token::Requires => "requires".into(),
            Token::Where => "where".into(),
            Token::Ensures => "ensures".into(),
            Token::Invariant => "invariant".into(),
            Token::Effects => "effects".into(),
            Token::Return => "return".into(),
            Token::If => "if".into(),
            Token::Else => "else".into(),
            Token::For => "for".into(),
            Token::While => "while".into(),
            Token::Break => "break".into(),
            Token::Continue => "continue".into(),
            Token::And => "and".into(),
            Token::Or => "or".into(),
            Token::Not => "not".into(),
            Token::Implies => "implies".into(),
            Token::In => "in".into(),
            Token::Eq => "=".into(),
            Token::EqEq => "==".into(),
            Token::Ne => "!=".into(),
            Token::Lt => "<".into(),
            Token::Gt => ">".into(),
            Token::Le => "<=".into(),
            Token::Ge => ">=".into(),
            Token::Plus => "+".into(),
            Token::Minus => "-".into(),
            Token::Star => "*".into(),
            Token::Slash => "/".into(),
            Token::Percent => "%".into(),
            Token::Amp => "&".into(),
            Token::Pipe => "|".into(),
            Token::Caret => "^".into(),
            Token::AmpAmp => "&&".into(),
            Token::PipePipe => "||".into(),
            Token::Tilde => "~".into(),
            Token::TildeTilde => "~~".into(),
            Token::Shl => "<<".into(),
            Token::Shr => ">>".into(),
            Token::LShr => ">>>".into(),
            Token::LParen => "(".into(),
            Token::RParen => ")".into(),
            Token::LBrace => "{".into(),
            Token::RBrace => "}".into(),
            Token::LBracket => "[".into(),
            Token::RBracket => "]".into(),
            Token::Comma => ",".into(),
            Token::Colon => ":".into(),
            Token::Dot => ".".into(),
            Token::DotDot => "..".into(),
            Token::Arrow => "->".into(),
            Token::Semicolon => ";".into(),
        };
        format!("`{text}`")
    }
}

/// A token prints as its human spelling (`` `{` ``), so diagnostics can just format it.
impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

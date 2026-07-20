use super::*;

#[test]
fn tokenizes_an_integer() {
    assert_eq!(tokenize("42"), vec![Token::Int(42)]);
}

#[test]
fn tokenizes_string_escapes() {
    // `\n`, `\t` and `\"` become the real characters, not two-char literals.
    assert_eq!(tokenize("\"a\\nb\""), vec![Token::Str("a\nb".to_string())]);
    assert_eq!(tokenize("\"x\\ty\""), vec![Token::Str("x\ty".to_string())]);
}

#[test]
fn tokenizes_a_char_literal() {
    // 'A' == 65, '0' == 48, '\n' == 10, code points as integers.
    assert_eq!(tokenize("'A'"), vec![Token::Int(65)]);
    assert_eq!(tokenize("'0'"), vec![Token::Int(48)]);
    assert_eq!(tokenize("'\\n'"), vec![Token::Int(10)]);
}

#[test]
fn tokenizes_a_hex_literal() {
    // Systems code is full of hex (ports, addresses, masks): 0x3F8 == 1016.
    assert_eq!(tokenize("0x3F8"), vec![Token::Int(1016)]);
    assert_eq!(tokenize("0xff"), vec![Token::Int(255)]);
    assert_eq!(tokenize("0X10"), vec![Token::Int(16)]);
    // Full 64-bit masks keep their bit pattern (0xFFF...F -> -1).
    assert_eq!(tokenize("0xFFFFFFFFFFFFFFFF"), vec![Token::Int(-1)]);
    assert_eq!(
        tokenize("0x000FFFFFFFFFF000"),
        vec![Token::Int(0x000FFFFFFFFFF000)]
    );
}

#[test]
fn skips_whitespace() {
    // No newline here, so no implicit `;`: this test isolates whitespace.
    assert_eq!(tokenize("  42\t 7"), vec![Token::Int(42), Token::Int(7)]);
}

#[test]
fn tokenizes_an_identifier() {
    assert_eq!(
        tokenize("tax_base"),
        vec![Token::Ident("tax_base".to_string())]
    );
}

#[test]
fn identifier_with_accents() {
    // RFC-0001 §3.4: identifiers support Unicode (accents, n-with-tilde). Sond source
    // can be written in any language.
    assert_eq!(
        tokenize("tâche données"),
        vec![
            Token::Ident("tâche".to_string()),
            Token::Ident("données".to_string()),
        ]
    );
}

#[test]
fn distinguishes_keywords_from_identifiers() {
    assert_eq!(
        tokenize("let fn letra"),
        vec![
            Token::Let,
            Token::Fn,
            // `letra` is NOT the keyword `let`: it is an identifier.
            Token::Ident("letra".to_string()),
        ]
    );
}

#[test]
fn tokenizes_operators_and_delimiters() {
    assert_eq!(
        tokenize("= + ( ) { } , : ->"),
        vec![
            Token::Eq,
            Token::Plus,
            Token::LParen,
            Token::RParen,
            Token::LBrace,
            Token::RBrace,
            Token::Comma,
            Token::Colon,
            Token::Arrow,
        ]
    );
}

#[test]
fn skips_line_comments() {
    // RFC-0002 §3.2: `//` comments run to end of line. The comment content
    // is dropped, but the line still terminates its statement, so an
    // implicit `;` is inserted after `42` (Go semantics).
    assert_eq!(
        tokenize("42 // this is a comment\n7"),
        vec![Token::Int(42), Token::Semicolon, Token::Int(7)]
    );
}

#[test]
fn tokenizes_a_decimal() {
    // Stored as the raw lexeme to preserve exact precision (Money ≠ f64).
    assert_eq!(tokenize("0.00"), vec![Token::Decimal("0.00".to_string())]);
    assert_eq!(tokenize("3.14"), vec![Token::Decimal("3.14".to_string())]);
}

#[test]
fn integer_stays_integer_without_a_dot() {
    // `42` alone is an Int, not a Decimal.
    assert_eq!(tokenize("42"), vec![Token::Int(42)]);
}

#[test]
fn tokenizes_a_string() {
    assert_eq!(tokenize("\"hello\""), vec![Token::Str("hello".to_string())]);
}

#[test]
fn tokenizes_a_sond_fragment() {
    // `let base = 42`: a real end-to-end Sond fragment.
    assert_eq!(
        tokenize("let base = 42"),
        vec![
            Token::Let,
            Token::Ident("base".to_string()),
            Token::Eq,
            Token::Int(42),
        ]
    );
}

#[test]
fn inserts_implicit_semicolon_at_end_of_line() {
    // RFC-0002 §3.5: a `;` is inserted when a line ends in a token that can
    // terminate a statement (here, an integer literal).
    assert_eq!(
        tokenize("let x = 42\nlet y = 7\n"),
        vec![
            Token::Let,
            Token::Ident("x".to_string()),
            Token::Eq,
            Token::Int(42),
            Token::Semicolon,
            Token::Let,
            Token::Ident("y".to_string()),
            Token::Eq,
            Token::Int(7),
            Token::Semicolon,
        ]
    );
}

#[test]
fn no_semicolon_after_a_trailing_operator() {
    // A line ending in an operator is a continuation: no implicit `;`.
    assert_eq!(
        tokenize("let x = 1 +\n2"),
        vec![
            Token::Let,
            Token::Ident("x".to_string()),
            Token::Eq,
            Token::Int(1),
            Token::Plus,
            Token::Int(2),
        ]
    );
}

#[test]
fn tokenizes_comparison_operators() {
    assert_eq!(
        tokenize("== != < > <= >="),
        vec![
            Token::EqEq,
            Token::Ne,
            Token::Lt,
            Token::Gt,
            Token::Le,
            Token::Ge,
        ]
    );
}

#[test]
fn tokenizes_brackets_dot_and_arithmetic() {
    assert_eq!(
        tokenize("[ ] . - * /"),
        vec![
            Token::LBracket,
            Token::RBracket,
            Token::Dot,
            Token::Minus,
            Token::Star,
            Token::Slash,
        ]
    );
}

#[test]
fn tokenizes_contract_keywords() {
    assert_eq!(
        tokenize("requires ensures effects in implies and or not return"),
        vec![
            Token::Requires,
            Token::Ensures,
            Token::Effects,
            Token::In,
            Token::Implies,
            Token::And,
            Token::Or,
            Token::Not,
            Token::Return,
        ]
    );
}

#[test]
fn tokenizes_boolean_literals() {
    assert_eq!(
        tokenize("true false"),
        vec![Token::Bool(true), Token::Bool(false)]
    );
}

#[test]
fn tokenizes_a_contract_line() {
    // `tax_base >= 0.00`: a real requires clause line.
    assert_eq!(
        tokenize("tax_base >= 0.00"),
        vec![
            Token::Ident("tax_base".to_string()),
            Token::Ge,
            Token::Decimal("0.00".to_string()),
        ]
    );
}

#[test]
fn no_semicolon_after_open_brace_or_blank_lines() {
    // `{` at end of line does not terminate a statement; blank lines do not
    // stack semicolons.
    assert_eq!(
        tokenize("fn f() {\n\n42\n}"),
        vec![
            Token::Fn,
            Token::Ident("f".to_string()),
            Token::LParen,
            Token::RParen,
            Token::LBrace,
            Token::Int(42),
            Token::Semicolon,
            Token::RBrace,
        ]
    );
}

#[test]
#[should_panic(expected = "unexpected character")]
fn rejects_a_character_it_does_not_know() {
    // A character with no meaning in Sond must stop the build, not vanish.
    // Silently dropping it is how `#[no_main]` in an included file became an
    // index expression that swallowed the declaration after it, with the
    // compiler reporting nothing at all.
    tokenize("#");
}

#[test]
#[should_panic(expected = "unexpected character")]
fn rejects_an_unknown_attribute_before_a_declaration() {
    tokenize("#[no_main]\nfn f() -> i64 { 0 }");
}

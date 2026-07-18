//! The Sond lexer (tokenizer).

use crate::token::Token;
use std::iter::Peekable;
use std::str::Chars;

/// Consumes the current character and pushes a single-character token.
fn push1(chars: &mut Peekable<Chars>, tokens: &mut Vec<Token>, tok: Token) {
    chars.next();
    tokens.push(tok);
}

/// Turns Sond source code into a sequence of tokens.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            // End of line: insert an implicit `;` if the line ends in a token
            // that can terminate a statement (RFC-0002 §3.5, Go-style).
            '\n' => {
                if ends_statement(tokens.last()) {
                    tokens.push(Token::Semicolon);
                }
                chars.next();
            }

            // Other whitespace is skipped.
            _ if c.is_whitespace() => {
                chars.next();
            }

            // Integer or decimal literal. Decimals keep their raw lexeme so no
            // precision is lost here (Money is fixed-point, not f64).
            _ if c.is_ascii_digit() => {
                // Hex literal `0x...`, ubiquitous in systems code (ports,
                // addresses, bit masks). Detected by a `0x`/`0X` prefix.
                let is_hex = c == '0' && {
                    let mut ahead = chars.clone();
                    ahead.next(); // the '0'
                    matches!(ahead.peek(), Some('x') | Some('X'))
                };
                if is_hex {
                    chars.next(); // '0'
                    chars.next(); // 'x'
                    let mut hex = String::new();
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_hexdigit() {
                            hex.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    // Parse as u64 and reinterpret as i64 so full 64-bit masks and
                    // addresses (e.g. 0xFFFFFFFFFFFFFFFF -> -1) keep their bit pattern.
                    let value =
                        u64::from_str_radix(&hex, 16).expect("valid hex digits after 0x") as i64;
                    tokens.push(Token::Int(value));
                    continue;
                }

                let mut lexeme = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        lexeme.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }

                // Fractional part: a `.` followed by at least one digit makes it
                // a decimal. We look two chars ahead by cloning the iterator, so
                // `42.foo` (a future field access) is not mistaken for a decimal.
                let is_decimal = chars.peek() == Some(&'.') && {
                    let mut lookahead = chars.clone();
                    lookahead.next(); // skip the '.'
                    lookahead.peek().is_some_and(|d| d.is_ascii_digit())
                };

                if is_decimal {
                    lexeme.push('.');
                    chars.next(); // consume the '.'
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() {
                            lexeme.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(Token::Decimal(lexeme));
                } else {
                    tokens.push(Token::Int(lexeme.parse().expect("only digits")));
                }
            }

            // Character literal `'A'` -> its code point (an integer). Ubiquitous
            // in systems code ('0', 'A', '\n'). A few common escapes are handled.
            '\'' => {
                chars.next(); // opening quote
                let c = chars.next().unwrap_or('\0');
                let code = if c == '\\' {
                    match chars.next().unwrap_or('\0') {
                        'n' => 10,
                        't' => 9,
                        'r' => 13,
                        '0' => 0,
                        '\\' => 92,
                        '\'' => 39,
                        other => other as i64,
                    }
                } else {
                    c as i64
                };
                if chars.peek() == Some(&'\'') {
                    chars.next(); // closing quote
                }
                tokens.push(Token::Int(code));
            }

            // String literal `"..."`, with the common backslash escapes so that
            // `print("line\n")` produces a real newline (not a literal `\n`).
            '"' => {
                chars.next(); // opening quote
                let mut s = String::new();
                while let Some(d) = chars.next() {
                    if d == '"' {
                        break;
                    }
                    if d == '\\' {
                        s.push(match chars.next().unwrap_or('\\') {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '0' => '\0',
                            '\\' => '\\',
                            '"' => '"',
                            other => other,
                        });
                    } else {
                        s.push(d);
                    }
                }
                tokens.push(Token::Str(s));
            }

            // Identifier or keyword. Start: Unicode letter or `_`
            // (RFC-0001 §3.4: includes accents and `ñ` via is_alphabetic).
            _ if c.is_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        ident.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(keyword_or_ident(ident));
            }

            // `/`, line comment `//` (RFC-0002 §3.2) or division `/`. For a
            // comment, the trailing '\n' is left unconsumed so it can still
            // trigger implicit semicolon insertion, just like a code line.
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    while let Some(&d) = chars.peek() {
                        if d == '\n' {
                            break;
                        }
                        chars.next();
                    }
                } else {
                    tokens.push(Token::Slash);
                }
            }

            // `-`, arrow `->` or subtraction `-`.
            '-' => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::Arrow);
                } else {
                    tokens.push(Token::Minus);
                }
            }

            // `=`, equality `==`, match arrow `=>`, or assignment `=`.
            '=' => {
                chars.next();
                match chars.peek() {
                    Some(&'=') => {
                        chars.next();
                        tokens.push(Token::EqEq);
                    }
                    Some(&'>') => {
                        chars.next();
                        tokens.push(Token::FatArrow);
                    }
                    _ => tokens.push(Token::Eq),
                }
            }

            // `!=` (a bare `!` is not yet a token).
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Ne);
                }
            }

            // `<`, `<=` or `<<`.
            '<' => {
                chars.next();
                match chars.peek() {
                    Some(&'=') => {
                        chars.next();
                        tokens.push(Token::Le);
                    }
                    Some(&'<') => {
                        chars.next();
                        tokens.push(Token::Shl);
                    }
                    _ => tokens.push(Token::Lt),
                }
            }

            // `>`, `>=` or `>>`.
            '>' => {
                chars.next();
                match chars.peek() {
                    Some(&'=') => {
                        chars.next();
                        tokens.push(Token::Ge);
                    }
                    Some(&'>') => {
                        chars.next();
                        // A third `>` makes it `>>>` (logical/zero-fill shift).
                        if chars.peek() == Some(&'>') {
                            chars.next();
                            tokens.push(Token::LShr);
                        } else {
                            tokens.push(Token::Shr);
                        }
                    }
                    _ => tokens.push(Token::Gt),
                }
            }

            // `&` (bitwise) or `&&` (logical and).
            '&' => {
                chars.next();
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::AmpAmp);
                } else {
                    tokens.push(Token::Amp);
                }
            }
            // `|` (bitwise) or `||` (logical or).
            '|' => {
                chars.next();
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::PipePipe);
                } else {
                    tokens.push(Token::Pipe);
                }
            }

            // Single-character operators and delimiters.
            '+' => push1(&mut chars, &mut tokens, Token::Plus),
            '*' => push1(&mut chars, &mut tokens, Token::Star),
            '%' => push1(&mut chars, &mut tokens, Token::Percent),
            '^' => push1(&mut chars, &mut tokens, Token::Caret),
            '~' => push1(&mut chars, &mut tokens, Token::Tilde),
            '(' => push1(&mut chars, &mut tokens, Token::LParen),
            ')' => push1(&mut chars, &mut tokens, Token::RParen),
            '{' => push1(&mut chars, &mut tokens, Token::LBrace),
            '}' => push1(&mut chars, &mut tokens, Token::RBrace),
            '[' => push1(&mut chars, &mut tokens, Token::LBracket),
            ']' => push1(&mut chars, &mut tokens, Token::RBracket),
            ',' => push1(&mut chars, &mut tokens, Token::Comma),
            ':' => push1(&mut chars, &mut tokens, Token::Colon),
            '.' => {
                chars.next();
                if chars.peek() == Some(&'.') {
                    chars.next();
                    tokens.push(Token::DotDot);
                } else {
                    tokens.push(Token::Dot);
                }
            }

            // Any other character: not handled yet; will be added via TDD.
            _ => {
                chars.next();
            }
        }
    }

    tokens
}

/// Whether the given (last-emitted) token can terminate a statement, and thus
/// triggers implicit `;` insertion at end of line (RFC-0002 §3.5). The set
/// grows via TDD as more statement-ending tokens are added (e.g. `true`,
/// `false`, `self`, `return`, `]`).
fn ends_statement(last: Option<&Token>) -> bool {
    matches!(
        last,
        Some(
            Token::Int(_)
                | Token::Decimal(_)
                | Token::Str(_)
                | Token::Bool(_)
                | Token::Ident(_)
                | Token::RParen
                | Token::RBrace
                | Token::RBracket
                | Token::Return
                | Token::Break
                | Token::Continue
        )
    )
}

/// Classifies an already-read word: returns its keyword token if it matches a
/// reserved word, otherwise an identifier.
fn keyword_or_ident(word: String) -> Token {
    match word.as_str() {
        "let" => Token::Let,
        "fn" => Token::Fn,
        "interrupt" => Token::Interrupt,
        "struct" => Token::Struct,
        "test" => Token::Test,
        "enum" => Token::Enum,
        "match" => Token::Match,
        "as" => Token::As,
        "impl" => Token::Impl,
        "pub" => Token::Pub,
        "priv" => Token::Priv,
        "requires" => Token::Requires,
        "ensures" => Token::Ensures,
        "invariant" => Token::Invariant,
        "effects" => Token::Effects,
        "return" => Token::Return,
        "if" => Token::If,
        "else" => Token::Else,
        "for" => Token::For,
        "while" => Token::While,
        "break" => Token::Break,
        "continue" => Token::Continue,
        "and" => Token::And,
        "or" => Token::Or,
        "not" => Token::Not,
        "implies" => Token::Implies,
        "in" => Token::In,
        "true" => Token::Bool(true),
        "false" => Token::Bool(false),
        _ => Token::Ident(word),
    }
}

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod tests;

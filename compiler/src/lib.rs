//! `sondc`, the reference compiler for the Sond language.
//!
//! Built bottom-up with strict TDD (red → green → refactor): first the lexer
//! (tokenizer), then the parser (AST), then the contract verifier. Each layer
//! is driven by tests that fail before the implementation exists.

pub mod ast;
pub mod check;
pub mod codegen;
pub mod decimal;
pub mod desugar;
pub mod eval;
pub mod header;
pub mod lexer;
pub mod parser;
pub mod resolve;
pub mod token;
pub mod typeck;
pub mod verify;

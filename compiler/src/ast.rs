//! Abstract syntax tree for Sond.
//!
//! Node types grow ONLY when a parser test requires them (TDD). Contracts,
//! effects and function bodies will be added in later cycles.

/// A type reference, e.g. `Money` or `Percent`.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A named type, e.g. `Money`.
    Named(String),
    /// A pointer to a struct, written `*Point`, a MUTABLE, shareable handle,
    /// declared up front (Sond has no `&` address-of). Same runtime representation
    /// as the value form (a base address); it differs only in the STATIC
    /// mutability rules: a value struct received as a parameter cannot have its
    /// fields mutated, a `*Point` can. A struct type is value XOR pointer.
    Ptr(Box<Type>),
}

/// Declaration visibility. The default is `Priv` (private to the module): a
/// generated header exposes only the `Pub` items, and the module checker will
/// reject references to another module's `Priv` items (RFC-0001 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Priv,
    Pub,
}

/// A function parameter, e.g. `tax_base: Money`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// A call argument. `name` is `Some` for a keyword argument (`decimals=2`),
/// `None` for a positional one. Whether keyword arguments are ultimately
/// desugared to positional is RFC-0002 CA#5; the parser accepts both.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

/// A top-level record type: `struct Name { field: Type, ... }`. Fields reuse
/// `Param` (a `name: Type` pair). Struct values give OS code real shape, a task
/// control block, a packet header, a socket, instead of flat arrays over globals.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub vis: Visibility,
    pub name: String,
    pub fields: Vec<Param>,
}

/// A module-level global binding: `let (mut)? NAME = <const expr>` at the top
/// level. Lives in `.data`; the initializer must be a constant.
#[derive(Debug, Clone, PartialEq)]
pub struct Global {
    pub vis: Visibility,
    pub mutable: bool,
    pub name: String,
    /// `Some(N)` for a module-level array `let NAME[N] = [..]` (a constant table
    /// in `.data`); `None` for a scalar global.
    pub array_len: Option<i64>,
    pub init: Expr,
}

/// A function declaration.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Function {
    pub vis: Visibility,
    pub name: String,
    pub params: Vec<Param>,
    /// Name of the return value, e.g. `vat_amount` in `-> (vat_amount: Money)`.
    pub ret_name: Option<String>,
    /// Return type. `None` when the function returns unit (`()`).
    pub ret: Option<Type>,
    /// Preconditions (`requires { ... }`), one `Expr` per proposition.
    pub requires: Vec<Expr>,
    /// Postconditions (`ensures { ... }`), one `Expr` per proposition.
    pub ensures: Vec<Expr>,
    /// Declared effects (`effects [DB, Red]`), by name.
    pub effects: Vec<String>,
    /// `interrupt fn`: an interrupt handler, codegen saves every GP register on
    /// entry, restores them on exit, and returns with `iretq` instead of `ret`.
    pub is_interrupt: bool,
    /// `test "name" { .. }`: a test block, lowered to a niladic function. It is NOT
    /// compiled into an ordinary program (only `sond test` gathers and runs them).
    pub is_test: bool,
    /// The human-readable name of a `test` block (the string after `test`), for the
    /// runner's report. `None` for ordinary functions.
    pub test_name: Option<String>,
    /// The function body.
    pub body: Block,
}

/// A unary operator.
#[derive(Debug, Clone, PartialEq)]
pub enum UnOp {
    /// `not`
    Not,
    /// `-` (negation)
    Neg,
    /// `~` (bitwise complement)
    BitNot,
}

/// A binary operator, with the precedence defined in RFC-0001 §4.16.
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    LShr,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Implies,
    In,
}

/// A brace-delimited block: a sequence of statements plus an optional tail
/// expression that is the block's value.
///
/// Design note (surfaced while implementing): Go-style implicit `;` insertion
/// (RFC-0002 §3.5) inserts a `;` before `}` even after the last expression, so
/// the Rust-style "trailing expression without `;` is the value" rule cannot be
/// used verbatim. The adopted rule is: **the last expression before `}` is the
/// tail value** (regardless of an implicit `;`). This interaction between §3.5
/// and tail expressions should become an RFC-0002 open question.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
}

/// A statement inside a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let (mut)? name (: Type)? = value`, or a fixed-size stack array
    /// `let name[N]` (then `array_len` is `Some(N)` and `name` holds the address
    /// of an N-word region reserved in the frame; `value` is ignored).
    Let {
        mutable: bool,
        name: String,
        ty: Option<Type>,
        value: Expr,
        array_len: Option<i64>,
    },
    /// `target = value`, e.g. `source.balance = source.balance - amount`.
    Assign { target: Expr, value: Expr },
    /// `return (expr)?`
    Return(Option<Expr>),
    /// The unified `for` loop: `for <cond> { .. }` (conditional, while-style) or
    /// `for { .. }` (infinite, when `cond` is `None`). An optional `invariant { .. }`
    /// clause (one `Expr` per proposition) states what holds before and after every
    /// iteration, the fact the prover uses to reason about the loop.
    For {
        cond: Option<Expr>,
        invariant: Vec<Expr>,
        body: Block,
    },
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// An expression used as a statement (its value is discarded).
    Expr(Expr),
}

/// An expression (used in bodies and in `requires`/`ensures` propositions).
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    /// Decimal literal kept as its raw lexeme to preserve exact precision.
    Decimal(String),
    Str(String),
    Bool(bool),
    /// A variable reference (identifier).
    Var(String),
    /// Field access, e.g. `source.balance`.
    Field(Box<Expr>, String),
    /// Function/method call, e.g. `items.len()`, `f(a, b)` or
    /// `round(x, decimals=2)` (positional and/or keyword arguments).
    Call(Box<Expr>, Vec<Arg>),
    /// List literal, e.g. `[0.0, 4.0, 10.0, 21.0]`.
    List(Vec<Expr>),
    /// Word-array indexing `base[index]` (8-byte elements): base + index*8.
    Index(Box<Expr>, Box<Expr>),
    /// A struct literal `Name { field: value, ... }`. Lowered by `desugar` to a
    /// word array plus one assignment per field; only valid as a `let` initializer
    /// for now. Not parsed in condition position (avoids the `if x { }` ambiguity).
    StructLit(String, Vec<(String, Box<Expr>)>),
    /// Prefix unary operation, e.g. `not x`, `-x`.
    Unary(UnOp, Box<Expr>),
    /// Infix binary operation, e.g. `a >= b`, `a implies b`.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `if cond { .. } else { .. }`, an expression (its branches are blocks
    /// whose tail expression is the branch value).
    If {
        cond: Box<Expr>,
        then_branch: Box<Block>,
        else_branch: Option<Box<Block>>,
    },
    /// `match scrutinee { Variant => block, ... }`, a pattern match over a sum type.
    /// The compiler REQUIRES every variant of the enum to be covered (exhaustive);
    /// each arm is a block whose tail is that arm's value.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `expr as Type`, an explicit numeric conversion (integer truncate/extend, or
    /// `cvtsi2sd`/`cvttsd2si` between an integer and `f64`).
    Cast(Box<Expr>, Type),
}

/// One arm of a `match`: a variant name, an optional payload binding, and the block
/// to run for it. `binding` is `Some(x)` for `Some(x) => ..` (the variant carries a
/// value bound to `x` inside the arm), `None` for a payload-free variant `None => ..`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub variant: String,
    pub binding: Option<String>,
    pub body: Block,
}

/// One variant of a sum type: a name and an OPTIONAL payload type. `payload` is
/// `Some(ty)` for a data-carrying variant (`Some(i64)`), `None` for a bare tag
/// (`None`). A payloaded variant is the foundation of `Option`/`Result`, the
/// nil-safety mechanism that makes "forgot to check for absence" unrepresentable.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
}

/// A sum type: `enum Name { A, B(T), C }`, a value is exactly one of the variants,
/// each optionally carrying a payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub vis: Visibility,
    pub name: String,
    pub variants: Vec<EnumVariant>,
}

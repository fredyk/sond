//! Abstract syntax tree for Sond.
//!
//! Node types grow ONLY when a parser test requires them (TDD). Contracts,
//! effects and function bodies will be added in later cycles.

/// A type reference, e.g. `Money` or `Percent`.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A named type, e.g. `Money`.
    Named(String),
    /// A pointer to a struct, written `*Point`: a MUTABLE, shareable handle,
    /// declared up front (Sond has no `&` address-of). Same runtime representation
    /// as the value form (a base address); it differs only in the STATIC
    /// mutability rules: a value struct received as a parameter cannot have its
    /// fields mutated, a `*Point` can. A struct type is value XOR pointer.
    Ptr(Box<Type>),
    /// `u32[]`: a VIEW of `T` over memory the caller owns, with no length of its own.
    /// The runtime representation is the base address (an i64), exactly like an array
    /// parameter; what it adds is the ELEMENT TYPE, so `v[i]` indexes at the element's
    /// width instead of assuming a 64-bit word. This is what an image buffer needs: too
    /// big for the stack, and its length belongs to the caller, not to the declaration.
    ///
    /// `u32[n]` names the parameter that carries that length. It costs NOTHING at runtime
    /// (the value is still one word) — the name exists so the bounds checker has something
    /// to prove an index against, and so that proving it against a DIFFERENT buffer's
    /// length is a compile error rather than a corrupted image. Indexing the length-less
    /// form is rejected: there is nothing to check, and quietly not checking is how a view
    /// would have become the one hole in a language that rejects what it cannot prove.
    View(Box<Type>, Option<String>),
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
/// `Param` (a `name: Type` pair). Struct values give OS code real shape (a task
/// control block, a packet header, a socket) instead of flat arrays over globals.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub vis: Visibility,
    pub name: String,
    pub fields: Vec<Param>,
}

/// A function signature without a body: the interface a `port` method declares
/// (RFC-0014). It is exactly the head of a `Function` (name, params, return,
/// contracts and effects) with no `body`, `is_interrupt` or `is_wrapping`, which
/// are implementation concerns a port does not fix.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub vis: Visibility,
    pub name: String,
    pub params: Vec<Param>,
    pub ret_name: Option<String>,
    pub ret: Option<Type>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<Expr>,
    pub effects: Vec<String>,
}

/// A `port Name { <signatures> }`: a named interface (RFC-0014). A caller depends
/// on the port; a per-target `provides Name for T { .. }` implements it, and the
/// compiler splices the one provider matching the active target. The port's
/// contracts are obligations on every provider and guarantees to every caller.
#[derive(Debug, Clone, PartialEq)]
pub struct PortDef {
    pub vis: Visibility,
    pub name: String,
    pub methods: Vec<Signature>,
}

/// A `provides Port for Target { <functions> }`: one target's implementation of a
/// port (RFC-0014). `target` is a target-profile name (`pic16`, `x86_64_linux`,
/// ...). Resolution picks the single provider whose target matches the build and
/// splices its `methods`; each must match a signature of `port`, honouring its
/// contracts. Whether every port method is implemented is checked, not assumed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDef {
    pub port: String,
    pub target: String,
    pub methods: Vec<Function>,
}

/// A `use Port` directive: the program depends on `port`'s interface (RFC-0014).
/// Resolution splices the one `provides Port for <active target>` in scope; a
/// missing, ambiguous, or signature-incomplete provider is a compile error.
#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub port: String,
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
    /// The declared type, when there is one. `Some(Type::View(..))` is a module-level
    /// buffer (`let NAME: u8[32]`), which reserves its own storage in `.bss`.
    pub ty: Option<Type>,
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
    /// `wrapping fn`: the body's integer arithmetic is DEFINED to wrap mod 2^N, the crypto
    /// substrate (ChaCha, Poly1305, SHA, Keccak, X25519…), where overflow is the specification,
    /// not a bug. Overflow-checking is suppressed for this function, so the checked-by-default
    /// discipline can hold everywhere else without drowning crypto in per-op annotations.
    pub is_wrapping: bool,
    /// `test "name" { .. }`: a test block, lowered to a niladic function. It is NOT
    /// compiled into an ordinary program (only `sond test` gathers and runs them).
    pub is_test: bool,
    /// The human-readable name of a `test` block (the string after `test`), for the
    /// runner's report. `None` for ordinary functions.
    pub test_name: Option<String>,
    /// True for a function the COMPILER synthesized (the `sond test` harness `main`),
    /// never for one the author wrote. The harness is the only code allowed to reference
    /// the four reserved `__cov_*` coverage symbols, which `check_undeclared_variables`
    /// predeclares for it alone; author code may neither read nor bind that prefix.
    pub is_harness: bool,
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
    /// The 1-based source line of each statement in `stmts` (parallel to it), for the RFC-0016
    /// `--debug` checkpoint. Empty for synthesized blocks (which carry no statements). May be
    /// shorter than `stmts` for blocks built without line info; a consumer treats a missing entry
    /// as line 0.
    pub stmt_lines: Vec<u32>,
}

impl Block {
    /// Appends a statement together with the source line it came from.
    ///
    /// `stmts` and `stmt_lines` are parallel BY INDEX, and a plain `stmts.push` leaves the second
    /// short: `check.rs` then reads the line with `.get(i).unwrap_or(line)` and quietly falls back
    /// to the enclosing statement's line. That is how a synthetic statement inserted by the `for`
    /// desugaring pushed every later line off by one and the error pointed at the `for` instead of
    /// at the offending line. Go through here and the two cannot come apart.
    ///
    /// A block that still carries a `tail` has one line already parked for it, so `stmt_lines` may
    /// legitimately run one ahead; the fill below is what puts them back in step.
    pub fn push_stmt(&mut self, stmt: Stmt, line: u32) {
        self.stmts.push(stmt);
        while self.stmt_lines.len() < self.stmts.len() {
            self.stmt_lines.push(line);
        }
    }

    /// Inserts a statement at `i`, moving its line along with it.
    pub fn insert_stmt(&mut self, i: usize, stmt: Stmt, line: u32) {
        self.stmts.insert(i, stmt);
        self.stmt_lines.insert(i, line);
    }
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
        /// `for i in a..b` was parsed as `i = a; for i < b { ... }`.
        /// True for the desugared loop; false for ordinary conditionals and foreach.
        is_range_for: bool,
    },
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// An expression used as a statement (its value is discarded).
    Expr(Expr),
    /// `spawn f(args)`: run the call on a new OS thread (RFC-0010 Layer B). The
    /// enclosing scope joins every thread it spawned before it returns, so the
    /// inner expression is always a call, never an arbitrary expression.
    Spawn(Expr),
    /// `par a() ~~ b() ~~ c()`: run the peers concurrently, each on its own OS
    /// thread, and JOIN THEM ALL before the next statement (RFC-0008 §5.1a, the
    /// inline `par` form over the RFC-0010 Layer B substrate). The `~~` is a flat,
    /// n-ary peer delimiter, not an infix operator: horizontal position carries no
    /// order. Each peer is a call, like `spawn`. Result binding and the independence
    /// proof are later slices; this form runs effectful peers in parallel and joins.
    Par(Vec<Expr>),
    /// `let a ~~ b ~~ c = par f() ~~ g() ~~ h()`: the POSITIONAL result binding of
    /// `par`. Each peer runs on its own thread; after the join, each result is bound
    /// to its name BY SOURCE POSITION (`a` is always `f`'s result, regardless of which
    /// thread finished first), so the binding is deterministic and the caller may then
    /// combine the results with any operator. `names.len()` must equal `peers.len()`.
    ParLet {
        mutable: bool,
        names: Vec<String>,
        peers: Vec<Expr>,
    },
    /// `let s = par(+) f() ~~ g() ~~ h()`: the parallel REDUCE. The peers run concurrently and
    /// their results are folded with `op` into a single value bound to `name`. Because the peers
    /// finish in an unspecified order, `op` MUST be commutative and associative (a commutative
    /// monoid: `+ * & | ^`), so the result is identical under every schedule (RFC-0008 §5.3).
    /// A non-commutative reducer (`-`, `/`) is a compile error.
    ParReduce {
        mutable: bool,
        name: String,
        op: BinOp,
        peers: Vec<Expr>,
    },
    /// `let r = graph { node a = e1  node b = e2  node c = merge(a, b)  yield c }` (RFC-0008 §5.1).
    /// A dependency graph: each `node` is a single-assignment binding whose only ordering is the
    /// data dependency on the other nodes it references. The block evaluates to `yield_expr`, bound
    /// to `name`. The nodes are executed in a topological order of their dependencies (a cycle is a
    /// compile error); independent nodes are free to run concurrently.
    GraphLet {
        mutable: bool,
        name: String,
        nodes: Vec<(String, Expr)>,
        yield_expr: Expr,
    },
    /// `lock (m) { .. }`: acquires a futex-based mutex at `m`, runs the body (as a lexical
    /// critical section), and releases the mutex unconditionally on any exit path (normal,
    /// return, break). `m` is an lvalue reference to a single 64-bit word used as the futex.
    Lock {
        name: String,
        body: Block,
    },
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
    /// A slice range `base[lo:hi]` (Python-style): a sub-`Slice` VIEW over `base`, no copy.
    /// `lo`/`hi` are optional (`base[:hi]`, `base[lo:]`, `base[:]`); lowered by `desugar` to
    /// `Slice { ptr: base.ptr + lo*8, len: hi - lo }`.
    SliceRange(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>),
    /// A struct literal `Name { field: value, ... }`. Lowered by `desugar` to a
    /// word array plus one assignment per field; only valid as a `let` initializer
    /// for now. Not parsed in condition position (avoids the `if x { }` ambiguity).
    StructLit(String, Vec<(String, Box<Expr>)>),
    /// Prefix unary operation, e.g. `not x`, `-x`.
    Unary(UnOp, Box<Expr>),
    /// Infix binary operation, e.g. `a >= b`, `a implies b`.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `if cond { .. } else { .. }`: an expression (its branches are blocks
    /// whose tail expression is the branch value).
    If {
        cond: Box<Expr>,
        then_branch: Box<Block>,
        else_branch: Option<Box<Block>>,
    },
    /// `match scrutinee { Variant => block, ... }`: a pattern match over a sum type.
    /// The compiler REQUIRES every variant of the enum to be covered (exhaustive);
    /// each arm is a block whose tail is that arm's value.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `expr as Type`: an explicit numeric conversion (integer truncate/extend, or
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
/// (`None`). A payloaded variant is the foundation of `Option`/`Result`: the
/// nil-safety mechanism that makes "forgot to check for absence" unrepresentable.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
}

/// A sum type: `enum Name { A, B(T), C }`: a value is exactly one of the variants,
/// each optionally carrying a payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub vis: Visibility,
    pub name: String,
    pub variants: Vec<EnumVariant>,
}

#[cfg(test)]
mod block_tests {
    use super::*;

    fn stmt() -> Stmt {
        Stmt::Continue
    }

    /// `push_stmt` takes a line because the caller knows it; it must be the line that lands in
    /// `stmt_lines`, not a placeholder. Written with a `0` there, the method compiled with an
    /// unused-parameter warning and got away with it: every caller happened to run on a block
    /// whose `stmt_lines` was already one ahead (blocks with a tail), so the fill never ran. On a
    /// block WITHOUT a tail it ran, and wrote a zero.
    #[test]
    fn push_stmt_records_the_line_it_was_given() {
        let mut b = Block::default();
        b.push_stmt(stmt(), 41);
        b.push_stmt(stmt(), 42);
        assert_eq!(
            b.stmt_lines,
            vec![41, 42],
            "the line passed in must be the line stored"
        );
    }

    /// The two vectors are parallel BY INDEX; every entry point must leave them the same length.
    #[test]
    fn both_entry_points_keep_the_two_vectors_in_step() {
        let mut b = Block::default();
        b.push_stmt(stmt(), 10);
        b.insert_stmt(0, stmt(), 9);
        b.push_stmt(stmt(), 11);
        assert_eq!(b.stmts.len(), b.stmt_lines.len());
        assert_eq!(b.stmt_lines, vec![9, 10, 11], "insert must place its line too");
    }
}

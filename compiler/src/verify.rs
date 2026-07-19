//! Static contract verification, the point of Sond's contracts.
//!
//! A runtime check is just an assert: it passes for the inputs you happened to
//! try and blows up months later, in production, on the one you didn't. The whole
//! promise of Sond is that a contract holds for EVERY input, *proven at compile
//! time*, so a wrong body is rejected before it ever runs.
//!
//! This is the first honest slice of that prover. It symbolically evaluates
//! straight-line function bodies over their parameters and decides the
//! postconditions in the decidable fragment (integer add/sub/mul and
//! comparisons). A postcondition that is provably false is a COMPILE ERROR.
//! Bodies it cannot yet reason about (loops, calls) are left to the runtime guard
//! and to a fuller prover (Fourier–Motzkin / SMT) later, never silently passed
//! off as "verified".

use crate::ast::{BinOp, Block, Expr, Function, Param, Stmt, Type};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A symbolic integer value expressed in terms of the parameters.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Sym {
    Const(i64),
    Var(String),
    Add(Box<Sym>, Box<Sym>),
    Sub(Box<Sym>, Box<Sym>),
    Mul(Box<Sym>, Box<Sym>),
}

/// A comparison proposition over symbolic values (`lhs <op> rhs`).
#[derive(Clone)]
struct SymProp {
    op: BinOp,
    lhs: Sym,
    rhs: Sym,
}

enum Outcome {
    Proven,
    Disproven,
    Unknown,
}

/// A contract proven false at compile time.
#[derive(Debug)]
pub struct VerifyError {
    pub message: String,
}

/// Verify every function's contract. `Err` if any postcondition is provably false
/// for an input the precondition allows, the program is rejected, not run.
pub fn verify(funcs: &[Function]) -> Result<(), VerifyError> {
    // The contract of every function, so a caller can verify modularly against a callee's
    // `requires`/`ensures` (its summary) without re-analysing the callee's body.
    let contracts: HashMap<String, &Function> =
        funcs.iter().map(|f| (f.name.clone(), f)).collect();
    for f in funcs {
        verify_function(f, &contracts)?;
    }
    Ok(())
}

/// Threaded state for the path evaluator: the callee contracts (for inter-procedural
/// summaries) and a counter that mints fresh, distinct symbols for call results.
struct Ctx<'a> {
    contracts: &'a HashMap<String, &'a Function>,
    fresh: usize,
    /// Parameters typed `f64`. Float division is IEEE-754 (`±inf`/`NaN`, never a trap), so
    /// the divide-by-zero obligation is raised for INTEGER division only, a divisor known
    /// to be a float is exempt.
    float_vars: HashSet<String>,
    /// Parameters typed `u64`. Division/modulo of unsigned operands emits `divq` (never
    /// `idivq`), which cannot overflow the quotient, so the `i64::MIN / -1` obligation is
    /// raised for SIGNED integer division only, a division with any unsigned operand is
    /// exempt (codegen combines types "unsigned wins").
    unsigned_vars: HashSet<String>,
    /// The inclusive `[min, max]` range (as i128) of each integer-typed parameter's declared
    /// type. Overflow reasoning bounds a `+`/`-`/`*` against its result type's range, `i32 + 1`
    /// must fit in `[i32::MIN, i32::MAX]`, not merely in i64, so a narrow-width sum that leaves
    /// its type but stays inside i64 is correctly rejected.
    int_types: HashMap<String, (i128, i128)>,
    /// The function's preconditions as constraints, available to reasoning done during symbolic
    /// evaluation (e.g. the div/mod sign bounds), which the per-path `cons` does not yet carry.
    requires: Vec<SymProp>,
    /// Why the evaluator left the verifiable fragment, if it did. The evaluator bails by
    /// returning `None`; this records the FIRST (deepest) reason so the diagnostic names the
    /// actual construct, `a `for` loop without an `invariant``, instead of a generic list.
    bail: Option<String>,
}

impl Ctx<'_> {
    /// A fresh symbolic variable for an opaque value (a call result), distinct from all
    /// parameters and from every other call result.
    fn fresh_var(&mut self) -> Sym {
        self.fresh += 1;
        Sym::Var(format!("__call{}", self.fresh))
    }

    /// Records why the body left the verifiable fragment and returns `None` to bail. The
    /// first reason wins (it is the deepest, most specific construct); propagated `None`s up
    /// the call stack do not overwrite it.
    fn bail<T>(&mut self, reason: &str) -> Option<T> {
        if self.bail.is_none() {
            self.bail = Some(reason.to_string());
        }
        None
    }
}

fn verify_function(
    f: &Function,
    contracts: &HashMap<String, &Function>,
) -> Result<(), VerifyError> {
    let reject = |msg: String| {
        Err(VerifyError {
            message: format!("function `{}`: {msg}", f.name),
        })
    };
    // Seed the environment with the parameters as free symbolic variables.
    let env: HashMap<String, Sym> = f
        .params
        .iter()
        .map(|p| (p.name.clone(), Sym::Var(p.name.clone())))
        .collect();
    // The preconditions, as constraints. Held in the Ctx so reasoning DONE DURING symbolic
    // evaluation can use them (e.g. deciding a divisor is positive, so `a / b` and `a % b` gain
    // their sign/range bounds), not only when the postcondition is discharged afterwards. They
    // are deliberately NOT seeded into the path constraints, so an obligation's recorded
    // assumptions stay the branch conditions alone (a clean diagnostic path clause).
    let requires: Vec<SymProp> = f
        .requires
        .iter()
        .filter_map(|e| sym_prop(e, &env))
        .collect();
    let init = State {
        env: env.clone(),
        cons: Vec::new(),
        obligations: Vec::new(),
    };
    let mut ctx = Ctx {
        contracts,
        fresh: 0,
        float_vars: f
            .params
            .iter()
            .filter(|p| matches!(&p.ty, Type::Named(n) if n == "f64"))
            .map(|p| p.name.clone())
            .collect(),
        unsigned_vars: f
            .params
            .iter()
            .filter(|p| matches!(&p.ty, Type::Named(n) if n == "u64"))
            .map(|p| p.name.clone())
            .collect(),
        int_types: f
            .params
            .iter()
            .filter_map(|p| match &p.ty {
                Type::Named(n) => int_range(n).map(|r| (p.name.clone(), r)),
                _ => None,
            })
            .collect(),
        requires: requires.clone(),
        bail: None,
    };
    let paths = eval_block_paths(&f.body, init, &mut ctx);
    // Every function is checked for its safety obligations (nonzero divisors, callee
    // preconditions), even without a postcondition. But if the body is OUTSIDE the
    // verifiable fragment (an unanalyzable loop, an early return, …), those obligations
    // cannot be gathered: a function with a contract is then rejected (prove it or it does
    // not compile), while a contract-free one is left as-is, no regression on code the
    // verifier simply cannot yet reason about.
    let Some(paths) = paths else {
        if f.ensures.is_empty() {
            return Ok(());
        }
        let detail = ctx.bail.clone().unwrap_or_else(|| {
            "the body uses a construct the verifier does not support yet".to_string()
        });
        return reject(format!(
            "cannot prove its contract, {detail}. Sond does not accept a contract it \
             cannot verify."
        ));
    };
    for mut path in paths {
        if let Some(rn) = &f.ret_name {
            path.env.insert(rn.clone(), path.ret.clone());
        }
        // Discharge every obligation (a nonzero divisor, a callee precondition) under the
        // precondition plus the assumptions in scope where it arose.
        for ob in &path.obligations {
            let mut asm = requires.clone();
            asm.extend(ob.assumptions.iter().cloned());
            if !obligation_holds(ob, &asm, &ctx.int_types) {
                return reject(format!(
                    "cannot prove {}{}{}",
                    ob.what,
                    path_where(&ob.assumptions),
                    counterexample_clause_alt(&ob.prop, ob.alt.as_ref(), &asm, &ctx.int_types)
                ));
            }
        }
        // Prove every postcondition on this path (if the function has any): the precondition
        // plus the path's branch conditions are the assumptions, the return value binds the
        // named return. Capture the path's branch conditions first, they name the case the
        // proof fails on (the red→green loop needs WHICH path, not just which contract).
        let where_clause = path_where(&path.cons);
        let mut assumptions = requires.clone();
        assumptions.extend(path.cons);
        for e in &f.ensures {
            let Some(p) = sym_prop(e, &path.env) else {
                return reject("cannot verify a postcondition in this form".to_string());
            };
            match prove(&p, &assumptions, &ctx.int_types) {
                Outcome::Proven => {}
                Outcome::Disproven => {
                    return reject(format!(
                        "postcondition `{}` is false{}{}",
                        show_prop(&p),
                        where_clause,
                        counterexample_clause(&p, &assumptions, &ctx.int_types)
                    ));
                }
                Outcome::Unknown => {
                    return reject(format!(
                        "cannot prove postcondition `{}`{}{}, Sond rejects a contract it \
                         cannot verify (add the assumptions it needs, or strengthen the prover)",
                        show_prop(&p),
                        where_clause,
                        counterexample_clause(&p, &assumptions, &ctx.int_types)
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A proof obligation raised inside a body: a proposition that must be provable under the
/// assumptions live where it arose (a snapshot of the path conditions), plus a
/// human-readable description of what it guards (a call precondition, a nonzero divisor).
#[derive(Clone)]
struct Obligation {
    assumptions: Vec<SymProp>,
    prop: SymProp,
    /// An ALTERNATIVE proposition: proving EITHER `prop` or `alt` discharges the obligation.
    /// This expresses a disjunctive safety condition. The signed-division overflow guard
    /// (`i64::MIN / -1`) is safe when `divisor != -1` OR `dividend != i64::MIN`, so `prop`
    /// is the first and `alt` the second; proving either rules the overflow out. `None` for
    /// the plain single-condition obligations (a nonzero divisor, a callee precondition).
    alt: Option<SymProp>,
    what: String,
}

/// One execution path through a function body: the variable environment at its end, the
/// branch conditions taken to reach it (as assumptions), and the proof obligations raised
/// along the way (call preconditions, nonzero divisors).
#[derive(Clone)]
struct State {
    env: HashMap<String, Sym>,
    cons: Vec<SymProp>,
    obligations: Vec<Obligation>,
}

/// A completed path: its final state plus the symbolic value the block/expression yields.
struct Path {
    env: HashMap<String, Sym>,
    cons: Vec<SymProp>,
    obligations: Vec<Obligation>,
    ret: Sym,
}

/// A conservative cap on the number of paths through a body (branches multiply). Past it,
/// the evaluator bails (rejects) rather than risk an exponential blow-up, sound.
const MAX_PATHS: usize = 256;

/// Evaluates a block to the set of paths through it, one per branch combination, each
/// carrying its branch conditions, call obligations and the symbolic value it yields.
/// `None` if the body leaves the verifiable fragment (a loop, an early return, a field
/// assignment, an uncontracted call): sound by default, the caller then rejects.
fn eval_block_paths(block: &Block, state: State, ctx: &mut Ctx) -> Option<Vec<Path>> {
    let states = eval_stmts(block, state, ctx)?;
    // The tail expression is the block's value; evaluate it on every state, forking again
    // if it is itself a branch. A value block must have a tail.
    let Some(tail) = block.tail.as_ref() else {
        return ctx.bail("a block used as a value with no tail expression");
    };
    let mut paths = Vec::new();
    for st in states {
        for (st2, ret) in eval_expr_paths(tail, st, ctx)? {
            paths.push(Path {
                env: st2.env,
                cons: st2.cons,
                obligations: st2.obligations,
                ret,
            });
            if paths.len() > MAX_PATHS {
                return ctx.bail("too many branch paths (the body exceeds the analysis cap)");
            }
        }
    }
    Some(paths)
}

/// Evaluates a block's STATEMENTS to the set of end states (one per branch combination),
/// ignoring any tail. The shared core of value-blocks (which then evaluate the tail) and
/// loop bodies (which have no value). `None` if it leaves the verifiable fragment.
fn eval_stmts(block: &Block, state: State, ctx: &mut Ctx) -> Option<Vec<State>> {
    let mut states = vec![state];
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                states = fork_bind(states, name, value, ctx)?;
            }
            Stmt::Assign {
                target: Expr::Var(name),
                value,
            } => {
                states = fork_bind(states, name, value, ctx)?;
            }
            // A field assignment or non-variable target is undecidable here.
            Stmt::Assign { .. } => {
                return ctx.bail("an assignment to a field or non-variable target");
            }
            // A statement-position `if` (or `match`) is evaluated for its EFFECTS, the mutations
            // its branches make and the obligations they raise, NOT ignored. Ignoring it was
            // unsound: a branch that assigns (e.g. `if c { x = -1 }`) would silently not update the
            // state, so an invariant a branch violates was spuriously "preserved". Its value is
            // discarded; a missing `else` is a no-op branch carrying only the negated guard.
            Stmt::Expr(e @ (Expr::If { .. } | Expr::Match { .. })) => {
                states = eval_effect(e, states, ctx)?;
            }
            // Any other statement-position expression (a value, or an uncontracted call like
            // `print_int`) has no effect on the scalar state the prover tracks, Sond has no
            // reference-to-local, so a call cannot reassign a caller's local, and no contract to
            // enforce, so it is soundly discarded.
            Stmt::Expr(_) => {}
            // A loop is verified via its invariant (init + preservation), then continues
            // with (invariant ∧ ¬condition). Without an invariant it is unverifiable.
            Stmt::For {
                cond,
                invariant,
                body,
            } if !invariant.is_empty() => {
                states = eval_loop(states, cond.as_ref(), invariant, body, ctx)?;
            }
            Stmt::For { .. } => {
                return ctx.bail("a `for` loop without an `invariant { .. }` clause");
            }
            Stmt::Return(_) => return ctx.bail("an early `return`"),
            Stmt::Break => return ctx.bail("a `break`"),
            Stmt::Continue => return ctx.bail("a `continue`"),
        }
        if states.len() > MAX_PATHS {
            return ctx.bail("too many branch paths (the body exceeds the analysis cap)");
        }
    }
    Some(states)
}

/// Evaluates a statement-position `if`/`match` for its EFFECTS across every incoming state,
/// discarding the value. Each `if` forks: the then-branch runs under the guard, the else-branch
/// (an empty no-op when absent) under its negation, and both continuations carry forward so a
/// later statement sees the path-specific facts. `None` if a branch leaves the fragment.
fn eval_effect(e: &Expr, states: Vec<State>, ctx: &mut Ctx) -> Option<Vec<State>> {
    let Expr::If {
        cond,
        then_branch,
        else_branch,
    } = e
    else {
        // A statement-position `match` is not yet modelled; reject rather than ignore (sound).
        return ctx.bail("a `match` in statement position");
    };
    let mut out = Vec::new();
    for st in states {
        let (then_props, else_props) = cond_props(cond, &st.env);
        let mut then_state = st.clone();
        then_state.cons.extend(then_props);
        out.extend(eval_body(then_branch, then_state, ctx)?);
        let mut else_state = st;
        else_state.cons.extend(else_props);
        match else_branch {
            Some(eb) => out.extend(eval_body(eb, else_state, ctx)?),
            None => out.push(else_state),
        }
    }
    Some(out)
}

/// Evaluates a block executed for effect only (a loop body): its statements, then its
/// tail if any, the tail's value is discarded, but a call in it still contributes its
/// obligations and summary. Returns the end states.
fn eval_body(block: &Block, state: State, ctx: &mut Ctx) -> Option<Vec<State>> {
    let states = eval_stmts(block, state, ctx)?;
    let Some(tail) = &block.tail else {
        return Some(states);
    };
    let mut out = Vec::new();
    for st in states {
        for (st2, _discarded) in eval_expr_paths(tail, st, ctx)? {
            out.push(st2);
        }
    }
    Some(out)
}

/// Binds `name` to `value` across every current state, forking when `value` is a branch.
fn fork_bind(
    states: Vec<State>,
    name: &str,
    value: &Expr,
    ctx: &mut Ctx,
) -> Option<Vec<State>> {
    let mut next = Vec::new();
    for st in states {
        for (mut st2, v) in eval_expr_paths(value, st, ctx)? {
            st2.env.insert(name.to_string(), v);
            next.push(st2);
        }
    }
    Some(next)
}

/// Evaluates an expression to `(state, value)` pairs, threading the state so a call can
/// add its summary and obligations. Forks on `if` (each branch a path, its guard added to
/// that path's conditions); resolves a contracted call to a fresh result variable
/// constrained by the callee's `ensures`, recording its `requires` as an obligation.
/// `None` if the expression uses an unsupported construct.
fn eval_expr_paths(e: &Expr, state: State, ctx: &mut Ctx) -> Option<Vec<(State, Sym)>> {
    match e {
        Expr::Int(n) => Some(vec![(state, Sym::Const(*n))]),
        Expr::Var(name) => {
            let v = state.env.get(name)?.clone();
            Some(vec![(state, v)])
        }
        Expr::Binary(op, l, r) if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) => {
            let mk = match op {
                BinOp::Add => Sym::Add as fn(Box<Sym>, Box<Sym>) -> Sym,
                BinOp::Sub => Sym::Sub,
                _ => Sym::Mul,
            };
            let mut out = Vec::new();
            for (s1, lv) in eval_expr_paths(l, state, ctx)? {
                for (s2, rv) in eval_expr_paths(r, s1, ctx)? {
                    out.push((s2, simplify(mk(Box::new(lv.clone()), Box::new(rv)))));
                }
            }
            Some(out)
        }
        // Division / modulo: raise the obligation that the divisor is nonzero (the
        // divide-by-zero guard). A constant/constant folds precisely; otherwise the result
        // is opaque (division is non-linear), but the guard is still enforced.
        Expr::Binary(op, l, r) if matches!(op, BinOp::Div | BinOp::Mod) => {
            // IEEE-754 float division never traps, only integer division must be guarded.
            let is_float = expr_is_float(r, &ctx.float_vars);
            // Signed integer division (`idivq`) also traps on `i64::MIN / -1` (quotient
            // overflow, #DE). It is UNSIGNED, and so exempt, when either operand is `u64`
            // (codegen combines types "unsigned wins"); float is exempt too.
            let is_signed_int = !is_float
                && !expr_is_unsigned(l, &ctx.unsigned_vars)
                && !expr_is_unsigned(r, &ctx.unsigned_vars);
            let mut out = Vec::new();
            for (s1, lv) in eval_expr_paths(l, state, ctx)? {
                for (mut s2, rv) in eval_expr_paths(r, s1, ctx)? {
                    let rv = simplify(rv);
                    if !is_float {
                        s2.obligations.push(Obligation {
                            assumptions: s2.cons.clone(),
                            prop: SymProp {
                                op: BinOp::Ne,
                                lhs: rv.clone(),
                                rhs: Sym::Const(0),
                            },
                            alt: None,
                            what: format!("the divisor `{}` is nonzero", show_sym(&rv)),
                        });
                    }
                    if is_signed_int {
                        // Overflow guard: `i64::MIN / -1` overflows the quotient. Safe when
                        // the divisor is not -1 OR the dividend is not i64::MIN, proving
                        // either discharges it.
                        s2.obligations.push(Obligation {
                            assumptions: s2.cons.clone(),
                            prop: SymProp {
                                op: BinOp::Ne,
                                lhs: rv.clone(),
                                rhs: Sym::Const(-1),
                            },
                            alt: Some(SymProp {
                                op: BinOp::Ne,
                                lhs: lv.clone(),
                                rhs: Sym::Const(i64::MIN),
                            }),
                            what: format!(
                                "the signed division `{} {} {}` does not overflow (i64::MIN {} -1)",
                                show_sym(&lv),
                                if matches!(op, BinOp::Div) { "/" } else { "%" },
                                show_sym(&rv),
                                if matches!(op, BinOp::Div) { "/" } else { "%" },
                            ),
                        });
                    }
                    let value = match (&lv, &rv) {
                        (Sym::Const(a), Sym::Const(b)) if *b != 0 => Sym::Const(match op {
                            BinOp::Div => a.wrapping_div(*b),
                            _ => a.wrapping_rem(*b),
                        }),
                        _ => {
                            // The result is opaque (division is non-linear), BUT integer `/` and
                            // `%` carry provable sign/magnitude bounds. When the divisor is
                            // positive and the dividend non-negative, teach the prover them:
                            // `0 <= a / b <= a` and `0 <= a % b < b`. Sound for truncated integer
                            // arithmetic, and only ADDS facts (never rejects), this is what lets
                            // `i / k` stay non-negative and `i % n` be a valid index into a
                            // size-n array.
                            let q = ctx.fresh_var();
                            let cmp = |op, lhs: &Sym, rhs: Sym| SymProp {
                                op,
                                lhs: lhs.clone(),
                                rhs,
                            };
                            // The facts in scope: the preconditions plus this path's branch
                            // conditions (the per-path `cons` does not carry the preconditions).
                            let mut asm = ctx.requires.clone();
                            asm.extend(s2.cons.iter().cloned());
                            let pos_divisor = matches!(
                                prove(&cmp(BinOp::Gt, &rv, Sym::Const(0)), &asm, &ctx.int_types),
                                Outcome::Proven
                            );
                            let nonneg_dividend = matches!(
                                prove(&cmp(BinOp::Ge, &lv, Sym::Const(0)), &asm, &ctx.int_types),
                                Outcome::Proven
                            );
                            if pos_divisor && nonneg_dividend {
                                s2.cons.push(cmp(BinOp::Ge, &q, Sym::Const(0)));
                                if matches!(op, BinOp::Div) {
                                    s2.cons.push(cmp(BinOp::Le, &q, lv.clone())); // a/b <= a
                                } else {
                                    s2.cons.push(cmp(BinOp::Lt, &q, rv.clone())); // a%b < b
                                }
                            }
                            q
                        }
                    };
                    out.push((s2, value));
                }
            }
            Some(out)
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            // A value `if` needs both arms (a missing else has no value to reason about).
            let Some(else_b) = else_branch.as_ref() else {
                return ctx.bail("an `if` used as a value without an `else` branch");
            };
            let (then_props, else_props) = cond_props(cond, &state.env);
            let mut out = Vec::new();
            for (branch, props) in [(then_branch.as_ref(), then_props), (else_b, else_props)] {
                let mut st = state.clone();
                st.cons.extend(props);
                for path in eval_block_paths(branch, st, ctx)? {
                    out.push((
                        State {
                            env: path.env,
                            cons: path.cons,
                            obligations: path.obligations,
                        },
                        path.ret,
                    ));
                }
            }
            Some(out)
        }
        Expr::Call(callee, args) => eval_call(callee, args, state, ctx),
        // Left shift is exact multiplication by a power of two: `e << k` == `e * 2^k`. Model it as
        // that product so a bounded operand keeps a provable (overflow-gated) bound.
        Expr::Binary(BinOp::Shl, l, r) => {
            let mut out = Vec::new();
            for (s1, lv) in eval_expr_paths(l, state, ctx)? {
                for (s2, rv) in eval_expr_paths(r, s1, ctx)? {
                    let value = match simplify(rv) {
                        Sym::Const(k) if (0..63).contains(&k) => {
                            simplify(Sym::Mul(Box::new(Sym::Const(1i64 << k)), Box::new(lv.clone())))
                        }
                        _ => ctx.fresh_var(),
                    };
                    out.push((s2, value));
                }
            }
            Some(out)
        }
        // Right shift `e >> k` / `e >>> k` = `e / 2^k` for a non-negative `e`: the result is opaque
        // (floor division is non-linear) but carries the same sign/magnitude facts as `/` -
        // `0 <= q`, `2^k*q <= e`, `e < 2^k*(q+1)`, which a bounded operand turns into a tight
        // range (`x < 256` gives `x >> 3 < 32`). Without a non-negative operand the facts do not
        // hold (an arithmetic `>>` of a negative value is negative), so they are only added then.
        Expr::Binary(BinOp::Shr | BinOp::LShr, l, r) => {
            let mut out = Vec::new();
            for (s1, lv) in eval_expr_paths(l, state, ctx)? {
                for (mut s2, rv) in eval_expr_paths(r, s1, ctx)? {
                    let q = ctx.fresh_var();
                    if let Sym::Const(k) = simplify(rv)
                        && (0..63).contains(&k)
                    {
                        let mut asm = ctx.requires.clone();
                        asm.extend(s2.cons.iter().cloned());
                        let nonneg = matches!(
                            prove(
                                &SymProp {
                                    op: BinOp::Ge,
                                    lhs: lv.clone(),
                                    rhs: Sym::Const(0)
                                },
                                &asm,
                                &ctx.int_types
                            ),
                            Outcome::Proven
                        );
                        if nonneg {
                            let pow = Sym::Const(1i64 << k);
                            let scaled =
                                simplify(Sym::Mul(Box::new(pow.clone()), Box::new(q.clone())));
                            s2.cons.push(SymProp {
                                op: BinOp::Ge,
                                lhs: q.clone(),
                                rhs: Sym::Const(0),
                            });
                            s2.cons.push(SymProp {
                                op: BinOp::Le,
                                lhs: scaled.clone(),
                                rhs: lv.clone(),
                            });
                            s2.cons.push(SymProp {
                                op: BinOp::Lt,
                                lhs: lv.clone(),
                                rhs: simplify(Sym::Add(Box::new(scaled), Box::new(pow))),
                            });
                        }
                    }
                    out.push((s2, q));
                }
            }
            Some(out)
        }
        // Bitwise AND with a non-negative constant mask confines the result to `[0, mask]` for ANY
        // operand, the low-bits idiom (`x & 0xFF`), sound regardless of the operand's sign.
        Expr::Binary(BinOp::BitAnd, l, r) => {
            let mut out = Vec::new();
            for (s1, lv) in eval_expr_paths(l, state, ctx)? {
                for (mut s2, rv) in eval_expr_paths(r, s1, ctx)? {
                    let q = ctx.fresh_var();
                    let mask = match (simplify(lv.clone()), simplify(rv)) {
                        (Sym::Const(m), _) | (_, Sym::Const(m)) => Some(m),
                        _ => None,
                    };
                    if let Some(m) = mask
                        && m >= 0
                    {
                        s2.cons.push(SymProp {
                            op: BinOp::Ge,
                            lhs: q.clone(),
                            rhs: Sym::Const(0),
                        });
                        s2.cons.push(SymProp {
                            op: BinOp::Le,
                            lhs: q.clone(),
                            rhs: Sym::Const(m),
                        });
                    }
                    out.push((s2, q));
                }
            }
            Some(out)
        }
        // An array or field READ yields an arbitrary value the linear engine cannot model, so it
        // is an opaque fresh variable, NOT a fragment exit. The base and index are still
        // evaluated for their own obligations (a division in the index must be proven safe) and to
        // fork on any branch they contain. Each read is a DISTINCT fresh value: two reads are not
        // assumed equal (sound, the array/struct may have changed between them).
        Expr::Index(base, index) => {
            let mut out = Vec::new();
            for (s1, _) in eval_expr_paths(base, state, ctx)? {
                for (s2, _) in eval_expr_paths(index, s1, ctx)? {
                    out.push((s2, ctx.fresh_var()));
                }
            }
            Some(out)
        }
        Expr::Field(base, _) => {
            let mut out = Vec::new();
            for (s2, _) in eval_expr_paths(base, state, ctx)? {
                out.push((s2, ctx.fresh_var()));
            }
            Some(out)
        }
        // a comparison used as a value, a cast, a list/struct literal, `|`/`^`, etc.: out of the
        // fragment (no linear model).
        _ => ctx.bail("an unsupported expression (a comparison used as a value, or an unsupported operator)"),
    }
}

/// Verifies a loop by its invariant (Hoare's while rule) and returns the continuation
/// state for each incoming path. For each path: (1) INIT, the invariant holds now; (2)
/// PRESERVATION, assuming the invariant and the loop condition, one body iteration
/// re-establishes the invariant (over fresh values of the variables it modifies); then
/// the continuation HAVOCs those variables and assumes `invariant ∧ ¬condition`. `None`
/// (reject) if init or preservation cannot be proved, or the body leaves the fragment.
fn eval_loop(
    states: Vec<State>,
    cond: Option<&Expr>,
    invariant: &[Expr],
    body: &Block,
    ctx: &mut Ctx,
) -> Option<Vec<State>> {
    let modified = modified_vars(body);
    let int_types = ctx.int_types.clone();
    let mut out = Vec::new();
    for st in states {
        // (1) INIT: every invariant proposition holds under the current assumptions.
        for inv in invariant {
            let p = sym_prop(inv, &st.env)?;
            if !matches!(prove(&p, &st.cons, &int_types), Outcome::Proven) {
                return ctx.bail("a loop invariant that does not hold on entry, check the `invariant { .. }`");
            }
        }
        // (2) PRESERVATION: havoc the modified variables to arbitrary values, assume the
        // invariant and the loop condition, run one iteration, and re-prove the invariant.
        let mut henv = st.env.clone();
        for v in &modified {
            henv.insert(v.clone(), ctx.fresh_var());
        }
        let inv_here: Vec<SymProp> = invariant.iter().filter_map(|e| sym_prop(e, &henv)).collect();
        if inv_here.len() != invariant.len() {
            return ctx.bail("a loop invariant proposition outside the verifiable fragment");
        }
        let mut pres_cons = inv_here;
        if let Some(c) = cond {
            pres_cons.extend(cond_props(c, &henv).0); // the condition holds inside the loop
        }
        let pres_state = State {
            env: henv,
            cons: pres_cons,
            obligations: Vec::new(),
        };
        for bs in eval_body(body, pres_state, ctx)? {
            // Any obligation inside the body (call precondition, nonzero divisor) must hold
            // under the assumptions live where it arose.
            for ob in &bs.obligations {
                if !obligation_holds(ob, &ob.assumptions, &int_types) {
                    return ctx.bail("a safety obligation inside the loop body that could not be proved");
                }
            }
            // The invariant must be re-established at the end of the iteration.
            for inv in invariant {
                let p = sym_prop(inv, &bs.env)?;
                if !matches!(prove(&p, &bs.cons, &int_types), Outcome::Proven) {
                    return ctx.bail("a loop invariant not preserved by the body, strengthen the `invariant { .. }`");
                }
            }
        }
        // (3) CONTINUATION: the loop is done. Facts that mention a modified variable spoke
        // of its pre-loop value, so they no longer describe it, drop them; the modified
        // variables become fresh, constrained only by the invariant and the negated guard.
        let mut cont = st;
        cont.cons.retain(|p| !prop_mentions(p, &modified));
        for v in &modified {
            cont.env.insert(v.clone(), ctx.fresh_var());
        }
        for inv in invariant {
            let p = sym_prop(inv, &cont.env)?;
            cont.cons.push(p);
        }
        if let Some(c) = cond {
            cont.cons.extend(cond_props(c, &cont.env).1); // ¬condition holds after the loop
        }
        out.push(cont);
    }
    Some(out)
}

/// The variables a loop body may change across iterations: every `Assign` target and
/// every `let` binding, recursing into nested `if`/`for` blocks. These are havoc'd after
/// the loop (their post-loop value is known only through the invariant).
fn modified_vars(block: &Block) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    collect_modified(block, &mut set);
    set
}

fn collect_modified(block: &Block, set: &mut std::collections::HashSet<String>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                set.insert(name.clone());
                collect_modified_expr(value, set);
            }
            Stmt::Assign {
                target: Expr::Var(name),
                value,
            } => {
                set.insert(name.clone());
                collect_modified_expr(value, set);
            }
            Stmt::Assign { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value)) => {
                collect_modified_expr(value, set)
            }
            Stmt::For { body, .. } => collect_modified(body, set),
            _ => {}
        }
    }
    if let Some(t) = &block.tail {
        collect_modified_expr(t, set);
    }
}

/// Recurses into `if` branch blocks (where an assignment can hide) to collect modifieds.
fn collect_modified_expr(e: &Expr, set: &mut std::collections::HashSet<String>) {
    if let Expr::If {
        then_branch,
        else_branch,
        ..
    } = e
    {
        collect_modified(then_branch, set);
        if let Some(eb) = else_branch {
            collect_modified(eb, set);
        }
    }
}

/// Whether a proposition mentions any of the given variables (either side).
fn prop_mentions(p: &SymProp, vars: &std::collections::HashSet<String>) -> bool {
    sym_mentions(&p.lhs, vars) || sym_mentions(&p.rhs, vars)
}

fn sym_mentions(s: &Sym, vars: &std::collections::HashSet<String>) -> bool {
    match s {
        Sym::Const(_) => false,
        Sym::Var(v) => vars.contains(v),
        Sym::Add(a, b) | Sym::Sub(a, b) | Sym::Mul(a, b) => {
            sym_mentions(a, vars) || sym_mentions(b, vars)
        }
    }
}

/// Resolves a call to a CONTRACTED function against its summary: its `ensures` (with the
/// callee's parameters replaced by the actual arguments and its named return by a fresh
/// variable) become known facts about the result; its `requires` become an obligation to
/// discharge under the assumptions live at the call. `None` for a call the verifier cannot
/// summarise (unknown callee, non-linear/absent contract shape, or branching arguments).
fn eval_call(
    callee: &Expr,
    args: &[crate::ast::Arg],
    state: State,
    ctx: &mut Ctx,
) -> Option<Vec<(State, Sym)>> {
    let Expr::Var(name) = callee else {
        return None;
    };
    let g = ctx.contracts.get(name).copied()?;
    // Evaluate the arguments (they may themselves branch or call), threading the state.
    let mut arg_states: Vec<(State, Vec<Sym>)> = vec![(state, Vec::new())];
    for arg in args {
        let mut next = Vec::new();
        for (st, acc) in arg_states {
            for (st2, v) in eval_expr_paths(&arg.value, st, ctx)? {
                let mut acc2 = acc.clone();
                acc2.push(v);
                next.push((st2, acc2));
            }
        }
        arg_states = next;
    }
    let mut out = Vec::new();
    for (mut st, arg_syms) in arg_states {
        // Substitution: the callee's parameter names map to the actual argument values,
        // its named return to a fresh, opaque result variable.
        let mut subst: HashMap<String, Sym> = g
            .params
            .iter()
            .zip(&arg_syms)
            .map(|(p, a)| (p.name.clone(), a.clone()))
            .collect();
        let result = ctx.fresh_var();
        if let Some(rn) = &g.ret_name {
            subst.insert(rn.clone(), result.clone());
        }
        // The callee's precondition is an obligation, checked under the assumptions live
        // here (snapshot BEFORE this call's own ensures are added, no circularity). A
        // precondition we cannot represent must not be silently dropped: bail (reject).
        for req in &g.requires {
            let op = sym_prop(req, &subst)?;
            st.obligations.push(Obligation {
                assumptions: st.cons.clone(),
                what: format!(
                    "the precondition `{}` required when calling `{name}`",
                    show_prop(&op)
                ),
                prop: op,
                alt: None,
            });
        }
        // The callee's postcondition constrains the result: add it as an assumption. One
        // we cannot represent is simply omitted (weaker, still sound).
        for ens in &g.ensures {
            if let Some(op) = sym_prop(ens, &subst) {
                st.cons.push(op);
            }
        }
        out.push((st, result));
    }
    Some(out)
}

/// Whether an expression is of float (`f64`) type, a best-effort check from the known
/// float variables (the `f64` parameters), decimal literals, `as f64` casts, and
/// propagation through arithmetic. Used to exempt float division from the divide-by-zero
/// obligation (IEEE-754 float division does not trap). Sound-leaning: it never marks an
/// integer expression as float, so no integer division escapes the check.
fn expr_is_float(e: &Expr, floats: &HashSet<String>) -> bool {
    match e {
        Expr::Decimal(_) => true,
        Expr::Var(v) => floats.contains(v),
        Expr::Binary(_, l, r) => expr_is_float(l, floats) || expr_is_float(r, floats),
        Expr::Unary(_, x) => expr_is_float(x, floats),
        Expr::Cast(_, ty) => matches!(ty, Type::Named(n) if n == "f64"),
        _ => false,
    }
}

/// Whether an expression is of unsigned (`u64`) type, best-effort from the known unsigned
/// variables (`u64` parameters), `as u64` casts, and propagation through arithmetic (codegen
/// combines "unsigned wins"). Used to exempt unsigned division from the signed-overflow
/// (`i64::MIN / -1`) obligation. Sound-leaning: it never marks a purely signed expression as
/// unsigned, so no signed division escapes the check (at worst it over-raises, a safe
/// rejection, on an unsigned value it fails to recognize).
fn expr_is_unsigned(e: &Expr, unsigned: &HashSet<String>) -> bool {
    match e {
        Expr::Var(v) => unsigned.contains(v),
        Expr::Binary(_, l, r) => expr_is_unsigned(l, unsigned) || expr_is_unsigned(r, unsigned),
        Expr::Unary(_, x) => expr_is_unsigned(x, unsigned),
        Expr::Cast(_, ty) => matches!(ty, Type::Named(n) if n == "u64"),
        _ => false,
    }
}

/// An obligation is discharged if its primary proposition is proven, or, for a disjunctive
/// safety obligation, its alternative is. Proving EITHER rules out the unsafe case.
fn obligation_holds(
    ob: &Obligation,
    asm: &[SymProp],
    int_types: &HashMap<String, (i128, i128)>,
) -> bool {
    matches!(prove(&ob.prop, asm, int_types), Outcome::Proven)
        || ob
            .alt
            .as_ref()
            .is_some_and(|a| matches!(prove(a, asm, int_types), Outcome::Proven))
}

/// The branch conditions a guard contributes: `(then-assumptions, else-assumptions)`.
/// A comparison gives itself on the then-side and its negation on the else-side; an
/// `&&` contributes both conjuncts on the then-side (its negation is a disjunction, so
/// the else-side is left empty, sound, just weaker). Anything else contributes nothing.
fn cond_props(cond: &Expr, env: &HashMap<String, Sym>) -> (Vec<SymProp>, Vec<SymProp>) {
    match cond {
        Expr::Binary(op, _, _) if is_comparison(op) => match sym_prop(cond, env) {
            Some(sp) => {
                let neg = negate_prop(&sp);
                (vec![sp], vec![neg])
            }
            None => (Vec::new(), Vec::new()),
        },
        Expr::Binary(BinOp::And, l, r) => {
            let mut then = cond_props(l, env).0;
            then.extend(cond_props(r, env).0);
            (then, Vec::new())
        }
        _ => (Vec::new(), Vec::new()),
    }
}

/// The logical negation of a comparison proposition (for the else-branch assumption).
fn negate_prop(p: &SymProp) -> SymProp {
    let op = match p.op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Ge => BinOp::Lt,
        BinOp::Gt => BinOp::Le,
        BinOp::Le => BinOp::Gt,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        ref other => other.clone(),
    };
    SymProp {
        op,
        lhs: p.lhs.clone(),
        rhs: p.rhs.clone(),
    }
}

fn eval_expr(e: &Expr, env: &HashMap<String, Sym>) -> Option<Sym> {
    match e {
        Expr::Int(n) => Some(Sym::Const(*n)),
        Expr::Var(name) => env.get(name).cloned(),
        Expr::Binary(op, l, r) => {
            let a = eval_expr(l, env)?;
            let b = eval_expr(r, env)?;
            let combined = match op {
                BinOp::Add => Sym::Add(Box::new(a), Box::new(b)),
                BinOp::Sub => Sym::Sub(Box::new(a), Box::new(b)),
                BinOp::Mul => Sym::Mul(Box::new(a), Box::new(b)),
                _ => return None, // comparisons/logic are not integer values
            };
            Some(simplify(combined))
        }
        // A struct/slice field read is a fixed value within the function, so treat it as a
        // consistent opaque symbolic variable, this lets a contract reason about a slice's
        // length (`requires { i < s.len }`). `s.field` before desugaring (the bounds check runs
        // then) and `s[k]` after it (a lowered field, the form the prover sees) name the same
        // slot the same way. Only reached from contract propositions and the bounds check, where
        // no mutation happens, so the "fixed value" assumption is sound.
        Expr::Field(base, field) => match base.as_ref() {
            Expr::Var(b) => Some(Sym::Var(format!("{b}.{field}"))),
            _ => None,
        },
        Expr::Index(base, idx) => match (base.as_ref(), idx.as_ref()) {
            (Expr::Var(b), Expr::Int(k)) => Some(Sym::Var(format!("{b}#{k}"))),
            _ => None,
        },
        _ => None,
    }
}

/// Constant-folds a symbolic expression as far as possible.
fn simplify(s: Sym) -> Sym {
    let fold = |a: Sym, b: Sym, f: fn(i64, i64) -> i64, mk: fn(Box<Sym>, Box<Sym>) -> Sym| match (
        simplify(a),
        simplify(b),
    ) {
        (Sym::Const(x), Sym::Const(y)) => Sym::Const(f(x, y)),
        (a, b) => mk(Box::new(a), Box::new(b)),
    };
    match s {
        Sym::Add(a, b) => fold(*a, *b, i64::wrapping_add, Sym::Add),
        Sym::Sub(a, b) => fold(*a, *b, i64::wrapping_sub, Sym::Sub),
        Sym::Mul(a, b) => fold(*a, *b, i64::wrapping_mul, Sym::Mul),
        other => other,
    }
}

fn sym_prop(e: &Expr, env: &HashMap<String, Sym>) -> Option<SymProp> {
    let Expr::Binary(op, l, r) = e else {
        return None;
    };
    if !is_comparison(op) {
        return None; // v1 handles single comparisons, not &&/||/implies
    }
    let lhs = simplify(eval_expr(l, env)?);
    let rhs = simplify(eval_expr(r, env)?);
    Some(SymProp {
        op: op.clone(),
        lhs,
        rhs,
    })
}

/// A linear expression `Σ coeff·var + k` over the parameters. A constraint is a `Lin`
/// read as `Lin ≥ 0`. This is the normal form the decision procedure works in.
#[derive(Clone, Debug)]
// The linear engine works in `i128`, not `i64`: it reasons over ℤ, and program constants
// span the full i64 range, so an i64-level computation like `i64::MIN * -1` (which a signed
// division must reason about) overflows i64, panicking in debug, wrapping to a WRONG (and
// unsound) answer in release. i128 gives 64 bits of headroom over any i64-derived value;
// arithmetic that would exceed even i128 returns `None` so the caller bails conservatively
// (an aborted proof is sound; a wrapped one is not).
struct Lin {
    coeffs: BTreeMap<String, i128>,
    k: i128,
}

impl Lin {
    fn constant(k: i64) -> Self {
        Lin {
            coeffs: BTreeMap::new(),
            k: k as i128,
        }
    }
    /// A bare constant given directly as i128 (a type bound such as `u64::MAX = 2^64 − 1` does
    /// not fit in i64).
    fn const_i128(k: i128) -> Self {
        Lin {
            coeffs: BTreeMap::new(),
            k,
        }
    }
    fn var(name: &str) -> Self {
        let mut coeffs = BTreeMap::new();
        coeffs.insert(name.to_string(), 1);
        Lin { coeffs, k: 0 }
    }
    /// If this is a bare constant (no variables), its value.
    fn as_const(&self) -> Option<i128> {
        self.coeffs.values().all(|&c| c == 0).then_some(self.k)
    }
    /// Drops variables whose coefficient has become zero (keeps the map canonical).
    fn pruned(mut self) -> Self {
        self.coeffs.retain(|_, &mut c| c != 0);
        self
    }
    /// Scale by `f`, or `None` on i128 overflow (the caller then bails conservatively -
    /// never wrapping into a wrong answer).
    fn scale(&self, f: i128) -> Option<Lin> {
        let mut coeffs = BTreeMap::new();
        for (v, c) in &self.coeffs {
            coeffs.insert(v.clone(), c.checked_mul(f)?);
        }
        Some(Lin {
            coeffs,
            k: self.k.checked_mul(f)?,
        })
    }
    fn add(&self, o: &Lin) -> Option<Lin> {
        let mut coeffs = self.coeffs.clone();
        for (v, c) in &o.coeffs {
            let e = coeffs.entry(v.clone()).or_insert(0);
            *e = e.checked_add(*c)?;
        }
        Some(
            Lin {
                coeffs,
                k: self.k.checked_add(o.k)?,
            }
            .pruned(),
        )
    }
    fn sub(&self, o: &Lin) -> Option<Lin> {
        self.add(&o.scale(-1)?)
    }
}

/// Converts a symbolic value to a linear expression, or `None` if it is non-linear
/// (a product of two non-constant terms), the prover stays within linear arithmetic.
fn lin_of_sym(s: &Sym) -> Option<Lin> {
    match s {
        Sym::Const(n) => Some(Lin::constant(*n)),
        Sym::Var(v) => Some(Lin::var(v)),
        Sym::Add(a, b) => lin_of_sym(a)?.add(&lin_of_sym(b)?),
        Sym::Sub(a, b) => lin_of_sym(a)?.sub(&lin_of_sym(b)?),
        Sym::Mul(a, b) => {
            let (la, lb) = (lin_of_sym(a)?, lin_of_sym(b)?);
            match (la.as_const(), lb.as_const()) {
                (Some(c), _) => lb.scale(c),
                (_, Some(c)) => la.scale(c),
                _ => None, // var*var, non-linear, out of fragment
            }
        }
    }
}

/// Fourier–Motzkin: is the conjunction of `Lin ≥ 0` constraints unsatisfiable over the
/// rationals? UNSAT over ℚ implies UNSAT over ℤ, so a positive answer soundly witnesses
/// that the constraints cannot all hold. Eliminates one variable per round by combining
/// each positive/negative pair; a bare negative constant is the contradiction. Bails to
/// `false` (conservatively "not shown unsat") if the system grows past a size cap.
fn unsat(mut cons: Vec<Lin>) -> bool {
    const CAP: usize = 600;
    loop {
        // A constant ≥ 0 constraint is trivially satisfied; a constant < 0 is the
        // contradiction that witnesses unsatisfiability.
        cons.retain(|c| c.as_const().is_none_or(|v| v < 0));
        if cons.iter().any(|c| c.as_const().is_some_and(|v| v < 0)) {
            return true;
        }
        // Pick a variable to eliminate; if none remain, the system is satisfiable.
        let Some(x) = cons.iter().flat_map(|c| c.coeffs.keys()).next().cloned() else {
            return false;
        };
        let (mut pos, mut neg, mut next) = (Vec::new(), Vec::new(), Vec::new());
        for c in cons {
            match c.coeffs.get(&x).copied().unwrap_or(0) {
                a if a > 0 => pos.push(c),
                a if a < 0 => neg.push(c),
                _ => next.push(c), // no x, carried over unchanged
            }
        }
        // For each (lower-bound, upper-bound) pair, add the x-free combination
        // `b·p + a·n` (a>0 the coeff in p, b>0 the magnitude in n): x cancels.
        for p in &pos {
            for n in &neg {
                let a = p.coeffs[&x];
                let Some(b) = n.coeffs[&x].checked_neg() else {
                    return false; // overflow, conservative bail (never a false "unsat")
                };
                let combined = p
                    .scale(b)
                    .and_then(|ps| n.scale(a).and_then(|ns| ps.add(&ns)));
                let Some(combined) = combined else {
                    return false; // overflow in the x-free combination, bail
                };
                next.push(combined);
            }
        }
        if next.len() > CAP {
            return false; // conservative bail, never a false "unsat"
        }
        cons = next;
    }
}

/// Does the assumption set (constraints `Lin ≥ 0`) entail `e ≥ 0`? True iff adding the
/// integer negation `e ≤ -1` (i.e. `-e - 1 ≥ 0`) makes the system unsatisfiable.
fn implies_geq0(assumptions: &[Lin], e: &Lin) -> bool {
    let mut sys = assumptions.to_vec();
    let Some(mut neg) = e.scale(-1) else {
        return false; // overflow negating the goal, can't show entailment (conservative)
    };
    neg.k -= 1; // -e - 1 ≥ 0  ==  e ≤ -1  ==  ¬(e ≥ 0) over ℤ
    sys.push(neg);
    unsat(sys)
}

/// The `≥ 0` constraints a comparison asserts as a fact (for an assumption), or the
/// constraints whose conjunction means the goal holds. `> `/`<` are integer-tightened
/// (`a > b` ⟺ `a - b - 1 ≥ 0`); `==` yields both directions; `!=` has no conjunctive
/// form (`None`).
fn holds_cons(op: &BinOp, lhs: &Lin, rhs: &Lin) -> Option<Vec<Lin>> {
    let diff = lhs.sub(rhs)?; // a - b
    let tighten = |mut e: Lin| {
        e.k -= 1;
        e
    };
    Some(match op {
        BinOp::Ge => vec![diff],
        BinOp::Gt => vec![tighten(diff)],
        BinOp::Le => vec![rhs.sub(lhs)?],
        BinOp::Lt => vec![tighten(rhs.sub(lhs)?)],
        BinOp::Eq => vec![diff.clone(), rhs.sub(lhs)?],
        _ => return None, // Ne, not a conjunction of ≥ 0 facts
    })
}

fn prove(
    p: &SymProp,
    assumptions: &[SymProp],
    int_types: &HashMap<String, (i128, i128)>,
) -> Outcome {
    // Reflexivity: `e OP e` is decided structurally, for ANY expression (including non-linear
    // ones the arithmetic engine can't touch). `a == a`, `a >= a`, `a <= a` always hold;
    // `a != a`, `a > a`, `a < a` never do. Sound under wrapping too, both sides are the same
    // value. This lets a contract restate its own body, e.g. `ensures { r == x * x }`.
    if p.lhs == p.rhs {
        return match p.op {
            BinOp::Eq | BinOp::Ge | BinOp::Le => Outcome::Proven,
            BinOp::Ne | BinOp::Gt | BinOp::Lt => Outcome::Disproven,
            _ => Outcome::Unknown,
        };
    }
    // Syntactic match: an assumption identical to the goal proves it directly. This is the
    // path for `!=` facts (e.g. `b != 0` from a precondition or a negated `b == 0` guard),
    // which the linear machinery below drops because `!=` is not a conjunctive `≥ 0` fact.
    if assumptions
        .iter()
        .any(|a| a.op == p.op && a.lhs == p.lhs && a.rhs == p.rhs)
    {
        return Outcome::Proven;
    }
    // The linear engine reasons over ℤ, but signed machine arithmetic WRAPS. Only trust it when
    // every `+`/`-`/`*` in the goal is proven not to overflow under the assumptions; otherwise a
    // ℤ-true relation may be false at runtime (`x + 1 > x` holds over ℤ but fails at i64::MAX).
    // The reflexive and syntactic matches above are wrapping-sound and already returned.
    if !arith_trustworthy(&p.lhs, assumptions, int_types)
        || !arith_trustworthy(&p.rhs, assumptions, int_types)
    {
        return Outcome::Unknown;
    }
    // The assumptions as `≥ 0` constraints. A non-linear or `!=` assumption is simply
    // dropped: fewer facts can only make proving harder (never wrongly succeed) and, since
    // dropping constraints preserves unsatisfiability, cannot cause a wrong "disproven".
    let mut a: Vec<Lin> = Vec::new();
    for ap in assumptions {
        if let (Some(l), Some(r)) = (lin_of_sym(&ap.lhs), lin_of_sym(&ap.rhs))
            && let Some(cs) = holds_cons(&ap.op, &l, &r)
        {
            a.extend(cs);
        }
    }
    let (Some(l), Some(r)) = (lin_of_sym(&p.lhs), lin_of_sym(&p.rhs)) else {
        return Outcome::Unknown; // goal outside linear arithmetic
    };
    // `!=` is the odd one out (its truth is a disjunction): the goal holds iff the two
    // sides are forced apart, i.e. equality is unsatisfiable under the assumptions.
    if p.op == BinOp::Ne {
        let (Some(lr), Some(rl)) = (l.sub(&r), r.sub(&l)) else {
            return Outcome::Unknown; // overflow forming the equality system, conservative
        };
        let mut eq_sys = a.clone();
        eq_sys.push(lr);
        eq_sys.push(rl);
        return if unsat(eq_sys) {
            Outcome::Proven
        } else {
            Outcome::Unknown
        };
    }
    let Some(holds) = holds_cons(&p.op, &l, &r) else {
        return Outcome::Unknown;
    };
    // Proven: the assumptions entail every constraint the goal needs.
    if holds.iter().all(|h| implies_geq0(&a, h)) {
        return Outcome::Proven;
    }
    // Disproven: the goal is impossible under the assumptions (adding it is unsatisfiable),
    // so the postcondition is false for every input the precondition allows.
    let mut with_goal = a;
    with_goal.extend(holds);
    if unsat(with_goal) {
        return Outcome::Disproven;
    }
    Outcome::Unknown
}

/// The inclusive `[min, max]` value range of a built-in integer type (as i128), or `None` for a
/// non-integer / unknown type name.
fn int_range(ty: &str) -> Option<(i128, i128)> {
    Some(match ty {
        "i8" => (i8::MIN as i128, i8::MAX as i128),
        "u8" => (0, u8::MAX as i128),
        "i16" => (i16::MIN as i128, i16::MAX as i128),
        "u16" => (0, u16::MAX as i128),
        "i32" => (i32::MIN as i128, i32::MAX as i128),
        "u32" => (0, u32::MAX as i128),
        "i64" => (i64::MIN as i128, i64::MAX as i128),
        "u64" => (0, u64::MAX as i128),
        _ => return None,
    })
}

/// Is the integer expression `result` guaranteed not to wrap, does its true value fit in its
/// RESULT TYPE's range, under the given path assumptions? The prover reasons over ℤ, but the
/// machine wraps at the type width: treating `a + b` as its exact value is only sound when the
/// value fits the type. This proves `type_min <= e <= type_max`. Every variable is implicitly in
/// its own declared range (from `int_types`; i64 when unknown), so a loop counter `r + 1` under
/// `r < n` is safe (`n <= MAX` forces `r <= MAX - 1`), while `x + 1` under only `x > 0` is not,
/// and an `i32` sum leaving `[i32::MIN, i32::MAX]` but fitting in i64 is correctly rejected. A
/// non-linear result (`a * b`, two variables) is never certified, returns `false`.
fn overflow_safe(result: &Sym, cons: &[SymProp], int_types: &HashMap<String, (i128, i128)>) -> bool {
    let Some(e) = lin_of_sym(result) else {
        return false; // non-linear: cannot bound it, so assume it may overflow
    };
    // The assumptions as `>= 0` constraints (the same lowering `prove` uses).
    let mut a: Vec<Lin> = Vec::new();
    for ap in cons {
        if let (Some(l), Some(r)) = (lin_of_sym(&ap.lhs), lin_of_sym(&ap.rhs))
            && let Some(cs) = holds_cons(&ap.op, &l, &r)
        {
            a.extend(cs);
        }
    }
    // The declared range of a variable; i64's full range when its type is unknown (a call
    // result, an untyped local), the widest possibility, so never an unsound tightening.
    let range_of = |v: &str| {
        int_types
            .get(v)
            .copied()
            .unwrap_or((i64::MIN as i128, i64::MAX as i128))
    };
    // Add each variable's implicit range, the result's and the assumptions', so the
    // entailment can chain them (e.g. `r < n` with `n <= MAX`).
    let mut vars: std::collections::HashSet<String> = e.coeffs.keys().cloned().collect();
    for c in &a {
        vars.extend(c.coeffs.keys().cloned());
    }
    for v in &vars {
        let (lo, hi) = range_of(v);
        let vv = Lin::var(v);
        let (Some(loc), Some(hic)) = (vv.sub(&Lin::const_i128(lo)), Lin::const_i128(hi).sub(&vv))
        else {
            return false; // i128 overflow forming a bound, conservative
        };
        a.push(loc); // v - lo >= 0
        a.push(hic); // hi - v >= 0
    }
    // The result's type range: the intersection of the ranges of the variables it is built from
    // (a well-typed expression's operands share a width, so this is that type's range; mixed
    // widths, a type error elsewhere, only tighten it). Default to i64 when it has no variable.
    let (mut rlo, mut rhi) = (i64::MIN as i128, i64::MAX as i128);
    let mut typed = false;
    for v in e.coeffs.keys() {
        if let Some((lo, hi)) = int_types.get(v) {
            if typed {
                rlo = rlo.max(*lo);
                rhi = rhi.min(*hi);
            } else {
                (rlo, rhi, typed) = (*lo, *hi, true);
            }
        }
    }
    // Prove `rlo <= e` (e - rlo >= 0) and `e <= rhi` (rhi - e >= 0).
    let (Some(lower), Some(upper)) = (e.sub(&Lin::const_i128(rlo)), Lin::const_i128(rhi).sub(&e))
    else {
        return false;
    };
    implies_geq0(&a, &lower) && implies_geq0(&a, &upper)
}

/// May the linear engine trust the exact ℤ value of `s` under `cons`? Yes iff every `+`/`-`/`*`
/// node in `s` is proven not to overflow its type, then the machine computes the same value the
/// ℤ arithmetic assumes. A single possibly-wrapping operation anywhere makes the whole expression
/// untrustworthy (a `>`/`>=`/… derived from it could be false at runtime).
fn arith_trustworthy(s: &Sym, cons: &[SymProp], int_types: &HashMap<String, (i128, i128)>) -> bool {
    match s {
        Sym::Const(_) | Sym::Var(_) => true,
        Sym::Add(a, b) | Sym::Sub(a, b) | Sym::Mul(a, b) => {
            arith_trustworthy(a, cons, int_types)
                && arith_trustworthy(b, cons, int_types)
                && overflow_safe(s, cons, int_types)
        }
    }
}

/// Is `0 <= idx < n` DISPROVEN under `assumptions`, i.e. is the index PROVABLY out of bounds
/// for an array of size `n`, given the facts in scope (a function's `requires`, the branch guards
/// and loop conditions on the path to the access)? This exposes the prover to the bounds check so
/// it can reject a VARIABLE index that must be out of range, the analog of the constant-index
/// check (`check_constant_bounds`), and the reusable core of the eventual prove-or-error bounds.
/// SOUND and NON-BREAKING: it returns true only when the negation is entailed; an index that
/// merely MIGHT be out of range (the common unproven case) returns false and is left alone.
/// Collects the names of all variables appearing in an expression (for seeding the symbolic
/// environment of the bounds check).
fn collect_expr_vars(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Var(n) => {
            out.insert(n.clone());
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            collect_expr_vars(l, out);
            collect_expr_vars(r, out);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => collect_expr_vars(x, out),
        Expr::Call(c, args) => {
            collect_expr_vars(c, out);
            for a in args {
                collect_expr_vars(&a.value, out);
            }
        }
        _ => {}
    }
}

/// The lower/upper safety props, the assumption set, and the variable type ranges.
type IndexProps = (SymProp, SymProp, Vec<SymProp>, HashMap<String, (i128, i128)>);

/// The two safety propositions `0 <= idx` and `idx < bound`, the assumptions in scope, and the
/// parameter type ranges, the shared setup of the bounds decisions below. `None` if the index
/// or the bound is outside the linear fragment (a call, a computed length). `bound` is a constant
/// `N` for a fixed-size array or `s.len` for a slice.
fn index_props(
    idx: &Expr,
    bound: &Expr,
    assumptions: &[Expr],
    params: &[Param],
) -> Option<IndexProps> {
    // Every variable in play is a free symbolic value, parameters AND locals (a loop counter is
    // a local, not a parameter). Seed the environment with all of them so `eval_expr` resolves
    // them (a var absent from the environment would otherwise make the index unanalyzable).
    let mut names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    collect_expr_vars(idx, &mut names);
    collect_expr_vars(bound, &mut names);
    for a in assumptions {
        collect_expr_vars(a, &mut names);
    }
    let env: HashMap<String, Sym> = names
        .iter()
        .map(|n| (n.clone(), Sym::Var(n.clone())))
        .collect();
    let (i, hi) = (eval_expr(idx, &env)?, eval_expr(bound, &env)?);
    let asm: Vec<SymProp> = assumptions.iter().filter_map(|e| sym_prop(e, &env)).collect();
    let int_types = int_types_of(params);
    let lower = SymProp {
        op: BinOp::Ge,
        lhs: i.clone(),
        rhs: Sym::Const(0),
    };
    let upper = SymProp {
        op: BinOp::Lt,
        lhs: i,
        rhs: hi,
    };
    Some((lower, upper, asm, int_types))
}

/// Do the assumptions FORCE the index out of `[0, bound)`, a provable overrun? (The lenient
/// check: reject only what is disproven.)
pub fn index_provably_oob(idx: &Expr, bound: &Expr, assumptions: &[Expr], params: &[Param]) -> bool {
    let Some((lower, upper, asm, it)) = index_props(idx, bound, assumptions, params) else {
        return false;
    };
    matches!(prove(&lower, &asm, &it), Outcome::Disproven)
        || matches!(prove(&upper, &asm, &it), Outcome::Disproven)
}

/// The integer-typed parameters and their value ranges (`i32` -> [-2^31, 2^31-1]), so the prover
/// can reason width-aware about overflow. Locals are untyped here (treated as unbounded i64).
fn int_types_of(params: &[Param]) -> HashMap<String, (i128, i128)> {
    params
        .iter()
        .filter_map(|p| match &p.ty {
            Type::Named(t) => int_range(t).map(|r| (p.name.clone(), r)),
            _ => None,
        })
        .collect()
}

/// Folds `e` to a constant integer, or `None` if it is not compile-time constant.
fn const_of(e: &Expr) -> Option<i64> {
    match eval_expr(e, &HashMap::new()) {
        Some(Sym::Const(n)) => Some(n),
        _ => None,
    }
}

/// Is `e >= 0` discharged by the assumptions, over the same symbolic environment as the bounds
/// check? Used to certify that a modulo's dividend is non-negative.
fn proven_nonneg(e: &Expr, assumptions: &[Expr], params: &[Param]) -> bool {
    let mut names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    collect_expr_vars(e, &mut names);
    for a in assumptions {
        collect_expr_vars(a, &mut names);
    }
    let env: HashMap<String, Sym> = names
        .iter()
        .map(|n| (n.clone(), Sym::Var(n.clone())))
        .collect();
    let Some(sym) = eval_expr(e, &env) else {
        return false;
    };
    let asm: Vec<SymProp> = assumptions.iter().filter_map(|a| sym_prop(a, &env)).collect();
    let prop = SymProp {
        op: BinOp::Ge,
        lhs: sym,
        rhs: Sym::Const(0),
    };
    matches!(prove(&prop, &asm, &int_types_of(params)), Outcome::Proven)
}

/// In-bounds proofs the LINEAR evaluator cannot express, because `%` and `&` are not linear (there
/// is no `Sym` for them): an index of the form `e % m` is confined to `[0, m)` when `e >= 0` and
/// `m > 0`, and `e & (2^k - 1)` is confined to `[0, 2^k)` for ANY `e` (the mask keeps only the low
/// bits, regardless of sign). Either is in bounds when its confinement ceiling fits the array.
fn index_bounded_by_structure(
    idx: &Expr,
    bound: &Expr,
    assumptions: &[Expr],
    params: &[Param],
) -> bool {
    let Some(n) = const_of(bound) else {
        return false; // a runtime bound (slice `s.len`) is not a static ceiling to compare against
    };
    match idx {
        Expr::Binary(BinOp::Mod, dividend, divisor) => {
            let Some(m) = const_of(divisor) else {
                return false;
            };
            // `e % m` ∈ [0, m) needs `m > 0` and a non-negative dividend; it fits iff `m <= n`.
            m > 0 && m <= n && proven_nonneg(dividend, assumptions, params)
        }
        Expr::Binary(BinOp::BitAnd, l, r) => {
            // A `2^k - 1` mask (all low bits set) confines the result to [0, mask]; fits iff
            // `mask < n`. Holds for any dividend sign, so no non-negativity obligation.
            let Some(mask) = const_of(l).or_else(|| const_of(r)) else {
                return false;
            };
            mask >= 0 && (mask & mask.wrapping_add(1)) == 0 && mask < n
        }
        _ => false,
    }
}

/// Is the index PROVEN to lie in `[0, bound)`, `0 <= idx` and `idx < bound` both discharged?
/// (The strict check: accept only what is proven; anything unproven is a compile error. This is
/// checked-by-default bounds.)
pub fn index_proven_in_bounds(
    idx: &Expr,
    bound: &Expr,
    assumptions: &[Expr],
    params: &[Param],
) -> bool {
    if index_bounded_by_structure(idx, bound, assumptions, params) {
        return true;
    }
    let Some((lower, upper, asm, it)) = index_props(idx, bound, assumptions, params) else {
        return false;
    };
    matches!(prove(&lower, &asm, &it), Outcome::Proven)
        && matches!(prove(&upper, &asm, &it), Outcome::Proven)
}

fn is_comparison(op: &BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
    )
}

fn show_sym(s: &Sym) -> String {
    match s {
        Sym::Const(n) => n.to_string(),
        Sym::Var(v) => v.clone(),
        Sym::Add(a, b) => format!("({} + {})", show_sym(a), show_sym(b)),
        Sym::Sub(a, b) => format!("({} - {})", show_sym(a), show_sym(b)),
        Sym::Mul(a, b) => format!("({} * {})", show_sym(a), show_sym(b)),
    }
}

fn show_prop(p: &SymProp) -> String {
    let op = match p.op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        _ => "?",
    };
    format!("{} {} {}", show_sym(&p.lhs), op, show_sym(&p.rhs))
}

/// Renders a path's branch conditions as a human-readable " on the path where `c1` and `c2`"
/// suffix, the exact case under which a proof failed. Empty for a straight-line path (no
/// branches), so a linear body's diagnostic carries no spurious clause. This is what lets the
/// red→green loop see WHICH case to fix, not merely that a contract failed.
fn path_where(cons: &[SymProp]) -> String {
    if cons.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = cons.iter().map(|c| format!("`{}`", show_prop(c))).collect();
    format!(" on the path where {}", parts.join(" and "))
}

/// The signed bit-width of the type whose range is `[lo, hi]`, for a NARROW signed type
/// (i8/i16/i32); `None` for i64, unsigned, or any non-standard range. Used to wrap an
/// intermediate result at its real width so a witness reflects the machine.
fn signed_range_width(lo: i128, hi: i128) -> Option<u32> {
    [8u32, 16, 32]
        .into_iter()
        .find(|&w| lo == -(1i128 << (w - 1)) && hi == (1i128 << (w - 1)) - 1)
}

/// Sign-extend the low `w` bits of `v`, i.e. wrap `v` into a signed `w`-bit type, exactly as
/// the machine's narrow arithmetic does.
fn canon_signed(v: i64, w: u32) -> i64 {
    let s = 64 - w;
    (v << s) >> s
}

/// The width at which an arithmetic NODE's result wraps: the common narrow signed width of all
/// its variables, or `None` (evaluate at full i64, no canonicalization) when it has no typed
/// variable, an i64/unsigned one, or a mix of widths. Conservative: only canonicalizes when the
/// width is certain, so it never fabricates a false witness.
fn node_signed_width(s: &Sym, int_types: &HashMap<String, (i128, i128)>) -> Option<u32> {
    let mut vars = BTreeSet::new();
    collect_sym_vars(s, &mut vars);
    let mut width = None;
    for v in &vars {
        let (lo, hi) = int_types.get(v)?;
        let w = signed_range_width(*lo, *hi)?;
        match width {
            None => width = Some(w),
            Some(x) if x == w => {}
            _ => return None, // mixed widths, a type error elsewhere; do not canonicalize
        }
    }
    width
}

/// Evaluates a symbolic value under a concrete assignment, using WRAPPING arithmetic at each
/// operation's real type width, the machine's semantics, so a witness is faithful to how the
/// compiled code runs (an `i32 + 1` at `i32::MAX` wraps to `i32::MIN`, not 2^31).
fn eval_sym(s: &Sym, env: &BTreeMap<String, i64>, int_types: &HashMap<String, (i128, i128)>) -> Option<i64> {
    let narrow = |raw: i64| match node_signed_width(s, int_types) {
        Some(w) => canon_signed(raw, w),
        None => raw,
    };
    Some(match s {
        Sym::Const(n) => *n,
        Sym::Var(v) => *env.get(v)?,
        Sym::Add(a, b) => narrow(eval_sym(a, env, int_types)?.wrapping_add(eval_sym(b, env, int_types)?)),
        Sym::Sub(a, b) => narrow(eval_sym(a, env, int_types)?.wrapping_sub(eval_sym(b, env, int_types)?)),
        Sym::Mul(a, b) => narrow(eval_sym(a, env, int_types)?.wrapping_mul(eval_sym(b, env, int_types)?)),
    })
}

/// Evaluates a comparison under a concrete assignment (`None` if a variable is unbound).
fn eval_prop(p: &SymProp, env: &BTreeMap<String, i64>, int_types: &HashMap<String, (i128, i128)>) -> Option<bool> {
    let (l, r) = (eval_sym(&p.lhs, env, int_types)?, eval_sym(&p.rhs, env, int_types)?);
    Some(match p.op {
        BinOp::Eq => l == r,
        BinOp::Ne => l != r,
        BinOp::Lt => l < r,
        BinOp::Gt => l > r,
        BinOp::Le => l <= r,
        BinOp::Ge => l >= r,
        _ => return None,
    })
}

fn collect_sym_vars(s: &Sym, out: &mut BTreeSet<String>) {
    match s {
        Sym::Const(_) => {}
        Sym::Var(v) => {
            out.insert(v.clone());
        }
        Sym::Add(a, b) | Sym::Sub(a, b) | Sym::Mul(a, b) => {
            collect_sym_vars(a, out);
            collect_sym_vars(b, out);
        }
    }
}

fn collect_consts(s: &Sym, out: &mut BTreeSet<i64>) {
    match s {
        Sym::Const(n) => {
            out.insert(*n);
        }
        Sym::Var(_) => {}
        Sym::Add(a, b) | Sym::Sub(a, b) | Sym::Mul(a, b) => {
            collect_consts(a, out);
            collect_consts(b, out);
        }
    }
}

/// Searches for a concrete input that satisfies every assumption yet breaks `goal`, a real,
/// VERIFIED counterexample (evaluated with wrapping i64, so it is exactly what the machine
/// does). Bounded: only up to 3 free variables, over a small candidate set seeded from the
/// constants in play plus the type's extremes; returns `None` (and the diagnostic simply
/// omits a counterexample) rather than overclaim when none is found in budget. A returned
/// witness is never a false positive, it is checked to actually violate the goal.
fn find_counterexample(
    goal: &SymProp,
    assumptions: &[SymProp],
    int_types: &HashMap<String, (i128, i128)>,
) -> Option<BTreeMap<String, i64>> {
    let mut vars = BTreeSet::new();
    collect_sym_vars(&goal.lhs, &mut vars);
    collect_sym_vars(&goal.rhs, &mut vars);
    for a in assumptions {
        collect_sym_vars(&a.lhs, &mut vars);
        collect_sym_vars(&a.rhs, &mut vars);
    }
    let vars: Vec<String> = vars.into_iter().collect();
    if vars.is_empty() || vars.len() > 3 {
        return None;
    }
    // Candidate values: the type's extremes and small magnitudes, plus each literal in the
    // props and its ±1 neighbours (the interesting points are next to the constants).
    let mut consts = BTreeSet::new();
    collect_consts(&goal.lhs, &mut consts);
    collect_consts(&goal.rhs, &mut consts);
    for a in assumptions {
        collect_consts(&a.lhs, &mut consts);
        collect_consts(&a.rhs, &mut consts);
    }
    // Ordered so the FRIENDLIEST witness is found first: small magnitudes, then the
    // literals in play and their neighbours, then the type's extremes (which still catch
    // overflow-shaped breakage). Dedup preserves this preference order.
    let mut cands: Vec<i64> = Vec::new();
    let mut seen = BTreeSet::new();
    let push = |v: i64, cands: &mut Vec<i64>, seen: &mut BTreeSet<i64>| {
        if seen.insert(v) {
            cands.push(v);
        }
    };
    for v in [0, 1, -1, 2, -2] {
        push(v, &mut cands, &mut seen);
    }
    for c in consts {
        for v in [c, c.wrapping_sub(1), c.wrapping_add(1)] {
            push(v, &mut cands, &mut seen);
        }
    }
    for v in [i64::MIN, i64::MAX] {
        push(v, &mut cands, &mut seen);
    }
    // Enumerate the (bounded) cartesian product of candidate assignments.
    let mut idx = vec![0usize; vars.len()];
    loop {
        let env: BTreeMap<String, i64> = vars
            .iter()
            .enumerate()
            .map(|(i, v)| (v.clone(), cands[idx[i]]))
            .collect();
        let assumptions_hold = assumptions
            .iter()
            .all(|a| eval_prop(a, &env, int_types) == Some(true));
        if assumptions_hold && eval_prop(goal, &env, int_types) == Some(false) {
            return Some(env);
        }
        // Odometer increment over the candidate indices.
        let mut i = 0;
        loop {
            if i == vars.len() {
                return None;
            }
            idx[i] += 1;
            if idx[i] < cands.len() {
                break;
            }
            idx[i] = 0;
            i += 1;
        }
    }
}

/// A human-readable " (counterexample: `x = -1`, `b = 0`)" clause, or "" if none was found
/// in budget, the actionable input the Prime Directive's feedback mandate asks for.
fn counterexample_clause(
    goal: &SymProp,
    assumptions: &[SymProp],
    int_types: &HashMap<String, (i128, i128)>,
) -> String {
    match find_counterexample(goal, assumptions, int_types) {
        Some(env) => {
            let parts: Vec<String> = env.iter().map(|(v, n)| format!("{v} = {n}")).collect();
            format!(" (counterexample: {})", parts.join(", "))
        }
        None => String::new(),
    }
}

/// Counterexample clause for a possibly-disjunctive obligation. A plain obligation wants a
/// witness that breaks `prop`. A disjunctive one (`prop` OR `alt`) needs a witness that
/// breaks BOTH, so it is sought against `prop` with `¬alt` added to the assumptions, an
/// assignment satisfying `asm ∧ ¬alt ∧ ¬prop` (e.g. `x = i64::MIN, y = -1`).
fn counterexample_clause_alt(
    prop: &SymProp,
    alt: Option<&SymProp>,
    asm: &[SymProp],
    int_types: &HashMap<String, (i128, i128)>,
) -> String {
    match alt {
        None => counterexample_clause(prop, asm, int_types),
        Some(a) => {
            let mut asm2 = asm.to_vec();
            asm2.push(negate_prop(a));
            counterexample_clause(prop, &asm2, int_types)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn parse(src: &str) -> Vec<Function> {
        Parser::new(tokenize(src)).parse_program()
    }

    #[test]
    fn proves_reflexive_equality_even_when_nonlinear() {
        // `r == x * x` with body `x * x` is trivially true (a == a), but `x * x` is
        // non-linear so the linear engine can't reason about it. Reflexivity must decide it.
        let funcs = parse("fn sq(x: i64) -> (r: i64) ensures { r == x * x } { x * x }");
        assert!(verify(&funcs).is_ok(), "e == e holds structurally");
    }

    #[test]
    fn rejects_reflexive_strict_inequality() {
        // `r > r` (here `r > x` with r bound to x) is always false, a == a means a > a is not.
        let funcs = parse("fn f(x: i64) -> (r: i64) ensures { r > r } { x }");
        assert!(verify(&funcs).is_err(), "a > a is never true");
    }

    #[test]
    fn rejects_a_provably_false_postcondition() {
        // The body returns 0 but promises a positive result: false for ALL inputs
        // (there are none), so the compiler must reject it, no need to run it.
        let funcs = parse("fn bad() -> (r: i64) ensures { r > 0 } { 0 }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn accepts_a_constant_true_postcondition() {
        // 6 * 7 == 42 is decided at compile time.
        let funcs = parse("fn ok() -> (r: i64) ensures { r == 42 } { 6 * 7 }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn proves_a_postcondition_from_a_precondition() {
        // identity: assuming x > 0, the result x > 0 holds, proven, no runtime
        // check needed, for every possible x.
        let funcs = parse("fn id(x: i64) -> (r: i64) requires { x > 0 } ensures { r > 0 } { x }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_loop_whose_statement_if_violates_the_invariant() {
        // SOUNDNESS: a statement-position `if` that assigns must be PROCESSED, not ignored, else
        // the invariant `x >= 0` is spuriously "preserved" while the branch sets x = -1. Regression
        // guard for a real unsoundness: verify accepted this, the ud2 guard trapped at runtime, and
        // in release (guard erased) it would silently return -1.
        let f = parse(
            "fn f() -> (r: i64) ensures { r >= 0 } { let mut x = 5\nfor x > 0 invariant { x >= 0 } { if x > 0 { x = 0 - 1 }\nlet d = 0 }\nx }",
        );
        assert!(verify(&f).is_err());
    }

    #[test]
    fn a_statement_if_that_preserves_the_invariant_still_verifies() {
        // The mirror of the soundness guard: a statement-`if` whose every branch keeps the
        // invariant is accepted, processing it must not over-reject.
        let f = parse(
            "fn f(n: i64 where n > 0) -> (r: i64) ensures { r >= 0 } { let mut x = 0\nfor x < n invariant { x >= 0 } { if x < n { x = x + 1 }\nlet d = 0 }\nx }",
        );
        assert!(verify(&f).is_ok());
    }

    #[test]
    fn an_array_read_is_an_opaque_value_not_a_fragment_exit() {
        // Reading `a[i]` no longer makes the whole body unverifiable: the read yields an arbitrary
        // value, so a contract that holds for ANY result still verifies, and one that constrains
        // the opaque value is (soundly) not provable.
        let ok = parse(
            "fn f(a: Slice, i: i64 where 0 <= i && i < a.len) -> (r: i64) ensures { r == r } { a[i] }",
        );
        assert!(verify(&ok).is_ok());
        // The safety obligation on the INDEX expression is still gathered through the read: a
        // division in the index must be proven safe.
        let div = parse(
            "fn g(a: Slice, k: i64) -> (r: i64) ensures { r == r } { a[10 / k] }",
        );
        assert!(verify(&div).is_err());
    }

    #[test]
    fn rejects_a_wrong_arithmetic_postcondition() {
        // The body returns x + 1 but claims x + 2; disproven symbolically.
        let funcs = parse(
            "fn inc(x: i64) -> (r: i64) ensures { r == 100 } { let a = 40\nlet b = 41\na + b }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn rejects_a_contract_it_cannot_prove() {
        // A loop is outside the current prover's reach. Sound by default: rather
        // than trust it, the compiler REJECTS a contract it cannot verify. (When
        // the prover learns loop invariants, this same program will be accepted.)
        let funcs = parse(
            "fn count(n: i64) -> (r: i64) ensures { r >= 0 } { let mut r = 0\nfor r < n { r = r + 1 }\nr }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn rejects_a_postcondition_it_cannot_decide_symbolically() {
        // r > 0 with r = x (a free parameter, no precondition): cannot be proven
        // for every x, so it is rejected rather than assumed.
        let funcs = parse("fn f(x: i64) -> (r: i64) ensures { r > 0 } { x }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn a_function_without_a_contract_is_fine() {
        assert!(verify(&parse("fn f() -> i64 { 42 }")).is_ok());
    }

    #[test]
    fn proves_a_shifted_bound_from_a_precondition() {
        // 0 < x < 1000 entails x + 1 > 1, the linear prover must shift a bound, which the
        // exact-match prover could not do. The upper bound is needed for soundness: `x + 1`
        // must be proven not to overflow (an unbounded `x > 0` alone is rejected, see
        // `rejects_a_proof_that_relies_on_signed_overflow`).
        let funcs = parse(
            "fn f(x: i64) -> (r: i64) requires { x > 0; x < 1000 } ensures { r > 1 } { x + 1 }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_proof_that_relies_on_signed_overflow() {
        // `x + 1 > x` is true over ℤ but FALSE at `x = i64::MAX` (x+1 wraps to i64::MIN). The
        // prover must not certify a postcondition that a wrapping add makes false, it treats
        // the possibly-overflowing `x + 1` as opaque, so `r > x` cannot be proven. (Compare
        // the bounded `proves_a_shifted_bound_from_a_precondition`, which IS accepted.)
        let funcs =
            parse("fn f(x: i64) -> (r: i64) requires { x > 0 } ensures { r > x } { x + 1 }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn rejects_a_proof_that_relies_on_signed_multiply_overflow() {
        // Same class via a linear multiply: `x * 2 > x` holds over ℤ but wraps at `x = MAX`.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) requires { x > 0 } ensures { r > x } { x * 2 }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn proves_an_increment_that_cannot_overflow() {
        // With an upper bound the add is proven not to wrap, so `r > x` is genuinely true.
        let funcs = parse(
            "fn f(x: i64) -> (r: i64) requires { x > 0; x < 100 } ensures { r > x } { x + 1 }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn proves_sign_and_magnitude_bounds_of_division_and_modulo() {
        // `i / 2` keeps a non-negative dividend non-negative.
        let d = parse("fn f(i: i64 where i >= 0) -> (r: i64) ensures { r >= 0 } { i / 2 }");
        assert!(verify(&d).is_ok());
        // `i % n` lands in [0, n) when i >= 0 and n > 0, the safe-index bound.
        let m = parse(
            "fn f(i: i64 where i >= 0, n: i64 where n > 0) -> (r: i64) ensures { r >= 0; r < n } { i % n }",
        );
        assert!(verify(&m).is_ok());
    }

    #[test]
    fn does_not_prove_a_division_bound_without_the_sign() {
        // Without `i >= 0`, `i / 2 >= 0` is false for negative i, must NOT be proven.
        let d = parse("fn f(i: i64) -> (r: i64) ensures { r >= 0 } { i / 2 }");
        assert!(verify(&d).is_err());
    }

    #[test]
    fn proves_the_range_of_a_right_shift() {
        // `x >>> 3` = x / 8 for x >= 0: lands in [0, 32) when x < 256, the byte-index bound of the
        // crypto down-counting loops. Previously the body left the fragment (a shift bailed).
        let s = parse(
            "fn f(x: i64 where x >= 0 && x < 256) -> (r: i64) ensures { r >= 0; r < 32 } { x >>> 3 }",
        );
        assert!(verify(&s).is_ok());
        // Arithmetic `>>` behaves the same on a non-negative operand.
        let a = parse("fn f(x: i64 where x >= 0) -> (r: i64) ensures { r >= 0 } { x >> 4 }");
        assert!(verify(&a).is_ok());
    }

    #[test]
    fn does_not_prove_a_right_shift_bound_without_the_sign() {
        // Arithmetic `x >> 1` of a negative `x` is negative, `r >= 0` must NOT be proven.
        let s = parse("fn f(x: i64) -> (r: i64) ensures { r >= 0 } { x >> 1 }");
        assert!(verify(&s).is_err());
    }

    #[test]
    fn proves_the_range_of_a_bitmask() {
        // `x & m` lands in [0, m] for any `x` when `m >= 0`, the low-bits confinement.
        let m = parse("fn f(x: i64) -> (r: i64) ensures { r >= 0; r <= 7 } { x & 7 }");
        assert!(verify(&m).is_ok());
    }

    #[test]
    fn treats_a_left_shift_as_a_multiply() {
        // `x << 3` == `x * 8` exactly, so a bounded operand keeps a provable bound.
        let s = parse(
            "fn f(x: i64 where x >= 0 && x < 10) -> (r: i64) ensures { r >= 0; r < 80 } { x << 3 }",
        );
        assert!(verify(&s).is_ok());
    }

    #[test]
    fn reasons_about_a_slice_length_field_in_a_contract() {
        // `s.len` is a consistent symbolic value: `where n < s.len` bounds `n` by the slice's
        // length, and the postcondition restating it is proven.
        let funcs = parse(
            "fn pick(s: Slice, n: i64 where n < s.len) -> (r: i64) requires { s.len > 0 } ensures { r < s.len } { n }",
        );
        assert!(verify(&funcs).is_ok());
        // Without the bound, `r < s.len` is not provable (n is unconstrained).
        let bad = parse("fn pick(s: Slice, n: i64) -> (r: i64) ensures { r < s.len } { n }");
        assert!(verify(&bad).is_err());
    }

    #[test]
    fn a_where_refinement_discharges_like_a_precondition() {
        // `where x > 0` on the parameter proves the postcondition, exactly as a `requires` would.
        let funcs = parse("fn f(x: i64 where x > 0) -> (r: i64) ensures { r > 0 } { x }");
        assert!(verify(&funcs).is_ok());
        // A `&&` refinement contributes both bounds, so `x + 1` is proven not to overflow.
        let funcs2 = parse(
            "fn f(x: i64 where x > 0 && x < 100) -> (r: i64) ensures { r > x } { x + 1 }",
        );
        assert!(verify(&funcs2).is_ok());
    }

    #[test]
    fn rejects_a_proof_that_relies_on_i32_overflow() {
        // i32::MAX = 2147483647. `x + 1` overflows i32 at x = i32::MAX (r wraps to i32::MIN, so
        // `r > x` is false), even though the sum fits comfortably in i64. The prover must bound
        // at the DECLARED width, not i64.
        let funcs = parse(
            "fn f(x: i32) -> (r: i32) requires { x > 0; x <= 2147483647 } ensures { r > x } { x + 1 }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn proves_a_bounded_i32_increment() {
        // With x < 100 the i32 add stays well inside [i32::MIN, i32::MAX], so `r > x` holds.
        let funcs = parse(
            "fn f(x: i32) -> (r: i32) requires { x > 0; x < 100 } ensures { r > x } { x + 1 }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn i32_overflow_rejection_exhibits_the_counterexample() {
        // The witness search must wrap `x + 1` at i32 width (not i64) to see the break, so the
        // diagnostic names the exact input: x = i32::MAX = 2147483647.
        let funcs = parse(
            "fn f(x: i32) -> (r: i32) requires { x > 0; x <= 2147483647 } ensures { r > x } { x + 1 }",
        );
        let err = verify(&funcs).unwrap_err();
        assert!(
            err.message.contains("counterexample") && err.message.contains("2147483647"),
            "expected an i32::MAX counterexample, got: {}",
            err.message
        );
    }

    #[test]
    fn proves_a_weaker_bound() {
        // x >= 10 entails x >= 3 (10 >= 3): a strictly weaker lower bound.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) requires { x >= 10 } ensures { r >= 3 } { x }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn proves_transitivity_of_two_assumptions() {
        // x >= y and y >= z entail x >= z, combining two assumptions, which needs a
        // real decision procedure (Fourier–Motzkin), not a single-assumption match.
        let funcs = parse(
            "fn f(x: i64, y: i64, z: i64) -> (r: i64) \
             requires { x >= y; y >= z } ensures { r >= z } { x }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn proves_a_sum_of_two_lower_bounds() {
        // a >= 3 and b >= 4 entail a + b >= 7 (a genuine linear combination). Upper bounds keep
        // the sum from overflowing so the prover may use its exact value.
        let funcs = parse(
            "fn f(a: i64, b: i64) -> (r: i64) requires { a >= 3; a < 1000; b >= 4; b < 1000 } ensures { r >= 7 } { a + b }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn still_rejects_an_unprovable_bound() {
        // x >= 0 does NOT entail x >= 5, the prover must remain sound and reject it.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) requires { x >= 0 } ensures { r >= 5 } { x }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn proves_a_postcondition_across_both_branches_of_an_if() {
        // Every path returns a value ≥ 0, so the postcondition holds on all of them.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) ensures { r >= 0 } { if x >= 0 { x } else { 0 } }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn uses_the_branch_condition_as_an_assumption() {
        // In the then-branch `x > 5` is known, so `r = x` gives `r > 0`; the else-branch
        // returns 1 > 0. The branch guard must enter the assumptions of its path.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) ensures { r > 0 } { if x > 5 { x } else { 1 } }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_when_one_branch_violates_the_postcondition() {
        // The else-branch returns -1, breaking `r >= 0`, one bad path rejects the whole.
        let funcs = parse(
            "fn f(x: i64) -> (r: i64) ensures { r >= 0 } { if x >= 0 { x } else { 0 - 1 } }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn names_the_failing_path_in_the_diagnostic() {
        // The else-branch (reached when `x < 0`) returns -1, breaking `r >= 0`. A good
        // diagnostic must name the PATH under which the proof fails, so the red→green
        // loop knows WHICH case to fix, not merely that the postcondition failed.
        let funcs = parse(
            "fn f(x: i64) -> (r: i64) ensures { r >= 0 } { if x >= 0 { x } else { 0 - 1 } }",
        );
        let err = verify(&funcs).expect_err("must reject the broken else-branch");
        assert!(
            err.message.contains("x < 0"),
            "diagnostic should name the failing path (`x < 0`); got: {}",
            err.message
        );
    }

    #[test]
    fn straight_line_diagnostic_has_no_spurious_path_clause() {
        // A function with no branches has no path condition to report, the diagnostic
        // must NOT invent an empty "on the path where" clause.
        let funcs = parse("fn f(x: i64) -> (r: i64) ensures { r >= 0 } { x }");
        let err = verify(&funcs).expect_err("must reject: x may be negative");
        assert!(
            !err.message.contains("on the path where"),
            "straight-line function should have no path clause; got: {}",
            err.message
        );
    }

    #[test]
    fn bail_diagnostic_names_the_loop_not_a_laundry_list() {
        // A `for` loop without an `invariant { .. }` leaves the verifiable fragment. The
        // diagnostic must name THAT specific construct, not enumerate every construct it
        // could have been (early return, uncontracted call), which tells qwen nothing.
        let funcs = parse(
            "fn count(n: i64) -> (r: i64) ensures { r >= 0 } { let mut r = 0\nfor r < n { r = r + 1 }\nr }",
        );
        let err = verify(&funcs).expect_err("a loop without an invariant is unverifiable");
        assert!(
            err.message.contains("invariant"),
            "should name the missing loop invariant; got: {}",
            err.message
        );
        assert!(
            !err.message.contains("early return"),
            "should not list unrelated constructs; got: {}",
            err.message
        );
    }

    #[test]
    fn rejection_shows_a_concrete_counterexample() {
        // `r >= 0` with r = x and no precondition is unprovable. Per the Prime Directive
        // (feedback mandate: "actionable, a counterexample input"), the diagnostic must
        // exhibit a concrete input that breaks the contract, not just say it failed.
        let funcs = parse("fn f(x: i64) -> (r: i64) ensures { r >= 0 } { x }");
        let err = verify(&funcs).expect_err("x may be negative");
        assert!(
            err.message.contains("counterexample"),
            "should give a counterexample; got: {}",
            err.message
        );
        assert!(
            err.message.contains("x ="),
            "should name the offending input; got: {}",
            err.message
        );
    }

    #[test]
    fn divisor_rejection_shows_a_zero_counterexample() {
        // An unguarded divisor: the counterexample that breaks the nonzero obligation is
        // `b = 0`, and the diagnostic must show it.
        let funcs = parse("fn d(a: i64, b: i64) -> i64 { a / b }");
        let err = verify(&funcs).expect_err("b may be zero");
        assert!(
            err.message.contains("b = 0"),
            "should show the divisor counterexample b = 0; got: {}",
            err.message
        );
    }

    #[test]
    fn rejects_signed_min_div_minus_one() {
        // `divisor != 0` is not enough for SIGNED division: `i64::MIN / -1` overflows the
        // quotient and traps (#DE / SIGFPE). A `requires { y != 0 }` proves nonzero but not
        // the overflow case, so the function must be REJECTED, not compiled.
        let funcs =
            parse("fn q(x: i64, y: i64) -> (r: i64) requires { y != 0 } { x / y }");
        let err = verify(&funcs).expect_err("MIN / -1 overflows even with y != 0");
        assert!(
            err.message.contains("overflow"),
            "should name the signed-division overflow; got: {}",
            err.message
        );
    }

    #[test]
    fn signed_mod_min_minus_one_also_rejected() {
        // `i64::MIN % -1` traps the same way as the division.
        let funcs =
            parse("fn r(x: i64, y: i64) -> (r: i64) requires { y != 0 } { x % y }");
        assert!(verify(&funcs).is_err(), "MIN % -1 overflows too");
    }

    #[test]
    fn accepts_signed_div_when_divisor_positive() {
        // `y > 0` proves BOTH obligations: y != 0 (nonzero) and y != -1 (no MIN/-1 overflow).
        let funcs =
            parse("fn q(x: i64, y: i64) -> (r: i64) requires { y > 0 } { x / y }");
        assert!(verify(&funcs).is_ok(), "y > 0 excludes both 0 and -1");
    }

    #[test]
    fn accepts_signed_div_when_dividend_above_min() {
        // Excluding the dividend from i64::MIN also discharges the overflow obligation.
        let funcs = parse(
            "fn q(x: i64, y: i64) -> (r: i64) requires { y != 0; x > 0 } { x / y }",
        );
        assert!(verify(&funcs).is_ok(), "x > 0 means x != i64::MIN");
    }

    #[test]
    fn unsigned_div_needs_only_nonzero() {
        // Unsigned division (`divq`) never has the MIN/-1 overflow, so `y != 0` suffices -
        // the overflow obligation must NOT be raised for u64.
        let funcs =
            parse("fn q(x: u64, y: u64) -> (r: u64) requires { y != 0 } { x / y }");
        assert!(verify(&funcs).is_ok(), "unsigned division only needs a nonzero divisor");
    }

    #[test]
    fn bail_diagnostic_names_an_unsupported_expression() {
        // A comparison used as a value (`let b = x > 0`) is outside the fragment. The
        // diagnostic must point at the expression, not at loops it does not contain.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) ensures { r >= 0 } { let b = x > 0\n0 }");
        let err = verify(&funcs).expect_err("a comparison-as-value is unsupported");
        assert!(
            err.message.contains("expression"),
            "should name the unsupported expression; got: {}",
            err.message
        );
        assert!(
            !err.message.contains("loop"),
            "should not mention loops it is not about; got: {}",
            err.message
        );
    }

    #[test]
    fn uses_a_callees_postcondition_as_a_summary() {
        // `nonneg` guarantees a result >= 0; the caller relies on that CONTRACT (not the
        // body) to prove its own postcondition, modular, inter-procedural reasoning.
        let funcs = parse(
            "fn nonneg(x: i64) -> (r: i64) ensures { r >= 0 } { if x >= 0 { x } else { 0 } }\n\
             fn use_it(y: i64) -> (r: i64) ensures { r >= 0 } { nonneg(y) }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn checks_a_callees_precondition_at_the_call_site() {
        // `inv` requires x > 0. The caller cannot show its argument is positive, so the
        // call's precondition is an unmet obligation, reject.
        let funcs = parse(
            "fn inv(x: i64) -> (r: i64) requires { x > 0 } ensures { r >= 0 } { x }\n\
             fn bad(y: i64) -> (r: i64) ensures { r >= 0 } { inv(y) }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn proves_a_callees_precondition_from_the_callers_precondition() {
        // The caller knows y > 5, which entails inv's precondition y > 0, accepted.
        let funcs = parse(
            "fn inv(x: i64) -> (r: i64) requires { x > 0 } ensures { r >= 0 } { x }\n\
             fn ok(y: i64) -> (r: i64) requires { y > 5 } ensures { r >= 0 } { inv(y) }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn proves_a_loop_postcondition_with_an_invariant() {
        // A counting loop with invariant `r >= 0`: it holds on entry (r=0), is preserved
        // (r+1 ≥ 0 when r ≥ 0), and gives `r >= 0` on exit, the postcondition is proven.
        let funcs = parse(
            "fn count(n: i64) -> (r: i64) requires { n >= 0 } ensures { r >= 0 } \
             { let mut r = 0\n for r < n invariant { r >= 0 } { r = r + 1 }\n r }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_loop_whose_invariant_does_not_hold_initially() {
        // The invariant `r >= 5` is false on entry (r starts at 0), reject.
        let funcs = parse(
            "fn f(n: i64) -> (r: i64) ensures { r >= 5 } \
             { let mut r = 0\n for r < n invariant { r >= 5 } { r = r + 1 }\n r }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn uses_the_negated_loop_condition_after_the_loop() {
        // After `for r < n`, the negated guard gives `r >= n`; with the invariant carrying
        // `r >= 0`, the exit state proves the postcondition `r >= n`.
        let funcs = parse(
            "fn f(n: i64) -> (r: i64) requires { n >= 0 } ensures { r >= n } \
             { let mut r = 0\n for r < n invariant { r >= 0 } { r = r + 1 }\n r }",
        );
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_loop_whose_invariant_is_not_preserved() {
        // Invariant `r <= 3` is true initially but the body can break it (r grows past 3),
        // so preservation fails, reject.
        let funcs = parse(
            "fn f(n: i64) -> (r: i64) ensures { r >= 0 } \
             { let mut r = 0\n for r < n invariant { r <= 3 } { r = r + 1 }\n r }",
        );
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn rejects_an_unguarded_division_even_without_a_contract() {
        // `a / b` with an unconstrained `b` can divide by zero, a compile error, even
        // though the function has no contract at all (div-by-zero is checked everywhere).
        let funcs = parse("fn d(a: i64, b: i64) -> i64 { a / b }");
        assert!(verify(&funcs).is_err());
    }

    #[test]
    fn accepts_division_by_a_nonzero_constant() {
        // `a / 2`: the divisor is a nonzero constant, so the obligation `2 != 0` is trivial.
        assert!(verify(&parse("fn half(a: i64) -> i64 { a / 2 }")).is_ok());
    }

    #[test]
    fn accepts_a_division_guarded_by_a_precondition() {
        // `requires { b > 0 }` discharges BOTH obligations of a signed division: the divisor
        // is nonzero and (b != -1) it cannot overflow. (`b != 0` alone is no longer enough -
        // see `rejects_signed_min_div_minus_one`.)
        let funcs = parse("fn d(a: i64, b: i64) -> i64 requires { b > 0 } { a / b }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn accepts_a_division_guarded_by_a_branch() {
        // A signed division whose then-branch establishes `b > 0` proves both obligations
        // (nonzero and, since b >= 1 > -1, no MIN/-1 overflow) from the branch guard.
        let funcs =
            parse("fn sd(a: i64, b: i64) -> i64 { if b > 0 { a / b } else { 0 } }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn unsigned_division_guarded_by_a_branch_needs_no_min_guard() {
        // For unsigned division the negated `b == 0` guard alone suffices, no overflow case.
        let funcs =
            parse("fn sd(a: u64, b: u64) -> u64 { if b == 0 { 0 } else { a / b } }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_modulo_by_a_possibly_zero_divisor() {
        // `%` has the same divide-by-zero hazard as `/`.
        assert!(verify(&parse("fn m(a: i64, b: i64) -> i64 { a % b }")).is_err());
    }
}

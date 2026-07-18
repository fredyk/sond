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

use crate::ast::{BinOp, Block, Expr, Function, Stmt, Type};
use std::collections::{BTreeMap, HashMap, HashSet};

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
}

impl Ctx<'_> {
    /// A fresh symbolic variable for an opaque value (a call result), distinct from all
    /// parameters and from every other call result.
    fn fresh_var(&mut self) -> Sym {
        self.fresh += 1;
        Sym::Var(format!("__call{}", self.fresh))
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
        return reject(
            "cannot prove its contract, either a loop invariant could not be established \
             or preserved (strengthen the `invariant { .. }`), or the body uses a construct \
             the verifier does not support yet (early return, or a call to a function \
             without a usable contract). Sond does not accept a contract it cannot verify."
                .to_string(),
        );
    };
    // The precondition holds on EVERY path (it constrains the inputs). Evaluate it in the
    // parameter-only environment so a later `let` cannot shadow a precondition variable.
    let requires: Vec<SymProp> = f
        .requires
        .iter()
        .filter_map(|e| sym_prop(e, &env))
        .collect();
    for mut path in paths {
        if let Some(rn) = &f.ret_name {
            path.env.insert(rn.clone(), path.ret.clone());
        }
        // Discharge every obligation (a nonzero divisor, a callee precondition) under the
        // precondition plus the assumptions in scope where it arose.
        for ob in &path.obligations {
            let mut asm = requires.clone();
            asm.extend(ob.assumptions.iter().cloned());
            if !matches!(prove(&ob.prop, &asm), Outcome::Proven) {
                return reject(format!(
                    "cannot prove {}{}",
                    ob.what,
                    path_where(&ob.assumptions)
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
            match prove(&p, &assumptions) {
                Outcome::Proven => {}
                Outcome::Disproven => {
                    return reject(format!(
                        "postcondition `{}` is false{}",
                        show_prop(&p),
                        where_clause
                    ));
                }
                Outcome::Unknown => {
                    return reject(format!(
                        "cannot prove postcondition `{}`{}, Sond rejects a contract it \
                         cannot verify (add the assumptions it needs, or strengthen the prover)",
                        show_prop(&p),
                        where_clause
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
    let tail = block.tail.as_ref()?;
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
                return None;
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
            Stmt::Assign { .. } => return None,
            Stmt::Expr(_) => {} // a discarded side effect does not change the result
            // A loop is verified via its invariant (init + preservation), then continues
            // with (invariant ∧ ¬condition). Without an invariant it is unverifiable.
            Stmt::For {
                cond,
                invariant,
                body,
            } if !invariant.is_empty() => {
                states = eval_loop(states, cond.as_ref(), invariant, body, ctx)?;
            }
            Stmt::For { .. } | Stmt::Return(_) | Stmt::Break | Stmt::Continue => return None,
        }
        if states.len() > MAX_PATHS {
            return None;
        }
    }
    Some(states)
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
                            what: format!("the divisor `{}` is nonzero", show_sym(&rv)),
                        });
                    }
                    let value = match (&lv, &rv) {
                        (Sym::Const(a), Sym::Const(b)) if *b != 0 => Sym::Const(match op {
                            BinOp::Div => a.wrapping_div(*b),
                            _ => a.wrapping_rem(*b),
                        }),
                        _ => ctx.fresh_var(), // non-linear result: opaque
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
            let else_b = else_branch.as_ref()?;
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
        _ => None, // comparisons-as-values, field/index reads, etc.: out of the fragment
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
    let mut out = Vec::new();
    for st in states {
        // (1) INIT: every invariant proposition holds under the current assumptions.
        for inv in invariant {
            let p = sym_prop(inv, &st.env)?;
            if !matches!(prove(&p, &st.cons), Outcome::Proven) {
                return None;
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
            return None; // an invariant proposition is outside the fragment
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
                if !matches!(prove(&ob.prop, &ob.assumptions), Outcome::Proven) {
                    return None;
                }
            }
            // The invariant must be re-established at the end of the iteration.
            for inv in invariant {
                let p = sym_prop(inv, &bs.env)?;
                if !matches!(prove(&p, &bs.cons), Outcome::Proven) {
                    return None;
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
struct Lin {
    coeffs: BTreeMap<String, i64>,
    k: i64,
}

impl Lin {
    fn constant(k: i64) -> Self {
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
    fn as_const(&self) -> Option<i64> {
        self.coeffs.values().all(|&c| c == 0).then_some(self.k)
    }
    /// Drops variables whose coefficient has become zero (keeps the map canonical).
    fn pruned(mut self) -> Self {
        self.coeffs.retain(|_, &mut c| c != 0);
        self
    }
    fn scale(&self, f: i64) -> Lin {
        Lin {
            coeffs: self.coeffs.iter().map(|(v, c)| (v.clone(), c * f)).collect(),
            k: self.k * f,
        }
    }
    fn add(&self, o: &Lin) -> Lin {
        let mut coeffs = self.coeffs.clone();
        for (v, c) in &o.coeffs {
            *coeffs.entry(v.clone()).or_insert(0) += c;
        }
        Lin {
            coeffs,
            k: self.k + o.k,
        }
        .pruned()
    }
    fn sub(&self, o: &Lin) -> Lin {
        self.add(&o.scale(-1))
    }
}

/// Converts a symbolic value to a linear expression, or `None` if it is non-linear
/// (a product of two non-constant terms), the prover stays within linear arithmetic.
fn lin_of_sym(s: &Sym) -> Option<Lin> {
    match s {
        Sym::Const(n) => Some(Lin::constant(*n)),
        Sym::Var(v) => Some(Lin::var(v)),
        Sym::Add(a, b) => Some(lin_of_sym(a)?.add(&lin_of_sym(b)?)),
        Sym::Sub(a, b) => Some(lin_of_sym(a)?.sub(&lin_of_sym(b)?)),
        Sym::Mul(a, b) => {
            let (la, lb) = (lin_of_sym(a)?, lin_of_sym(b)?);
            match (la.as_const(), lb.as_const()) {
                (Some(c), _) => Some(lb.scale(c)),
                (_, Some(c)) => Some(la.scale(c)),
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
                let b = -n.coeffs[&x];
                next.push(p.scale(b).add(&n.scale(a)));
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
    let mut neg = e.scale(-1);
    neg.k -= 1; // -e - 1 ≥ 0  ==  e ≤ -1  ==  ¬(e ≥ 0) over ℤ
    sys.push(neg);
    unsat(sys)
}

/// The `≥ 0` constraints a comparison asserts as a fact (for an assumption), or the
/// constraints whose conjunction means the goal holds. `> `/`<` are integer-tightened
/// (`a > b` ⟺ `a - b - 1 ≥ 0`); `==` yields both directions; `!=` has no conjunctive
/// form (`None`).
fn holds_cons(op: &BinOp, lhs: &Lin, rhs: &Lin) -> Option<Vec<Lin>> {
    let diff = lhs.sub(rhs); // a - b
    let tighten = |mut e: Lin| {
        e.k -= 1;
        e
    };
    Some(match op {
        BinOp::Ge => vec![diff],
        BinOp::Gt => vec![tighten(diff)],
        BinOp::Le => vec![rhs.sub(lhs)],
        BinOp::Lt => vec![tighten(rhs.sub(lhs))],
        BinOp::Eq => vec![diff.clone(), rhs.sub(lhs)],
        _ => return None, // Ne, not a conjunction of ≥ 0 facts
    })
}

fn prove(p: &SymProp, assumptions: &[SymProp]) -> Outcome {
    // Syntactic match: an assumption identical to the goal proves it directly. This is the
    // path for `!=` facts (e.g. `b != 0` from a precondition or a negated `b == 0` guard),
    // which the linear machinery below drops because `!=` is not a conjunctive `≥ 0` fact.
    if assumptions
        .iter()
        .any(|a| a.op == p.op && a.lhs == p.lhs && a.rhs == p.rhs)
    {
        return Outcome::Proven;
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
        let mut eq_sys = a.clone();
        eq_sys.push(l.sub(&r));
        eq_sys.push(r.sub(&l));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn parse(src: &str) -> Vec<Function> {
        Parser::new(tokenize(src)).parse_program()
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
        // x > 0 entails x + 1 > 1, the linear prover must shift a bound, which the
        // exact-match prover could not do.
        let funcs =
            parse("fn f(x: i64) -> (r: i64) requires { x > 0 } ensures { r > 1 } { x + 1 }");
        assert!(verify(&funcs).is_ok());
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
        // a >= 3 and b >= 4 entail a + b >= 7 (a genuine linear combination).
        let funcs = parse(
            "fn f(a: i64, b: i64) -> (r: i64) requires { a >= 3; b >= 4 } ensures { r >= 7 } { a + b }",
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
    fn uses_a_callees_postcondition_as_a_summary() {
        // `nonneg` guarantees a result >= 0; the caller relies on that CONTRACT (not the
        // body) to prove its own postcondition, modular, inter-procedural reasoning.
        let funcs = parse(
            "fn nonneg(x: i64) -> (r: i64) ensures { r >= 0 } { if x >= 0 { x } else { 0 - x } }\n\
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
        // `requires { b != 0 }` discharges the divisor obligation directly.
        let funcs = parse("fn d(a: i64, b: i64) -> i64 requires { b != 0 } { a / b }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn accepts_a_division_guarded_by_a_branch() {
        // The safe_div shape: in the else-branch `b != 0` is known from the negated guard.
        let funcs = parse("fn sd(a: i64, b: i64) -> i64 { if b == 0 { 0 } else { a / b } }");
        assert!(verify(&funcs).is_ok());
    }

    #[test]
    fn rejects_a_modulo_by_a_possibly_zero_divisor() {
        // `%` has the same divide-by-zero hazard as `/`.
        assert!(verify(&parse("fn m(a: i64, b: i64) -> i64 { a % b }")).is_err());
    }
}

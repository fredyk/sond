//! `s.utf8` on a `string` bound to a literal: the bytes that literal was written with.
//!
//! The substitution runs once the whole program is parsed, over each function with its own
//! scopes. It used to happen while parsing, from one map of NAME -> text shared by the whole
//! program and never scoped: a `string` parameter called `ruta` received the bytes of a
//! `let ruta: string = ".."` written in another function of another file, whichever the include
//! order put first, and a reassigned or shadowed `s` kept the text of its first `let`.
//!
//! A `NAME.utf8` becomes the literal's bytes only when the binding it resolves to, lexically,
//! is a `let NAME: string = ".."` (local or module-level) that NOTHING assigns afterwards. Then
//! the text is the same at every use, loops and branches included. Anything else (a parameter,
//! a reassigned string, a binding of another kind) stays `NAME.utf8`, and the codegen resolves
//! it with the flow of the function it is in.

use crate::ast::{Block, Expr, Function, Global, Stmt, Type};
use std::collections::HashMap;

/// One binding of a name: the literal it was bound to, if it was a `string` literal, and
/// whether anything assigns it.
#[derive(Default, Clone)]
struct Binding {
    text: Option<String>,
    assigned: bool,
}

/// Is this `let` the lowering of `let NAME: string = "..."`? The parser types it as a view of
/// code points (`u32[len]`) and keeps the literal as the value.
fn string_literal(ty: &Option<Type>, value: &Expr) -> Option<String> {
    match (ty, value) {
        (Some(Type::View(elem, Some(_))), Expr::Str(text)) if matches!(elem.as_ref(), Type::Named(n) if n == "u32") => {
            Some(text.clone())
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Record which bindings are assigned.
    Assignments,
    /// Replace `NAME.utf8` by the literal's bytes where that is sound.
    Substitute,
}

struct Walker<'a> {
    phase: Phase,
    /// Module-level bindings (index `0..globals.len()`), then this function's locals.
    bindings: &'a mut Vec<Binding>,
    /// Id of the next local binding. The first phase creates them; the second walks the same
    /// code in the same order, so it meets them again under the same ids, `assigned` included.
    next: usize,
    scopes: Vec<HashMap<String, usize>>,
}

impl Walker<'_> {
    fn bind(&mut self, name: &str, text: Option<String>) {
        let id = self.next;
        self.next += 1;
        if self.phase == Phase::Assignments {
            self.bindings.push(Binding {
                text,
                assigned: false,
            });
        } else {
            debug_assert_eq!(self.bindings[id].text, text, "both phases bind alike");
        }
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .insert(name.to_string(), id);
    }

    fn resolve(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn block(&mut self, b: &mut Block) {
        self.scopes.push(HashMap::new());
        for s in &mut b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &mut b.tail {
            self.expr(t);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::Let {
                name,
                ty,
                value,
                refinement,
                ..
            } => {
                // The value is read BEFORE the new binding exists: `let s: string = s` would read
                // the outer `s`.
                let substituted = self.expr(value);
                for r in refinement.iter_mut() {
                    self.expr(r);
                }
                // `let b = s.utf8` is a view of BYTES, and its length is their count, typed here
                // so the binding carries it the same way any other view does. Only when the value
                // IS that request: a `.utf8` buried in a larger expression says nothing about it.
                if self.phase == Phase::Substitute
                    && ty.is_none()
                    && substituted
                    && let Expr::Field(inner, _) = &*value
                    && let Expr::Str(text) = inner.as_ref()
                {
                    *ty = Some(Type::View(
                        Box::new(Type::Named("u8".to_string())),
                        Some(text.len().to_string()),
                    ));
                }
                let text = string_literal(ty, value);
                self.bind(name, text);
            }
            Stmt::Assign { target, value } => {
                self.expr(value);
                if let Expr::Var(n) = target {
                    if self.phase == Phase::Assignments
                        && let Some(id) = self.resolve(n)
                    {
                        self.bindings[id].assigned = true;
                    }
                } else {
                    self.expr(target);
                }
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            Stmt::For {
                cond,
                invariant,
                body,
                ..
            } => {
                if let Some(c) = cond {
                    self.expr(c);
                }
                for i in invariant.iter_mut() {
                    self.expr(i);
                }
                self.block(body);
            }
            Stmt::Break | Stmt::Continue => {}
            Stmt::Expr(e) | Stmt::Spawn(e) => {
                self.expr(e);
            }
            Stmt::Par(peers) => {
                for p in peers.iter_mut() {
                    self.expr(p);
                }
            }
            Stmt::ParLet { names, peers, .. } => {
                for p in peers.iter_mut() {
                    self.expr(p);
                }
                for n in names.iter() {
                    self.bind(n, None);
                }
            }
            Stmt::ParReduce { name, peers, .. } => {
                for p in peers.iter_mut() {
                    self.expr(p);
                }
                self.bind(name, None);
            }
            Stmt::GraphLet {
                name,
                nodes,
                yield_expr,
                ..
            } => {
                self.scopes.push(HashMap::new());
                for (n, _) in nodes.iter() {
                    self.bind(n, None);
                }
                for (_, e) in nodes.iter_mut() {
                    self.expr(e);
                }
                self.expr(yield_expr);
                self.scopes.pop();
                self.bind(name, None);
            }
            Stmt::Lock { body, .. } => self.block(body),
        }
    }

    /// Walks `e`; true when `e` itself was turned into a literal's bytes.
    fn expr(&mut self, e: &mut Expr) -> bool {
        match e {
            Expr::Field(base, field) => {
                if field == "utf8"
                    && self.phase == Phase::Substitute
                    && let Expr::Var(n) = base.as_ref()
                    && let Some(id) = self.resolve(n)
                    && !self.bindings[id].assigned
                    && let Some(text) = self.bindings[id].text.clone()
                {
                    **base = Expr::Str(text);
                    return true;
                }
                self.expr(base);
            }
            Expr::Call(callee, args) => {
                self.expr(callee);
                for a in args.iter_mut() {
                    self.expr(&mut a.value);
                }
            }
            Expr::List(xs) => {
                for x in xs.iter_mut() {
                    self.expr(x);
                }
            }
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expr::SliceRange(base, lo, hi) => {
                self.expr(base);
                if let Some(lo) = lo {
                    self.expr(lo);
                }
                if let Some(hi) = hi {
                    self.expr(hi);
                }
            }
            Expr::StructLit(_, fields) => {
                for (_, v) in fields.iter_mut() {
                    self.expr(v);
                }
            }
            Expr::Unary(_, x) | Expr::Cast(x, _) => {
                self.expr(x);
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.expr(cond);
                self.block(then_branch);
                if let Some(b) = else_branch {
                    self.block(b);
                }
            }
            Expr::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms.iter_mut() {
                    self.scopes.push(HashMap::new());
                    if let Some(b) = &arm.binding {
                        self.bind(b, None);
                    }
                    self.block(&mut arm.body);
                    self.scopes.pop();
                }
            }
            Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) => {}
        }
        false
    }
}

fn walk(
    phase: Phase,
    f: &mut Function,
    globals_scope: &HashMap<String, usize>,
    n_globals: usize,
    bindings: &mut Vec<Binding>,
) {
    let mut w = Walker {
        phase,
        bindings,
        next: n_globals,
        scopes: vec![globals_scope.clone(), HashMap::new()],
    };
    for p in &f.params {
        w.bind(&p.name, None);
    }
    for r in f.requires.iter_mut().chain(f.ensures.iter_mut()) {
        w.expr(r);
    }
    w.block(&mut f.body);
}

/// Resolves every `NAME.utf8` of `funcs` whose binding is an unassigned `string` literal.
pub(super) fn resolve_string_bytes(globals: &[Global], funcs: &mut [&mut Function]) {
    let mut global_bindings = Vec::new();
    let mut globals_scope = HashMap::new();
    for g in globals {
        globals_scope.insert(g.name.clone(), global_bindings.len());
        global_bindings.push(Binding {
            text: string_literal(&g.ty, &g.init),
            assigned: false,
        });
    }
    // Every assignment of the program first, so a module-level string assigned in any function
    // is known to be one before any function is rewritten.
    let n_globals = global_bindings.len();
    let mut locals: Vec<Vec<Binding>> = Vec::with_capacity(funcs.len());
    for f in funcs.iter_mut() {
        walk(
            Phase::Assignments,
            f,
            &globals_scope,
            n_globals,
            &mut global_bindings,
        );
        locals.push(global_bindings.split_off(n_globals));
    }
    for (f, own) in funcs.iter_mut().zip(locals) {
        let mut bindings = global_bindings.clone();
        bindings.extend(own);
        walk(
            Phase::Substitute,
            f,
            &globals_scope,
            n_globals,
            &mut bindings,
        );
    }
}

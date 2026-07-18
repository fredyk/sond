//! Struct lowering.
//!
//! A struct value is laid out as a fixed-size word array, one 8-byte word per
//! field, in declaration order. So `let p: Point` lowers to `let p[N]` (N = field
//! count) and `p.field` lowers to `p[field_index]`. This reuses the existing
//! word-array codegen: no new codegen is needed, structs are pure front-end sugar.
//!
//! Methods (`impl Point { fn m(self, ..) { .. } }`) are parsed into free functions
//! named `Point_m` with a `self` receiver typed as the struct. Here we lower a
//! method CALL `p.m(a)` to `Point_m(p, a)`, a minimal OOP over the flat namespace,
//! again with no new codegen.
//!
//! First slices (grown by TDD): field access on struct-typed locals and params,
//! struct literals, and method calls. Nested structs and value/pointer mutability
//! rules are follow-ups.

use crate::ast::{
    Arg, BinOp, Block, EnumDef, Expr, Function, Global, MatchArm, Param, Stmt, StructDef, Type,
};
use std::collections::{HashMap, HashSet};

/// What the desugarer knows about one enum variant: which enum it belongs to, its
/// integer tag (declaration order), whether its enum carries ANY payload (so the whole
/// enum uses the 2-word struct representation), and whether THIS variant carries one.
struct VariantInfo {
    enum_name: String,
    tag: i64,
    /// The owning enum has ≥1 payloaded variant → `{__tag, __payload}` struct form.
    enum_payloaded: bool,
    /// This specific variant carries a value (`Some(i64)` vs `None`).
    has_payload: bool,
}

/// Field indices of the synthetic 2-word struct that backs a payloaded enum.
const TAG: &str = "__tag";
const PAYLOAD: &str = "__payload";

/// Lowers sum types to forms the codegen already understands, and RETURNS the synthetic
/// struct definitions that payloaded enums need (the caller feeds them into
/// `desugar_structs`). Two representations, chosen per enum:
///
/// * **Payload-free** (`enum Shape { Circle, Square }`) → each variant becomes its
///   integer tag, and `match` becomes an if/else-if chain over integer equality. One
///   word, returnable by value, unchanged from before.
/// * **Payloaded** (`enum Opt { Some(i64), None }`) → a 2-word struct `{__tag, __payload}`.
///   A constructor `Some(v)` becomes the struct literal `Opt { __tag: 0, __payload: v }`;
///   a bare `None` becomes `Opt { __tag: 1, __payload: 0 }`; and `match` reads `.__tag`,
///   binding `.__payload` inside a `Some(x) => ..` arm. This reuses the struct word-array
///   codegen entirely, no new backend, and is the `Option`/`Result` (nil-safety) base.
///
/// The last arm of a `match` is the unconditional `else` (sound because exhaustiveness
/// was checked beforehand).
pub fn desugar_enums(funcs: &mut [Function], enums: &[EnumDef]) -> Vec<StructDef> {
    let mut info: HashMap<String, VariantInfo> = HashMap::new();
    let mut synth: Vec<StructDef> = Vec::new();
    for e in enums {
        let payloaded = e.variants.iter().any(|v| v.payload.is_some());
        for (i, v) in e.variants.iter().enumerate() {
            info.insert(
                v.name.clone(),
                VariantInfo {
                    enum_name: e.name.clone(),
                    tag: i as i64,
                    enum_payloaded: payloaded,
                    has_payload: v.payload.is_some(),
                },
            );
        }
        // A payloaded enum is backed by a 2-word record: the tag then the payload.
        if payloaded {
            synth.push(StructDef {
                vis: e.vis,
                name: e.name.clone(),
                fields: vec![
                    Param {
                        name: TAG.to_string(),
                        ty: Type::Named("i64".to_string()),
                    },
                    Param {
                        name: PAYLOAD.to_string(),
                        ty: Type::Named("i64".to_string()),
                    },
                ],
            });
        }
    }
    for f in funcs {
        lower_enum_block(&mut f.body, &info);
    }
    synth
}

fn lower_enum_block(b: &mut Block, info: &HashMap<String, VariantInfo>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                lower_enum_expr(value, info)
            }
            Stmt::Assign { target, value } => {
                lower_enum_expr(target, info);
                lower_enum_expr(value, info);
            }
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    lower_enum_expr(c, info);
                }
                lower_enum_block(body, info);
            }
            _ => {}
        }
    }
    if let Some(t) = &mut b.tail {
        lower_enum_expr(t, info);
    }
}

/// The struct literal for a variant used as a VALUE, either a bare variant (`None`,
/// no payload → payload word 0) or a constructor (`Some(v)` → payload word = `v`).
fn variant_struct_lit(vi: &VariantInfo, payload: Expr) -> Expr {
    Expr::StructLit(
        vi.enum_name.clone(),
        vec![
            (TAG.to_string(), Box::new(Expr::Int(vi.tag))),
            (PAYLOAD.to_string(), Box::new(payload)),
        ],
    )
}

fn lower_enum_expr(e: &mut Expr, info: &HashMap<String, VariantInfo>) {
    match e {
        // A bare variant name (`Red`, `None`). Payload-free enum → its integer tag;
        // payloaded enum → the 2-word struct literal with an unused (0) payload.
        Expr::Var(n) => {
            if let Some(vi) = info.get(n) {
                *e = if vi.enum_payloaded {
                    variant_struct_lit(vi, Expr::Int(0))
                } else {
                    Expr::Int(vi.tag)
                };
            }
        }
        // A constructor call `Some(v)`: a payloaded variant applied to its payload.
        // Becomes the 2-word struct literal carrying `v`. (Non-constructor calls fall
        // through to the normal recursion.)
        Expr::Call(callee, args) => {
            let ctor = if let Expr::Var(n) = callee.as_ref() {
                info.get(n).filter(|vi| vi.has_payload).map(|vi| vi.tag)
            } else {
                None
            };
            if let (Some(_), Expr::Var(n)) = (ctor, callee.as_ref()) {
                let vi = &info[n];
                // Lower the payload expression first (it may itself be a variant).
                let mut payload = args
                    .first()
                    .map(|a| a.value.clone())
                    .unwrap_or(Expr::Int(0));
                lower_enum_expr(&mut payload, info);
                *e = variant_struct_lit(vi, payload);
            } else {
                lower_enum_expr(callee, info);
                for a in args.iter_mut() {
                    lower_enum_expr(&mut a.value, info);
                }
            }
        }
        Expr::Match { scrutinee, arms } => {
            lower_enum_expr(scrutinee, info);
            for a in arms.iter_mut() {
                lower_enum_block(&mut a.body, info);
            }
            // The enum these arms belong to decides the representation.
            let payloaded = arms
                .first()
                .and_then(|a| info.get(&a.variant))
                .map(|vi| vi.enum_payloaded)
                .unwrap_or(false);
            *e = if payloaded {
                lower_match_payload(scrutinee, arms, info)
            } else {
                lower_match_int(scrutinee, arms, info)
            };
        }
        Expr::Binary(_, l, r) => {
            lower_enum_expr(l, info);
            lower_enum_expr(r, info);
        }
        Expr::Unary(_, x) => lower_enum_expr(x, info),
        Expr::Index(b, i) => {
            lower_enum_expr(b, info);
            lower_enum_expr(i, info);
        }
        Expr::Field(b, _) => lower_enum_expr(b, info),
        Expr::List(items) => items.iter_mut().for_each(|it| lower_enum_expr(it, info)),
        Expr::StructLit(_, fl) => fl.iter_mut().for_each(|(_, v)| lower_enum_expr(v, info)),
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            lower_enum_expr(cond, info);
            lower_enum_block(then_branch, info);
            if let Some(eb) = else_branch {
                lower_enum_block(eb, info);
            }
        }
        Expr::Cast(inner, _) => lower_enum_expr(inner, info),
        _ => {}
    }
}

/// If/else-if chain for a PAYLOAD-FREE match: the scrutinee is an integer tag, so each
/// arm tests `scrutinee == tag`. Scrutinee and bodies are already lowered.
fn lower_match_int(scrutinee: &Expr, arms: &[MatchArm], info: &HashMap<String, VariantInfo>) -> Expr {
    let n = arms.len();
    let mut acc = arms[n - 1].body.clone(); // final else = the last arm
    for i in (0..n - 1).rev() {
        let tag = info.get(&arms[i].variant).map(|vi| vi.tag).unwrap_or(0);
        let cond = Expr::Binary(
            BinOp::Eq,
            Box::new(scrutinee.clone()),
            Box::new(Expr::Int(tag)),
        );
        let if_expr = Expr::If {
            cond: Box::new(cond),
            then_branch: Box::new(arms[i].body.clone()),
            else_branch: Some(Box::new(acc)),
        };
        acc = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(if_expr)),
        };
    }
    match acc.tail {
        Some(t) => *t,
        None => Expr::Int(0),
    }
}

/// If/else-if chain for a PAYLOADED match: the scrutinee is a 2-word struct, so each arm
/// tests `scrutinee.__tag == tag` and, when it binds (`Some(x) => ..`), prepends
/// `let x = scrutinee.__payload` to the arm body. Scrutinee and bodies are already lowered.
fn lower_match_payload(
    scrutinee: &Expr,
    arms: &[MatchArm],
    info: &HashMap<String, VariantInfo>,
) -> Expr {
    let n = arms.len();
    let mut acc = arm_block(scrutinee, &arms[n - 1]); // final else = the last arm
    for i in (0..n - 1).rev() {
        let tag = info.get(&arms[i].variant).map(|vi| vi.tag).unwrap_or(0);
        let cond = Expr::Binary(
            BinOp::Eq,
            Box::new(Expr::Field(Box::new(scrutinee.clone()), TAG.to_string())),
            Box::new(Expr::Int(tag)),
        );
        let if_expr = Expr::If {
            cond: Box::new(cond),
            then_branch: Box::new(arm_block(scrutinee, &arms[i])),
            else_branch: Some(Box::new(acc)),
        };
        acc = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(if_expr)),
        };
    }
    match acc.tail {
        Some(t) => *t,
        None => Expr::Int(0),
    }
}

/// The block a payloaded arm runs: its body, with `let binding = scrutinee.__payload`
/// prepended when the arm binds the carried value (`Some(x) => ..`).
fn arm_block(scrutinee: &Expr, arm: &MatchArm) -> Block {
    let mut b = arm.body.clone();
    if let Some(bind) = &arm.binding {
        b.stmts.insert(
            0,
            Stmt::Let {
                mutable: false,
                name: bind.clone(),
                ty: None,
                value: Expr::Field(Box::new(scrutinee.clone()), PAYLOAD.to_string()),
                array_len: None,
            },
        );
    }
    b
}

/// Hoists every struct literal that appears in an EXPRESSION position, a call
/// argument, an operand, a branch tail: anywhere except the direct initializer of a
/// `let`, into a preceding `let __litN = <lit>`, replacing the literal with `__litN`.
/// After this pass a struct literal exists ONLY as a `let` initializer, which the struct
/// pass already lowers to an allocated word-array, so a constructor works in any
/// position: `f(Some(42))`, `return Some(x)`, `if c { None } else { Some(y) }`. Runs
/// AFTER enum desugaring (which turns constructors into struct literals) and BEFORE
/// struct desugaring. This is the prerequisite for passing/returning enum values by
/// value without a special case in the backend.
pub fn hoist_struct_literals(funcs: &mut [Function]) {
    for f in funcs {
        let mut counter = 0usize;
        hoist_block(&mut f.body, &mut counter);
    }
}

fn hoist_block(b: &mut Block, counter: &mut usize) {
    let mut out: Vec<Stmt> = Vec::with_capacity(b.stmts.len());
    for mut stmt in std::mem::take(&mut b.stmts) {
        let mut hoists: Vec<Stmt> = Vec::new();
        match &mut stmt {
            // A direct `let p = StructLit` stays (the struct pass lowers it); only hoist
            // literals nested inside a non-literal initializer.
            Stmt::Let { value, .. } => {
                if !matches!(value, Expr::StructLit(..)) {
                    hoist_in_expr(value, &mut hoists, counter);
                }
            }
            Stmt::Assign { target, value } => {
                hoist_in_expr(target, &mut hoists, counter);
                hoist_in_expr(value, &mut hoists, counter);
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => hoist_in_expr(e, &mut hoists, counter),
            Stmt::For { cond, body, .. } => {
                if let Some(c) = cond {
                    hoist_in_expr(c, &mut hoists, counter);
                }
                hoist_block(body, counter);
            }
            _ => {}
        }
        out.extend(hoists); // the temps precede the statement that uses them
        out.push(stmt);
    }
    b.stmts = out;
    // The block's tail: hoist nested literals into appended temps (evaluated after the
    // statements, before the tail). If the tail IS a literal, it becomes that temp's var
    //, so a struct-literal branch tail turns into a struct address a backend can return.
    if let Some(tail) = &mut b.tail {
        let mut hoists: Vec<Stmt> = Vec::new();
        hoist_in_expr(tail, &mut hoists, counter);
        b.stmts.extend(hoists);
    }
}

fn hoist_in_expr(e: &mut Expr, hoists: &mut Vec<Stmt>, counter: &mut usize) {
    match e {
        Expr::StructLit(_, fields) => {
            // Hoist any literals nested in the field values first (inner temps precede).
            for (_, v) in fields.iter_mut() {
                hoist_in_expr(v, hoists, counter);
            }
            let name = format!("__lit{}", *counter);
            *counter += 1;
            let lit = std::mem::replace(e, Expr::Var(name.clone()));
            hoists.push(Stmt::Let {
                mutable: false,
                name,
                ty: None,
                value: lit,
                array_len: None,
            });
        }
        Expr::Call(c, args) => {
            hoist_in_expr(c, hoists, counter);
            for a in args.iter_mut() {
                hoist_in_expr(&mut a.value, hoists, counter);
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            hoist_in_expr(l, hoists, counter);
            hoist_in_expr(r, hoists, counter);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            hoist_in_expr(x, hoists, counter)
        }
        Expr::List(items) => items
            .iter_mut()
            .for_each(|it| hoist_in_expr(it, hoists, counter)),
        // An `if` owns its branch blocks: hoist within THEM at their own level (a branch's
        // struct-literal tail becomes a local in that branch), never across the boundary.
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            hoist_in_expr(cond, hoists, counter);
            hoist_block(then_branch, counter);
            if let Some(eb) = else_branch {
                hoist_block(eb, counter);
            }
        }
        _ => {}
    }
}

/// struct name -> (field name -> word index)
type FieldMap = HashMap<String, HashMap<String, i64>>;
/// struct name -> field count (its size in words)
type SizeMap = HashMap<String, i64>;
/// mangled names (`Type_method`) of every free function, used to spot method calls
type MethodSet = HashSet<String>;

/// The underlying struct name of a type, whether written as a value (`Point`) or
/// a pointer (`*Point`). Both lower to the same base-address representation, so
/// they resolve field access identically. Returns None for non-named types.
fn base_struct(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(n) => Some(n),
        Type::Ptr(inner) => base_struct(inner),
    }
}

/// Lowers all struct declarations, `let`s, field accesses and method calls in
/// `funcs` to the word-array + free-function forms the codegen already understands.
pub fn desugar_structs(funcs: &mut [Function], structs: &[StructDef]) {
    let mut fields: FieldMap = HashMap::new();
    let mut sizes: SizeMap = HashMap::new();
    for s in structs {
        let mut fm = HashMap::new();
        for (i, f) in s.fields.iter().enumerate() {
            fm.insert(f.name.clone(), i as i64);
        }
        sizes.insert(s.name.clone(), s.fields.len() as i64);
        fields.insert(s.name.clone(), fm);
    }
    // A `let r = f(..)` whose callee returns a struct/enum: annotate it with that return
    // type (`let r: Ret = f(..)`), so the existing type-annotation path below reserves r's
    // region and registers it as a struct local. The value stays a call; the backend
    // receives it by value (RAX:RDX). Done before lowering so `r.field`/`match r` resolve.
    let fn_ret_struct: HashMap<String, String> = funcs
        .iter()
        .filter_map(|f| match &f.ret {
            Some(t) => base_struct(t)
                .filter(|tn| sizes.contains_key(*tn))
                .map(|tn| (f.name.clone(), tn.to_string())),
            None => None,
        })
        .collect();
    for f in funcs.iter_mut() {
        annotate_struct_returns_block(&mut f.body, &fn_ret_struct);
    }
    // Every function name is a potential method target (`Type_method`); a call
    // `p.method(..)` on a struct-typed `p` is rewritten to `Type_method(p, ..)`.
    let methods: MethodSet = funcs.iter().map(|f| f.name.clone()).collect();
    for f in funcs.iter_mut() {
        // Struct-typed locals are function-scoped, matching the array frame model.
        let mut locals: HashMap<String, String> = HashMap::new();
        // A struct parameter is passed by reference (its base address), so field
        // access inside the body resolves just like a struct local.
        for p in &f.params {
            // Both a value struct (`p: Point`) and a pointer struct (`p: *Point`)
            // are received as a base address, so field access resolves the same.
            if let Some(tn) = base_struct(&p.ty).filter(|t| sizes.contains_key(*t)) {
                locals.insert(p.name.clone(), tn.to_string());
            }
        }
        desugar_block(&mut f.body, &fields, &sizes, &mut locals, &methods);
    }
}

/// Annotates each `let r = f(..)` whose callee `f` returns a struct/enum with that
/// return type (`let r: Ret = f(..)`), recursing into nested loop and `if` blocks. Idempotent
/// and only touches un-annotated lets bound directly to a call of a struct-returning fn.
fn annotate_struct_returns_block(b: &mut Block, fn_ret_struct: &HashMap<String, String>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let {
                ty: ty @ None,
                value: Expr::Call(callee, _),
                array_len: None,
                ..
            } => {
                if let Expr::Var(fname) = callee.as_ref()
                    && let Some(tn) = fn_ret_struct.get(fname)
                {
                    *ty = Some(Type::Named(tn.clone()));
                }
            }
            Stmt::For { body, .. } => annotate_struct_returns_block(body, fn_ret_struct),
            _ => {}
        }
    }
    if let Some(tail) = &mut b.tail {
        annotate_struct_returns_expr(tail, fn_ret_struct);
    }
}

/// Recurses into `if` branch blocks within an expression tail (where a `let`-annotation
/// site can hide), applying [`annotate_struct_returns_block`] to each branch.
fn annotate_struct_returns_expr(e: &mut Expr, fn_ret_struct: &HashMap<String, String>) {
    if let Expr::If {
        then_branch,
        else_branch,
        ..
    } = e
    {
        annotate_struct_returns_block(then_branch, fn_ret_struct);
        if let Some(eb) = else_branch {
            annotate_struct_returns_block(eb, fn_ret_struct);
        }
    }
}

fn desugar_block(
    b: &mut Block,
    fields: &FieldMap,
    sizes: &SizeMap,
    locals: &mut HashMap<String, String>,
    methods: &MethodSet,
) {
    let mut out: Vec<Stmt> = Vec::with_capacity(b.stmts.len());
    for mut stmt in std::mem::take(&mut b.stmts) {
        // `let p = Name { .. }`: allocate the array, then one assignment per field.
        let is_lit = matches!(
            &stmt,
            Stmt::Let { value: Expr::StructLit(sname, _), .. } if sizes.contains_key(sname)
        );
        if is_lit {
            let (pname, sname, lit) = match &mut stmt {
                Stmt::Let {
                    name,
                    value,
                    array_len,
                    ..
                } => {
                    let Expr::StructLit(sname, lit) = value else {
                        unreachable!()
                    };
                    *array_len = Some(sizes[sname]);
                    let taken = (name.clone(), sname.clone(), std::mem::take(lit));
                    *value = Expr::Int(0);
                    taken
                }
                _ => unreachable!(),
            };
            locals.insert(pname.clone(), sname.clone());
            out.push(stmt);
            for (fname, fval) in lit {
                let idx = fields[&sname][&fname];
                let mut v = *fval;
                desugar_expr(&mut v, fields, sizes, locals, methods);
                out.push(Stmt::Assign {
                    target: Expr::Index(
                        Box::new(Expr::Var(pname.clone())),
                        Box::new(Expr::Int(idx)),
                    ),
                    value: v,
                });
            }
        } else {
            desugar_stmt(&mut stmt, fields, sizes, locals, methods);
            out.push(stmt);
        }
    }
    b.stmts = out;
    if let Some(t) = &mut b.tail {
        desugar_expr(t, fields, sizes, locals, methods);
    }
}

fn desugar_stmt(
    s: &mut Stmt,
    fields: &FieldMap,
    sizes: &SizeMap,
    locals: &mut HashMap<String, String>,
    methods: &MethodSet,
) {
    match s {
        Stmt::Let {
            name,
            ty,
            array_len,
            value,
            ..
        } => {
            // `let p: StructName` or `let p: *StructName` (no explicit array)
            // allocates one word per field; the pointer form is stored the same.
            let struct_ty = match (&array_len, ty.as_ref().and_then(|t| base_struct(t))) {
                (None, Some(tn)) => sizes.get(tn).map(|n| (tn.to_string(), *n)),
                _ => None,
            };
            if let Some((tn, n)) = struct_ty {
                locals.insert(name.clone(), tn);
                *array_len = Some(n);
            }
            desugar_expr(value, fields, sizes, locals, methods);
        }
        Stmt::Assign { target, value } => {
            desugar_expr(target, fields, sizes, locals, methods);
            desugar_expr(value, fields, sizes, locals, methods);
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => desugar_expr(e, fields, sizes, locals, methods),
        Stmt::For { cond, body, .. } => {
            if let Some(c) = cond {
                desugar_expr(c, fields, sizes, locals, methods);
            }
            desugar_block(body, fields, sizes, locals, methods);
        }
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
    }
}

fn desugar_expr(
    e: &mut Expr,
    fields: &FieldMap,
    sizes: &SizeMap,
    locals: &mut HashMap<String, String>,
    methods: &MethodSet,
) {
    match e {
        // `local.field` where `local` is a known struct becomes `local[index]`.
        Expr::Field(base, field) => {
            desugar_expr(base, fields, sizes, locals, methods);
            let idx = if let Expr::Var(name) = base.as_ref() {
                locals
                    .get(name)
                    .and_then(|sname| fields.get(sname))
                    .and_then(|fm| fm.get(field))
                    .copied()
            } else {
                None
            };
            if let Some(idx) = idx {
                let b = std::mem::replace(base.as_mut(), Expr::Int(0));
                *e = Expr::Index(Box::new(b), Box::new(Expr::Int(idx)));
            }
        }
        Expr::Binary(_, l, r) => {
            desugar_expr(l, fields, sizes, locals, methods);
            desugar_expr(r, fields, sizes, locals, methods);
        }
        Expr::Index(b, i) => {
            // Slice indexing: `s[i]` on a `Slice`-typed value reads element i through its
            // `.ptr` field (`peek64(s.ptr + i*8)`), unlike a bare array where the name IS the
            // base address. Rewriting the base to `s.ptr` reuses the array-index codegen.
            if let Expr::Var(name) = b.as_ref()
                && locals.get(name).map(String::as_str) == Some(SLICE_TYPE)
            {
                let base = std::mem::replace(&mut **b, Expr::Int(0));
                **b = Expr::Field(Box::new(base), "ptr".to_string());
            }
            desugar_expr(b, fields, sizes, locals, methods);
            desugar_expr(i, fields, sizes, locals, methods);
        }
        Expr::Call(c, args) => {
            // Method call: `p.m(a..)` where `p` is a struct-typed local/param and a
            // free function `Type_m` exists -> `Type_m(p, a..)`. The receiver is
            // passed first, by reference (its base address), like any struct arg.
            let rewrite = if let Expr::Field(base, method) = c.as_ref() {
                if let Expr::Var(name) = base.as_ref() {
                    locals.get(name).and_then(|sname| {
                        let mangled = format!("{sname}_{method}");
                        methods
                            .contains(&mangled)
                            .then(|| (mangled, (**base).clone()))
                    })
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((mangled, recv)) = rewrite {
                **c = Expr::Var(mangled);
                let rest = std::mem::take(args);
                args.push(Arg {
                    name: None,
                    value: recv,
                });
                args.extend(rest);
            }
            desugar_expr(c, fields, sizes, locals, methods);
            for a in args.iter_mut() {
                desugar_expr(&mut a.value, fields, sizes, locals, methods);
            }
        }
        Expr::Unary(_, x) => desugar_expr(x, fields, sizes, locals, methods),
        Expr::List(items) => {
            for it in items.iter_mut() {
                desugar_expr(it, fields, sizes, locals, methods);
            }
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            desugar_expr(cond, fields, sizes, locals, methods);
            desugar_block(then_branch, fields, sizes, locals, methods);
            if let Some(eb) = else_branch {
                desugar_block(eb, fields, sizes, locals, methods);
            }
        }
        Expr::Match { scrutinee, arms } => {
            desugar_expr(scrutinee, fields, sizes, locals, methods);
            for arm in arms.iter_mut() {
                desugar_block(&mut arm.body, fields, sizes, locals, methods);
            }
        }
        // A struct literal outside a `let` initializer is not lowered here (codegen
        // rejects it); still desugar its field values for safety.
        Expr::StructLit(_, flit) => {
            for (_, v) in flit.iter_mut() {
                desugar_expr(v, fields, sizes, locals, methods);
            }
        }
        Expr::Cast(inner, _) => desugar_expr(inner, fields, sizes, locals, methods),
        // A `SliceRange` is lowered to a `Slice { .. }` literal by `lower_slice_ranges` (which
        // runs earlier), so it should not reach here; recurse defensively if it does.
        Expr::SliceRange(base, lo, hi) => {
            desugar_expr(base, fields, sizes, locals, methods);
            if let Some(l) = lo {
                desugar_expr(l, fields, sizes, locals, methods);
            }
            if let Some(h) = hi {
                desugar_expr(h, fields, sizes, locals, methods);
            }
        }
        Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Var(_) => {}
    }
}

/// The built-in `Slice { ptr, len }` fat pointer, a data pointer plus a runtime length, the
/// length-carrying array (RFC-0012 §6). Injected before struct lowering so `s: Slice` params,
/// `s.ptr`/`s.len` field access, and slice indexing `s[i]` all resolve through the existing
/// struct machinery (a 2-word block passed by reference / returned in RAX:RDX). A user
/// definition of `Slice`, if present, is left untouched.
pub fn inject_builtin_structs(structs: &mut Vec<StructDef>) {
    if structs.iter().any(|s| s.name == SLICE_TYPE) {
        return;
    }
    let i64t = || Type::Named("i64".to_string());
    structs.push(StructDef {
        vis: crate::ast::Visibility::Pub,
        name: SLICE_TYPE.to_string(),
        fields: vec![
            Param {
                name: "ptr".to_string(),
                ty: i64t(),
            },
            Param {
                name: "len".to_string(),
                ty: i64t(),
            },
        ],
    });
}

/// The name of the built-in fat-pointer type. Indexing `s[i]` on a value of this type reads
/// through its `.ptr` field (element i is `peek64(s.ptr + i*8)`), unlike a bare array where the
/// name itself is the base address.
const SLICE_TYPE: &str = "Slice";

/// Lowers `a.len` on a fixed-size array `let a[N]` (or a module-level `let K[N] = ..`) to the
/// compile-time constant `N`. An array's length is statically known, arrays are fixed-size
/// frame/`.data` regions, so `.len` is pure front-end sugar, resolved here (before every check
/// and codegen) exactly like struct field access. This gives refactor-safe iteration
/// (`for i < a.len`) and lets the constant-bounds check fold `a[a.len - 1]`. A future
/// length-carrying slice type would make `.len` a runtime field; for these fixed arrays it is
/// the known constant. A struct with a field literally named `len` is unaffected: only names
/// known to be arrays are rewritten.
pub fn lower_array_len(funcs: &mut [Function], globals: &[Global]) {
    let mut global_sizes: HashMap<String, i64> = HashMap::new();
    for g in globals {
        if let Some(n) = g.array_len {
            global_sizes.insert(g.name.clone(), n);
        }
    }
    for f in funcs {
        let mut sizes = global_sizes.clone();
        collect_array_sizes_block(&f.body, &mut sizes);
        len_block(&mut f.body, &sizes);
    }
}

/// Collects every `let name[N]` array size reachable in a body (function-scoped, matching the
/// codegen frame model where each array, including one declared inside a branch or loop, gets
/// its own reserved slot).
fn collect_array_sizes_block(b: &Block, out: &mut HashMap<String, i64>) {
    for s in &b.stmts {
        match s {
            Stmt::Let {
                name,
                array_len: Some(n),
                value,
                ..
            } => {
                out.insert(name.clone(), *n);
                collect_array_sizes_expr(value, out);
            }
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                collect_array_sizes_expr(value, out)
            }
            Stmt::Assign { target, value } => {
                collect_array_sizes_expr(target, out);
                collect_array_sizes_expr(value, out);
            }
            Stmt::For {
                cond, invariant, body, ..
            } => {
                if let Some(c) = cond {
                    collect_array_sizes_expr(c, out);
                }
                for inv in invariant {
                    collect_array_sizes_expr(inv, out);
                }
                collect_array_sizes_block(body, out);
            }
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        collect_array_sizes_expr(t, out);
    }
}

fn collect_array_sizes_expr(e: &Expr, out: &mut HashMap<String, i64>) {
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_array_sizes_expr(cond, out);
            collect_array_sizes_block(then_branch, out);
            if let Some(eb) = else_branch {
                collect_array_sizes_block(eb, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_array_sizes_expr(scrutinee, out);
            for a in arms {
                collect_array_sizes_block(&a.body, out);
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            collect_array_sizes_expr(l, out);
            collect_array_sizes_expr(r, out);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            collect_array_sizes_expr(x, out)
        }
        Expr::Call(c, args) => {
            collect_array_sizes_expr(c, out);
            for a in args {
                collect_array_sizes_expr(&a.value, out);
            }
        }
        Expr::List(items) => items.iter().for_each(|it| collect_array_sizes_expr(it, out)),
        Expr::StructLit(_, fields) => {
            fields.iter().for_each(|(_, v)| collect_array_sizes_expr(v, out))
        }
        _ => {}
    }
}

fn len_block(b: &mut Block, sizes: &HashMap<String, i64>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                len_expr(value, sizes)
            }
            Stmt::Assign { target, value } => {
                len_expr(target, sizes);
                len_expr(value, sizes);
            }
            Stmt::For {
                cond, invariant, body, ..
            } => {
                if let Some(c) = cond {
                    len_expr(c, sizes);
                }
                for inv in invariant.iter_mut() {
                    len_expr(inv, sizes);
                }
                len_block(body, sizes);
            }
            _ => {}
        }
    }
    if let Some(t) = &mut b.tail {
        len_expr(t, sizes);
    }
}

fn len_expr(e: &mut Expr, sizes: &HashMap<String, i64>) {
    // `a.len` where `a` is a known fixed-size array → the constant size.
    if let Expr::Field(base, field) = e
        && field == "len"
        && let Expr::Var(name) = base.as_ref()
        && let Some(&n) = sizes.get(name)
    {
        *e = Expr::Int(n);
        return;
    }
    match e {
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            len_expr(l, sizes);
            len_expr(r, sizes);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => len_expr(x, sizes),
        Expr::Call(c, args) => {
            len_expr(c, sizes);
            for a in args.iter_mut() {
                len_expr(&mut a.value, sizes);
            }
        }
        Expr::List(items) => items.iter_mut().for_each(|it| len_expr(it, sizes)),
        Expr::StructLit(_, fields) => {
            fields.iter_mut().for_each(|(_, v)| len_expr(v, sizes))
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            len_expr(cond, sizes);
            len_block(then_branch, sizes);
            if let Some(eb) = else_branch {
                len_block(eb, sizes);
            }
        }
        Expr::Match { scrutinee, arms } => {
            len_expr(scrutinee, sizes);
            for a in arms.iter_mut() {
                len_block(&mut a.body, sizes);
            }
        }
        _ => {}
    }
}

/// Lowers a slice range `base[lo:hi]` (Python-style) to a sub-`Slice` VIEW, no copy, over the
/// same backing store: `Slice { ptr: base.ptr + lo*8, len: hi - lo }`. Omitted bounds default to
/// `lo = 0` and `hi = base.len`. Runs before struct lowering + hoisting, so the produced
/// `Slice { .. }` literal is hoisted and lowered like any other. Ranges apply to a `Slice` base
/// (indexing through its `.ptr`); a bare array is sliced by wrapping it in a `Slice` first.
pub fn lower_slice_ranges(funcs: &mut [Function]) {
    for f in funcs {
        sr_block(&mut f.body);
    }
}

/// Builds the `Slice { ptr, len }` view for `base[lo:hi]`.
fn slice_range_to_struct(base: Expr, lo: Option<Box<Expr>>, hi: Option<Box<Expr>>) -> Expr {
    let lo_e = lo.map_or(Expr::Int(0), |b| *b);
    let hi_e = hi.map_or_else(
        || Expr::Field(Box::new(base.clone()), "len".to_string()),
        |b| *b,
    );
    let ptr = Expr::Binary(
        BinOp::Add,
        Box::new(Expr::Field(Box::new(base), "ptr".to_string())),
        Box::new(Expr::Binary(
            BinOp::Mul,
            Box::new(lo_e.clone()),
            Box::new(Expr::Int(8)),
        )),
    );
    let len = Expr::Binary(BinOp::Sub, Box::new(hi_e), Box::new(lo_e));
    Expr::StructLit(
        SLICE_TYPE.to_string(),
        vec![
            ("ptr".to_string(), Box::new(ptr)),
            ("len".to_string(), Box::new(len)),
        ],
    )
}

fn sr_block(b: &mut Block) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                sr_expr(value)
            }
            Stmt::Assign { target, value } => {
                sr_expr(target);
                sr_expr(value);
            }
            Stmt::For {
                cond, invariant, body, ..
            } => {
                if let Some(c) = cond {
                    sr_expr(c);
                }
                for inv in invariant.iter_mut() {
                    sr_expr(inv);
                }
                sr_block(body);
            }
            _ => {}
        }
    }
    if let Some(t) = &mut b.tail {
        sr_expr(t);
    }
}

fn sr_expr(e: &mut Expr) {
    match e {
        Expr::SliceRange(base, lo, hi) => {
            // Lower children first, then this node (bottom-up: a nested `s[a:b][c:d]` works).
            sr_expr(base);
            if let Some(l) = lo {
                sr_expr(l);
            }
            if let Some(h) = hi {
                sr_expr(h);
            }
            let base_e = std::mem::replace(base.as_mut(), Expr::Int(0));
            *e = slice_range_to_struct(base_e, lo.take(), hi.take());
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            sr_expr(l);
            sr_expr(r);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => sr_expr(x),
        Expr::Call(c, args) => {
            sr_expr(c);
            for a in args.iter_mut() {
                sr_expr(&mut a.value);
            }
        }
        Expr::List(items) => items.iter_mut().for_each(sr_expr),
        Expr::StructLit(_, fields) => fields.iter_mut().for_each(|(_, v)| sr_expr(v)),
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            sr_expr(cond);
            sr_block(then_branch);
            if let Some(eb) = else_branch {
                sr_block(eb);
            }
        }
        Expr::Match { scrutinee, arms } => {
            sr_expr(scrutinee);
            for a in arms.iter_mut() {
                sr_block(&mut a.body);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::emit_program_g;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn compile_and_capture(src: &str) -> String {
        use std::process::Command;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let mut p = Parser::new(tokenize(src));
        let mut funcs = p.parse_program();
        let mut structs = p.take_structs();
        let enums = p.take_enums();
        inject_builtin_structs(&mut structs);
        // `a.len` on a fixed-size array lowers to its constant size (before the other passes).
        lower_array_len(&mut funcs, &[]);
        lower_slice_ranges(&mut funcs);
        // Payloaded enums synthesize `{__tag, __payload}` structs; lower enums first,
        // then feed those structs into the struct pass, exactly the driver's order.
        let synth = desugar_enums(&mut funcs, &enums);
        structs.extend(synth);
        hoist_struct_literals(&mut funcs);
        desugar_structs(&mut funcs, &structs);
        // Pass structs to codegen too, so a function returning a small struct/enum
        // returns it by value (RAX:RDX), the driver does the same.
        let asm = emit_program_g(&[], &structs, &funcs);
        let dir = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stem = format!("sondc_desugar_{}_{}", std::process::id(), n);
        let asm_path = dir.join(format!("{stem}.s"));
        let bin_path = dir.join(&stem);
        std::fs::write(&asm_path, asm).unwrap();
        assert!(
            Command::new("cc")
                .arg(&asm_path)
                .arg("-o")
                .arg(&bin_path)
                .status()
                .unwrap()
                .success(),
            "compile failed"
        );
        let out = Command::new(&bin_path).output().unwrap();
        let _ = std::fs::remove_file(&asm_path);
        let _ = std::fs::remove_file(&bin_path);
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    #[test]
    fn array_len_lowers_to_the_constant_size() {
        // `a.len` on `let a[8]` is the compile-time constant 8.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[8]\n\
             print_int(a.len)\n\
             0\n\
             }",
        );
        assert_eq!(out, "8");
    }

    #[test]
    fn array_len_drives_a_refactor_safe_loop() {
        // The idiomatic use: iterate to `a.len` rather than a hardcoded size.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[4]\n\
             let mut i = 0\n\
             for i < a.len { a[i] = i * 10  i = i + 1 }\n\
             let mut s = 0\n\
             i = 0\n\
             for i < a.len { s = s + a[i]  i = i + 1 }\n\
             print_int(s)\n\
             0\n\
             }",
        );
        assert_eq!(out, "60");
    }

    #[test]
    fn builtin_slice_carries_length_across_a_call_and_indexes_through_ptr() {
        // No `struct Slice` is declared, it is built in. The length travels IN the slice,
        // and `s[i]` reads element i through `.ptr` (not the struct block itself).
        let out = compile_and_capture(
            "fn sum(s: Slice) -> i64 {\n\
             let mut i = 0\n\
             let mut acc = 0\n\
             for i < s.len { acc = acc + s[i]  i = i + 1 }\n\
             acc\n\
             }\n\
             fn main() -> i64 {\n\
             let a[4]\n\
             a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
             let s = Slice { ptr: a, len: a.len }\n\
             print_int(sum(s))\n\
             0\n\
             }",
        );
        assert_eq!(out, "100");
    }

    #[test]
    fn slice_range_is_a_view_over_the_backing_store() {
        // `s[1:4]` is a sub-slice view: elements 20,30,40, summed = 90. No copy, it points
        // into the same array.
        let out = compile_and_capture(
            "fn sum(s: Slice) -> i64 {\n\
             let mut i = 0\n\
             let mut acc = 0\n\
             for i < s.len { acc = acc + s[i]  i = i + 1 }\n\
             acc\n\
             }\n\
             fn main() -> i64 {\n\
             let a[5]\n\
             a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40  a[4] = 50\n\
             let s = Slice { ptr: a, len: a.len }\n\
             let mid = s[1:4]\n\
             print_int(sum(mid))\n\
             0\n\
             }",
        );
        assert_eq!(out, "90");
    }

    #[test]
    fn slice_range_default_bounds() {
        // `s[:2]` (lo=0), `s[2:]` (hi=len), `s[:]` (whole). Sums: 10+20=30, 30+40+50=120, all=150.
        let base = "fn sum(s: Slice) -> i64 {\n\
             let mut i = 0\n\
             let mut acc = 0\n\
             for i < s.len { acc = acc + s[i]  i = i + 1 }\n\
             acc\n\
             }\n";
        let mk = |range: &str| {
            format!(
                "{base}fn main() -> i64 {{\n\
                 let a[5]\n\
                 a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40  a[4] = 50\n\
                 let s = Slice {{ ptr: a, len: a.len }}\n\
                 print_int(sum({range}))\n\
                 0\n\
                 }}"
            )
        };
        assert_eq!(compile_and_capture(&mk("s[:2]")), "30");
        assert_eq!(compile_and_capture(&mk("s[2:]")), "120");
        assert_eq!(compile_and_capture(&mk("s[:]")), "150");
    }

    #[test]
    fn slice_of_a_slice_composes() {
        // `s[1:5][1:3]`, a view of a view. s[1:5]=20,30,40,50; then [1:3]=30,40; sum=70.
        let out = compile_and_capture(
            "fn sum(s: Slice) -> i64 {\n\
             let mut i = 0\n\
             let mut acc = 0\n\
             for i < s.len { acc = acc + s[i]  i = i + 1 }\n\
             acc\n\
             }\n\
             fn main() -> i64 {\n\
             let a[6]\n\
             a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40  a[4] = 50  a[5] = 60\n\
             let s = Slice { ptr: a, len: a.len }\n\
             let inner = s[1:5]\n\
             let inner2 = inner[1:3]\n\
             print_int(sum(inner2))\n\
             0\n\
             }",
        );
        assert_eq!(out, "70");
    }

    #[test]
    fn slice_element_write_goes_through_ptr() {
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[3]\n\
             let s = Slice { ptr: a, len: a.len }\n\
             let mut i = 0\n\
             for i < s.len { s[i] = i + 1  i = i + 1 }\n\
             print_int(a[0] + a[1] + a[2])\n\
             0\n\
             }",
        );
        assert_eq!(out, "6");
    }

    #[test]
    fn array_len_lowers_inside_a_struct_literal() {
        // `a.len` used as a struct-literal field value must lower too (the fat-pointer idiom
        // `Slice { ptr: a, len: a.len }`).
        let out = compile_and_capture(
            "struct Slice { ptr: i64, len: i64 }\n\
             fn main() -> i64 {\n\
             let a[7]\n\
             let s = Slice { ptr: a, len: a.len }\n\
             print_int(s.len)\n\
             0\n\
             }",
        );
        assert_eq!(out, "7");
    }

    #[test]
    fn struct_field_access_lowers_to_word_slots() {
        // A struct is a word array; `p.x`/`p.y` are slots 0 and 1.
        let out = compile_and_capture(
            "struct Point { x: i64, y: i64 }\n\
             fn main() -> i64 {\n\
             let p: Point\n\
             p.x = 7\n\
             p.y = 35\n\
             print_int(p.x + p.y)\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
    }

    #[test]
    fn struct_literal_lowers_to_array_plus_field_assignments() {
        // `let p = Point { x: 40, y: 2 }` allocates and initialises the fields.
        let out = compile_and_capture(
            "struct Point { x: i64, y: i64 }\n\
             fn main() -> i64 {\n\
             let p = Point { x: 40, y: 2 }\n\
             print_int(p.x + p.y)\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
    }

    #[test]
    fn struct_param_is_passed_by_reference() {
        // A struct arg passes its base address; the callee's `p.x` reads slot 0.
        let out = compile_and_capture(
            "struct Point { x: i64, y: i64 }\n\
             fn sum(p: Point) -> i64 { p.x + p.y }\n\
             fn main() -> i64 {\n\
             let q: Point\n\
             q.x = 30\n\
             q.y = 12\n\
             print_int(sum(q))\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
    }

    #[test]
    fn pointer_struct_type_lowers_like_a_value_struct() {
        // `let c: *Point` and `fn f(c: *Point)` parse and run: same base-address
        // representation as the value form (the value/pointer difference is a
        // separate static mutability rule, not a codegen difference).
        let out = compile_and_capture(
            "struct Point { x: i64, y: i64 }\n\
             fn bump(c: *Point) -> i64 { c.x + c.y }\n\
             fn main() -> i64 {\n\
             let c: *Point = Point { x: 40, y: 2 }\n\
             print_int(bump(c))\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
    }

    #[test]
    fn payloaded_enum_constructs_and_matches_with_binding() {
        // `Option`: a payloaded variant `Some(i64)` and a bare `None`. Constructing
        // each, then a `match` that binds the carried value, the nil-safety core.
        let out = compile_and_capture(
            "enum Opt { Some(i64), None }\n\
             fn main() -> i64 {\n\
             let a = Some(42)\n\
             let b = None\n\
             print_int(match a { Some(x) => x, None => 0 })\n\
             print_int(match b { Some(x) => x, None => 7 })\n\
             0\n\
             }",
        );
        // 42 from the `Some(42)` arm, 7 from the `None` fallback.
        assert_eq!(out, "427");
    }

    #[test]
    fn print_int_prints_a_signed_value_with_its_sign() {
        // `print_int` of a signed (`i64`) negative must show `-`, not its huge unsigned
        // two's-complement reading. An unsigned value still prints unsigned (see below).
        let out = compile_and_capture("fn main() -> i64 { print_int(0 - 5)  print_int(42)  0 }");
        assert_eq!(out, "-542");
    }

    #[test]
    fn print_int_of_a_u64_still_prints_unsigned() {
        // A `u64` whose high bit is set is a large positive number, not a negative, the
        // signed path must NOT be taken for unsigned types.
        let out = compile_and_capture(
            "fn main() -> i64 { let h: u64 = 0 - 1  print_int(h)  0 }",
        );
        assert_eq!(out, "18446744073709551615");
    }

    #[test]
    fn enum_returned_by_value_from_a_function() {
        // `safe_div` RETURNS an `Opt` by value, the most common use of Option/Result.
        // The struct is returned in registers (RAX:RDX) and received into `r`'s region.
        let out = compile_and_capture(
            "enum Opt { Some(i64), None }\n\
             fn safe_div(a: i64, b: i64) -> Opt { if b == 0 { None } else { Some(a / b) } }\n\
             fn main() -> i64 {\n\
             let r = safe_div(84, 2)\n\
             print_int(match r { Some(x) => x, None => 0 })\n\
             let z = safe_div(1, 0)\n\
             print_int(match z { Some(x) => x, None => 999 })\n\
             0\n\
             }",
        );
        // safe_div(84,2) = Some(42) -> 42 ; safe_div(1,0) = None -> 999
        assert_eq!(out, "42999");
    }

    #[test]
    fn constructor_in_argument_position_is_hoisted() {
        // A constructor `Some(40)` used directly as a call ARGUMENT (not first bound to
        // a `let`) is materialized into a temp, so it passes by reference like any struct.
        let out = compile_and_capture(
            "enum Opt { Some(i64), None }\n\
             fn unwrap_or(o: Opt, d: i64) -> i64 { match o { Some(x) => x, None => d } }\n\
             fn main() -> i64 {\n\
             print_int(unwrap_or(Some(40), 0) + 2)\n\
             print_int(unwrap_or(None, 7))\n\
             0\n\
             }",
        );
        assert_eq!(out, "427");
    }

    #[test]
    fn payloaded_enum_passes_through_a_function_parameter() {
        // A payloaded enum value passes by reference like any struct; the callee's
        // `match o { .. }` reads the tag and binds the payload. This is `unwrap_or`.
        let out = compile_and_capture(
            "enum Opt { Some(i64), None }\n\
             fn unwrap_or(o: Opt, d: i64) -> i64 { match o { Some(x) => x, None => d } }\n\
             fn main() -> i64 {\n\
             let a = Some(40)\n\
             let b = None\n\
             print_int(unwrap_or(a, 0) + 2)\n\
             print_int(unwrap_or(b, 7))\n\
             0\n\
             }",
        );
        // unwrap_or(Some(40),0)+2 = 42 ; unwrap_or(None,7) = 7
        assert_eq!(out, "427");
    }

    #[test]
    fn method_call_lowers_to_free_function_with_self_receiver() {
        // `impl Point { fn sum(self) {..} }` + `p.sum()` -> `Point_sum(p)`.
        let out = compile_and_capture(
            "struct Point { x: i64, y: i64 }\n\
             impl Point {\n\
             fn sum(self) -> i64 { self.x + self.y }\n\
             }\n\
             fn main() -> i64 {\n\
             let p = Point { x: 40, y: 2 }\n\
             print_int(p.sum())\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
    }
}

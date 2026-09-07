//! Struct lowering.
//!
//! A struct value is laid out as a fixed-size word array, one 8-byte word per
//! field, in declaration order. So `let p: Point` lowers to `let p[N]` (N = field
//! count) and `p.field` lowers to `p[field_index]`. This reuses the existing
//! word-array codegen: no new codegen is needed, structs are pure front-end sugar.
//!
//! Methods (`impl Point { fn m(self, ..) { .. } }`) are parsed into free functions
//! named `Point_m` with a `self` receiver typed as the struct. Here we lower a
//! method CALL `p.m(a)` to `Point_m(p, a)`: a minimal OOP over the flat namespace,
//! again with no new codegen.
//!
//! First slices (grown by TDD): field access on struct-typed locals and params,
//! struct literals, and method calls. Nested structs and value/pointer mutability
//! rules are follow-ups.

use crate::ast::{
    Arg, BinOp, Block, EnumDef, Expr, Function, Global, MatchArm, Param, Stmt, StructDef, Type,
    UnOp,
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
///   codegen entirely (no new backend), and is the `Option`/`Result` (nil-safety) base.
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
fn lower_match_int(
    scrutinee: &Expr,
    arms: &[MatchArm],
    info: &HashMap<String, VariantInfo>,
) -> Expr {
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
            stmt_lines: Vec::new(),
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
            stmt_lines: Vec::new(),
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
        // Through `insert_stmt`, so the arm's own lines do not shift under the binding. This one
        // is not reporting wrong lines today only because name resolution runs before this pass;
        // left as a plain `stmts.insert` it is the same trap as the `for` desugaring, armed and
        // waiting for the day the order of the phases changes.
        let line = b.stmt_lines.first().copied().unwrap_or(0);
        b.insert_stmt(
            0,
            Stmt::Let {
                mutable: false,
                name: bind.clone(),
                ty: None,
                value: Expr::Field(Box::new(scrutinee.clone()), PAYLOAD.to_string()),
                array_len: None,
                refinement: Vec::new(),
            },
            line,
        );
    }
    b
}

/// Hoists every struct literal that appears in an EXPRESSION position, a call
/// argument, an operand, a branch tail: anywhere except the direct initializer of a
/// `let`: into a preceding `let __litN = <lit>`, replacing the literal with `__litN`.
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
    // statements, before the tail). If the tail IS a literal, it becomes that temp's var,
    // so a struct-literal branch tail turns into a struct address a backend can return.
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
                refinement: Vec::new(),
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
        // A view is a base address of ELEMENTS, never a struct value.
        Type::View(..) => None,
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

/// Lowers struct-field access inside the contract propositions (`requires`/`ensures`), run
/// AFTER the prover, so the prover sees the source-level `s.len` (readable diagnostics and a
/// name it reasons about), while codegen still receives the lowered slot read for the runtime
/// guard. Without this a contract reading a field reaches codegen as a raw `Field` and fails to
/// compile its guard.
pub fn desugar_contracts(funcs: &mut [Function], structs: &[StructDef]) {
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
    let methods: MethodSet = funcs.iter().map(|f| f.name.clone()).collect();
    for f in funcs.iter_mut() {
        let mut locals: HashMap<String, String> = HashMap::new();
        for p in &f.params {
            if let Some(tn) = base_struct(&p.ty).filter(|t| sizes.contains_key(*t)) {
                locals.insert(p.name.clone(), tn.to_string());
            }
        }
        // A struct-typed named return is in scope in `ensures` (`ensures { r.x >= 0 }`).
        if let (Some(rn), Some(rt)) = (&f.ret_name, &f.ret)
            && let Some(tn) = base_struct(rt).filter(|t| sizes.contains_key(*t))
        {
            locals.insert(rn.clone(), tn.to_string());
        }
        for e in f.requires.iter_mut().chain(f.ensures.iter_mut()) {
            desugar_expr(e, &fields, &sizes, &mut locals, &methods);
        }
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
        Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Spawn(e) => {
            desugar_expr(e, fields, sizes, locals, methods)
        }
        Stmt::Par(peers) => {
            for p in peers {
                desugar_expr(p, fields, sizes, locals, methods);
            }
        }
        Stmt::ParLet { peers, .. } | Stmt::ParReduce { peers, .. } => {
            for p in peers {
                desugar_expr(p, fields, sizes, locals, methods);
            }
        }
        Stmt::GraphLet {
            nodes, yield_expr, ..
        } => {
            for (_, e) in nodes {
                desugar_expr(e, fields, sizes, locals, methods);
            }
            desugar_expr(yield_expr, fields, sizes, locals, methods);
        }
        Stmt::For { cond, body, .. } => {
            if let Some(c) = cond {
                desugar_expr(c, fields, sizes, locals, methods);
            }
            desugar_block(body, fields, sizes, locals, methods);
        }
        Stmt::Lock { body, .. } => {
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
            let slice_name = match b.as_ref() {
                Expr::Var(name) if locals.get(name).map(String::as_str) == Some(SLICE_TYPE) => {
                    Some(name.clone())
                }
                _ => None,
            };
            if let Some(sname) = slice_name {
                // A constant NEGATIVE index counts from the end: `s[-k]` → `s[s.len - k]`
                // (`s.len + i` since i is negative). Only constant negatives are normalized,
                // a variable index pays no runtime branch.
                if is_negative_const(i) {
                    let idx = std::mem::replace(&mut **i, Expr::Int(0));
                    **i = Expr::Binary(
                        BinOp::Add,
                        Box::new(Expr::Field(Box::new(Expr::Var(sname)), "len".to_string())),
                        Box::new(idx),
                    );
                }
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
/// compile-time constant `N`. An array's length is statically known (arrays are fixed-size
/// frame/`.data` regions), so `.len` is pure front-end sugar, resolved here (before every check
/// and codegen) exactly like struct field access. This gives refactor-safe iteration
/// (`for i < a.len`) and lets the constant-bounds check fold `a[a.len - 1]`. A future
/// length-carrying slice type would make `.len` a runtime field; for these fixed arrays it is
/// the known constant. A struct with a field literally named `len` is unaffected: only names
/// known to be arrays are rewritten.
/// Replaces every `name.len` in the body (where `name` is a known fixed-size
/// array in the current scope) with a constant `i64` literal equal to the
/// array's length. Scopes: variables declared inside an `if`/`else` body don't
/// leak out, so `m.len` at outer scope resolves to the outer `m`, not the inner one.
pub fn lower_array_len(funcs: &mut [Function], globals: &[Global]) {
    let mut global_map: HashMap<String, i64> = HashMap::new();
    for g in globals {
        if let Some(n) = g.array_len {
            global_map.insert(g.name.clone(), n);
        }
    }
    for f in funcs {
        let mut sizes: Vec<HashMap<String, i64>> = vec![global_map.clone()];
        walk_sizes_block(&mut f.body, &mut sizes);
    }
}

fn walk_sizes_block(b: &mut Block, sizes: &mut Vec<HashMap<String, i64>>) {
    for s in &mut b.stmts {
        // Insert any array declaration into the current scope before processing
        // the value expression.
        insert_array_into_scope(s, sizes);
        // Process the value/init of any statement.
        walk_sizes_in_expr_for_stmt(s, sizes);
    }
    if let Some(t) = &mut b.tail {
        walk_sizes_in_expr(t, sizes);
    }
}

fn insert_array_into_scope(s: &mut Stmt, sizes: &mut [HashMap<String, i64>]) {
    if let Stmt::Let { name, array_len: Some(arr_n), .. } = s {
        sizes.last_mut().unwrap().insert(name.clone(), *arr_n);
    } else if let Stmt::Let { name, ty: Some(Type::View(_, Some(len_str))), .. } = s
        && len_str.parse::<i64>().is_ok()
    {
        sizes.last_mut().unwrap().insert(name.clone(), len_str.parse().unwrap());
    }
}

fn walk_sizes_in_expr_for_stmt(s: &mut Stmt, sizes: &mut Vec<HashMap<String, i64>>) {
    match s {
        Stmt::Let { value, .. } => walk_sizes_in_expr(value, sizes),
        Stmt::Return(Some(v)) | Stmt::Expr(v) => walk_sizes_in_expr(v, sizes),
        Stmt::Assign { target, value } => {
            walk_sizes_in_expr(target, sizes);
            walk_sizes_in_expr(value, sizes);
        }
        Stmt::For { cond, invariant, body, .. } => {
            if let Some(c) = cond {
                walk_sizes_in_expr(c, sizes);
            }
            for inv in invariant.iter_mut() {
                walk_sizes_in_expr(inv, sizes);
            }
            walk_sizes_block(body, sizes);
        }
        _ => {}
    }
}

fn walk_sizes_in_expr(e: &mut Expr, sizes: &mut Vec<HashMap<String, i64>>) {
    // Replace `name.len` if `name` is a known array in the current scope.
    if let Expr::Field(base, field) = e
        && field == "len"
        && let Expr::Var(name) = base.as_ref()
        && sizes.iter().rev().flat_map(|m| m.get(name)).copied().next().is_some()
    {
        let Expr::Var(name) = base.as_ref() else {
            return;
        };
        if let Some(n) = sizes.iter().rev().flat_map(|m| m.get(name)).copied().next() {
            *e = Expr::Int(n);
            return;
        }
    }
    match e {
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            walk_sizes_in_expr(cond, sizes);
            sizes.push(HashMap::new());
            walk_sizes_block(then_branch, sizes);
            sizes.pop();
            if let Some(eb) = else_branch {
                sizes.push(HashMap::new());
                walk_sizes_block(eb, sizes);
                sizes.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            walk_sizes_in_expr(scrutinee, sizes);
            for a in arms.iter_mut() {
                sizes.push(HashMap::new());
                walk_sizes_block(&mut a.body, sizes);
                sizes.pop();
            }
        }
        Expr::Binary(_, l, r) | Expr::Index(l, r) => {
            walk_sizes_in_expr(l, sizes);
            walk_sizes_in_expr(r, sizes);
        }
        Expr::Unary(_, x) | Expr::Field(x, _) | Expr::Cast(x, _) => {
            walk_sizes_in_expr(x, sizes)
        }
        Expr::Call(c, args) => {
            walk_sizes_in_expr(c, sizes);
            for a in args.iter_mut() {
                walk_sizes_in_expr(&mut a.value, sizes);
            }
        }
        Expr::List(items) => items.iter_mut().for_each(|it| walk_sizes_in_expr(it, sizes)),
        Expr::StructLit(_, fields) => {
            fields.iter_mut().for_each(|(_, v)| walk_sizes_in_expr(v, sizes))
        }
        _ => {}
    }
}
/// Lowers a slice range `base[lo:hi]` (Python-style) to a sub-`Slice` VIEW (no copy), over the
/// same backing store: `Slice { ptr: base.ptr + lo*8, len: hi - lo }`. Omitted bounds default to
/// `lo = 0` and `hi = base.len`. Runs before struct lowering + hoisting, so the produced
/// `Slice { .. }` literal is hoisted and lowered like any other. Ranges apply to a `Slice` base
/// (indexing through its `.ptr`); a bare array is sliced by wrapping it in a `Slice` first.
pub fn lower_slice_ranges(funcs: &mut [Function]) {
    for f in funcs {
        sr_block(&mut f.body);
    }
}

/// True if `e` is a compile-time-constant NEGATIVE integer, `-1` (parsed as `Unary(Neg, 1)`)
/// or a literal `Int(n<0)`. Such an index/bound counts from the end of the slice.
fn is_negative_const(e: &Expr) -> bool {
    match e {
        Expr::Int(n) => *n < 0,
        Expr::Unary(UnOp::Neg, x) => matches!(x.as_ref(), Expr::Int(n) if *n > 0),
        _ => false,
    }
}

/// A range bound counted from the end when negative: `-k` → `base.len + (-k)` (= `len - k`).
fn end_relative(bound: Expr, base: &Expr) -> Expr {
    if is_negative_const(&bound) {
        Expr::Binary(
            BinOp::Add,
            Box::new(Expr::Field(Box::new(base.clone()), "len".to_string())),
            Box::new(bound),
        )
    } else {
        bound
    }
}

/// Builds the `Slice { ptr, len }` view for `base[lo:hi]`. A negative `lo`/`hi` counts from the
/// end (`s[:-1]`, `s[-2:]`).
fn slice_range_to_struct(base: Expr, lo: Option<Box<Expr>>, hi: Option<Box<Expr>>) -> Expr {
    let lo_e = lo.map_or(Expr::Int(0), |b| end_relative(*b, &base));
    let hi_e = hi.map_or_else(
        || Expr::Field(Box::new(base.clone()), "len".to_string()),
        |b| end_relative(*b, &base),
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
                cond,
                invariant,
                body,
                ..
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

/// A single pre-codegen pass that gives every shadowing local declaration a binding
/// of its own, so the backend's name-keyed frame cannot confuse two variables that
/// happen to share a name.
///
/// The x64 codegen reserves one frame slot per local NAME across the whole function
/// (`frame_layout::collect_locals` keys by name and dedupes), so two `let n` in
/// different scopes collapse into one stack slot and the inner one writes over the
/// outer one's value, which then survives the block — and even overwrites a
/// parameter of the same name. The scope checker (`check::check_undeclared_variables`)
/// already knows these are two different variables; the frame is what does not.
/// Giving each declaration a unique name here, and rewriting every reference that
/// resolves to it, makes the name-keyed frame (and every name-keyed metadata map in
/// codegen) agree with what the checker already understood, with no change to the
/// frame machinery itself.
///
/// The rewrite mirrors the checker's scope structure exactly: an `if`/`else` branch,
/// a `for` body, and a `match` arm each open a scope that closes when the block ends,
/// so a name declared there stops existing at the closing brace. Only shadowed
/// declarations are renamed; references use the innermost binding in scope at their
/// point in the walk. Runs on `func.body` only (parameters keep their names and are
/// the outermost scope); contract propositions are not touched.
pub fn disambiguate_shadows(funcs: &mut [Function]) {
    for f in funcs {
        let mut scopes: Vec<HashMap<String, String>> = Vec::new();
        let mut top: HashMap<String, String> = HashMap::new();
        // Parameters are in scope for the whole body and never renamed.
        for p in &f.params {
            top.insert(p.name.clone(), p.name.clone());
        }
        scopes.push(top);
        let mut r = ShadowRenamer { counter: 0 };
        rename_block(&mut f.body, &mut scopes, &mut r);
    }
}

/// Node-counters for synthesizing fresh binding names. A `#` cannot appear in a Sond
/// identifier (the lexer accepts only alphanumerics and `_`), so a generated name can
/// never collide with a source-spelled one.
struct ShadowRenamer {
    counter: u32,
}

impl ShadowRenamer {
    fn fresh(&mut self, name: &str) -> String {
        self.counter += 1;
        format!("{name}#{}", self.counter)
    }
}

/// The scope stack: each frame maps the source name to the binding name it currently
/// denotes (the binding name is the source name unless the declaration shadows an
/// enclosing one). Innermost scope is last, mirroring the checker.
type ShadowScopes = Vec<HashMap<String, String>>;

/// Resolves a source name to its current binding, innermost scope first. `None` when
/// the name is not a local at all (a global, a module-level constant, a function name).
fn shadow_resolve<'a>(scopes: &'a ShadowScopes, name: &str) -> Option<&'a str> {
    for s in scopes.iter().rev() {
        if let Some(b) = s.get(name) {
            return Some(b.as_str());
        }
    }
    None
}

fn rename_block(b: &mut Block, scopes: &mut ShadowScopes, r: &mut ShadowRenamer) {
    for s in &mut b.stmts {
        rename_stmt(s, scopes, r);
    }
    if let Some(t) = &mut b.tail {
        rename_expr(t, scopes, r);
    }
}

fn rename_stmt(s: &mut Stmt, scopes: &mut ShadowScopes, r: &mut ShadowRenamer) {
    match s {
        Stmt::Let { name, value, .. } => {
            // The initializer resolves against the OLD binding (`let n = n + 1` reads the
            // outer `n`), so the value is renamed before the new declaration binds.
            rename_expr(value, scopes, r);
            shadow_bind(scopes, r, name);
        }
        Stmt::Assign { target, value } => {
            rename_expr(target, scopes, r);
            rename_expr(value, scopes, r);
        }
        Stmt::Return(Some(e)) => rename_expr(e, scopes, r),
        Stmt::Return(None) => {}
        Stmt::Expr(e) | Stmt::Spawn(e) => rename_expr(e, scopes, r),
        Stmt::Par(peers) => {
            for p in peers {
                rename_expr(p, scopes, r);
            }
        }
        Stmt::ParLet { names, peers, .. } => {
            // The peers resolve against the OLD scope; the names bind afterwards.
            for p in peers {
                rename_expr(p, scopes, r);
            }
            for n in names.iter_mut() {
                shadow_bind(scopes, r, n);
            }
        }
        Stmt::ParReduce { name, peers, .. } => {
            for p in peers {
                rename_expr(p, scopes, r);
            }
            shadow_bind(scopes, r, name);
        }
        Stmt::GraphLet {
            name,
            nodes,
            yield_expr,
            ..
        } => {
            // A node can depend on an earlier node's binding, so each binds as it is
            // walked (mirroring the checker).
            for (n, e) in nodes.iter_mut() {
                rename_expr(e, scopes, r);
                shadow_bind(scopes, r, n);
            }
            rename_expr(yield_expr, scopes, r);
            shadow_bind(scopes, r, name);
        }
        Stmt::For { cond, body, .. } => {
            if let Some(c) = cond {
                rename_expr(c, scopes, r);
            }
            scopes.push(HashMap::new());
            rename_block(body, scopes, r);
            scopes.pop();
        }
        Stmt::Lock { body, .. } => {
            scopes.push(HashMap::new());
            rename_block(body, scopes, r);
            scopes.pop();
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn rename_expr(e: &mut Expr, scopes: &mut ShadowScopes, r: &mut ShadowRenamer) {
    match e {
        Expr::Var(name) => {
            if let Some(b) = shadow_resolve(scopes, name) {
                *name = b.to_string();
            }
        }
        Expr::Field(base, _) => rename_expr(base, scopes, r),
        Expr::Call(callee, args) => {
            rename_expr(callee, scopes, r);
            for a in args.iter_mut() {
                rename_expr(&mut a.value, scopes, r);
            }
        }
        Expr::Index(base, index) => {
            rename_expr(base, scopes, r);
            rename_expr(index, scopes, r);
        }
        Expr::Unary(_, inner) => rename_expr(inner, scopes, r),
        Expr::Binary(_, l, rr) => {
            rename_expr(l, scopes, r);
            rename_expr(rr, scopes, r);
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            rename_expr(cond, scopes, r);
            scopes.push(HashMap::new());
            rename_block(then_branch, scopes, r);
            scopes.pop();
            if let Some(eb) = else_branch {
                scopes.push(HashMap::new());
                rename_block(eb, scopes, r);
                scopes.pop();
            }
        }
        Expr::Match { scrutinee, arms } => {
            rename_expr(scrutinee, scopes, r);
            for arm in arms.iter_mut() {
                scopes.push(HashMap::new());
                if let Some(binding) = &mut arm.binding {
                    shadow_bind(scopes, r, binding);
                }
                rename_block(&mut arm.body, scopes, r);
                scopes.pop();
            }
        }
        Expr::Cast(inner, _) => rename_expr(inner, scopes, r),
        Expr::List(items) => {
            for it in items.iter_mut() {
                rename_expr(it, scopes, r);
            }
        }
        Expr::SliceRange(base, lo, hi) => {
            rename_expr(base, scopes, r);
            if let Some(l) = lo {
                rename_expr(l, scopes, r);
            }
            if let Some(h) = hi {
                rename_expr(h, scopes, r);
            }
        }
        Expr::StructLit(_, fields) => {
            for (_, v) in fields.iter_mut() {
                rename_expr(v, scopes, r);
            }
        }
        Expr::Int(_) | Expr::Decimal(_) | Expr::Str(_) | Expr::Bool(_) => {}
    }
}

/// Registers a declaration's binding in the top scope, renaming the declaration when it
/// shadows any enclosing binding (so the frame gives it a slot of its own). The source
/// name in `name` is replaced by the binding name that references must use from here on.
fn shadow_bind(scopes: &mut ShadowScopes, r: &mut ShadowRenamer, name: &mut String) {
    let fresh = if scopes.iter().any(|s| s.contains_key(name.as_str())) {
        r.fresh(name)
    } else {
        name.clone()
    };
    scopes.last_mut().unwrap().insert(name.clone(), fresh.clone());
    *name = fresh;
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
        // Contracts lower after the prover in the real driver; here (no prover) just before codegen.
        desugar_contracts(&mut funcs, &structs);
        // Pass structs to codegen too, so a function returning a small struct/enum
        // returns it by value (RAX:RDX), the driver does the same.
        let asm = emit_program_g(&[], &structs, &funcs);
        // The scratch dir goes even if the assert below panics: a `remove_file` on the happy
        // path leaves the failing run's files behind for good, and this temp dir is RAM.
        let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = crate::scratch::Scratch::new("sondc_desugar");
        let asm_path = dir.join("prog.s");
        let bin_path = dir.join("prog");
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
    fn foreach_over_a_slice_iterates_its_elements() {
        // `for x in s { .. }` binds x to each element in turn.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[4]\n\
             a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40\n\
             let s = Slice { ptr: a, len: a.len }\n\
             let mut acc = 0\n\
             for x in s { acc = acc + x }\n\
             print_int(acc)\n\
             0\n\
             }",
        );
        assert_eq!(out, "100");
    }

    #[test]
    fn foreach_over_an_array_iterates_its_elements() {
        // Works on a bare fixed array too (its `.len` is the constant size).
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[3]\n\
             a[0] = 5  a[1] = 6  a[2] = 7\n\
             let mut acc = 0\n\
             for x in a { acc = acc + x }\n\
             print_int(acc)\n\
             0\n\
             }",
        );
        assert_eq!(out, "18");
    }

    #[test]
    fn foreach_over_a_sub_slice_range() {
        // Iterate a bound sub-slice, composes with ranges.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[5]\n\
             a[0] = 1  a[1] = 2  a[2] = 3  a[3] = 4  a[4] = 5\n\
             let s = Slice { ptr: a, len: a.len }\n\
             let mid = s[1:4]\n\
             let mut acc = 0\n\
             for x in mid { acc = acc + x }\n\
             print_int(acc)\n\
             0\n\
             }",
        );
        assert_eq!(out, "9"); // 2+3+4
    }

    #[test]
    fn for_range_is_half_open_and_increments_after_the_body() {
        // `for i in 0..3` visits 0,1,2, the counter increment must run AFTER the body's tail,
        // not before it (a prior bug printed 1,2,3).
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             for i in 0..3 { print_int(i) }\n\
             0\n\
             }",
        );
        assert_eq!(out, "012");
    }

    #[test]
    fn negative_slice_index_counts_from_the_end() {
        // `s[-1]` is the last element, `s[-2]` the second-to-last.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a[5]\n\
             a[0] = 10  a[1] = 20  a[2] = 30  a[3] = 40  a[4] = 50\n\
             let s = Slice { ptr: a, len: a.len }\n\
             print_int(s[-1] + s[-2])\n\
             0\n\
             }",
        );
        assert_eq!(out, "90"); // 50 + 40
    }

    #[test]
    fn negative_range_bounds_count_from_the_end() {
        // `s[:-1]` = all but last (10+20+30+40=100); `s[-2:]` = last two (40+50=90).
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
        assert_eq!(compile_and_capture(&mk("s[:-1]")), "100");
        assert_eq!(compile_and_capture(&mk("s[-2:]")), "90");
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
        // `s[1:5][1:3]`: a view of a view. s[1:5]=20,30,40,50; then [1:3]=30,40; sum=70.
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
    fn a_contract_reading_a_struct_field_lowers_its_runtime_guard() {
        // `requires { s.len > 0 }` reads a Slice field in the runtime guard. It must lower like a
        // body field access, reaching codegen as a raw `Field` used to crash the guard emit.
        let out = compile_and_capture(
            "fn first(s: Slice) -> i64 requires { s.len > 0 } { s[0] }\n\
             fn main() -> i64 {\n\
             let a[3]\n\
             a[0] = 42\n\
             let s = Slice { ptr: a, len: a.len }\n\
             print_int(first(s))\n\
             0\n\
             }",
        );
        assert_eq!(out, "42");
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
        let out = compile_and_capture("fn main() -> i64 { let h: u64 = 0 - 1  print_int(h)  0 }");
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

    // Shadowing of `.len` across nested scopes: the fix ensures that a `let m` inside
    // an `if` block does not shadow the outer `m` for `.len` resolution at the outer level.
    // These tests go through `lower_array_len` (the full desugar pipeline via compile_and_capture),
    // so they exercise the scope-aware walk.

    #[test]
    fn array_len_shadowing_in_if_block_resolves_outer_scope() {
        // The outer `m: string = "hola"` has length 4. The inner `m: string = "0123456789"` has
        // length 10 but is in a nested scope, so `m.len` at outer scope must resolve to 4.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let m: string = \"hola\"\n\
             if 1 == 1 { let m: string = \"0123456789\" }\n\
             print_int(m.len)\n\
             0\n\
             }",
        );
        assert_eq!(out, "4");
    }

    #[test]
    fn array_len_shadowing_in_dead_branch_does_not_affect_outer() {
        // Even when the branch is dead (always false), the outer scope must still resolve correctly.
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let m: string = \"hola\"\n\
             if 1 == 0 { let m: string = \"0123456789\" }\n\
             print_int(m.len)\n\
             0\n\
             }",
        );
        assert_eq!(out, "4");
    }

    #[test]
    fn array_len_earlier_use_not_affected_by_later_shadowing() {
        // `a.len` is evaluated BEFORE the inner `a` shadows it, so it must resolve to the outer `a` (5).
        let out = compile_and_capture(
            "fn main() -> i64 {\n\
             let a: string = \"12345\"\n\
             let r = a.len\n\
             if 1 == 1 { let a: string = \"xy\" }\n\
             print_int(r)\n\
             0\n\
             }",
        );
        assert_eq!(out, "5");
    }
}

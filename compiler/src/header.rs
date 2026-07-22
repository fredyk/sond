//! Header emission, the public interface of a module as a `.snd` "header".
//!
//! `sondc --emit-header foo.snd` produces the interface: only the `pub`
//! declarations, function bodies stripped (the contract, not the implementation).
//! This is the artifact that goes to `~/.local/include/sond/` (and `dist/include/`
//! in the repo). A consumer compiles against the header (signatures + contracts to
//! type/contract-check the caller) and links the separately-compiled
//! implementation. Emitting only `pub` items is exactly the visibility boundary.
//!
//! This module owns the SERIALIZATION of the interface; separate compilation
//! (compile-against-header + link) is a later cycle.

use crate::ast::{BinOp, Expr, Function, Global, PortDef, Signature, StructDef, Type, UnOp, Visibility};

/// Renders the public interface of a program (its `pub` fns, structs and globals)
/// as Sond header text. Private (`Priv`) items are omitted; function bodies are
/// dropped, leaving the signature (and, in a later cycle, the contracts).
pub fn emit_header(
    src: &str,
    funcs: &[Function],
    structs: &[StructDef],
    globals: &[Global],
    ports: &[PortDef],
) -> String {
    // Each declaration becomes one block (its doc comment + signature). The blocks
    // are joined with a blank line, so there is an empty line between one
    // signature and the next declaration's doc comment.
    let mut blocks: Vec<String> = Vec::new();
    for s in structs.iter().filter(|s| s.vis == Visibility::Pub) {
        let mut b = String::new();
        emit_doc(&mut b, src, "struct", &s.name);
        let fields = s
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, type_name(&f.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        b.push_str(&format!("pub struct {} {{ {fields} }}\n", s.name));
        blocks.push(b);
    }
    for g in globals.iter().filter(|g| g.vis == Visibility::Pub) {
        let mut b = String::new();
        emit_doc(&mut b, src, "let", &g.name);
        // The initializer is implementation detail, a header names the binding only.
        b.push_str(&format!("pub let {}\n", g.name));
        blocks.push(b);
    }
    for f in funcs.iter().filter(|f| f.vis == Visibility::Pub) {
        let mut b = String::new();
        emit_doc(&mut b, src, "fn", &f.name);
        b.push_str(&signature(f));
        b.push('\n');
        blocks.push(b);
    }
    // A `port` IS an interface, so it belongs in the header verbatim (RFC-0014). Its `provides`
    // are per-target implementation, like a function body, and never appear here.
    for p in ports.iter().filter(|p| p.vis == Visibility::Pub) {
        let mut b = String::new();
        emit_doc(&mut b, src, "port", &p.name);
        b.push_str(&format!("pub port {} {{\n", p.name));
        for m in &p.methods {
            b.push_str(&format!("    {}\n", port_method_line(m)));
        }
        b.push_str("}\n");
        blocks.push(b);
    }
    let mut out =
        String::from("// Auto-generated Sond header. Public interface only, do not edit.\n\n");
    out.push_str(&blocks.join("\n"));
    out
}

/// Writes the doc comment (the contiguous `//` block immediately above the
/// declaration) into `out`, if the source has one for `kind name`.
fn emit_doc(out: &mut String, src: &str, kind: &str, name: &str) {
    if let Some(doc) = doc_for(src, kind, name) {
        for line in doc {
            if line.is_empty() {
                out.push_str("//\n");
            } else {
                out.push_str(&format!("// {line}\n"));
            }
        }
    }
}

/// Finds the `//` documentation block immediately above the declaration of
/// `kind name` (`fn add`, `struct Point`, `let VERSION`) in the source.
fn doc_for(src: &str, kind: &str, name: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = src.lines().collect();
    let decl = lines.iter().position(|l| decl_matches(l, kind, name))?;
    let mut doc = Vec::new();
    let mut j = decl;
    while j > 0 {
        j -= 1;
        let p = lines[j].trim_start();
        if let Some(rest) = p.strip_prefix("//") {
            doc.push(rest.trim().to_string());
        } else {
            break; // a blank or code line ends the doc block
        }
    }
    doc.reverse();
    (!doc.is_empty()).then_some(doc)
}

/// Whether `line` declares `kind name` (with an optional `pub`/`priv` prefix).
fn decl_matches(line: &str, kind: &str, name: &str) -> bool {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub ")
        .or_else(|| t.strip_prefix("priv "))
        .unwrap_or(t);
    let Some(t) = t.strip_prefix(kind) else {
        return false;
    };
    let t = t.trim_start();
    t.strip_prefix(name)
        .is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// The signature line of a public function: name, parameters, return type, and
/// its contract (`requires`/`ensures`), but no body. The contract is the point:
/// a consumer type/contract-checks against it without seeing the implementation.
fn signature(f: &Function) -> String {
    let params = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_name(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = match &f.ret {
        Some(t) => format!("pub fn {}({params}) -> {}", f.name, type_name(t)),
        None => format!("pub fn {}({params})", f.name),
    };
    if !f.requires.is_empty() {
        s.push_str(&format!(" requires {{ {} }}", render_props(&f.requires)));
    }
    if !f.ensures.is_empty() {
        s.push_str(&format!(" ensures {{ {} }}", render_props(&f.ensures)));
    }
    s
}

/// Renders one port-method signature as it appears inside a `port { }` block: `fn
/// name(params) -> ret` with the contract, no `pub`, no body. A named return is kept
/// (`-> (r: T)`) because a port's `ensures` refers to it.
fn port_method_line(m: &Signature) -> String {
    let params = m
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_name(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = match (&m.ret_name, &m.ret) {
        (Some(rn), Some(t)) => format!("fn {}({params}) -> ({rn}: {})", m.name, type_name(t)),
        (None, Some(t)) => format!("fn {}({params}) -> {}", m.name, type_name(t)),
        _ => format!("fn {}({params})", m.name),
    };
    if !m.requires.is_empty() {
        s.push_str(&format!(" requires {{ {} }}", render_props(&m.requires)));
    }
    if !m.ensures.is_empty() {
        s.push_str(&format!(" ensures {{ {} }}", render_props(&m.ensures)));
    }
    s
}

/// Renders a list of contract propositions, separated by `; `.
fn render_props(props: &[Expr]) -> String {
    props
        .iter()
        .map(expr_to_source)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Renders an expression back to Sond source (used for contract propositions in a
/// header). Faithful for the fragment that appears in contracts; readability over
/// exact re-parenthesization.
fn expr_to_source(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Decimal(s) => s.clone(),
        Expr::Str(s) => format!("{s:?}"),
        Expr::Bool(b) => b.to_string(),
        Expr::Var(name) => name.clone(),
        Expr::Field(base, name) => format!("{}.{name}", expr_to_source(base)),
        Expr::Index(base, idx) => format!("{}[{}]", expr_to_source(base), expr_to_source(idx)),
        Expr::SliceRange(base, lo, hi) => {
            let s = |o: &Option<Box<Expr>>| o.as_ref().map(|e| expr_to_source(e)).unwrap_or_default();
            format!("{}[{}:{}]", expr_to_source(base), s(lo), s(hi))
        }
        Expr::List(items) => format!(
            "[{}]",
            items
                .iter()
                .map(expr_to_source)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Call(callee, args) => {
            let a = args
                .iter()
                .map(|arg| match &arg.name {
                    Some(n) => format!("{n}={}", expr_to_source(&arg.value)),
                    None => expr_to_source(&arg.value),
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({a})", expr_to_source(callee))
        }
        Expr::Unary(op, x) => {
            let sym = match op {
                UnOp::Not => "not ",
                UnOp::Neg => "-",
                UnOp::BitNot => "~",
            };
            format!("{sym}{}", expr_to_source(x))
        }
        Expr::Binary(op, l, r) => {
            format!(
                "{} {} {}",
                expr_to_source(l),
                binop_symbol(op),
                expr_to_source(r)
            )
        }
        Expr::If { .. } => "if …".to_string(),
        Expr::Match { .. } => "match …".to_string(),
        Expr::Cast(inner, _) => format!("{} as …", expr_to_source(inner)),
        Expr::StructLit(name, fields) => {
            let inner = fields
                .iter()
                .map(|(f, v)| format!("{f}: {}", expr_to_source(v)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name} {{ {inner} }}")
        }
    }
}

fn binop_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::LShr => ">>>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Implies => "implies",
        BinOp::In => "in",
    }
}

fn type_name(t: &Type) -> String {
    match t {
        Type::Named(s) => s.clone(),
        Type::Ptr(inner) => format!("*{}", type_name(inner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    #[test]
    fn header_preserves_the_doc_comment_above_a_declaration() {
        // The `//` block immediately above a public declaration is its
        // documentation and must survive into the header.
        let src = "// Adds two numbers.\n\
                   // Wraps on overflow.\n\
                   pub fn add(a: i64, b: i64) -> i64 { a + b }\n\
                   // a private helper\n\
                   fn secret() -> i64 { 0 }";
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let h = emit_header(src, &funcs, &[], &[], &[]);
        assert!(h.contains("// Adds two numbers."), "first doc line kept");
        assert!(h.contains("// Wraps on overflow."), "second doc line kept");
        assert!(h.contains("pub fn add"), "signature kept");
        assert!(!h.contains("secret"), "private fn and its comment excluded");
    }

    #[test]
    fn header_renders_a_functions_contract() {
        // The contract IS the interface: a header keeps requires/ensures, drops body.
        let src = "pub fn dec(x: i64) -> i64 requires { x > 0 } ensures { x >= 0 } { x - 1 }";
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let h = emit_header(src, &funcs, &[], &[], &[]);
        assert!(h.contains("pub fn dec(x: i64) -> i64"), "signature");
        assert!(h.contains("requires { x > 0 }"), "precondition rendered");
        assert!(h.contains("ensures { x >= 0 }"), "postcondition rendered");
        assert!(!h.contains("x - 1"), "body still stripped");
    }

    #[test]
    fn emits_only_public_items_and_strips_bodies() {
        let src = "pub fn add(a: i64, b: i64) -> i64 { a + b }\n\
                   fn secret() -> i64 { 0 }\n\
                   pub struct P { x: i64 }\n\
                   priv let K = 3\n\
                   pub let N = 5";
        let mut p = Parser::new(tokenize(src));
        let funcs = p.parse_program();
        let structs = p.take_structs();
        let globals = p.take_globals();
        let h = emit_header(src, &funcs, &structs, &globals, &[]);

        assert!(
            h.contains("pub fn add(a: i64, b: i64) -> i64"),
            "public signature kept"
        );
        assert!(!h.contains("a + b"), "body stripped");
        assert!(!h.contains("secret"), "private fn omitted");
        assert!(h.contains("pub struct P { x: i64 }"), "public struct kept");
        assert!(h.contains("pub let N"), "public global named");
        assert!(
            !h.contains("= 5"),
            "global initializer (implementation) omitted"
        );
        assert!(!h.contains("K"), "private global omitted");
    }
}

#[cfg(test)]
mod port_header_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    #[test]
    fn a_public_port_is_emitted_as_the_interface_but_providers_are_not() {
        // A `port` is the interface, so it belongs in the header (RFC-0014); a `provides`
        // is per-target implementation, like a body, and must never appear.
        let src = "// A serial line.\n\
                   pub port Serial {\n\
                     fn serial_getc() -> (b: i64) ensures { b >= -1 }\n\
                     fn serial_putc(b: i64)\n\
                   }\n\
                   provides Serial for host { fn serial_getc() -> i64 { 0 } fn serial_putc(b: i64) { } }";
        let mut p = Parser::new(tokenize(src));
        p.parse_program();
        let ports = p.take_ports();
        let h = emit_header(src, &[], &[], &[], &ports);
        assert!(h.contains("// A serial line."), "the port's doc comment is kept");
        assert!(h.contains("pub port Serial {"), "the port is emitted");
        assert!(h.contains("fn serial_getc() -> (b: i64)"), "a method keeps its named return");
        assert!(h.contains("ensures { b >= -1 }"), "a method keeps its contract");
        assert!(h.contains("fn serial_putc(b: i64)"), "a bodyless method is rendered");
        assert!(!h.contains("provides"), "a provider is implementation, never in the header");
        assert!(!h.contains("{ 0 }"), "no provider body leaks in");
    }

    #[test]
    fn a_private_port_is_omitted() {
        let src = "port Internal { fn f() }";
        let mut p = Parser::new(tokenize(src));
        p.parse_program();
        let ports = p.take_ports();
        let h = emit_header(src, &[], &[], &[], &ports);
        assert!(!h.contains("Internal"), "a private (non-pub) port is not part of the interface");
    }
}

//! RFC-0014 port resolution: bind each `use Port` to the one provider that
//! implements it for the active target, or fail with a message that names the
//! target lacking an implementation.
//!
//! Resolution is static and total. For every `use Port` the checker requires
//! exactly one `provides Port for <active target>`; zero is a compile error (the
//! deliverable that replaces hand-stubbing a target's primitives), more than one
//! is ambiguity, and a provider that omits a port method (or gives it the wrong
//! arity) fails against the port rather than at some later call site. Full
//! contract verification of a provider against its port is a later slice; this
//! pass establishes existence, uniqueness, and shape.

use crate::ast::{Function, PortDef, ProviderDef, Signature, UseDecl};

/// Resolves every `use Port` against the providers for `target`, returning the
/// selected providers' methods to splice into the translation unit (so codegen
/// sees them as ordinary functions). Fails with the first error when a use does
/// not resolve to exactly one complete provider. A port `use`d more than once
/// splices its provider only once, so no duplicate symbols are introduced.
pub fn resolve_ports(
    ports: &[PortDef],
    providers: &[ProviderDef],
    uses: &[UseDecl],
    target: &str,
) -> Result<Vec<Function>, String> {
    let mut spliced = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for u in uses {
        let Some(port) = ports.iter().find(|p| p.name == u.port) else {
            return Err(format!(
                "`use {}`: no port named `{}` is declared.",
                u.port, u.port
            ));
        };

        let for_port: Vec<&ProviderDef> =
            providers.iter().filter(|pr| pr.port == u.port).collect();
        let matching: Vec<&ProviderDef> = for_port
            .iter()
            .copied()
            .filter(|pr| pr.target == target)
            .collect();

        let prov = match matching.as_slice() {
            [] => {
                let mut targets: Vec<&str> =
                    for_port.iter().map(|pr| pr.target.as_str()).collect();
                targets.sort_unstable();
                targets.dedup();
                let found = if targets.is_empty() {
                    "(none)".to_string()
                } else {
                    targets.join(", ")
                };
                return Err(format!(
                    "no implementation of port `{port_name}` for target `{target}`. \
                     `use {port_name}` needs a `provides {port_name} for {target} {{ ... }}`. \
                     found providers for: {found}",
                    port_name = u.port,
                ));
            }
            [prov] => *prov,
            more => {
                return Err(format!(
                    "port `{}` has {} providers for target `{}`; exactly one is allowed.",
                    u.port,
                    more.len(),
                    target
                ));
            }
        };
        check_complete(port, prov, target)?;
        // Splice each used port's provider exactly once, even if `use`d repeatedly. Each
        // spliced method carries the port's contract, so `verify` proves the body meets it.
        if !seen.contains(&u.port.as_str()) {
            seen.push(&u.port);
            for m in &prov.methods {
                let sig = port
                    .methods
                    .iter()
                    .find(|s| s.name == m.name)
                    .expect("check_complete ensured every provider method matches a signature");
                spliced.push(fold_contract(m, sig));
            }
        }
    }
    Ok(spliced)
}

/// Produces the provider method with the port signature's contract folded in: the
/// port's `requires` become assumptions the body may rely on, its `ensures` become
/// obligations `verify` proves the body meets, and the port's named return is
/// adopted so an `ensures` that mentions it resolves. The provider's own clauses are
/// kept alongside. This is what makes a port a verified boundary and not just a header.
fn fold_contract(method: &Function, sig: &Signature) -> Function {
    let mut f = method.clone();
    let mut requires = sig.requires.clone();
    requires.extend(f.requires);
    f.requires = requires;
    let mut ensures = sig.ensures.clone();
    ensures.extend(f.ensures);
    f.ensures = ensures;
    if f.ret_name.is_none() {
        f.ret_name = sig.ret_name.clone();
    }
    f
}

/// A provider must implement every signature of its port, with a matching arity.
fn check_complete(port: &PortDef, prov: &ProviderDef, target: &str) -> Result<(), String> {
    for sig in &port.methods {
        let Some(m) = prov.methods.iter().find(|m| m.name == sig.name) else {
            return Err(format!(
                "provider `provides {} for {}` is missing method `{}` required by the port.",
                port.name, target, sig.name
            ));
        };
        if m.params.len() != sig.params.len() {
            return Err(format!(
                "provider `{} for {}`: method `{}` takes {} parameters but the port declares {}.",
                port.name,
                target,
                sig.name,
                m.params.len(),
                sig.params.len()
            ));
        }
        // Each parameter must match the port's in both name and type. The name, because the
        // port's contract is written against it; the type, because a mismatched type means the
        // provider does not implement the interface a caller was verified against.
        for (mp, sp) in m.params.iter().zip(&sig.params) {
            if mp.name != sp.name {
                return Err(format!(
                    "provider `{} for {}`: method `{}` names a parameter `{}` where the port \
                     declares `{}`; a provider must use the port's parameter names so the \
                     port's contract applies to it.",
                    port.name, target, sig.name, mp.name, sp.name
                ));
            }
            if mp.ty != sp.ty {
                return Err(format!(
                    "provider `{} for {}`: method `{}` gives parameter `{}` type `{}` but the \
                     port declares type `{}`.",
                    port.name,
                    target,
                    sig.name,
                    mp.name,
                    type_name(&mp.ty),
                    type_name(&sp.ty)
                ));
            }
        }
        if m.ret != sig.ret {
            return Err(format!(
                "provider `{} for {}`: method `{}` has return type `{}` but the port declares `{}`.",
                port.name,
                target,
                sig.name,
                opt_type_name(&m.ret),
                opt_type_name(&sig.ret)
            ));
        }
    }
    Ok(())
}

/// Renders a type name (`Type`), reusing the existing renderer.
fn type_name(t: &crate::ast::Type) -> String {
    match t {
        crate::ast::Type::Named(s) => s.clone(),
        crate::ast::Type::Ptr(inner) => format!("*{}", type_name(inner)),
    }
}

/// Renders an optional return type: the type, or `()` for unit.
fn opt_type_name(t: &Option<crate::ast::Type>) -> String {
    match t {
        Some(t) => type_name(t),
        None => "()".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    /// Parses a program and runs resolution for `target`.
    fn resolve(src: &str, target: &str) -> Result<Vec<Function>, String> {
        let mut p = Parser::new(tokenize(src));
        p.parse_program();
        let ports = p.take_ports();
        let providers = p.take_providers();
        let uses = p.take_uses();
        resolve_ports(&ports, &providers, &uses, target)
    }

    #[test]
    fn a_use_with_a_matching_provider_resolves_and_splices_its_methods() {
        let r = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial\n\
             fn main() -> i64 { 0 }",
            "pic16",
        );
        let spliced = r.expect("a use with its target provider must resolve");
        let names: Vec<&str> = spliced.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["putc"], "the selected provider's methods are spliced in");
    }

    #[test]
    fn only_the_active_targets_provider_is_spliced() {
        // Two providers exist; only the one for the build target contributes code.
        let spliced = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for host { fn putc(b: i64) { let h = 1 } }\n\
             provides Serial for pic16 { fn putc(b: i64) { let p = 2 } }\n\
             use Serial",
            "pic16",
        )
        .expect("resolves for pic16");
        assert_eq!(spliced.len(), 1, "exactly one provider's methods, not both");
    }

    #[test]
    fn a_port_used_twice_is_spliced_once() {
        let spliced = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial\n\
             use Serial",
            "pic16",
        )
        .expect("resolves");
        assert_eq!(spliced.len(), 1, "no duplicate symbols from a repeated use");
    }

    #[test]
    fn no_provider_for_the_active_target_is_an_error_that_names_the_target() {
        // The whole point: the error names the target that lacks an implementation, so a
        // missing port becomes a task instead of an "unknown function" at the call site.
        let e = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for host { fn putc(b: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("pic16"), "names the target lacking a provider: {e}");
        assert!(e.contains("Serial"), "names the port: {e}");
        assert!(e.contains("host"), "lists the targets that do have a provider: {e}");
    }

    #[test]
    fn two_providers_for_the_same_target_are_ambiguous() {
        let e = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(b: i64) { } }\n\
             provides Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("exactly one"), "ambiguity is rejected, never silent: {e}");
    }

    #[test]
    fn a_provider_missing_a_port_method_is_rejected() {
        let e = resolve(
            "port Serial { fn init() fn putc(b: i64) }\n\
             provides Serial for pic16 { fn init() { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("putc"), "names the missing method: {e}");
        assert!(e.contains("missing"), "says it is missing: {e}");
    }

    #[test]
    fn a_provider_method_with_the_wrong_arity_is_rejected() {
        let e = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc() { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("putc"), "names the method: {e}");
        assert!(e.contains("parameters"), "explains the arity mismatch: {e}");
    }

    #[test]
    fn a_spliced_method_carries_the_ports_contract() {
        // The port's ensures is folded onto the provider method, so `verify` proves the body
        // meets it. This is what makes a port a verified boundary, not a bare header.
        let spliced = resolve(
            "port Doubler { fn dbl(x: i64) -> (r: i64) ensures { r == x + x } }\n\
             provides Doubler for host { fn dbl(x: i64) -> i64 { x + x } }\n\
             use Doubler",
            "host",
        )
        .expect("resolves");
        assert_eq!(spliced.len(), 1);
        assert_eq!(spliced[0].ensures.len(), 1, "the port's ensures is folded in");
        assert_eq!(spliced[0].ret_name.as_deref(), Some("r"), "adopts the port's named return");
    }

    #[test]
    fn a_provider_that_renames_a_parameter_is_rejected() {
        let e = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(byte: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("parameter"), "explains the parameter-name mismatch: {e}");
        assert!(e.contains('b'), "names the port's parameter: {e}");
    }

    #[test]
    fn a_provider_with_a_mismatched_parameter_type_is_rejected() {
        // Arity and names match, but the type does not: the provider does not implement the
        // port's interface, and a caller verified against the port would be lied to.
        let e = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(b: *i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("putc"), "names the method: {e}");
        assert!(e.contains("type"), "explains the type mismatch: {e}");
    }

    #[test]
    fn a_provider_with_a_mismatched_return_type_is_rejected() {
        let e = resolve(
            "port Reader { fn get() -> i64 }\n\
             provides Reader for host { fn get() -> *i64 { 0 } }\n\
             use Reader",
            "host",
        )
        .unwrap_err();
        assert!(e.contains("get"), "names the method: {e}");
        assert!(e.contains("return"), "explains the return-type mismatch: {e}");
    }

    #[test]
    fn using_an_undeclared_port_is_an_error() {
        let e = resolve("use Serial", "pic16").unwrap_err();
        assert!(e.contains("no port named"), "a use of an undeclared port fails: {e}");
    }

    #[test]
    fn a_program_with_no_ports_resolves_trivially() {
        let r = resolve("fn main() -> i64 { 0 }", "host");
        assert!(r.is_ok(), "no ports, nothing to resolve");
    }
}

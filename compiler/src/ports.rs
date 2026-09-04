//! RFC-0014 port resolution: bind each `use Iface` to the one provider that
//! implements it for the active target, or fail with a message that names the
//! target lacking an implementation.
//!
//! Resolution is static and total. For every `use Iface` the checker requires
//! exactly one `impl Port for <active target>`; zero is a compile error (the
//! deliverable that replaces hand-stubbing a target's primitives), more than one
//! is ambiguity, and a provider that omits a port method (or gives it the wrong
//! arity) fails against the port rather than at some later call site. Full
//! contract verification of a provider against its port is a later slice; this
//! pass establishes existence, uniqueness, and shape.

use crate::ast::{Function, PortDef, ProviderDef, Signature, UseDecl};

/// Resolves every `use Iface` against the providers for `target`, returning the
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
    // Every provider must implement a declared port. A typo in a provider's port name would
    // otherwise leave it orphaned and silently unused, and a later `use` would fail as if the
    // provider had never been written.
    for pr in providers {
        if !ports.iter().any(|p| p.name == pr.port) {
            return Err(format!(
                "`impl {} for {}`: no interface named `{}` is declared.",
                pr.port, pr.target, pr.port
            ));
        }
    }

    let mut spliced = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for u in uses {
        let Some(port) = ports.iter().find(|p| p.name == u.port) else {
            return Err(format!(
                "`use {}`: no interface named `{}` is declared.",
                u.port, u.port
            ));
        };

        let for_port: Vec<&ProviderDef> = providers.iter().filter(|pr| pr.port == u.port).collect();
        let matching: Vec<&ProviderDef> = for_port
            .iter()
            .copied()
            .filter(|pr| pr.target == target)
            .collect();

        let prov = match matching.as_slice() {
            [] => {
                let mut targets: Vec<&str> = for_port.iter().map(|pr| pr.target.as_str()).collect();
                targets.sort_unstable();
                targets.dedup();
                let found = if targets.is_empty() {
                    "(none)".to_string()
                } else {
                    targets.join(", ")
                };
                return Err(format!(
                    "no implementation of interface `{port_name}` for target `{target}`. \
                     `use {port_name}` needs an `impl {port_name} for {target} {{ ... }}`. \
                     found implementations for: {found}",
                    port_name = u.port,
                ));
            }
            [prov] => *prov,
            more => {
                return Err(format!(
                    "interface `{}` has {} implementations for target `{}`; exactly one is allowed.",
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
                "`impl {} for {}` is missing method `{}` required by the interface.",
                port.name, target, sig.name
            ));
        };
        if m.params.len() != sig.params.len() {
            return Err(format!(
                "`impl {} for {}`: method `{}` takes {} parameters but the interface declares {}.",
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
                    "`impl {} for {}`: method `{}` names a parameter `{}` where the interface \
                     declares `{}`; an implementation must use the interface's parameter names so \
                     the interface's contract applies to it.",
                    port.name, target, sig.name, mp.name, sp.name
                ));
            }
            if mp.ty != sp.ty {
                return Err(format!(
                    "`impl {} for {}`: method `{}` gives parameter `{}` type `{}` but the \
                     interface declares type `{}`.",
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
                "`impl {} for {}`: method `{}` has return type `{}` but the interface declares `{}`.",
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
        crate::ast::Type::View(inner, len) => match len {
            Some(n) => format!("{}[{n}]", type_name(inner)),
            None => format!("{}[]", type_name(inner)),
        },
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
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial\n\
             fn main() -> i64 { 0 }",
            "pic16",
        );
        let spliced = r.expect("a use with its target provider must resolve");
        let names: Vec<&str> = spliced.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["putc"],
            "the selected provider's methods are spliced in"
        );
    }

    #[test]
    fn only_the_active_targets_provider_is_spliced() {
        // Two providers exist; only the one for the build target contributes code.
        let spliced = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for host { fn putc(b: i64) { let h = 1 } }\n\
             impl Serial for pic16 { fn putc(b: i64) { let p = 2 } }\n\
             use Serial",
            "pic16",
        )
        .expect("resolves for pic16");
        assert_eq!(spliced.len(), 1, "exactly one provider's methods, not both");
    }

    #[test]
    fn a_port_used_twice_is_spliced_once() {
        let spliced = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc(b: i64) { } }\n\
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
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for host { fn putc(b: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(
            e.contains("pic16"),
            "names the target lacking a provider: {e}"
        );
        assert!(e.contains("Serial"), "names the port: {e}");
        assert!(
            e.contains("host"),
            "lists the targets that do have a provider: {e}"
        );
    }

    #[test]
    fn two_providers_for_the_same_target_are_ambiguous() {
        let e = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc(b: i64) { } }\n\
             impl Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(
            e.contains("exactly one"),
            "ambiguity is rejected, never silent: {e}"
        );
    }

    #[test]
    fn a_provider_missing_a_port_method_is_rejected() {
        let e = resolve(
            "iface Serial { fn init() fn putc(b: i64) }\n\
             impl Serial for pic16 { fn init() { } }\n\
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
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc() { } }\n\
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
            "iface Doubler { fn dbl(x: i64) -> (r: i64) ensures { r == x + x } }\n\
             impl Doubler for host { fn dbl(x: i64) -> i64 { x + x } }\n\
             use Doubler",
            "host",
        )
        .expect("resolves");
        assert_eq!(spliced.len(), 1);
        assert_eq!(
            spliced[0].ensures.len(),
            1,
            "the port's ensures is folded in"
        );
        assert_eq!(
            spliced[0].ret_name.as_deref(),
            Some("r"),
            "adopts the port's named return"
        );
    }

    #[test]
    fn a_spliced_method_carries_the_ports_requires_so_the_caller_is_checked() {
        // The other half of the contract boundary: the port's `requires` is folded onto the
        // spliced method, so the general verifier holds every caller to it (verified e2e:
        // narrow(999) against requires { x <= 10 } is a call-site precondition failure).
        let spliced = resolve(
            "iface Narrow { fn narrow(x: i64) -> (r: i64) requires { x <= 10 } ensures { r == x } }\n\
             impl Narrow for host { fn narrow(x: i64) -> i64 { x } }\n\
             use Narrow",
            "host",
        )
        .expect("resolves");
        assert_eq!(
            spliced[0].requires.len(),
            1,
            "the port's requires is folded onto the method"
        );
        assert_eq!(spliced[0].ensures.len(), 1, "and its ensures");
    }

    #[test]
    fn a_provider_that_renames_a_parameter_is_rejected() {
        let e = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc(byte: i64) { } }\n\
             use Serial",
            "pic16",
        )
        .unwrap_err();
        assert!(
            e.contains("parameter"),
            "explains the parameter-name mismatch: {e}"
        );
        assert!(e.contains('b'), "names the port's parameter: {e}");
    }

    #[test]
    fn a_provider_with_a_mismatched_parameter_type_is_rejected() {
        // Arity and names match, but the type does not: the provider does not implement the
        // port's interface, and a caller verified against the port would be lied to.
        let e = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serial for pic16 { fn putc(b: *i64) { } }\n\
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
            "iface Reader { fn get() -> i64 }\n\
             impl Reader for host { fn get() -> *i64 { 0 } }\n\
             use Reader",
            "host",
        )
        .unwrap_err();
        assert!(e.contains("get"), "names the method: {e}");
        assert!(
            e.contains("return"),
            "explains the return-type mismatch: {e}"
        );
    }

    #[test]
    fn using_an_undeclared_port_is_an_error() {
        let e = resolve("use Serial", "pic16").unwrap_err();
        assert!(
            e.contains("no interface named"),
            "a use of an undeclared port fails: {e}"
        );
    }

    #[test]
    fn a_provider_for_an_undeclared_port_is_rejected() {
        // A typo in a provider's port name would otherwise leave it orphaned and silently
        // unused, and a later `use` would fail as if the provider were never written.
        let e = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             impl Serail for pic16 { fn putc(b: i64) { } }",
            "pic16",
        )
        .unwrap_err();
        assert!(e.contains("Serail"), "names the misspelled port: {e}");
        assert!(
            e.contains("no interface named"),
            "says the port is undeclared: {e}"
        );
    }

    #[test]
    fn two_distinct_ports_each_resolve_and_splice_independently() {
        // A real program depends on several interfaces at once. Each `use` binds to its own
        // port's provider for the active target, and both providers' methods are spliced.
        let spliced = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             iface Clock { fn now() -> i64 }\n\
             impl Serial for host { fn putc(b: i64) { } }\n\
             impl Clock for host { fn now() -> i64 { 0 } }\n\
             use Serial\n\
             use Clock",
            "host",
        )
        .expect("both ports resolve for host");
        let mut names: Vec<&str> = spliced.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["now", "putc"],
            "both providers' methods spliced"
        );
    }

    #[test]
    fn one_port_resolving_does_not_hide_another_ports_missing_provider() {
        // Serial resolves for host, but Clock has no host provider: the whole program is
        // rejected, not silently accepted because the first `use` happened to resolve.
        let e = resolve(
            "iface Serial { fn putc(b: i64) }\n\
             iface Clock { fn now() -> i64 }\n\
             impl Serial for host { fn putc(b: i64) { } }\n\
             impl Clock for pic16 { fn now() -> i64 { 0 } }\n\
             use Serial\n\
             use Clock",
            "host",
        )
        .unwrap_err();
        assert!(
            e.contains("Clock"),
            "names the port with no host provider: {e}"
        );
        assert!(e.contains("host"), "names the target lacking it: {e}");
    }

    #[test]
    fn a_program_with_no_ports_resolves_trivially() {
        let r = resolve("fn main() -> i64 { 0 }", "host");
        assert!(r.is_ok(), "no ports, nothing to resolve");
    }
}

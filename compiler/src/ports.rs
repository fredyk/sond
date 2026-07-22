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

use crate::ast::{PortDef, ProviderDef, UseDecl};

/// Checks every `use Port` against the providers for `target`. Returns `Ok` when
/// each use resolves to exactly one complete provider, else the first error.
pub fn resolve_ports(
    ports: &[PortDef],
    providers: &[ProviderDef],
    uses: &[UseDecl],
    target: &str,
) -> Result<(), String> {
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

        match matching.as_slice() {
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
            [prov] => check_complete(port, prov, target)?,
            more => {
                return Err(format!(
                    "port `{}` has {} providers for target `{}`; exactly one is allowed.",
                    u.port,
                    more.len(),
                    target
                ));
            }
        }
    }
    Ok(())
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    /// Parses a program and runs resolution for `target`.
    fn resolve(src: &str, target: &str) -> Result<(), String> {
        let mut p = Parser::new(tokenize(src));
        p.parse_program();
        let ports = p.take_ports();
        let providers = p.take_providers();
        let uses = p.take_uses();
        resolve_ports(&ports, &providers, &uses, target)
    }

    #[test]
    fn a_use_with_a_matching_provider_resolves() {
        let r = resolve(
            "port Serial { fn putc(b: i64) }\n\
             provides Serial for pic16 { fn putc(b: i64) { } }\n\
             use Serial\n\
             fn main() -> i64 { 0 }",
            "pic16",
        );
        assert!(r.is_ok(), "a use with its target provider must resolve, got {r:?}");
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

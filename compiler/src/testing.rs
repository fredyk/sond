//! The `sond test` harness: turns the `test "..." { .. }` blocks in a source file into a
//! runnable program whose `main` runs each test in an ISOLATED forked child and reports a
//! pass/fail tally.
//!
//! Each test runs in its own `fork`ed process, so a failing `assert` (which exits nonzero)
//! or an outright crash (a trap) takes down only that child, the parent `wait4`s, records
//! the outcome, and moves on. The process exit code is the number of failed tests (0 when
//! all pass). This relies on the testing library (`assert`, `__test_report`) and the Linux
//! syscall numbers, both pulled in when the source `include`s `x86_64/linux/testing.snd`.
//!
//! The harness `main` is produced as Sond SOURCE and parsed with the ordinary parser, so it
//! is always a well-formed AST, no hand-built nodes to keep in sync with the language.

use crate::ast::Function;
use crate::lexer::tokenize;
use crate::parser::Parser;

/// Escapes a test's display name for embedding in a Sond string literal.
fn escape(name: &str) -> String {
    name.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Builds the `main` that `sond test` runs. It forks each test into its own child, waits,
/// tallies failures, and exits with that count. Returns a `main` Function.
pub fn synth_harness(funcs: &[Function]) -> Function {
    let tests: Vec<&Function> = funcs.iter().filter(|f| f.is_test).collect();
    let n = tests.len();
    let mut src = String::from("fn main() -> i64 {\n    let mut __failed = 0\n");
    for (k, t) in tests.iter().enumerate() {
        let label = escape(t.test_name.as_deref().unwrap_or(&t.name));
        let sym = &t.name;
        src.push_str(&format!("    print(\"test {label} ... \")\n"));
        src.push_str(&format!("    let __p{k} = syscall(SYS_fork)\n"));
        src.push_str(&format!(
            "    if __p{k} == 0 {{ {sym}() syscall(SYS_exit, 0) }}\n"
        ));
        src.push_str(&format!("    let __s{k}[1]\n"));
        src.push_str(&format!("    syscall(SYS_wait4, __p{k}, __s{k}, 0, 0)\n"));
        // wait4 writes a 32-bit int status into the 8-byte slot; mask off the high 4 bytes
        // of stack garbage before testing it (0 == the child exited cleanly = the test passed).
        src.push_str(&format!(
            "    __failed = __test_report(__s{k}[0] & 0xffffffff, __failed)\n"
        ));
    }
    src.push_str(&format!("    print(\"\\n{n} test(s), \")\n"));
    src.push_str("    print_int(__failed)\n");
    src.push_str("    print(\" failed\\n\")\n");
    src.push_str("    __failed\n}\n");

    let mut parsed = Parser::new(tokenize(&src)).parse_program();
    parsed
        .pop()
        .expect("synthesized harness must parse to one function")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Vec<Function> {
        Parser::new(tokenize(src)).parse_program()
    }

    #[test]
    fn harness_is_a_main_that_references_every_test_symbol() {
        let funcs = parse("test \"a\" { let x = 1 }\ntest \"b\" { let y = 2 }");
        let main = synth_harness(&funcs);
        assert_eq!(main.name, "main");
        // The generated source (round-tripped through the parser) drives each test symbol.
        // Rebuild the source view to assert both test symbols are invoked.
        let src = format!("{:?}", main.body);
        assert!(src.contains("__test_0"), "harness calls the first test");
        assert!(src.contains("__test_1"), "harness calls the second test");
    }

    #[test]
    fn harness_with_no_tests_still_parses_to_main() {
        let main = synth_harness(&parse("fn f() -> i64 { 0 }"));
        assert_eq!(main.name, "main");
    }

    #[test]
    fn harness_uses_the_real_test_symbol_not_a_sequential_index() {
        // A non-test fn sits between the tests, so the second test's symbol is __test_2,
        // not __test_1, the harness must use the actual name.
        let funcs = parse("test \"a\" { let x = 1 }\nfn helper() -> i64 { 0 }\ntest \"b\" { let y = 2 }");
        let main = synth_harness(&funcs);
        let src = format!("{:?}", main.body);
        assert!(src.contains("__test_0"));
        assert!(src.contains("__test_2"), "uses the real symbol __test_2");
    }
}

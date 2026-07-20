//! The `sond test` harness: turns the `test "..." { .. }` blocks in a source file into a
//! runnable program whose `main` runs each test in an ISOLATED forked child and reports a
//! pass/fail tally.
//!
//! Each test runs in its own `fork`ed process, so a failing `assert` (which exits nonzero)
//! or an outright crash (a trap) takes down only that child; the parent `wait4`s, records
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

/// The per-test block: announce it, fork, run it in the child (which exits, so a failing
/// assertion or crash is isolated), wait, and fold the outcome into `__failed`. Shared by the
/// plain and coverage harnesses.
fn fork_test_block(k: usize, sym: &str, label: &str) -> String {
    format!(
        "    print(\"test {label} ... \")\n\
         \x20   let __p{k} = syscall(SYS_fork)\n\
         \x20   if __p{k} == 0 {{ {sym}() syscall(SYS_exit, 0) }}\n\
         \x20   let __s{k}[1]\n\
         \x20   syscall(SYS_wait4, __p{k}, __s{k}, 0, 0)\n\
         \x20   __failed = __test_report(__s{k}[0] & 0xffffffff, __failed)\n"
    )
}

/// Parses a synthesized `fn main() { .. }` source into a Function (always well-formed, since
/// it goes through the ordinary parser).
fn parse_main(src: &str) -> Function {
    Parser::new(tokenize(src))
        .parse_program()
        .pop()
        .expect("synthesized harness must parse to one function")
}

/// Builds the `main` that `sond test` runs. It forks each test into its own child, waits,
/// tallies failures, and exits with that count. Returns a `main` Function.
pub fn synth_harness(funcs: &[Function]) -> Function {
    let tests: Vec<&Function> = funcs.iter().filter(|f| f.is_test).collect();
    let n = tests.len();
    let mut src = String::from("fn main() -> i64 {\n    let mut __failed = 0\n");
    for (k, t) in tests.iter().enumerate() {
        let label = escape(t.test_name.as_deref().unwrap_or(&t.name));
        src.push_str(&fork_test_block(k, &t.name, &label));
    }
    src.push_str(&format!("    print(\"\\n{n} test(s), \")\n"));
    src.push_str("    print_int(__failed)\n");
    src.push_str("    print(\" failed\\n\")\n");
    src.push_str("    __failed\n}\n");
    parse_main(&src)
}

/// Like [`synth_harness`], but the harness first mmaps a SHARED coverage buffer of
/// `__cov_total` words (so the instrumentation in the forked children accumulates into it),
/// points `__cov_ptr` at it, runs the tests, then counts the covered points and reports the
/// coverage. Compiled with `emit_program_coverage_g`, which instruments the code under test.
pub fn synth_coverage_harness(funcs: &[Function], min_coverage: u32) -> Function {
    let tests: Vec<&Function> = funcs.iter().filter(|f| f.is_test).collect();
    let n = tests.len();
    // PROT_READ|PROT_WRITE = 3; MAP_SHARED|MAP_ANONYMOUS = 1|32 = 33; fd = -1; offset = 0.
    let mut src = String::from(
        "fn main() -> i64 {\n\
         \x20   let __n = __cov_total\n\
         \x20   let __buf = syscall(SYS_mmap, 0, __n * 8, 3, 33, 0 - 1, 0)\n\
         \x20   __cov_ptr = __buf\n\
         \x20   let mut __failed = 0\n",
    );
    for (k, t) in tests.iter().enumerate() {
        let label = escape(t.test_name.as_deref().unwrap_or(&t.name));
        src.push_str(&fork_test_block(k, &t.name, &label));
    }
    src.push_str(
        "    let mut __covered = 0\n\
         \x20   let mut __i = 0\n\
         \x20   for __i < __n {\n\
         \x20       if peek64(__buf + __i * 8) != 0 { __covered = __covered + 1 }\n\
         \x20       __i = __i + 1\n\
         \x20   }\n",
    );
    // Name each function that is not fully covered, so a blocked build is actionable. Each
    // __cov_fn_table entry is 4 words: [name_ptr, name_len, first_point, point_count].
    src.push_str(
        "    let mut __fi = 0\n\
         \x20   for __fi < __cov_nfns {\n\
         \x20       let __base = __cov_fn_table + __fi * 32\n\
         \x20       let __miss = 0\n\
         \x20       let mut __j = 0\n\
         \x20       for __j < peek64(__base + 24) {\n\
         \x20           if peek64(__buf + (peek64(__base + 16) + __j) * 8) == 0 { __miss = 1 }\n\
         \x20           __j = __j + 1\n\
         \x20       }\n\
         \x20       if __miss == 1 {\n\
         \x20           print(\"  not fully covered: \")\n\
         \x20           syscall(SYS_write, 1, peek64(__base), peek64(__base + 8))\n\
         \x20           print(\"\\n\")\n\
         \x20       }\n\
         \x20       __fi = __fi + 1\n\
         \x20   }\n",
    );
    src.push_str(&format!("    print(\"\\n{n} test(s), \")\n"));
    src.push_str("    print_int(__failed)\n");
    src.push_str("    print(\" failed | coverage \")\n");
    src.push_str("    print_int(__covered)\n");
    src.push_str("    print(\"/\")\n");
    src.push_str("    print_int(__n)\n");
    src.push_str("    print(\" (\")\n");
    src.push_str("    print_int(__covered * 100 / __n)\n");
    src.push_str("    print(\"%)\\n\")\n");
    // The gate: coverage below the threshold is a failure, distinct from test failures.
    // Integer form of `covered/n < min/100` is `covered*100 < min*n`.
    src.push_str(&format!(
        "    let __gatefail = if __covered * 100 < {min_coverage} * __n {{ 1 }} else {{ 0 }}\n"
    ));
    src.push_str(&format!(
        "    if __gatefail == 1 {{ print(\"coverage below {min_coverage}% threshold\\n\") }}\n"
    ));
    src.push_str("    __failed + __gatefail\n}\n");
    parse_main(&src)
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
    fn coverage_harness_maps_the_buffer_and_counts() {
        let funcs = parse("test \"a\" { let x = 1 }\ntest \"b\" { let y = 2 }");
        let main = synth_coverage_harness(&funcs, 80);
        assert_eq!(main.name, "main");
        let src = format!("{:?}", main.body);
        // Reads the point count, mmaps a shared buffer, points __cov_ptr at it, and both
        // tests are driven.
        assert!(src.contains("__cov_total"), "reads the total point count");
        assert!(src.contains("SYS_mmap"), "mmaps a shared coverage buffer");
        assert!(src.contains("__cov_ptr"), "points the instrumentation buffer at the mmap");
        assert!(src.contains("__test_0") && src.contains("__test_1"), "runs both tests");
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

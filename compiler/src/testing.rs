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
use crate::token::Token;

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
/// it goes through the ordinary parser). The result is marked `is_harness`: it is compiler
/// output, not author code, and that mark is what lets the undeclared-variable check accept
/// its references to the reserved `__cov_*` coverage symbols while refusing them everywhere
/// else.
fn parse_main(src: &str) -> Function {
    let mut f = Parser::new(tokenize(src))
        .parse_program()
        .pop()
        .expect("synthesized harness must parse to one function");
    f.is_harness = true;
    f
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
    // An empty denominator is not 100%: `covered * 100 / 0` is a divide by zero, and the harness
    // died with the line half printed, leaving the run with no verdict at all. It is also not a
    // pass -- `0 * 100 < 80 * 0` is false -- so the entry that answers for nothing would have
    // sailed through the gate. Both roads end at the same rule: measuring nothing is a failure.
    src.push_str("    let __den = if __n == 0 { 1 } else { __n }\n");
    src.push_str("    print_int(__covered * 100 / __den)\n");
    src.push_str("    print(\"%)\\n\")\n");
    // The gate: coverage below the threshold is a failure, distinct from test failures.
    // Integer form of `covered/n < min/100` is `covered*100 < min*n`.
    src.push_str(&format!(
        "    let mut __gatefail = if __covered * 100 < {min_coverage} * __n {{ 1 }} else {{ 0 }}\n"
    ));
    src.push_str(&format!(
        "    if __gatefail == 1 {{ print(\"coverage below {min_coverage}% threshold\\n\") }}\n"
    ));
    src.push_str(
        "    if __n == 0 {\n\
         \x20       print(\"nothing measured: this entry answers for no instrumented code\\n\")\n\
         \x20       __gatefail = 1\n\
         \x20   }\n",
    );
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
        assert!(
            src.contains("__cov_ptr"),
            "points the instrumentation buffer at the mmap"
        );
        assert!(
            src.contains("__test_0") && src.contains("__test_1"),
            "runs both tests"
        );
    }

    #[test]
    fn harness_uses_the_real_test_symbol_not_a_sequential_index() {
        // A non-test fn sits between the tests, so the second test's symbol is __test_2,
        // not __test_1, the harness must use the actual name.
        let funcs =
            parse("test \"a\" { let x = 1 }\nfn helper() -> i64 { 0 }\ntest \"b\" { let y = 2 }");
        let main = synth_harness(&funcs);
        let src = format!("{:?}", main.body);
        assert!(src.contains("__test_0"));
        assert!(src.contains("__test_2"), "uses the real symbol __test_2");
    }
}

/// Rejects a program whose `test` blocks did not all become tests.
///
/// The token stream is an oracle the parser cannot influence: the lexer classifies `test` as a
/// keyword only when a string literal follows it, which is exactly a `test "name" { .. }` block,
/// so counting those tokens counts the blocks the AUTHOR wrote. If fewer functions come out
/// marked as tests than blocks went in, the parser dropped some, and the harness would then run
/// a subset and report it as the whole: a green suite with phantom tests, which is worse than a
/// red one because nobody looks. The coverage gate is computed on the same subset, so it lies
/// too, and the compiler refuses to build under 80%: an author is then sent to add tests that
/// are already there.
///
/// This happened: a file declaring 70 blocks reported "56 test(s), 0 failed" and named none of
/// the missing 14, because statements orphaned by a bad edit ended the top-level parse early.
pub fn check_all_tests_registered(tokens: &[Token], funcs: &[Function]) -> Result<(), String> {
    let declared = tokens.iter().filter(|t| matches!(t, Token::Test)).count();
    let registered = funcs.iter().filter(|f| f.is_test).count();
    if declared != registered {
        return Err(format!(
            "the source declares {declared} `test` block(s) but only {registered} became tests, \
             so {} would never run and the suite would report itself green without them. This is \
             usually a block that did not close, or a statement left outside every function, \
             which ends the parse early and takes everything after it.",
            declared - registered
        ));
    }
    Ok(())
}

#[cfg(test)]
mod registered_tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let toks = tokenize(src);
        let funcs = Parser::new(tokenize(src)).parse_program();
        check_all_tests_registered(&toks, &funcs)
    }

    #[test]
    fn a_file_whose_blocks_all_parse_is_accepted() {
        let src = "fn f() -> i64 { 1 }\n\
                   test \"one\" { assert_eq(f(), 1) }\n\
                   test \"two\" { assert_eq(f(), 1) }\n";
        assert!(check(src).is_ok(), "two blocks, two tests");
    }

    #[test]
    fn the_word_test_outside_a_block_is_not_counted() {
        // `test` is an ordinary identifier unless a string follows it, so a function or variable
        // called `test` must not inflate the count and make a correct file fail.
        let src = "fn test() -> i64 { 1 }\n\
                   test \"only one\" { let test = 2\nassert_eq(test, 2) }\n";
        assert!(
            check(src).is_ok(),
            "one real block; the identifiers are not blocks"
        );
    }

    #[test]
    fn a_dropped_block_is_reported_with_both_counts() {
        // Two blocks written, one parsed: the shape a bad edit leaves behind.
        let toks = tokenize(
            "fn f() -> i64 { 1 }\n\
             test \"one\" { assert_eq(f(), 1) }\n\
             test \"two\" { assert_eq(f(), 1) }\n",
        );
        let funcs = Parser::new(tokenize(
            "fn f() -> i64 { 1 }\ntest \"one\" { assert_eq(f(), 1) }\n",
        ))
        .parse_program();
        let err =
            check_all_tests_registered(&toks, &funcs).expect_err("a dropped block is refused");
        assert!(
            err.contains('2') && err.contains('1'),
            "name both counts: {err}"
        );
        assert!(
            err.contains("never run"),
            "say what the consequence is: {err}"
        );
    }
}

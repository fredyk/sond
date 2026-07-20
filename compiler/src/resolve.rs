//! Source-level `include` resolution: the pragmatic, Go-simple package on-ramp.
//!
//! A program can pull in a reusable library with a top-level line
//! `include "path.snd"` (path relative to the including file). The compiler splices
//! that file's declarations in before parsing, so a `chacha20.snd` can be shared by
//! every file that needs it. Includes are resolved recursively and DEDUPED by
//! canonical path, so pulling the same library in from two places defines its
//! functions only once (no duplicate-definition errors). This is a stepping stone
//! toward the RFC-0006 package format, not a full module system: one flat namespace,
//! no visibility, but it makes real multi-file programs (an SSH stack) buildable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Reads `entry` and inlines every `include "..."` it (transitively) names, returning
/// the combined source ready for the lexer.
pub fn resolve_includes(entry: &Path) -> std::io::Result<String> {
    Ok(resolve_includes_scoped(entry)?.0)
}

/// Like [`resolve_includes`], but also returns the set of function names that COVERAGE should
/// measure, the code the entry is responsible for testing. If the entry defines functions of
/// its own (a program), those are the scope; if it only holds `test` blocks (a library's test
/// file), the scope is the functions of its DIRECT includes (the library under test). Deeper,
/// transitively-included dependencies are assumed tested by their own suites, so they do not
/// drag down the entry's coverage. (The testing library is filtered out downstream.)
pub fn resolve_includes_scoped(entry: &Path) -> std::io::Result<(String, HashSet<String>)> {
    let mut seen = HashSet::new();
    let mut out = String::new();
    let mut depth0 = HashSet::new();
    let mut depth1 = HashSet::new();
    resolve_into(entry, &mut seen, &mut out, 0, &mut depth0, &mut depth1)?;
    // Which functions is this entry on the hook for covering?
    //
    // A file named `*_test.snd` exists to test what it includes, so the subject is its direct
    // includes and never its own top-level functions, which are test scaffolding.
    //
    // Deciding this by "does the entry define any functions of its own" instead, as this did
    // before, is a trap: adding one helper to a test file reclassified it as a program, and the
    // library under test dropped out of the denominator. A suite covering 1 of 5 library
    // functions then reported 1/1 and sailed through the 80% gate. The gate is the whole point,
    // so the rule it runs on has to be stated rather than inferred.
    let is_test_file = entry
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("_test.snd"));
    let scope = if is_test_file {
        if depth1.is_empty() { depth0 } else { depth1 }
    } else if depth0.is_empty() {
        depth1
    } else {
        depth0
    };
    Ok((out, scope))
}

fn resolve_into(
    path: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut String,
    depth: usize,
    depth0: &mut HashSet<String>,
    depth1: &mut HashSet<String>,
) -> std::io::Result<()> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canon) {
        return Ok(()); // already included, skip to avoid duplicate definitions
    }
    let src = std::fs::read_to_string(path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for line in src.lines() {
        match parse_include(line.trim()) {
            Some(rel) => resolve_into(&resolve_target(dir, rel), seen, out, depth + 1, depth0, depth1)?,
            None => {
                if let Some(name) = extract_fn_name(line) {
                    match depth {
                        0 => {
                            depth0.insert(name);
                        }
                        1 => {
                            depth1.insert(name);
                        }
                        _ => {}
                    }
                }
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(())
}

/// The name declared by a top-level `fn`/`pub fn`/`priv fn` line, if any (methods and
/// `test` blocks are not free functions and are ignored, the coverage scope of a library is
/// its free functions).
fn extract_fn_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub ")
        .or_else(|| t.strip_prefix("priv "))
        .unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Resolves an include path, in order: relative to the including file, then a `lib/`
/// root found by walking up the tree, then the system library `~/.local/lib/sond/`.
/// The system root lets an isolated program (e.g. a sandboxed AI worker) pull in the
/// shared libraries by their canonical path (`include "any/crypto/cipher/xor.snd"`),
/// while seeing only the `.snd` libraries, never the compiler's own source.
fn resolve_target(dir: &Path, rel: &str) -> PathBuf {
    let direct = dir.join(rel);
    if direct.exists() {
        return direct;
    }
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let candidate = d.join("lib").join(rel);
        if candidate.exists() {
            return candidate;
        }
        cur = d.parent();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let sys = Path::new(&home).join(".local/lib/sond").join(rel);
        if sys.exists() {
            return sys;
        }
    }
    direct // fall back to the relative path (error surfaces on read)
}

/// If `line` is a top-level `include "..."` directive, returns the quoted path.
fn parse_include(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("include")?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(name);
        writeln!(std::fs::File::create(&p).unwrap(), "{body}").unwrap();
        p
    }

    #[test]
    fn inlines_an_included_file_once() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let lib = tmp(&format!("sond_lib_{k}.snd"), "fn helper() -> i64 { 42 }");
        let libname = lib.file_name().unwrap().to_str().unwrap();
        // main includes the lib TWICE; it must appear only once.
        let main = tmp(
            &format!("sond_main_{k}.snd"),
            &format!(
                "include \"{libname}\"\ninclude \"{libname}\"\nfn main() -> i64 {{ helper() }}"
            ),
        );
        let combined = resolve_includes(&main).unwrap();
        assert_eq!(combined.matches("fn helper").count(), 1, "included once");
        assert!(combined.contains("fn main"), "keeps the main file");
    }

    #[test]
    fn a_test_files_coverage_scope_is_the_library_it_includes_not_its_own_helpers() {
        // The gate that makes a Sond program refuse to build without 80% coverage is only
        // worth anything if it measures the code under test. A `_test.snd` file that happens
        // to define a helper of its own must not shrink the denominator to that helper: that
        // turns "1 of 5 library functions covered" into a green "1 of 1".
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let lib = tmp(
            &format!("sond_covlib_{k}.snd"),
            "fn a1() -> i64 { 1 }\nfn a2() -> i64 { 2 }\nfn a3() -> i64 { 3 }",
        );
        let libname = lib.file_name().unwrap().to_str().unwrap();
        let entry = tmp(
            &format!("sond_covscope_{k}_test.snd"),
            &format!("include \"{libname}\"\nfn helper() -> i64 {{ 7 }}\ntest \"t\" {{ }}"),
        );
        let (_, scope) = resolve_includes_scoped(&entry).unwrap();
        assert!(scope.contains("a1"), "the library under test is the scope");
        assert!(scope.contains("a3"), "all of it, not a sample");
        assert!(
            !scope.contains("helper"),
            "a helper in the test file is scaffolding, not the subject"
        );
    }

    #[test]
    fn a_program_is_still_measured_on_its_own_functions() {
        // The counterpart: a program that includes the standard library is not on the hook
        // for covering the standard library.
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let lib = tmp(&format!("sond_progdep_{k}.snd"), "fn dep() -> i64 { 1 }");
        let libname = lib.file_name().unwrap().to_str().unwrap();
        let entry = tmp(
            &format!("sond_prog_{k}.snd"),
            &format!("include \"{libname}\"\nfn mine() -> i64 {{ dep() }}\ntest \"t\" {{ }}"),
        );
        let (_, scope) = resolve_includes_scoped(&entry).unwrap();
        assert!(scope.contains("mine"), "its own functions");
        assert!(!scope.contains("dep"), "not its dependencies");
    }

    #[test]
    fn a_line_that_only_starts_with_include_is_not_a_directive() {
        // `included` / `include_x` are ordinary identifiers, not directives.
        assert_eq!(parse_include("included = 5"), None);
        assert_eq!(parse_include("let include_flag = 1"), None);
        assert_eq!(parse_include("include \"lib.snd\""), Some("lib.snd"));
    }
}

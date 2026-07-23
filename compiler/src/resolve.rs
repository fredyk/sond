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

/// Resolves an include path, trying a short list of CANDIDATES in order until one
/// exists, so a program may write the short `include "serial"` and reach
/// `any/serial.snd`. For a given `rel` the candidates are: the path as written, the
/// same with `.snd` appended (so the extension is optional), then both again under
/// the `any/` prefix (the target-agnostic library root, tried only when the exact
/// path is not found). Each candidate is searched through the roots below.
fn resolve_target(dir: &Path, rel: &str) -> PathBuf {
    for cand in include_candidates(rel) {
        if let Some(found) = find_in_roots(dir, &cand) {
            return found;
        }
    }
    dir.join(rel) // fall back to the path as written (error surfaces on read)
}

/// The ordered candidate paths for an include of `rel`: exact first (so an explicit
/// `any/serial.snd` resolves immediately and unchanged), then `.snd`-appended, then
/// the same two under `any/`. The `any/` prefix makes `"serial"` an alias of
/// `"any/serial"` when the exact path is not found.
fn include_candidates(rel: &str) -> Vec<String> {
    let has_ext = rel.ends_with(".snd");
    let mut cands = vec![rel.to_string()];
    if !has_ext {
        cands.push(format!("{rel}.snd"));
    }
    // Only alias into `any/` when the path is not already an `any/` path.
    if !rel.starts_with("any/") {
        cands.push(format!("any/{rel}"));
        if !has_ext {
            cands.push(format!("any/{rel}.snd"));
        }
    }
    cands
}

/// Searches for one candidate path through the roots, in order: relative to the
/// including file, then a `lib/` root found by walking up the tree, then the system
/// library `~/.local/lib/sond/`. The system root lets an isolated program (e.g. a
/// sandboxed AI worker) pull in the shared libraries by their canonical path, while
/// seeing only the `.snd` libraries, never the compiler's own source.
fn find_in_roots(dir: &Path, rel: &str) -> Option<PathBuf> {
    let direct = dir.join(rel);
    if direct.exists() {
        return Some(direct);
    }
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let candidate = d.join("lib").join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
        cur = d.parent();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let sys = Path::new(&home).join(".local/lib/sond").join(rel);
        if sys.exists() {
            return Some(sys);
        }
    }
    None
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

    // A fresh temp directory that is a `lib/` root: `root/lib/<subpath>` plus a `root/main.snd`
    // entry that includes `include_line`. Returns the entry path. Used to exercise include
    // resolution against a walk-up `lib/` root without touching the real ~/.local/lib/sond.
    fn lib_fixture(k: usize, subpath: &str, body: &str, include_line: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sond_res_{}_{k}", std::process::id()));
        let lib_file = root.join("lib").join(subpath);
        std::fs::create_dir_all(lib_file.parent().unwrap()).unwrap();
        std::fs::write(&lib_file, body).unwrap();
        let entry = root.join("main.snd");
        std::fs::write(&entry, format!("{include_line}\nfn main() -> i64 {{ 0 }}")).unwrap();
        entry
    }

    #[test]
    fn a_bare_name_resolves_under_any_with_the_snd_extension_added() {
        // `include "serial"` must reach `any/serial.snd`: the extension is optional and the
        // `any/` prefix is tried when the exact path is not found (JMV's alias request).
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let entry = lib_fixture(k, "any/serial.snd", "fn serial_getc() -> i64 { 7 }", "include \"serial\"");
        let combined = resolve_includes(&entry).unwrap();
        assert!(combined.contains("fn serial_getc"), "`include \"serial\"` pulled in any/serial.snd");
    }

    #[test]
    fn the_snd_extension_is_optional_on_an_explicit_any_path() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let entry = lib_fixture(k, "any/frame.snd", "fn frame_encode() -> i64 { 1 }", "include \"any/frame\"");
        let combined = resolve_includes(&entry).unwrap();
        assert!(combined.contains("fn frame_encode"), "`any/frame` resolved to any/frame.snd");
    }

    #[test]
    fn an_explicit_full_path_still_resolves_exactly() {
        // The alias must not change behaviour for the canonical, extension-carrying paths the
        // whole stdlib already uses.
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let entry = lib_fixture(k, "x86_64/linux/io.snd", "fn close_fd(fd: i64) -> i64 { 0 }", "include \"x86_64/linux/io.snd\"");
        let combined = resolve_includes(&entry).unwrap();
        assert!(combined.contains("fn close_fd"), "explicit path unchanged");
    }

    #[test]
    fn a_bare_and_a_full_include_of_the_same_file_are_deduped() {
        // `include "serial"` and `include "any/serial.snd"` name the same file; it must appear
        // once, not twice (which would be duplicate definitions).
        static N: AtomicUsize = AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("sond_resd_{}_{k}", std::process::id()));
        let lib_file = root.join("lib").join("any").join("serial.snd");
        std::fs::create_dir_all(lib_file.parent().unwrap()).unwrap();
        std::fs::write(&lib_file, "fn serial_putc(b: i64) { }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(&entry, "include \"serial\"\ninclude \"any/serial.snd\"\nfn main() -> i64 { 0 }").unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert_eq!(combined.matches("fn serial_putc").count(), 1, "included once despite two spellings");
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

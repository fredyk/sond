//! Source-level `include` resolution, the pragmatic, Go-simple package on-ramp.
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
    let scope = if depth0.is_empty() { depth1 } else { depth0 };
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
/// shared libraries by their canonical path, `include "any/crypto/cipher/xor.snd"` -
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
    fn a_line_that_only_starts_with_include_is_not_a_directive() {
        // `included` / `include_x` are ordinary identifiers, not directives.
        assert_eq!(parse_include("included = 5"), None);
        assert_eq!(parse_include("let include_flag = 1"), None);
        assert_eq!(parse_include("include \"lib.snd\""), Some("lib.snd"));
    }
}

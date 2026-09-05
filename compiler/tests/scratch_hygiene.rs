//! Test scratch directories have to disappear when the test ends.
//!
//! `env::temp_dir()` is tmpfs on this machine: what a test leaves there is RAM that never comes
//! back. The resolve tests created a directory per case and deleted none, and by the time anyone
//! looked there were 6.947 of them holding 610 MB, the oldest from twelve days earlier -- on a
//! machine that had just started killing processes for want of memory. Nothing failed; the suite
//! was green the whole time, which is why it ran for twelve days.
//!
//! So the rule is not "remember to clean up". It is that a test cannot name a scratch path
//! without getting the cleanup with it, and this guard is what keeps the door shut.
use std::path::{Path, PathBuf};

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every Rust source under the compiler, with the file that implements scratch itself left out.
fn fuentes() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut pila = vec![compiler().join("src"), compiler().join("tests")];
    while let Some(d) = pila.pop() {
        let Ok(e) = std::fs::read_dir(&d) else { continue };
        for x in e.flatten() {
            let p = x.path();
            if p.is_dir() {
                pila.push(p);
            } else if p.extension().is_some_and(|s| s == "rs")
                && p.file_name().is_some_and(|n| n != "scratch.rs")
            {
                v.push(p);
            }
        }
    }
    v.sort();
    assert!(v.len() > 10, "the compiler has more sources than this: {}", v.len());
    v
}

/// The lines of `file` that belong to test code: everything in a `tests/` file, and everything
/// after a `mod tests {` in a source file.
fn lineas_de_test(file: &Path, body: &str) -> Vec<(usize, String)> {
    // A file called `tests.rs` IS the test module: it holds no `mod tests` line to look for, and
    // the first version of this guard walked straight past codegen/tests.rs because of it.
    let en_tests_dir = file.components().any(|c| c.as_os_str() == "tests")
        || file.file_name().is_some_and(|n| n == "tests.rs");
    let mut dentro = en_tests_dir;
    let mut v = Vec::new();
    for (i, l) in body.lines().enumerate() {
        if l.trim_start().starts_with("mod tests") {
            dentro = true;
        }
        if dentro {
            v.push((i + 1, l.to_string()));
        }
    }
    v
}

/// Does this line CALL `temp_dir()`?
///
/// Two things that mention it are not calls, and a guard that counts them makes passing depend on
/// not writing them: the guard's own source, which has to hold the pattern it hunts, and a test
/// named after the thing it checks (`..._not_in_the_system_temp_dir`). So: outside string
/// literals, and not the tail of a longer identifier.
fn nombra_el_temporal_del_sistema(linea: &str) -> bool {
    let bytes = linea.as_bytes();
    let mut en_cadena = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if en_cadena => i += 1,
            b'"' => en_cadena = !en_cadena,
            _ if !en_cadena && bytes[i..].starts_with(b"temp_dir()") => {
                let antes = bytes[..i].last().copied().unwrap_or(b' ');
                if !antes.is_ascii_alphanumeric() && antes != b'_' {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[test]
fn the_guard_counts_calls_and_not_the_words_that_name_them() {
    assert!(nombra_el_temporal_del_sistema("    let d = std::env::temp_dir().join(x);"));
    assert!(nombra_el_temporal_del_sistema("let d = env::temp_dir();"));
    assert!(
        !nombra_el_temporal_del_sistema("            if t.contains(\"temp_dir()\") {"),
        "the guard's own source names the pattern it hunts"
    );
    assert!(
        !nombra_el_temporal_del_sistema("fn the_new_tree_is_staged_not_in_the_system_temp_dir() {"),
        "a test named after the thing it checks is not a call"
    );
}

#[test]
fn no_test_names_a_scratch_path_without_getting_the_cleanup_with_it() {
    let mut culprits = Vec::new();
    for f in fuentes() {
        let body = std::fs::read_to_string(&f).unwrap();
        for (n, l) in lineas_de_test(&f, &body) {
            let t = l.trim_start();
            if t.starts_with("//") || t.starts_with("///") {
                continue;
            }
            if nombra_el_temporal_del_sistema(t) {
                culprits.push(format!("{}:{n}", f.display()));
            }
        }
    }
    assert!(
        culprits.is_empty(),
        "these test lines build a scratch path by hand, so nothing deletes it when the test ends \
         -- use sondc::scratch::Temporal, which removes its directory on drop: {culprits:#?}"
    );
}

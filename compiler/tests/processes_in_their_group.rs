//! Nothing `sond` starts may be spawned outside a process group of its own.
//!
//! The harness forks a child per test, so the process that spins is one level below whatever
//! `Child::kill` reaches. The deadline only bites if the thing being killed is the whole group,
//! and that is decided at spawn time, in a line nobody looks at again. So the rule is not "call
//! `process_group` when you spawn something": it is that `spawn(` appears exactly once in the
//! driver, inside `spawn_in_group`, and this test is what keeps it there.

use std::path::PathBuf;

fn driver() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/sond.rs")
}

/// Does this line of code (not of comment, not of string) call `spawn`?
fn calls_spawn(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < b.len() {
        if in_string {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b[i] == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            return false;
        }
        if b[i..].starts_with(b"spawn(") && (i == 0 || !is_part_of_name(b[i - 1])) {
            return true;
        }
        i += 1;
    }
    false
}

fn is_part_of_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[test]
fn the_guard_reads_code_and_not_the_prose_around_it() {
    assert!(calls_spawn("    let h = Command::new(x).spawn().ok()?;"), "a real call");
    assert!(calls_spawn("cmd.process_group(0).spawn()"), "a real call after a builder");
    assert!(!calls_spawn("    // never call spawn( here"), "a comment is not a call");
    assert!(!calls_spawn("    eprintln!(\"cannot spawn( it\");"), "a string is not a call");
    assert!(!calls_spawn("    let x = respawn(3);"), "another identifier ending in spawn");
}

#[test]
fn the_driver_spawns_only_through_spawn_in_group() {
    let src = std::fs::read_to_string(driver()).expect("the driver is there");
    let mut inside = false;
    let mut loose = Vec::new();
    for (n, line) in src.lines().enumerate() {
        if line.starts_with("fn spawn_in_group(") {
            inside = true;
        } else if inside && line == "}" {
            inside = false;
        }
        if !inside && calls_spawn(line) {
            loose.push(format!("sond.rs:{}: {}", n + 1, line.trim()));
        }
    }
    assert!(
        loose.is_empty(),
        "a process spawned outside `spawn_in_group` has no group of its own, so the deadline cannot \
         reach what it forks:\n{}",
        loose.join("\n")
    );
}

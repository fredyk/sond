//! Nothing `sond` starts may be spawned outside a process group of its own.
//!
//! The harness forks a child per test, so the process that spins is one level below whatever
//! `Child::kill` reaches. The deadline only bites if the thing being killed is the whole group,
//! and that is decided at spawn time, in a line nobody looks at again. So the rule is not "call
//! `process_group` when you spawn something": it is that `spawn` appears exactly once in the
//! driver, inside `lanzar`, and this test is what keeps it there.

use std::path::PathBuf;

fn driver() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/sond.rs")
}

/// Does this line of code (not of comment, not of string) call `spawn`?
fn llama_a_spawn(linea: &str) -> bool {
    let b = linea.as_bytes();
    let mut i = 0;
    let mut en_cadena = false;
    while i < b.len() {
        if en_cadena {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                en_cadena = false;
            }
            i += 1;
            continue;
        }
        if b[i] == b'"' {
            en_cadena = true;
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            return false;
        }
        if b[i..].starts_with(b"spawn(") && (i == 0 || !es_parte_de_nombre(b[i - 1])) {
            return true;
        }
        i += 1;
    }
    false
}

fn es_parte_de_nombre(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

#[test]
fn the_guard_reads_code_and_not_the_prose_around_it() {
    assert!(llama_a_spawn("    let h = Command::new(x).spawn().ok()?;"), "a real call");
    assert!(llama_a_spawn("cmd.process_group(0).spawn()"), "a real call after a builder");
    assert!(!llama_a_spawn("    // never call spawn( here"), "a comment is not a call");
    assert!(!llama_a_spawn("    eprintln!(\"cannot spawn( it\");"), "a string is not a call");
    assert!(!llama_a_spawn("    let x = respawn(3);"), "another identifier ending in spawn");
}

#[test]
fn the_driver_spawns_only_through_lanzar() {
    let src = std::fs::read_to_string(driver()).expect("the driver is there");
    let mut dentro = false;
    let mut sueltas = Vec::new();
    for (n, linea) in src.lines().enumerate() {
        if linea.starts_with("fn lanzar(") {
            dentro = true;
        } else if dentro && linea == "}" {
            dentro = false;
        }
        if !dentro && llama_a_spawn(linea) {
            sueltas.push(format!("sond.rs:{}: {}", n + 1, linea.trim()));
        }
    }
    assert!(
        sueltas.is_empty(),
        "a process spawned outside `lanzar` has no group of its own, so the deadline cannot \
         reach what it forks:\n{}",
        sueltas.join("\n")
    );
}

//! Where a parse error says it is: the line and column of the file a person wrote.
//!
//! `resolve.rs` splices every `include`d file into one buffer, and the compiler used to parse the
//! tokens of that buffer. An error with no position was unfindable (the hunt through a 1201-line
//! file had to be done by chopping it function by function), and a position taken from the spliced
//! text sends the author to a line their file does not have. Every case here goes through the
//! real binary and asserts the message names the source file, its own line, and the column of the
//! offending token -- with the token written the way a person writes it (`` `{` ``), not the way
//! the lexer names it internally (`LBrace`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory for one case, named after the process so concurrent runs do not collide.
fn case_dir(case: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("parse-loc-{}-{}", std::process::id(), case));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory must be creatable");
    dir
}

/// Writes `name` = `content` into `dir`.
fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&p, content).expect("the test source must be writable");
    p
}

/// Runs `sondc` on `entry`, expects it to REFUSE the file, and returns the first line it printed.
fn first_error_line(entry: &Path) -> String {
    let out_bin = entry.with_extension("bin");
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(entry)
        .arg("-o")
        .arg(&out_bin)
        .output()
        .expect("sondc must be runnable");
    assert!(
        !out.status.success(),
        "`{}` must NOT compile, but sondc accepted it",
        entry.display()
    );
    let dicho = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let line = dicho.lines().next().unwrap_or("").to_string();
    assert!(
        !line.is_empty(),
        "a refusal must say why; sondc printed nothing"
    );
    let _ = std::fs::remove_file(&out_bin);
    line
}

/// The `(file, line, column)` a parse error message blames, per
/// `sondc: parse error in {file}:{line}:{col}: {message}`.
fn parse_error_location(msg: &str) -> Option<(String, u32, u32)> {
    let tail = msg.strip_prefix("sondc: parse error in ")?;
    let sep = tail.rfind(": ")?;
    let loc = &tail[..sep];
    let parts: Vec<&str> = loc.rsplitn(3, ':').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[2].to_string(),
        parts[1].parse().ok()?,
        parts[0].parse().ok()?,
    ))
}

#[test]
fn a_parse_error_names_the_line_and_the_column_of_the_offending_token() {
    // One line of Sond with an explicit `;` where the `if` wants a `{`. The `;` sits in its own
    // column; the message must name line 1 and exactly that column.
    let dir = case_dir("single-line");
    let src = "fn main() -> i64 { if a == 1; }";
    let entry = write(&dir, "prog.snd", src);
    let col = src.find(';').unwrap() + 1; // 1-based column of the `;`

    let msg = first_error_line(&entry);
    assert_eq!(
        parse_error_location(&msg),
        Some((format!("{}/prog.snd", dir.display()), 1, col as u32)),
        "the error must name the file, line 1 and the column of the `;`, got: {msg}"
    );
}

#[test]
fn an_error_in_the_third_include_names_that_file_and_its_own_line() {
    // Entry = `include a`, `include b`, `include c` and its own `fn main`: spliced, the broken
    // line of `c` shifts down by every line of `a`, `b` and the include lines. The reported
    // position must be c's file and c's own line, not that offset.
    let dir = case_dir("three-includes");
    write(&dir, "a.snd", "fn a_ok() -> i64 { 1 }\n");
    write(&dir, "b.snd", "fn b_ok() -> i64 { 2 }\nfn b_two() -> i64 { 3 }\n");
    let csrc = "fn c_ok() -> i64 { 1 }\nfn c_bad() -> i64 { if x == 1; }";
    write(&dir, "c.snd", csrc);
    let entry = write(
        &dir,
        "main.snd",
        "include \"a.snd\"\ninclude \"b.snd\"\ninclude \"c.snd\"\nfn main() -> i64 { 0 }\n",
    );

    let msg = first_error_line(&entry);
    let (file, line, col) =
        parse_error_location(&msg).unwrap_or_else(|| panic!("the message must carry a file:line:col, got: {msg}"));
    assert!(
        file.ends_with("/c.snd"),
        "the error must blame the file that was actually written, got: {file}"
    );
    assert_eq!(
        line, 2,
        "line {line} is a spliced offset; `if x == 1;` sits on line 2 of c.snd: {msg}"
    );
    assert_eq!(
        col as usize,
        csrc.lines().nth(1).unwrap().find(';').unwrap() + 1,
        "the column must be the `;` in c.snd's own line: {msg}"
    );
}

#[test]
fn an_error_on_the_last_line_of_a_file_without_final_newline_stays_inside_the_file() {
    // No trailing newline: an EOF-style complaint computed from the spliced buffer used to name a
    // line the author's file does not have. Whatever the message says, its line must exist.
    let dir = case_dir("no-trailing-newline");
    let body = "fn main() -> i64 {\nif a == 1";
    let entry = write(&dir, "prog.snd", body);

    let msg = first_error_line(&entry);
    let (file, line, column) =
        parse_error_location(&msg).unwrap_or_else(|| panic!("missing location: {msg}"));
    let total = body.lines().count() as u32;
    assert!(
        file.ends_with("/prog.snd"),
        "the error must blame the entry file, got: {file}"
    );
    assert!(
        line <= total,
        "the error names line {line}, but the file only has {total} lines: {msg}"
    );
    assert!(line >= 1, "line numbers are 1-based: {msg}");
    assert!(column >= 1, "columns are 1-based: {msg}");
}

#[test]
fn the_message_writes_tokens_the_way_a_person_writes_them() {
    // `` `{` `` and `` `;` ``, never `LBrace` or `Semicolon` -- lexer-internal names mean nothing
    // to whoever wrote the file.
    let dir = case_dir("human-tokens");
    let src = "fn main() -> i64 { if a == 1; }";
    let entry = write(&dir, "prog.snd", src);

    let msg = first_error_line(&entry);
    assert!(
        msg.contains("expected `{`, got `;`"),
        "tokens must be written as in the source, got: {msg}"
    );
    assert!(
        !msg.contains("LBrace") && !msg.contains("Semicolon"),
        "internal lexer names must not reach a person, got: {msg}"
    );

    // a `struct` field whose `:` is missing: the refusal must say `:`, where, and name the
    // offending token by the word it is made of.
    let dir2 = case_dir("human-tokens-colon");
    let src2 = "struct Point { x i64 }\nfn main() -> i64 { 0 }\n";
    let e2 = write(&dir2, "prog.snd", src2);
    let msg2 = first_error_line(&e2);
    assert!(
        msg2.contains("expected `:`"),
        "the missing colon must be named as `:`, got: {msg2}"
    );
    assert!(
        msg2.contains("got `i64`"),
        "the offending token is shown by its name, got: {msg2}"
    );
    assert!(
        msg2.contains("prog.snd:1:"),
        "the location must be the line the field was written on, got: {msg2}"
    );
    assert!(
        !msg2.contains("Colon"),
        "internal lexer names must not reach a person, got: {msg2}"
    );
}

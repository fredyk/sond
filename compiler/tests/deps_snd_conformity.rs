//! The two banks of the manifest, crossed: the Sond side writes it, the Rust side reads it.
//!
//! `lib/any/pkg/get.snd` WRITES `deps.snd` and `compiler/src/deps.rs` READS it. Each has tests of
//! its own, and both were green on 2026-09-01 while they spoke different languages: one wrote a
//! flat four-field line and the other expected a `use … = sond:sha256:…` block. A `sond get`
//! would have left behind a file the compiler refuses, and no test saw it because none of them
//! crossed the artefacts.
//!
//! This test freezes no literal -- one would go stale the moment either side moved. It makes the
//! Sond side WRITE, for real, and hands the result to the Rust parser. Move one bank without the
//! other and it shows up here.

use std::path::PathBuf;
use std::process::Command;

const HASH: &str = "923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d";
const SHA: &str = "1111222233334444555566667777888899990000";
const SRC: &str = "github.com/fredyk/sond-strings";

/// The repo root: this file lives in `compiler/tests/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ always has a parent")
        .to_path_buf()
}

#[test]
fn what_get_snd_writes_is_what_deps_rs_reads() {
    let root = repo_root();
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let deps = tmp.join(format!("deps-cross-{}.snd", std::process::id()));
    let _ = std::fs::remove_file(&deps);

    // The program is written INSIDE the tree and under a subdirectory: `include` resolves by
    // walking up to the project's `lib/`, so from /tmp it would find nothing, and from the root
    // itself there is nowhere left to walk up to.
    let source_dir = root.join("examples");
    let source = source_dir.join(format!("cross-deps-{}.snd", std::process::id()));
    let bin = tmp.join(format!("cross-deps-{}", std::process::id()));
    std::fs::write(
        &source,
        format!(
            r#"include "any/pkg/get.snd"

fn main() -> i64 {{
    let p: string = "{}"
    pkg_deps_path = p.utf8
    let src: string = "{SRC}"
    let sha: string = "{SHA}"
    let h: string = "{HASH}"
    let nm: string = "strings"
    let ok = pkg_deps_write(src.utf8, sha.utf8, h.utf8, nm.utf8)
    if ok == 1 {{ return 0 }}
    1
}}
"#,
            deps.display()
        ),
    )
    .expect("the crossing source must be writable");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&source)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc must be runnable");
    let _ = std::fs::remove_file(&source);
    assert!(
        out.status.success(),
        "the deps.snd writer must compile, and sondc said: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin).output().expect("the binary must run");
    assert!(
        run.status.success(),
        "pkg_deps_write failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(&bin);

    let written = std::fs::read_to_string(&deps).expect("get.snd must have written the file");
    let _ = std::fs::remove_file(&deps);

    // And now the other bank, over those exact bytes.
    let read = sondc::deps::parse_deps(&written).unwrap_or_else(|e| {
        panic!("deps.rs cannot read what get.snd writes.\nWritten:\n{written}\nError: {e}")
    });
    let e = read
        .entries
        .get("strings")
        .unwrap_or_else(|| panic!("the `strings` entry is missing.\nWritten:\n{written}"));
    assert_eq!(e.hash, HASH, "the hash does not survive the trip:\n{written}");
    assert_eq!(
        e.from.as_deref(),
        Some(format!("{SRC}@{SHA}").as_str()),
        "the origin does not survive the trip:\n{written}"
    );
    assert_eq!(e.grants, "{}", "the capability slot:\n{written}");
}

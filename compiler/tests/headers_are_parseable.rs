//! Every published header is read by the compiler that wrote it.
//!
//! `tools/install.sh` generates one header per `.snd` under `lib/` and publishes it in two
//! places: `dist/include/` (inside the repo, versioned) and `~/.local/include/sond/`, the FHS
//! path RFC-0006 names as the package interface. It is THE artefact whoever consumes the library
//! compiles against.
//!
//! On 2026-09-05, 132 of those 193 headers did not parse under `sondc`. All for the same reason:
//! a `pub fn` signature with no body, which the parser read as a definition missing its `{`.
//! Nothing saw it because the `header.rs` tests look for SUBSTRINGS in the text, and a text that
//! reads well can still be impossible to compile.
//!
//! No literal is frozen here: the real header is emitted, with the real emitter, and handed to
//! the real front end. Change one bank without the other and it shows up here.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The repo root: this file lives in `compiler/tests/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ always has a parent")
        .to_path_buf()
}

fn snd_files(dir: &Path, v: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            snd_files(&p, v);
        } else if p.extension().is_some_and(|x| x == "snd") {
            v.push(p);
        }
    }
}

#[test]
fn every_published_header_parses_under_the_compiler_that_wrote_it() {
    let root = repo_root();
    let lib = root.join("lib");
    let mut files = Vec::new();
    snd_files(&lib, &mut files);
    files.sort();
    assert!(
        files.len() > 100,
        "the whole library was expected under {}, and {} turned up",
        lib.display(),
        files.len()
    );

    // Emitted and read with THE binary, the same command `tools/install.sh` runs. Rebuilding the
    // pipeline inside this test would skip include resolution, which is part of what the header
    // ends up saying, and would go green over something install.sh does not produce.
    let sondc = env!("CARGO_BIN_EXE_sondc");
    let scratch = sondc::scratch::Scratch::new("sond_hdrs");
    let tmp = scratch.path();

    let mut broken: Vec<String> = Vec::new();
    for (n, f) in files.iter().enumerate() {
        let emitted = Command::new(sondc)
            .arg(f)
            .arg("--emit-header")
            .output()
            .expect("sondc starts");
        if !emitted.status.success() {
            broken.push(format!(
                "{}: its header could not be emitted: {}",
                f.display(),
                String::from_utf8_lossy(&emitted.stderr).trim()
            ));
            continue;
        }
        // The header is saved with its real extension: it is the file that gets published.
        let dst = tmp.join(format!("h{n}.snd"));
        std::fs::write(&dst, &emitted.stdout).expect("the header is written");
        // `--entry-info` parses the file and writes no binary.
        let read = Command::new(sondc)
            .arg("--entry-info")
            .arg(&dst)
            .output()
            .expect("sondc starts");
        if !read.status.success() {
            let e = String::from_utf8_lossy(&read.stderr);
            let e = e.trim().replace(&dst.display().to_string(), "<header>");
            broken.push(format!("{}: {e}", f.display()));
        }
    }
    assert!(
        broken.is_empty(),
        "{} of {} headers do not parse under the compiler that writes them. They are published in \
         dist/include/ and in ~/.local/include/sond/, which means they ship broken:\n{}",
        broken.len(),
        files.len(),
        broken.join("\n")
    );
}

// `tools/publish-file.sh SRC DST [MODE]`: replace a published file that something may be running.
//
// The installer used `install -m 755` straight over ~/.local/bin/sondc and ~/.local/bin/sond.
// `install` unlinks the destination and writes a fresh file at the same name, so a process
// already running the old one is fine -- it keeps its inode. What is NOT fine is the window in
// between: for the length of the write the path is missing, and then short. Measured at 24.330
// looks with something wrong at the path. A compile starting in that window execs a truncated
// compiler, and nothing anywhere says so.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn scratch(name: &str) -> sondc::scratch::Scratch {
    sondc::scratch::Scratch::new(&format!("sond_pubf_{name}"))
}

fn publish(src: &Path, dst: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(repo().join("tools/publish-file.sh"))
        .arg(src)
        .arg(dst)
        .output()
        .expect("tools/publish-file.sh runs")
}

#[test]
fn the_installed_path_never_names_a_half_written_file() {
    // This is the one that separates the two mechanisms. `install` unlinks the destination and
    // then writes the new bytes into a fresh file at the same name: between those two steps the
    // path is missing, and while the write runs it names a SHORT file. A compile that starts in
    // that window execs a truncated compiler. Measured here rather than argued: a watcher asks
    // the path for its size, over and over, while the publish runs.
    let base = scratch("halfwritten");
    let dst = base.join("sondc");
    let src = base.join("new");
    // Big enough that writing it is not instantaneous.
    let bytes = vec![b'x'; 48 * 1024 * 1024];
    std::fs::write(&src, &bytes).unwrap();
    std::fs::write(&dst, &bytes).unwrap();
    let full = bytes.len() as u64;

    let watched = dst.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bad = std::sync::Arc::new(AtomicUsize::new(0));
    let looks = std::sync::Arc::new(AtomicUsize::new(0));
    let watcher = {
        let (stop, bad, looks) = (stop.clone(), bad.clone(), looks.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                looks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match std::fs::metadata(&watched) {
                    Ok(m) if m.len() == full => {}
                    _ => {
                        bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        })
    };

    let out = publish(&src, &dst);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let looks = looks.load(std::sync::atomic::Ordering::Relaxed);
    assert!(looks > 100, "the watcher barely ran ({looks} looks): it measured nothing");
    assert_eq!(
        bad.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the installed path was missing or short during the publish, over {looks} looks"
    );
}

#[test]
fn a_published_binary_is_executable_and_is_the_file_that_was_asked_for() {
    let base = scratch("mode");
    let src = base.join("new.sh");
    std::fs::write(&src, "#!/bin/sh\nexit 7\n").unwrap();
    let dst = base.join("bin/thing");

    let out = publish(&src, &dst);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let code = Command::new(&dst).status().expect("the published file runs").code();
    assert_eq!(code, Some(7), "the published file is the one that was handed over, and runs");
}

#[test]
fn a_publish_with_no_source_leaves_the_installed_file_untouched() {
    let base = scratch("fail");
    let dst = base.join("sondc");
    std::fs::write(&dst, "the compiler everyone is calling\n").unwrap();

    let out = publish(&base.join("nonexistent"), &dst);

    assert!(!out.status.success(), "a publish with no source must fail");
    assert_eq!(
        std::fs::read_to_string(&dst).unwrap(),
        "the compiler everyone is calling\n"
    );
}

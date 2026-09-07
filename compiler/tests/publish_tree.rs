// `tools/publish-tree.sh SRC DST`: replace a published directory with a new one.
//
// The installer used to do this with `rm -rf "$DST"` followed by a copy, over three shared
// paths: ~/.local/lib/sond, ~/.local/include/sond and the repo's dist/include. Two things
// follow from that order and neither announces itself. Between the delete and the copy the
// path does not exist, so any compiler resolving `include "any/json.snd"` in that window
// fails with an error that blames the include. And if the copy stops halfway -- a full disk,
// a Ctrl-C, a killed session -- what is left published is a fragment, with nothing to say so.
//
// These tests pin the two properties a publish has to have: it is all-or-nothing, and the
// destination is readable throughout.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(name: &str) -> sondc::scratch::Scratch {
    sondc::scratch::Scratch::new(&format!("sond_pub_{name}"))
}

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn publish(src: &Path, dst: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(repo().join("tools/publish-tree.sh"))
        .arg(src)
        .arg(dst)
        .output()
        .expect("tools/publish-tree.sh runs")
}

#[test]
fn a_publish_that_cannot_start_leaves_the_destination_exactly_as_it_was() {
    let base = scratch("fail");
    let dst = base.join("published");
    write(&dst.join("any/json.snd"), "the tree everybody is compiling against\n");

    let out = publish(&base.join("nonexistent-source"), &dst);

    assert!(!out.status.success(), "a publish with no source must fail");
    assert_eq!(
        std::fs::read_to_string(dst.join("any/json.snd")).unwrap(),
        "the tree everybody is compiling against\n",
        "a failed publish must not have touched what was already published"
    );
}

#[test]
fn a_publish_mirrors_the_source_and_drops_what_is_no_longer_in_it() {
    let base = scratch("mirror");
    let src = base.join("lib");
    let dst = base.join("published");
    write(&src.join("any/json.snd"), "new\n");
    write(&src.join("any/strings.snd"), "also new\n");
    write(&dst.join("any/json.snd"), "old\n");
    write(&dst.join("any/retired.snd"), "a module that no longer exists\n");

    let out = publish(&src, &dst);
    assert!(
        out.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(std::fs::read_to_string(dst.join("any/json.snd")).unwrap(), "new\n");
    assert_eq!(std::fs::read_to_string(dst.join("any/strings.snd")).unwrap(), "also new\n");
    assert!(
        !dst.join("any/retired.snd").exists(),
        "the published tree is the source, not the source plus whatever was there before"
    );
}

#[test]
fn the_published_tree_stays_readable_while_it_is_being_replaced() {
    // The window the old order opened, measured rather than argued: a reader asking for one
    // file of the published tree, over and over, while the publish runs. With a delete
    // followed by a copy the file is missing for as long as the copy takes; with a swap it is
    // never missing, because the path points at a complete tree at every instant.
    let base = scratch("gap");
    let src = base.join("lib");
    let dst = base.join("published");
    // Enough files that a copy is not instantaneous, so a gap would be seen if there were one.
    for i in 0..500 {
        write(&src.join(format!("any/mod{i}.snd")), "fn f() -> i64 { 1 }\n");
    }
    // Publish once first: this measures the steady state, which is every publish after the
    // destination has become what the design says it is.
    let first = base.join("lib-first");
    write(&first.join("any/mod0.snd"), "the old one\n");
    assert!(publish(&first, &dst).status.success(), "the setup publish must succeed");

    let probe = dst.join("any/mod0.snd");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let misses = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let watcher = {
        let (stop, misses, reads) = (stop.clone(), misses.clone(), reads.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if std::fs::read(&probe).is_err() {
                    misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
    };

    let out = publish(&src, &dst);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();

    assert!(
        out.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reads = reads.load(std::sync::atomic::Ordering::Relaxed);
    assert!(reads > 100, "the watcher barely ran ({reads} reads): it measured nothing");
    assert_eq!(
        misses.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the published tree went missing during the publish, over {reads} reads"
    );
}

#[test]
fn the_new_tree_is_staged_beside_the_destination_not_in_the_system_temp_dir() {
    // `rename` is only atomic WITHIN one filesystem. Staged in /tmp -- which is tmpfs here, a
    // different filesystem from $HOME -- the final `mv` stops being a rename and becomes a copy,
    // and the swap that the whole design rests on quietly is not one any more. Nothing about the
    // output would change, so this is pinned by where the staging directory appears rather than
    // left to whoever edits the script next.
    let base = scratch("staging");
    let src = base.join("lib");
    let dst = base.join("nested/published");
    for i in 0..400 {
        write(&src.join(format!("any/mod{i}.snd")), "fn f() -> i64 { 1 }\n");
    }
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();

    let parent = dst.parent().unwrap().to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (stop, seen) = (stop.clone(), seen.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(rd) = std::fs::read_dir(&parent) {
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().contains(".tree.") {
                            seen.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
        })
    };

    let out = publish(&src, &dst);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();

    assert!(
        out.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        seen.load(std::sync::atomic::Ordering::Relaxed),
        "the new tree was never staged in the destination's own directory, so the final move \
         can cross filesystems and stop being atomic"
    );
}

#[test]
fn as_directory_leaves_a_real_directory_and_still_publishes_all_or_nothing() {
    // dist/include is generated, and git has 26 of its files committed anyway. A symlink there
    // would show up as a symlink replacing a tracked directory, so that destination asks for a
    // real directory. What it still needs is that a publish which cannot start changes nothing.
    let base = scratch("asdir");
    let src = base.join("gen");
    let dst = base.join("dist/include");
    write(&src.join("any/json.snd"), "generated header\n");
    write(&dst.join("any/old.snd"), "previous generation\n");

    let out = Command::new("bash")
        .arg(repo().join("tools/publish-tree.sh"))
        .arg("--as-directory")
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("publish-tree.sh runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert!(!dst.is_symlink(), "this destination has to stay a real directory");
    assert!(dst.is_dir());
    assert_eq!(std::fs::read_to_string(dst.join("any/json.snd")).unwrap(), "generated header\n");
    assert!(!dst.join("any/old.snd").exists(), "the tree is the source, not an accumulation");

    let failed = Command::new("bash")
        .arg(repo().join("tools/publish-tree.sh"))
        .arg("--as-directory")
        .arg(base.join("nonexistent"))
        .arg(&dst)
        .output()
        .expect("publish-tree.sh runs");
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read_to_string(dst.join("any/json.snd")).unwrap(),
        "generated header\n",
        "a publish that cannot start must leave the previous generation in place"
    );
}

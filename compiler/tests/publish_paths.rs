// A tree that other things read from -- ~/.local/lib/sond, ~/.local/include/sond, dist/include --
// gets replaced by the install script. Written as `rm -rf DST; cp -r SRC DST` the destination does
// not exist for the whole length of the copy, and a reader that walks in during it sees a fragment
// and cannot tell it from a finished tree. Measured on dist/include: the tree "went missing during
// the publish, over 4.725 reads". The same shape on a single file with `install -m` leaves the path
// "missing or short during the publish, over 24.330 looks".
//
// tools/publish-tree.sh and tools/publish-file.sh do the swap by rename, and this guard is what
// keeps a future script from going back to the open-coded version. The list of scripts is read off
// the directory, not written here, so a new one is covered the day it lands.
use std::path::PathBuf;

/// The variables a script fills with `mktemp -d`. A path under one of those is a staging area the
/// script just made and deletes on the way out: nobody else can hold it open, so replacing a file
/// there has no reader to protect. Read off the script instead of listed here, so the exemption
/// belongs to the shape of the code and not to a name someone remembered to add.
fn staging_vars(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (name, rest) = l.split_once('=')?;
            if !name.chars().all(|c| c.is_ascii_uppercase() || c == '_') || name.is_empty() {
                return None;
            }
            rest.contains("mktemp -d").then(|| name.to_string())
        })
        .collect()
}

/// Does this line write to a path the script did not just create for itself?
fn writes_outside_staging(line: &str, staging: &[String]) -> bool {
    let dst = match line.split_whitespace().last() {
        Some(d) => d.trim_matches('"'),
        None => return false,
    };
    !staging.iter().any(|v| dst.starts_with(&format!("${v}")) || dst.starts_with(&format!("${{{v}}}")))
}

fn tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("tools")
}

/// The shell scripts under tools/, minus the two that implement publishing itself.
fn scripts() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(tools())
        .expect("tools/ is there")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "sh"))
        .filter(|p| {
            !matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("publish-tree.sh") | Some("publish-file.sh")
            )
        })
        .collect();
    v.sort();
    assert!(v.len() > 3, "tools/ should hold several scripts, found {}", v.len());
    v
}

/// Does this script empty a published destination and then copy back into it? The pair is what
/// makes the gap: something is removed, and later something is copied back over the same kind of
/// destination. Either alone is fine.
fn empties_and_refills(body: &str) -> bool {
    let staging = staging_vars(body);
    let borra = body
        .lines()
        .any(|l| l.trim_start().starts_with("rm -rf") && writes_outside_staging(l, &staging));
    let copia = body.lines().any(|l| {
        let t = l.trim_start();
        (t.starts_with("cp -r") || t.starts_with("cp -R")) && writes_outside_staging(l, &staging)
    });
    borra && copia
}

/// Does this script write a file over a published path with `install`?
fn installs_over_a_published_path(body: &str) -> bool {
    let staging = staging_vars(body);
    body.lines()
        .any(|l| l.trim_start().starts_with("install -m") && writes_outside_staging(l, &staging))
}

#[test]
fn no_tool_replaces_a_published_tree_by_emptying_it_first() {
    let mut culprits = Vec::new();
    for s in scripts() {
        if empties_and_refills(&std::fs::read_to_string(&s).unwrap()) {
            culprits.push(s.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    assert!(
        culprits.is_empty(),
        "these scripts empty a destination and copy into it, so the path is missing for the whole \
         copy -- publish through tools/publish-tree.sh instead: {culprits:?}"
    );
}

#[test]
fn no_tool_installs_a_binary_over_a_path_something_may_be_reading() {
    let mut culprits = Vec::new();
    for s in scripts() {
        if installs_over_a_published_path(&std::fs::read_to_string(&s).unwrap()) {
            culprits.push(s.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    assert!(
        culprits.is_empty(),
        "`install` unlinks the destination and writes a new file in its place, so the path is \
         absent and then short while the copy runs -- publish through tools/publish-file.sh \
         instead: {culprits:?}"
    );
}

#[test]
fn the_guard_reads_code_and_not_the_comment_that_explains_the_code() {
    // A script that publishes correctly says WHY in a comment, and the words it has to say are
    // the words this guard hunts for. If the comment counted, passing would mean deleting the
    // explanation: the tree ends up fixed and mute.
    let comentado = r#"#!/usr/bin/env bash
# never do this: rm -rf "$HOME/.local/lib/sond"
#   cp -r "$REPO/lib/." "$HOME/.local/lib/sond/"
# and never `install -m 755 x $HOME/.local/bin/sondc` either.
"$REPO/tools/publish-tree.sh" "$REPO/lib" "$HOME/.local/lib/sond"
"#;
    assert!(!empties_and_refills(comentado), "a comment is not a publish");
    assert!(!installs_over_a_published_path(comentado), "a comment is not a publish");

    // The same lines uncommented are still caught: the guard has not been loosened into silence,
    // which is the easy way to make a false positive go away.
    let real = r#"#!/usr/bin/env bash
rm -rf "$HOME/.local/lib/sond"
cp -r "$REPO/lib/." "$HOME/.local/lib/sond/"
install -m 755 x "$HOME/.local/bin/sondc"
"#;
    assert!(empties_and_refills(real));
    assert!(installs_over_a_published_path(real));

    // And a staging area the script made for itself is not a publish either.
    let staging = r#"#!/usr/bin/env bash
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
install -m 755 "$REPO/target/release/sondc" "$STAGE/bin/sondc"
rm -rf "$STAGE/lib"
cp -r "$REPO/lib/." "$STAGE/lib/sond/"
"#;
    assert!(!empties_and_refills(staging), "nobody can be reading a fresh mktemp -d");
    assert!(!installs_over_a_published_path(staging), "nobody can be reading a fresh mktemp -d");
}

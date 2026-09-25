//! `tools/install.sh` publishes the binaries cargo JUST BUILT, wherever cargo put them.
//!
//! It used to copy them from `compiler/target/release/`, a path written into the script. On a
//! machine whose cargo builds elsewhere (`build.target-dir` in `~/.cargo/config.toml`, or
//! `CARGO_TARGET_DIR`), the build went to the other place and the script kept publishing
//! whatever was left in `compiler/target/`: `sond` and `sondc` stayed three weeks old while
//! every install reported success. The directory is asked of cargo, through
//! `tools/release-dir.sh`, the one place install.sh takes it from.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ always has a parent")
        .to_path_buf()
}

#[test]
fn the_release_dir_is_the_one_cargo_builds_into() {
    let root = repo_root();
    let elsewhere = sondc::scratch::Scratch::new("release_dir");
    let out = Command::new(root.join("tools/release-dir.sh"))
        .env("CARGO_TARGET_DIR", elsewhere.path())
        .output()
        .expect("tools/release-dir.sh runs");
    assert!(
        out.status.success(),
        "release-dir.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        elsewhere.path().join("release").display().to_string(),
        "with CARGO_TARGET_DIR set, cargo builds there, so that is where the binaries are"
    );
}

#[test]
fn install_takes_the_binaries_from_the_release_dir() {
    let script = std::fs::read_to_string(repo_root().join("tools/install.sh"))
        .expect("tools/install.sh is readable");
    assert!(
        !script.contains("compiler/target/release"),
        "install.sh must not name a target directory of its own: cargo decides where it builds"
    );
    assert!(
        script.contains("tools/release-dir.sh"),
        "install.sh must ask tools/release-dir.sh where the binaries are"
    );
}

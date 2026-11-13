//! Two Sond shared objects loaded into one process each run their OWN functions.
//!
//! A `.so` exports every `pub fn`, and every program that includes a library carries a copy of
//! that library's `pub fn`s. Linked as a plain `-shared` object, a call from one exported function
//! to another went through the PLT, and the dynamic linker bound it to the first object in the
//! process that exports the name. A host that loads two Sond libraries built from different
//! versions of the same included module then ran library B's calls on library A's copy, with A's
//! arena and outside whatever lock the host holds for B. The objects are linked `-Bsymbolic`, so a
//! call inside a library always reaches the definition in that same library.
use std::path::PathBuf;
use std::process::Command;

fn sondc_shared(src: &str, out: &PathBuf) {
    let snd = out.with_extension("snd");
    std::fs::write(&snd, src).expect("the source must be writable");
    let r = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&snd)
        .arg("--shared")
        .arg("-o")
        .arg(out)
        .output()
        .expect("sondc must be runnable");
    assert!(
        r.status.success(),
        "sondc --shared must build {}: {}",
        out.display(),
        String::from_utf8_lossy(&r.stderr)
    );
}

#[test]
fn a_call_inside_a_shared_object_reaches_that_objects_own_definition() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("bind-own-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // The same exported name in both libraries, with different bodies.
    sondc_shared(
        "pub fn which() -> i64 { 1 }\npub fn via_a() -> i64 { which() }\n",
        &dir.join("libsoa.so"),
    );
    sondc_shared(
        "pub fn which() -> i64 { 2 }\npub fn via_b() -> i64 { which() }\n",
        &dir.join("libsob.so"),
    );
    let c = dir.join("host.c");
    std::fs::write(
        &c,
        "long via_a(void);\nlong via_b(void);\nint main(void) { return (int)(via_a() * 10 + via_b()); }\n",
    )
    .unwrap();
    let host = dir.join("host");
    let r = Command::new("cc")
        .arg(&c)
        .arg("-o")
        .arg(&host)
        .arg(format!("-L{}", dir.display()))
        .args(["-lsoa", "-lsob"])
        .arg(format!("-Wl,-rpath,{}", dir.display()))
        .output()
        .expect("cc must be runnable");
    assert!(r.status.success(), "the host must link: {}", String::from_utf8_lossy(&r.stderr));

    let status = Command::new(&host).status().expect("the host must run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        status.code(),
        Some(12),
        "via_a must call libsoa's which (1) and via_b libsob's (2); 11 means libsob's call was bound to libsoa"
    );
}

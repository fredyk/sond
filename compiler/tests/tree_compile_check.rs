use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn target_for_path(rel: &str) -> Option<&'static str> {
    if rel.starts_with("examples/uefi/") {
        Some("--uefi")
    } else if rel.starts_with("examples/pic16/") || rel.starts_with("lib/pic/") {
        Some("--pic16")
    } else if rel.contains("/kernel/") || rel.starts_with("examples/freestanding/") {
        Some("--freestanding")
    } else if rel.ends_with("_test.snd") {
        Some("--test")
    } else {
        None
    }
}

const PATH_OVERRIDES: &[(&str, &str)] = &[
    ("lib/x86_64/drivers/net/virtio-net.snd", "--uefi"),
    ("lib/x86_64/net/dhcp.snd", "--uefi"),
    ("lib/x86_64/net/tcp-server.snd", "--uefi"),
    ("lib/x86_64/linux/capture.snd", "--uefi"),
    ("lib/x86_64/linux/capture_test.snd", ""),
    ("lib/x86_64/linux/kernel/6.18/init/main.snd", "--uefi"),
    ("lib/any/crypto/kdf/hkdf-sha256_test.snd", "--uefi"),
];

const KNOWN_EXCEPTIONS: &[(&str, &str)] = &[
    // `x25_A_LIMBS` sits on line 9 of selftest.snd as the developer sees it, and the error
    // below says 381. Both numbers are honest and only one of them is useful: the checker runs
    // after `include` resolution, so it counts lines in the merged source, and this file pulls
    // in the library ahead of itself.
    //
    // That is a trap, not a detail. A number in an error message reads as an instruction to go
    // look there, and line 381 does not exist in the file the developer has open. The fix is a
    // line map from the resolver -- merged line -> (file, line) -- so the message can name the
    // file it really means. Until that exists, this expectation pins the current behaviour so
    // nobody mistakes it for the intended one.
    ("examples/host/crypto/x25519-lib/selftest.snd", "variable `x25_A_LIMBS` is not declared in function `test_field_ops` at line 381"),
];

const KNOWN_MODULES: &[(&str, &str)] = &[
    ("lib/any/serial.snd", "no functions found"),
    ("lib/pic/16F/684/serial.snd", "no functions found"),
    ("lib/x86_64/linux/syscall-nums.snd", "no functions found"),
];

fn compile(repo: &Path, rel: &str) -> (i32, String) {
    let full = repo.join(rel);
    let target = PATH_OVERRIDES
        .iter()
        .find(|(r, _)| *r == rel)
        .map(|(_, t)| *t)
        .or_else(|| target_for_path(rel));
    // `CARGO_BIN_EXE_sondc` is the binary this very `cargo test` invocation just built. A plain
    // `Command::new("sondc")` would test whatever copy sits in `PATH`, which may be weeks old.
    // Named after the process: two concurrent runs (a second `cargo test` in another worktree,
    // an editor task) would otherwise write the same file and truncate each other's output,
    // failing a file that compiles perfectly well.
    let out_bin = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tree_compile_check-{}.bin", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sondc"));
    cmd.arg(&full);
    if let Some(t) = target { cmd.arg(t); }
    cmd.args(["-o", out_bin.to_str().unwrap()]);
    let output = cmd.output().unwrap();
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let first_line = stderr.lines().next().unwrap_or("").to_string();
    (code, first_line)
}

#[test]
fn tree_compile_check() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut snd_files: Vec<String> = Vec::new();
    for dir in &["lib", "examples"] {
        let root = repo.join(dir);
        if !root.exists() { continue; }
        for entry in walkdir(&root) {
            let ext = entry.extension().and_then(|e| e.to_str());
            if ext == Some("snd") {
                snd_files.push(entry.strip_prefix(&repo).unwrap().to_str().unwrap().to_string());
            }
        }
    }
    snd_files.sort();
    let exceptions_map: BTreeMap<&str, &str> = KNOWN_EXCEPTIONS.iter().copied().collect();
    let modules_map: BTreeMap<&str, &str> = KNOWN_MODULES.iter().copied().collect();
    let mut exceptions_matched: BTreeMap<&str, bool> = KNOWN_EXCEPTIONS.iter().map(|(r, _)| (*r, false)).collect();
    let mut modules_matched: BTreeMap<&str, bool> = KNOWN_MODULES.iter().map(|(r, _)| (*r, false)).collect();
    let mut ok_count = 0u32;
    let mut ko_count = 0u32;
    let mut mod_count = 0u32;
    let mut new_failures: Vec<String> = Vec::new();
    for rel in &snd_files {
        let (code, first_line) = compile(&repo, rel);
        if code == 0 {
            // Collected, not `panic!`ed: dying on the first one hides how many of the remaining
            // 282 files also changed verdict, and every rerun would only ever show one line.
            if let Some(expected_err) = exceptions_map.get(rel.as_str()) {
                new_failures.push(format!("KNOWN_EXCEPTION compiled: `{}` used to fail with `{}` but now compiles. Remove it from KNOWN_EXCEPTIONS.", rel, expected_err));
                *exceptions_matched.get_mut(rel.as_str()).unwrap() = true;
            } else if modules_map.contains_key(rel.as_str()) {
                new_failures.push(format!("KNOWN_MODULE compiled: `{}` used to emit 'no functions found' but now compiles. Remove it from KNOWN_MODULES.", rel));
                *modules_matched.get_mut(rel.as_str()).unwrap() = true;
            }
            ok_count += 1;
        } else if first_line.contains("no functions found") {
            mod_count += 1;
            if let Some(expected_err) = modules_map.get(rel.as_str()) {
                if first_line.contains(expected_err) { *modules_matched.get_mut(rel.as_str()).unwrap() = true; }
            }
        } else {
            if let Some(expected_err) = exceptions_map.get(rel.as_str()) {
                if first_line.contains(expected_err) {
                    *exceptions_matched.get_mut(rel.as_str()).unwrap() = true;
                } else {
                    new_failures.push(format!("KNOWN_EXCEPTION wrong error: `{}` expected `{}`, got `{}`", rel, expected_err, first_line));
                }
                ko_count += 1;
            } else {
                new_failures.push(format!("UNEXPECTED KO: `{}` :: {}", rel, first_line));
                ko_count += 1;
            }
        }
    }
    for (rel, _) in KNOWN_MODULES {
        if !modules_matched[rel] {
            new_failures.push(format!("KNOWN_MODULE error changed: `{}` no longer produces 'no functions found'", rel));
        }
    }
    for (rel, expected) in KNOWN_EXCEPTIONS {
        if !exceptions_matched[rel] {
            if !new_failures.iter().any(|m| m.contains(rel)) {
                new_failures.push(format!("KNOWN_EXCEPTION error changed: `{}` expected `{}` but error differs", rel, expected));
            }
        }
    }
    let total = ok_count + ko_count + mod_count;
    println!("\n=== tree_compile_check summary ===");
    println!("total files: {}", total);
    println!("  OK:  {}", ok_count);
    println!("  KO:  {}", ko_count);
    println!("  MOD: {}", mod_count);
    if !new_failures.is_empty() {
        eprintln!("\n--- FAILURES ---");
        for msg in &new_failures { eprintln!("{}", msg); }
        panic!("{} unexpected or changed result(s)", new_failures.len());
    }
    println!("\nAll files match expected verdicts.");
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() { stack.push(path); } else { results.push(path); }
            }
        }
    }
    results
}

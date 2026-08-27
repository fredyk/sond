// cve_cases.rs: verify that the CVE case studies in tests/cve/ behave as expected.
//
// Four vuln.snd files must be rejected with a specific error substring.
// All six fixed.snd files must compile to native x86-64.
// Two cases compile by design (known compiler limitations).  If one of
// these starts rejecting the test fires red so the author can remove it
// from the KNOWN_COMPILES list.
//
// Pattern follows tree_compile_check.rs: compile-time const arrays, per-entry
// tracking, end-of-test assertions that all entries were matched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn compile(repo: &Path, rel: &str) -> (i32, String) {
    let full = repo.join(rel);
    let out_bin = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cve_cases-{}.bin", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sondc"));
    cmd.arg(&full);
    cmd.args(["-o", out_bin.to_str().unwrap()]);
    let output = cmd.output().unwrap();
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let first_line = stderr.lines().next().unwrap_or("").to_string();
    (code, first_line)
}

// Cases that must be rejected; value is the substring that must appear in
// the compiler's first error line.
const CVE_REJECT: &[(&str, &str)] = &[
    (
        "tests/cve/heartbleed/vuln.snd",
        "cannot prove `0` is in bounds for slice `record`",
    ),
    (
        "tests/cve/signed-length/vuln.snd",
        "cannot prove `sig_len - 1` is in bounds for slice `sig`",
    ),
    (
        "tests/cve/untrusted-index/vuln.snd",
        "cannot prove `stream_id` is in bounds for slice `strmout`",
    ),
    (
        "tests/cve/divzero-input/vuln.snd",
        "cannot prove the divisor `__call1` is nonzero",
    ),
];

// Cases that compile today by design: known limitations in the compiler.
// If one of these starts rejecting, the test fires red so the author can
// remove it from this list.
const KNOWN_COMPILES: &[(&str, &str)] = &[
    (
        "tests/cve/offbyone-loop/vuln.snd",
        "uses peek8/poke8 on raw pointers, not Slice indexing",
    ),
    (
        "tests/cve/intoverflow-check/vuln.snd",
        "uses peek8/poke8 on raw pointers; overflow guard unchecked",
    ),
];

#[test]
fn cve_compile_check() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    assert!(
        repo.ends_with("the-ai-language.oc-los-casos-cve-no-los-ejecuta-nadie"),
        "repo path '{}' does not look like the repository root",
        repo.display()
    );

    let cve_map: BTreeMap<&str, &str> = CVE_REJECT.iter().copied().collect();
    let compiles_map: BTreeMap<&str, &str> = KNOWN_COMPILES.iter().copied().collect();

    let mut compiles_matched: BTreeMap<&str, bool> = KNOWN_COMPILES
        .iter()
        .map(|(r, _)| (*r, false))
        .collect();
    let mut cve_matched: BTreeMap<&str, bool> = CVE_REJECT.iter().map(|(r, _)| (*r, false)).collect();

    // Collect every file to compile (vuln + fixed) in deterministic order.
    let mut to_compile: Vec<(String, bool /* is_vuln */)> = Vec::new();
    for (rel, _) in CVE_REJECT {
        to_compile.push((rel.to_string(), true));
    }
    for (rel, _) in KNOWN_COMPILES {
        to_compile.push((rel.to_string(), true));
    }
    for (rel, _) in CVE_REJECT {
        let dir = Path::new(rel).parent().unwrap();
        let fixed = dir.join("fixed.snd").to_str().unwrap().to_string();
        to_compile.push((fixed, false));
    }
    for (rel, _) in KNOWN_COMPILES {
        let dir = Path::new(rel).parent().unwrap();
        let fixed = dir.join("fixed.snd").to_str().unwrap().to_string();
        to_compile.push((fixed, false));
    }
    to_compile.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel, is_vuln) in &to_compile {
        let (code, first_line) = compile(&repo, rel.as_str());

        if code == 0 {
            // === COMPILED ===
            if *is_vuln {
                // A vuln.snd compiled — check KNOWN_COMPILES.
                if let Some(_reason) = compiles_map.get(rel.as_str()) {
                    *compiles_matched.get_mut(rel.as_str()).unwrap() = true;
                } else {
                    // Unexpected compilation — not expected anywhere.
                    panic!(
                        "UNEXPECTED_OK: `{}` compiled but has no KNOWN_COMPILES entry.",
                        rel
                    );
                }
            }
            // fixed.snd compiling is the expected success path.
        } else {
            // === REJECTED ===
            if *is_vuln {
                // Is it a KNOWN_COMPILES entry that now rejects?
                if let Some(reason) = compiles_map.get(rel.as_str()) {
                    // Known fix: the compiler now rejects what used to
                    // compile.  Fire red so the author can remove it.
                    panic!(
                        "KNOWN_COMPILES no longer compiles: `{}` used to compile but now rejects with: '{}'. Remove it from KNOWN_COMPILES.",
                        rel, reason
                    );
                } else if let Some(expected_substr) = cve_map.get(rel.as_str()) {
                    // Expected CVE reject: verify the error message matches.
                    if first_line.contains(expected_substr) {
                        *cve_matched.get_mut(rel.as_str()).unwrap() = true;
                    } else {
                        panic!(
                            "CVE error mismatch: `{}` expected error containing '{}' but got '{}'",
                            rel, expected_substr, first_line
                        );
                    }
                } else {
                    panic!("UNEXPECTED_KO: `{}` :: {}", rel, first_line);
                }
            }
            // fixed.snd that rejects is not expected.
            if !*is_vuln {
                panic!("fixed.snd unexpectedly failed: `{}` :: {}", rel, first_line);
            }
        }
    }

    // Every KNOWN_COMPILES must have been encountered and compiled.
    for (rel, _) in KNOWN_COMPILES {
        assert!(
            compiles_matched[rel],
            "KNOWN_COMPILES '{}' no longer compiles (reason changed)",
            rel
        );
    }

    // Every CVE reject must have been encountered and matched.
    for &(rel, expected) in CVE_REJECT {
        assert!(
            cve_matched[rel],
            "CVE reject '{}' expected '{}' but error changed",
            rel, expected
        );
    }
}
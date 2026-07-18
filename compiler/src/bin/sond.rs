//! `sond`, the Sond toolchain driver (in the spirit of `go` / `cargo`).
//!
//! It orchestrates the `sondc` compiler:
//!   sond test  <file.snd>   build the file's `test "..." { .. }` blocks and run them
//!   sond build <file.snd>   compile the file to a native binary
//!   sond run   <file.snd>   compile the file and run it
//!
//! `sond test` compiles with `sondc --test` (which synthesizes a harness that runs each
//! test) to a temporary binary, runs it, and propagates its exit code, nonzero if any
//! test failed.

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

/// Locate `sondc`: prefer one sitting next to this `sond` binary, else fall back to `PATH`.
fn find_sondc() -> PathBuf {
    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sib = dir.join("sondc");
        if sib.exists() {
            return sib;
        }
    }
    PathBuf::from("sondc")
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: sond <command> [args]\n\n\
         commands:\n  \
         test  <file.snd>   build the file's test blocks and run them\n  \
         build <file.snd>   compile the file to a native binary\n  \
         run   <file.snd>   compile the file and run it"
    );
    ExitCode::from(2)
}

/// Runs a compiled binary and returns its exit code as this process's code.
fn run_binary(path: &PathBuf) -> ExitCode {
    match Command::new(path).status() {
        Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("sond: cannot run `{}`: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// Compiles `file` via `sondc` with the given extra flags to `out`. Returns true on success.
fn compile(sondc: &PathBuf, file: &str, out: &PathBuf, extra: &[&str]) -> bool {
    let mut cmd = Command::new(sondc);
    // Hide sondc's "wrote native binary" note (stdout); keep its stderr for real errors.
    cmd.args(extra)
        .arg(file)
        .arg("-o")
        .arg(out)
        .stdout(Stdio::null());
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(_) => false,
        Err(e) => {
            eprintln!("sond: cannot run sondc: {e}");
            false
        }
    }
}

fn temp_out(file: &str, suffix: &str) -> PathBuf {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sond");
    env::temp_dir().join(format!("{stem}{suffix}"))
}

/// The coverage threshold from `--min-coverage=N` (default 80).
fn min_coverage(args: &[String]) -> u32 {
    args.iter()
        .find_map(|a| a.strip_prefix("--min-coverage="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(80)
}

/// Mandatory gate: compile the file's `test` blocks with coverage instrumentation and run
/// them. Returns true iff every test passed AND coverage met the threshold. The run's output
/// (per-test results + the coverage line) goes to the user. This is what makes tests a
/// compilation requirement, no artifact is produced unless it holds.
fn run_gated_tests(sondc: &PathBuf, file: &str, min_cov: u32) -> bool {
    let testbin = temp_out(file, ".gatebin");
    let min = format!("--min-coverage={min_cov}");
    let flags = ["--test", "--coverage", min.as_str()];
    if !compile(sondc, file, &testbin, &flags) {
        return false;
    }
    let ok = Command::new(&testbin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&testbin);
    ok
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return usage();
    }
    let sondc = find_sondc();
    match args[1].as_str() {
        "test" => {
            // sond test [--coverage] [--min-coverage=N] <file.snd>, forward the flags to
            // sondc --test; the file is the first non-flag argument.
            // sond test <lib.snd>, run a file's tests with the mandatory coverage gate.
            // This is for LIBRARIES (no `main` to build into an executable); the same gate
            // runs inside `sond build` for programs. Coverage is always measured and enforced.
            let rest = &args[2..];
            let Some(file) = rest.iter().find(|a| !a.starts_with('-')) else {
                eprintln!("sond test: missing <file.snd>");
                return ExitCode::from(2);
            };
            if run_gated_tests(&sondc, file, min_coverage(&args)) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "build" => {
            // sond build <program.snd> [out], MANDATORY tests: the program's `test` blocks
            // must pass with >= the coverage threshold, or no binary is produced. Only then is
            // the real (uninstrumented) binary emitted. This is Sond's rule that you cannot
            // ship code that isn't tested.
            let rest = &args[2..];
            let mut positional = rest.iter().filter(|a| !a.starts_with('-'));
            let Some(file) = positional.next() else {
                eprintln!("sond build: missing <file.snd>");
                return ExitCode::from(2);
            };
            if !run_gated_tests(&sondc, file, min_coverage(&args)) {
                eprintln!(
                    "sond: build blocked, the program's tests must pass with at least {}% coverage \
                     before it can be built",
                    min_coverage(&args)
                );
                return ExitCode::FAILURE;
            }
            let out = positional
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::path::Path::new(file).file_stem().unwrap()));
            if compile(&sondc, file, &out, &[]) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "run" => {
            // sond run <program.snd>, same mandatory gate as build, then run the program.
            let rest = &args[2..];
            let Some(file) = rest.iter().find(|a| !a.starts_with('-')) else {
                eprintln!("sond run: missing <file.snd>");
                return ExitCode::from(2);
            };
            if !run_gated_tests(&sondc, file, min_coverage(&args)) {
                eprintln!(
                    "sond: run blocked, the program's tests must pass with at least {}% coverage",
                    min_coverage(&args)
                );
                return ExitCode::FAILURE;
            }
            let out = temp_out(file, ".runbin");
            if !compile(&sondc, file, &out, &[]) {
                return ExitCode::FAILURE;
            }
            let code = run_binary(&out);
            let _ = std::fs::remove_file(&out);
            code
        }
        other => {
            eprintln!("sond: unknown command `{other}`");
            usage()
        }
    }
}

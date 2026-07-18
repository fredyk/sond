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

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return usage();
    }
    let sondc = find_sondc();
    match args[1].as_str() {
        "test" => {
            let Some(file) = args.get(2) else {
                eprintln!("sond test: missing <file.snd>");
                return ExitCode::from(2);
            };
            let out = temp_out(file, ".testbin");
            if !compile(&sondc, file, &out, &["--test"]) {
                return ExitCode::FAILURE;
            }
            let code = run_binary(&out);
            let _ = std::fs::remove_file(&out);
            code
        }
        "build" => {
            let Some(file) = args.get(2) else {
                eprintln!("sond build: missing <file.snd>");
                return ExitCode::from(2);
            };
            let out = args
                .get(3)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::path::Path::new(file).file_stem().unwrap()));
            if compile(&sondc, file, &out, &[]) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        "run" => {
            let Some(file) = args.get(2) else {
                eprintln!("sond run: missing <file.snd>");
                return ExitCode::from(2);
            };
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

//! `sond`: the Sond toolchain driver (in the spirit of `go` / `cargo`).
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

/// A scratch directory of this run's own, removed when it goes out of scope.
///
/// The compiled test harness cannot be named after the source file. `mod.snd` is `mod.snd` in
/// every directory on the machine, and `sond` runs many at a time -- a fleet of agents, `make
/// -j`, two terminals. Sharing one path means one run executes the binary another just wrote,
/// or deletes it out from under a run that is still going. Measured on twenty concurrent pairs
/// of a passing and a failing file: the failing one reported success four times.
///
/// The name is unpredictable and the directory is created, never opened: `create_dir` fails if
/// the name is taken, so exclusivity comes from the kernel and not from hoping the number is
/// unused. 0700 keeps another user on the machine from putting a binary where this one is about
/// to run.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn nuevo() -> Option<Scratch> {
        use std::os::unix::fs::PermissionsExt;
        let base = env::temp_dir();
        for intento in 0..64u32 {
            let n = azar(intento);
            let dir = base.join(format!(".sond-{}-{n:016x}", std::process::id()));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                    return Some(Scratch { dir });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    eprintln!("sond: cannot create a scratch directory in {}: {e}", base.display());
                    return None;
                }
            }
        }
        eprintln!("sond: cannot create a scratch directory in {}", base.display());
        None
    }

    fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 64 unpredictable bits, from the kernel when it will give them and from the clock when it will
/// not. Only the name depends on this; whether the directory is ours is decided by `create_dir`.
fn azar(intento: u32) -> u64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom")
        && f.read_exact(&mut b).is_ok()
    {
        return u64::from_ne_bytes(b);
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    t.wrapping_mul(6364136223846793005).wrapping_add(intento as u64)
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
    let Some(scratch) = Scratch::nuevo() else {
        return false;
    };
    let testbin = scratch.join("gate");
    let min = format!("--min-coverage={min_cov}");
    let flags = ["--test", "--coverage", min.as_str()];
    if !compile(sondc, file, &testbin, &flags) {
        return false;
    }
    Command::new(&testbin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
                    "sond: build blocked: the program's tests must pass with at least {}% coverage \
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
                    "sond: run blocked: the program's tests must pass with at least {}% coverage",
                    min_coverage(&args)
                );
                return ExitCode::FAILURE;
            }
            let Some(scratch) = Scratch::nuevo() else {
                return ExitCode::FAILURE;
            };
            let out = scratch.join("run");
            if !compile(&sondc, file, &out, &[]) {
                return ExitCode::FAILURE;
            }
            run_binary(&out)
        }
        other => {
            eprintln!("sond: unknown command `{other}`");
            usage()
        }
    }
}

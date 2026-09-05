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
use std::path::{Path, PathBuf};
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
         test  <dir>        run every gate in the tree, and name what no gate reads\n  \
         \x20                 (--timeout=N seconds per gate, default 600)\n  \
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

unsafe extern "C" {
    /// `kill(2)`. A negative `pid` signals every process in that process group.
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Spawns `cmd` as the leader of a process group of its own, so the whole tree it grows can be
/// signalled later with one call.
///
/// Nothing that `sond` starts is ever a single process. The test harness forks a child per test
/// so a crashing test cannot take the run down with it, which puts the code that actually spins
/// one level below whatever `Child::kill` can reach. Without a group of its own, killing a hung
/// gate kills the bookkeeping and leaves the loop running.
fn lanzar(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    // ETXTBSY is not a failure, it is a race: another process forked while this binary was still
    // open for writing, and `exec` refuses until that descriptor is gone. It shows up exactly
    // when the machine is busy, which is when a run can least afford to be filed as broken.
    for _ in 0..50 {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            otro => return otro,
        }
    }
    cmd.spawn()
}

/// SIGKILL to a process group, by the group leader's pid.
fn matar_grupo(lider: u32) {
    // SAFETY: kill(2) with a negative pid and SIGKILL; the pid is one this process spawned and
    // has not reaped, so it cannot have been reused by an unrelated process.
    unsafe {
        kill(-(lider as i32), 9);
    }
}

/// Waits for `hijo` up to `plazo`, killing it if it runs past. `None` means it was killed.
///
/// A gate that never finishes does not fail: it holds the run with nothing on screen, which reads
/// exactly like a slow machine until somebody gives up. So the wait has a deadline HERE, in the
/// tool, rather than in whatever the caller happened to wrap the command in.
///
/// What gets killed is the process GROUP, not the process. A grandchild that survives keeps a
/// core at 100% for as long as the machine is up, and -- because it inherited the run's stdout --
/// it holds that pipe open: the same run finished in 20s writing to a file and had not returned
/// after 200s through a pipe. `hijo` must therefore come from `lanzar`.
fn esperar(mut hijo: std::process::Child, plazo: std::time::Duration) -> Option<bool> {
    let inicio = std::time::Instant::now();
    loop {
        match hijo.try_wait() {
            Ok(Some(estado)) => return Some(estado.success()),
            Ok(None) => {
                if inicio.elapsed() >= plazo {
                    matar_grupo(hijo.id());
                    let _ = hijo.kill();
                    let _ = hijo.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return Some(false),
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

/// Clears the scratch of runs that are no longer alive: the processes first, then the directory.
///
/// `Drop` does not run on SIGKILL, and Ctrl-C at the terminal reaches `sond` but not a gate that
/// now lives in a process group of its own. Whoever makes the mess cannot be the only one who
/// clears it, so every run starts by reconciling: if the pid in a scratch name is not a live
/// `sond`, that run is over and whatever is still executing out of its directory is an orphan
/// holding a core.
///
/// A pid that IS a live `sond` is left strictly alone. Several run at once here -- a fleet of
/// agents, `make -j`, two terminals -- and a sweep that cannot tell them apart deletes the binary
/// the run next door is about to execute.
fn barrer_restos() {
    let base = env::temp_dir();
    let Ok(entradas) = std::fs::read_dir(&base) else { return };
    let mut muertos: Vec<PathBuf> = Vec::new();
    for e in entradas.flatten() {
        let nombre = e.file_name().to_string_lossy().into_owned();
        let Some(resto) = nombre.strip_prefix(".sond-") else { continue };
        let Some((pid, _)) = resto.split_once('-') else { continue };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        if !es_un_sond_vivo(pid) {
            muertos.push(e.path());
        }
    }
    if muertos.is_empty() {
        return;
    }
    matar_lo_que_corre_desde(&muertos);
    for d in &muertos {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Is `pid` a running `sond`? A pid alone is not enough: pids are reused, and a scratch whose
/// number now belongs to something else is an orphan just the same.
fn es_un_sond_vivo(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        Ok(c) => c.trim_end() == "sond",
        Err(_) => false,
    }
}

/// SIGKILL to every process whose executable lives under one of `dirs`.
fn matar_lo_que_corre_desde(dirs: &[PathBuf]) {
    let Ok(procs) = std::fs::read_dir("/proc") else { return };
    for e in procs.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else { continue };
        if dirs.iter().any(|d| exe.starts_with(d)) {
            // SAFETY: kill(2) on a pid read from /proc; the worst a reused pid can do here is
            // outlive the check, and the executable path is the one this tool wrote.
            unsafe {
                kill(pid as i32, 9);
            }
        }
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

/// The per-gate deadline from `--timeout=N` seconds (default 600).
fn plazo(args: &[String]) -> std::time::Duration {
    std::time::Duration::from_secs(
        args.iter()
            .find_map(|a| a.strip_prefix("--timeout="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(600),
    )
}

/// The coverage threshold from `--min-coverage=N` (default 80).
fn min_coverage(args: &[String]) -> u32 {
    args.iter()
        .find_map(|a| a.strip_prefix("--min-coverage="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(80)
}

/// What a gate run came back with.
///
/// `Fallo` and `NoArranca` both end in "no verdict", and reporting them the same way is how a
/// machine with no compiler on it produced a report saying the tests were slow. They are
/// different facts and the run says which one it got.
enum Veredicto {
    Paso,
    Fallo,
    Colgada,
    NoArranca(String),
}

/// Mandatory gate: compile the file's `test` blocks with coverage instrumentation and run
/// them. Returns true iff every test passed AND coverage met the threshold. The run's output
/// (per-test results + the coverage line) goes to the user. This is what makes tests a
/// compilation requirement, no artifact is produced unless it holds.
fn run_gated_tests(sondc: &PathBuf, file: &str, min_cov: u32) -> bool {
    match run_gated_tests_deadline(sondc, file, min_cov, None) {
        Veredicto::Paso => true,
        Veredicto::NoArranca(e) => {
            eprintln!("sond: {e}");
            false
        }
        _ => false,
    }
}

/// The gate, with an optional deadline covering the compile AND the run.
///
/// `Colgada` means it was killed for running past the deadline, which is a different answer from
/// `Fallo` and has to be reported as its own thing: "did not finish" is what a hung test looks
/// like, and calling it a failure hides which one it was.
fn run_gated_tests_deadline(
    sondc: &PathBuf,
    file: &str,
    min_cov: u32,
    plazo: Option<std::time::Duration>,
) -> Veredicto {
    let Some(scratch) = Scratch::nuevo() else {
        return Veredicto::NoArranca("cannot create a scratch directory".to_string());
    };
    let testbin = scratch.join("gate");
    let min = format!("--min-coverage={min_cov}");
    let flags = ["--test", "--coverage", min.as_str()];
    match plazo {
        None => {
            if !compile(sondc, file, &testbin, &flags) {
                return Veredicto::Fallo;
            }
            match Command::new(&testbin).status() {
                Ok(s) if s.success() => Veredicto::Paso,
                Ok(_) => Veredicto::Fallo,
                Err(e) => Veredicto::NoArranca(format!("cannot run the gate for {file}: {e}")),
            }
        }
        Some(p) => {
            let inicio = std::time::Instant::now();
            let hijo = match lanzar(
                Command::new(sondc)
                    .args(flags)
                    .arg(file)
                    .arg("-o")
                    .arg(&testbin)
                    .stdout(Stdio::null()),
            ) {
                Ok(h) => h,
                Err(e) => {
                    return Veredicto::NoArranca(format!("cannot run `{}`: {e}", sondc.display()));
                }
            };
            match esperar(hijo, p) {
                None => return Veredicto::Colgada,
                Some(false) => return Veredicto::Fallo,
                Some(true) => {}
            }
            let queda = p.saturating_sub(inicio.elapsed());
            let hijo = match lanzar(&mut Command::new(&testbin)) {
                Ok(h) => h,
                Err(e) => {
                    return Veredicto::NoArranca(format!("cannot run the gate for {file}: {e}"));
                }
            };
            match esperar(hijo, queda) {
                None => Veredicto::Colgada,
                Some(true) => Veredicto::Paso,
                Some(false) => Veredicto::Fallo,
            }
        }
    }
}

/// What `sondc --entry-info` says about one file: how many `test` blocks it declares, and which
/// files its coverage gate answers for.
struct EntryInfo {
    tests: usize,
    covers: Vec<PathBuf>,
    /// Functions of its own a gate could measure. Zero means no gate can exist for this file --
    /// a header of constants, or the test harness, whose every function the coverage build skips
    /// -- so it is not a hole in the tree, and asking for a `_test.snd` beside it asks for a gate
    /// that would measure nothing.
    measurable: usize,
}

/// `Err` is the compiler not running at all -- a different fact from a file that does not
/// resolve, and one that makes every answer after it meaningless. `Ok(None)` is the file.
fn entry_info(sondc: &PathBuf, file: &Path) -> Result<Option<EntryInfo>, String> {
    let out = match Command::new(sondc).arg("--entry-info").arg(file).output() {
        Ok(o) => o,
        Err(e) => return Err(format!("cannot run `{}`: {e}", sondc.display())),
    };
    if !out.status.success() {
        // A file that does not even resolve is not silently skipped: it is not measured either.
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return Ok(None);
    }
    let texto = String::from_utf8_lossy(&out.stdout);
    Ok(Some(EntryInfo {
        tests: texto
            .lines()
            .find_map(|l| l.strip_prefix("tests: "))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
        covers: texto
            .lines()
            .filter_map(|l| l.strip_prefix("covers: "))
            .map(PathBuf::from)
            .collect(),
        measurable: texto
            .lines()
            .find_map(|l| l.strip_prefix("measurable: "))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    }))
}

/// Every `.snd` under `dir`, sorted, so a run reports the same order twice.
fn snd_files(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut pila = vec![dir.to_path_buf()];
    while let Some(d) = pila.pop() {
        let Ok(entradas) = std::fs::read_dir(&d) else { continue };
        for e in entradas.flatten() {
            let p = e.path();
            let nombre = e.file_name();
            let nombre = nombre.to_string_lossy();
            if nombre.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                pila.push(p);
            } else if p.extension().is_some_and(|x| x == "snd") {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

/// `sond test <dir>`: run every gate in the tree, then say what no gate reads.
///
/// A single file passing its own gate says nothing about the tree it lives in. A module that no
/// entry includes and that has no test file of its own is in NOBODY's denominator: it ships
/// unread, and every command in this toolchain reports success, because nobody ever pointed one
/// at it. Measured across the Sond projects on this machine, 58 files of 174 were in that state.
///
/// Which files an entry answers for is not decided here: it is asked of `sondc --entry-info`,
/// which reports the same scope the gate itself runs on.
fn test_tree(sondc: &PathBuf, dir: &Path, min_cov: u32, plazo: std::time::Duration) -> ExitCode {
    let ficheros = snd_files(dir);
    if ficheros.is_empty() {
        eprintln!("sond test: no .snd files under `{}`", dir.display());
        return ExitCode::from(2);
    }

    let mut medidos: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut entradas: Vec<PathBuf> = Vec::new();
    let mut ilegibles: Vec<PathBuf> = Vec::new();
    for f in &ficheros {
        match entry_info(sondc, f) {
            Ok(Some(info)) if info.tests > 0 => {
                entradas.push(f.clone());
                for c in info.covers {
                    medidos.insert(std::fs::canonicalize(&c).unwrap_or(c));
                }
            }
            // Nothing here a gate could read, so there is no gate to ask for.
            Ok(Some(info)) if info.measurable == 0 => {
                medidos.insert(std::fs::canonicalize(f).unwrap_or_else(|_| f.clone()));
            }
            Ok(Some(_)) => {}
            Ok(None) => ilegibles.push(f.clone()),
            // With no compiler there is nothing to say about the tree: reporting every file as
            // unmeasured would be a diagnosis of the code for a fault of the machine.
            Err(e) => {
                eprintln!("sond: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let mut fallos: Vec<PathBuf> = Vec::new();
    let mut colgadas: Vec<PathBuf> = Vec::new();
    for e in &entradas {
        println!("== {}", e.display());
        match run_gated_tests_deadline(sondc, &e.to_string_lossy(), min_cov, Some(plazo)) {
            Veredicto::Paso => {}
            Veredicto::Fallo => fallos.push(e.clone()),
            Veredicto::Colgada => {
                println!("   did not finish within {}s", plazo.as_secs());
                colgadas.push(e.clone());
            }
            Veredicto::NoArranca(msg) => {
                eprintln!("sond: {msg}");
                return ExitCode::FAILURE;
            }
        }
    }

    // A file is unmeasured unless some entry answers for it. An entry itself is a gate, not a
    // subject: it is measured by definition.
    let canon: std::collections::HashSet<PathBuf> = entradas
        .iter()
        .map(|e| std::fs::canonicalize(e).unwrap_or_else(|_| e.clone()))
        .collect();
    let sin_puerta: Vec<&PathBuf> = ficheros
        .iter()
        .filter(|f| {
            let c = std::fs::canonicalize(f).unwrap_or_else(|_| (*f).clone());
            !canon.contains(&c) && !medidos.contains(&c)
        })
        .collect();

    println!();
    println!(
        "{} file(s), {} gate(s), {} failed, {} hung, {} unmeasured",
        ficheros.len(),
        entradas.len(),
        fallos.len(),
        colgadas.len(),
        sin_puerta.len()
    );
    for f in &fallos {
        println!("  FAILED   {}", f.display());
    }
    for f in &colgadas {
        println!("  HUNG     {}", f.display());
    }
    for f in &sin_puerta {
        println!("  NO GATE  {}", f.display());
    }
    for f in &ilegibles {
        println!("  UNREAD   {}", f.display());
    }
    if !sin_puerta.is_empty() {
        eprintln!(
            "sond: {} file(s) above are in no entry's coverage denominator: nothing measures \
             them. Give each one a `<name>_test.snd` beside it, or include it from an entry that \
             answers for it.",
            sin_puerta.len()
        );
    }
    if fallos.is_empty() && colgadas.is_empty() && sin_puerta.is_empty() && ilegibles.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return usage();
    }
    barrer_restos();
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
            let ruta = std::path::Path::new(file);
            if ruta.is_dir() {
                return test_tree(&sondc, ruta, min_coverage(&args), plazo(&args));
            }
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

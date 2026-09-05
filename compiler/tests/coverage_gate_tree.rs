//! Which files does a coverage gate actually read, and which files does no gate read at all.
//!
//! Sond's promise is that code cannot be shipped untested. A module that no entry measures slips
//! out from under that promise without anything going red: `sond test` passes on every file it is
//! pointed at, and nobody points it at the module. Measured across the Sond projects on this
//! machine, 58 of 174 files were in nobody's denominator.
//!
//! So the toolchain has to be able to answer both halves: what an entry answers for
//! (`sondc --cov-files`), and what a tree leaves unclaimed (`sond test <dir>`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(nombre: &str) -> sondc::scratch::Temporal {
    sondc::scratch::Temporal::nuevo(&format!("sond-cov-{nombre}"))
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// `sondc --entry-info <file>` as the tools see it: how many tests the file declares, and which
/// files its gate answers for.
fn entry_info(file: &Path) -> (usize, Vec<String>) {
    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg("--entry-info")
        .arg(file)
        .output()
        .expect("sondc runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let texto = String::from_utf8_lossy(&out.stdout).into_owned();
    let tests = texto
        .lines()
        .find_map(|l| l.strip_prefix("tests: "))
        .and_then(|n| n.parse().ok())
        .expect("the count is always printed");
    let cubre = texto
        .lines()
        .filter_map(|l| l.strip_prefix("covers: "))
        .filter_map(|l| l.rsplit('/').next())
        .map(str::to_string)
        .collect();
    (tests, cubre)
}

#[test]
fn entry_info_names_the_files_the_gate_will_read() {
    let base = scratch("files");
    write(&base.join("motor.snd"), "fn arranca() -> i64 { 1 }\n");
    write(&base.join("frenos.snd"), "fn frena() -> i64 { 2 }\n");
    write(&base.join("frenos_test.snd"), "include \"frenos.snd\"\ntest \"t\" { }\n");
    write(
        &base.join("main.snd"),
        "include \"motor.snd\"\ninclude \"frenos.snd\"\nfn mia() -> i64 { arranca() }\ntest \"t\" { }\n",
    );

    let (tests, nombres) = entry_info(&base.join("main.snd"));
    assert_eq!(tests, 1, "the entry declares one test");
    assert!(nombres.contains(&"main.snd".to_string()), "it answers for itself: {nombres:?}");
    assert!(nombres.contains(&"motor.snd".to_string()), "and for what nobody else measures");
    assert!(
        !nombres.contains(&"frenos.snd".to_string()),
        "not for a module that has its own test file: {nombres:?}"
    );

    // A plain module is not an entry, and says so with a count of zero rather than by failing.
    let (tests, _) = entry_info(&base.join("motor.snd"));
    assert_eq!(tests, 0, "a module with no test blocks declares none");
}

/// `sond test <dir>`: run every gate in the tree, and report what no gate reads.
fn sond_test_dir(dir: &Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg(dir)
        .output()
        .expect("sond runs");
    let mut texto = String::from_utf8_lossy(&out.stdout).into_owned();
    texto.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), texto)
}

/// A tree needs a lib/ of its own so `include "x86_64/linux/testing.snd"` resolves by walking up,
/// and never depends on what this machine happens to have installed.
fn arbol(nombre: &str) -> sondc::scratch::Temporal {
    let base = scratch(nombre);
    let out = Command::new("cp")
        .arg("-R")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("lib"))
        .arg(&base)
        .output()
        .unwrap();
    assert!(out.status.success(), "the tree needs a lib/ of its own");
    base
}

#[test]
fn a_module_no_entry_measures_is_named_instead_of_passing_unnoticed() {
    let base = arbol("huerfano");
    // Measured, because its own test file covers it.
    write(&base.join("src/frenos.snd"), "fn frena() -> i64 { 2 }\n");
    write(
        &base.join("src/frenos_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"frenos.snd\"\n\
         test \"frena\" { assert_eq(frena(), 2) }\n",
    );
    // In nobody's denominator: no test file of its own, and no entry includes it.
    write(&base.join("src/olvidado.snd"), "fn nadie_me_mide() -> i64 { 7 }\n");

    let (rc, salida) = sond_test_dir(&base.join("src"));
    assert_ne!(rc, 0, "a tree with unmeasured code must not pass:\n{salida}");
    assert!(
        salida.contains("olvidado.snd"),
        "the report has to name the file, not just count it:\n{salida}"
    );
    assert!(
        !salida.contains("frenos.snd\n") || salida.contains("frena"),
        "the measured module must not be accused:\n{salida}"
    );

    // Give it a gate and the same tree passes: the check is about coverage, not about file count.
    write(
        &base.join("src/olvidado_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"olvidado.snd\"\n\
         test \"mide\" { assert_eq(nadie_me_mide(), 7) }\n",
    );
    let (rc, salida) = sond_test_dir(&base.join("src"));
    assert_eq!(rc, 0, "once every module is measured the tree passes:\n{salida}");
}

#[test]
fn a_failing_test_anywhere_in_the_tree_fails_the_run() {
    let base = arbol("rojo");
    write(&base.join("src/bien.snd"), "fn bien() -> i64 { 1 }\n");
    write(
        &base.join("src/bien_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"bien.snd\"\n\
         test \"pasa\" { assert_eq(bien(), 1) }\n",
    );
    write(&base.join("src/mal.snd"), "fn mal() -> i64 { 1 }\n");
    write(
        &base.join("src/mal_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"mal.snd\"\n\
         test \"falla\" { assert_eq(mal(), 999) }\n",
    );
    let (rc, salida) = sond_test_dir(&base.join("src"));
    assert_ne!(rc, 0, "one red gate is a red tree:\n{salida}");
    assert!(salida.contains("mal_test.snd"), "and it names which one:\n{salida}");
}

#[test]
fn a_gate_that_never_finishes_is_a_named_failure_and_not_a_stalled_run() {
    // A test that hangs does not fail: it holds the whole tree with nothing on screen, and the
    // run reads as "slow today" until somebody gets tired of waiting. The first real tree this
    // was pointed at sat on one gate until the deadline outside it fired, and the report said
    // nothing at all -- the same silence a green run has.
    let base = arbol("colgado");
    write(&base.join("src/rapido.snd"), "fn rapido() -> i64 { 1 }\n");
    write(
        &base.join("src/rapido_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"rapido.snd\"\n\
         test \"pasa\" { assert_eq(rapido(), 1) }\n",
    );
    write(&base.join("src/lento.snd"), "fn lento() -> i64 { 1 }\n");
    write(
        &base.join("src/lento_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"lento.snd\"\n\
         test \"gira\" { let mut i = 0\n for i >= 0 { i = i + 1 }\n assert_eq(lento(), 1) }\n",
    );

    let inicio = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg("--timeout=20")
        .arg(base.join("src"))
        .output()
        .expect("sond runs");
    let mut salida = String::from_utf8_lossy(&out.stdout).into_owned();
    salida.push_str(&String::from_utf8_lossy(&out.stderr));

    assert!(
        inicio.elapsed() < std::time::Duration::from_secs(300),
        "the run has to end on its own, not wait to be killed"
    );
    assert_ne!(out.status.code().unwrap_or(-1), 0, "a hung gate is a red tree:\n{salida}");
    assert!(
        salida.contains("lento_test.snd"),
        "and the report names the gate that hung:\n{salida}"
    );
    assert!(
        salida.contains("rapido_test.snd") || salida.contains("1 gate"),
        "the gates that did finish are still reported:\n{salida}"
    );
}

/// Every process still running out of the scratch of that run of `sond`, wherever it put it.
///
/// The scratch is named `.sond-<pid>-<random>`, so the pid in the path is what ties a process to
/// the run that started it -- no need to know which directory the temp dir happened to be.
fn supervivientes(pid: u32) -> Vec<u32> {
    let marca = format!("/.sond-{pid}-");
    let mut vivos = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else { return vivos };
    for e in procs.flatten() {
        let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        if let Ok(exe) = std::fs::read_link(format!("/proc/{p}/exe"))
            && exe.to_string_lossy().contains(&marca)
        {
            vivos.push(p);
        }
    }
    vivos
}

/// A killed gate leaves a grandchild spinning, and that grandchild holds the run open.
///
/// `Child::kill` reaches the one process `sond` spawned. But the harness forks a child per test
/// so a crashing test cannot take the run down with it, so what is actually spinning is a
/// GRANDchild: SIGKILL never reaches it. Measured on this tree: after the run printed its report
/// and exited, two gates were still at 100% of a core 14 and 5 minutes later, and because they
/// still held the inherited stdout, the same run that finished in 20s writing to a file had not
/// returned after 200s through a pipe.
#[test]
fn a_hung_gate_leaves_no_descendant_spinning() {
    let base = arbol("descendientes");
    write(&base.join("src/lento.snd"), "fn lento() -> i64 { 1 }\n");
    write(
        &base.join("src/lento_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"lento.snd\"\n\
         test \"gira\" { let mut i = 0\n for i >= 0 { i = i + 1 }\n assert_eq(lento(), 1) }\n",
    );

    let registro = base.join("salida.txt");
    let mut hijo = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg("--timeout=20")
        .arg(base.join("src"))
        .stdout(std::fs::File::create(&registro).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .expect("sond runs");
    let pid = hijo.id();
    hijo.wait().expect("the run ends on its own");

    // The grandchild is killed by its own path, not by ours: give the kernel a moment to reap it.
    let mut vivos = Vec::new();
    for _ in 0..50 {
        vivos = supervivientes(pid);
        if vivos.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Leave the machine no worse off than the assert found it.
    for p in &vivos {
        let _ = Command::new("kill").arg("-9").arg(p.to_string()).status();
    }
    assert!(
        vivos.is_empty(),
        "the run ended but {} process(es) are still spinning: {vivos:?}",
        vivos.len()
    );
}

/// A gate that could not even be STARTED is not a gate that hung.
///
/// The two answers arrive at the same place -- no verdict -- and the driver reported both as
/// "did not finish", so a machine with no compiler on it produced a report that said the tests
/// were slow. The concrete way this shows up under load is `ETXTBSY`: another process forked
/// while the gate binary was still open for writing, `exec` refuses, and a run that was fine
/// gets filed as hung.
#[test]
fn a_gate_that_could_not_be_started_says_so_instead_of_reading_as_a_hang() {
    let base = arbol("sin-compilador");
    write(&base.join("src/algo.snd"), "fn algo() -> i64 { 1 }\n");
    write(
        &base.join("src/algo_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"algo.snd\"\n\
         test \"t\" { assert_eq(algo(), 1) }\n",
    );

    // A `sond` with no `sondc` beside it, and nothing on PATH either.
    let solo = base.join("bin");
    std::fs::create_dir_all(&solo).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_sond"), solo.join("sond")).unwrap();

    let out = Command::new(solo.join("sond"))
        .arg("test")
        .arg("--timeout=20")
        .arg(base.join("src"))
        .env("PATH", "")
        .output()
        .expect("sond runs");
    let mut texto = String::from_utf8_lossy(&out.stdout).into_owned();
    texto.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_ne!(out.status.code().unwrap_or(-1), 0, "no compiler is not a pass:\n{texto}");
    assert!(
        !texto.contains("HUNG") && !texto.contains("did not finish"),
        "a compiler that cannot be run is not a hang:\n{texto}"
    );
    assert!(
        texto.contains("sondc"),
        "and the report says what could not be started:\n{texto}"
    );
}

/// A gate whose denominator is empty is not a gate that passes.
///
/// `covered * 100 / n` with n = 0 is a divide by zero: the harness died with the line half
/// printed (`coverage 0/0 (`), and the run had no verdict at all. And had it survived, the gate
/// reads `0 * 100 < 80 * 0`, which is false: measuring nothing would have passed at 100%.
/// Found on `x509/realcert_test.snd`, whose includes are all measured by gates of their own.
#[test]
fn a_gate_that_measures_nothing_fails_instead_of_passing_at_full_marks() {
    let base = arbol("sin-denominador");
    // Includes only the test harness, which is never instrumented: nothing to measure.
    write(
        &base.join("src/vacio_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ntest \"t\" { assert_eq(1, 1) }\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg(base.join("src/vacio_test.snd"))
        .output()
        .expect("sond runs");
    let mut texto = String::from_utf8_lossy(&out.stdout).into_owned();
    texto.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_ne!(out.status.code().unwrap_or(-1), 0, "measuring nothing is not a pass:\n{texto}");
    assert!(
        texto.contains("nothing measured"),
        "and the report says the denominator is empty rather than dying mid-line:\n{texto}"
    );
}

/// A function named without its lines is not actionable.
///
/// `tls_handshake_auth` is 130 lines; "not fully covered: tls_handshake_auth" leaves whoever
/// has to close the gap reading the whole thing to guess which seven of its thirty-two points
/// never ran. The report has to say WHERE.
#[test]
fn a_partly_covered_function_names_the_lines_that_never_ran() {
    let base = arbol("lineas");
    // `si_es_par` runs; the `else` arm of `clasifica` and the whole of `nunca` do not.
    write(
        &base.join("src/motor.snd"),
        "fn clasifica(n: i64) -> i64 {\n\
         \x20   if n == 0 {\n\
         \x20       return 10\n\
         \x20   } else {\n\
         \x20       return 20\n\
         \x20   }\n\
         }\n",
    );
    write(
        &base.join("src/motor_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"motor.snd\"\n\
         test \"cero\" { assert_eq(clasifica(0), 10) }\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg(base.join("src/motor_test.snd"))
        .output()
        .expect("sond runs");
    let mut texto = String::from_utf8_lossy(&out.stdout).into_owned();
    texto.push_str(&String::from_utf8_lossy(&out.stderr));

    assert!(texto.contains("clasifica"), "the function is named:\n{texto}");
    // Line 5 of motor.snd is `return 20`, the only statement the test never reaches.
    assert!(
        texto.contains("motor.snd:5"),
        "and the line that never ran is named, file and all:\n{texto}"
    );
}

/// The other kind of empty denominator: one that was handed to somebody else.
///
/// `x509/realcert_test.snd` drives the DER walk with a real cloudflare certificate, and every
/// module it includes has a `_test.snd` next door. Measuring those again here would make the
/// figure mean "what this file happens to include", so it answers for nothing NEW -- and that
/// is the system working, not a hole. It must not be read as the failure above.
#[test]
fn a_gate_whose_modules_are_all_measured_next_door_is_not_a_hole() {
    let base = arbol("delegado");
    write(&base.join("src/bytes.snd"), "fn empuja() -> i64 { 2 }\n");
    write(
        &base.join("src/bytes_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"bytes.snd\"\n\
         test \"empuja\" { assert_eq(empuja(), 2) }\n",
    );
    write(
        &base.join("src/integracion_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"bytes.snd\"\n\
         test \"junto\" { assert_eq(empuja(), 2) }\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_sond"))
        .arg("test")
        .arg(base.join("src/integracion_test.snd"))
        .output()
        .expect("sond runs");
    let mut texto = String::from_utf8_lossy(&out.stdout).into_owned();
    texto.push_str(&String::from_utf8_lossy(&out.stderr));

    assert_eq!(out.status.code().unwrap_or(-1), 0, "nothing is unmeasured here:\n{texto}");
    assert!(
        !texto.contains("nothing measured"),
        "an empty denominator that was DELEGATED is not the hole:\n{texto}"
    );
    assert!(
        texto.contains("measured by their own gates"),
        "and the report says where the code is measured instead:\n{texto}"
    );
}

/// A file with no code a gate could ever measure is not a hole in the tree.
///
/// `x86_64/linux/testing.snd` holds `assert` and its siblings and nothing else; the coverage
/// build never instruments a single one of them, so no entry can ever "answer for" it. Listing
/// it as unmeasured asks for a gate that could not exist. Same for a file of pure constants.
#[test]
fn a_file_with_nothing_to_measure_is_not_reported_as_unmeasured() {
    let base = arbol("nada-que-medir");
    write(&base.join("src/puertos.snd"), "let PUERTO_HTTP = 80\nlet PUERTO_HTTPS = 443\n");
    // The harness itself, in the swept directory: every function in it is one the coverage
    // build skips by name, so no entry can ever answer for it.
    std::fs::copy(base.join("lib/x86_64/linux/testing.snd"), base.join("src/testing.snd")).unwrap();
    write(&base.join("src/motor.snd"), "fn arranca() -> i64 { 1 }\n");
    write(
        &base.join("src/motor_test.snd"),
        "include \"x86_64/linux/testing.snd\"\ninclude \"motor.snd\"\n\
         test \"arranca\" { assert_eq(arranca(), 1) }\n",
    );

    let (_, salida) = sond_test_dir(&base.join("src"));
    assert!(
        !salida.contains("puertos.snd"),
        "a file of constants has no gate to give it:\n{salida}"
    );
    assert!(
        !salida.contains("testing.snd"),
        "and neither has the test harness itself:\n{salida}"
    );
}

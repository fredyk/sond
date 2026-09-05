//! Cada cabecera que se publica la lee el compilador que la escribio.
//!
//! `tools/install.sh` genera una cabecera por cada `.snd` de `lib/` y la publica en dos sitios:
//! `dist/include/` (dentro del repo, versionado) y `~/.local/include/sond/`, la ruta FHS que
//! RFC-0006 senala como la interfaz del paquete. Es EL artefacto contra el que compila quien
//! consume la libreria.
//!
//! El 05/09/2026, 132 de esas 193 cabeceras no las parseaba `sondc`. Todas por lo mismo: una
//! firma `pub fn` sin cuerpo, que el parser leia como una definicion a la que le falta el `{`.
//! Nada lo veia porque los tests de `header.rs` buscan SUBCADENAS en el texto, y un texto que se
//! lee bien puede ser imposible de compilar.
//!
//! Aqui no se congela ningun literal: se emite la cabecera de verdad, con el emisor de verdad, y
//! se le da al front-end de verdad. Quien cambie una de las dos orillas sin la otra lo ve aqui.

use std::path::{Path, PathBuf};
use std::process::Command;

/// La raiz del repo: este fichero vive en `compiler/tests/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ siempre tiene padre")
        .to_path_buf()
}

fn snd_files(dir: &Path, v: &mut Vec<PathBuf>) {
    let Ok(entradas) = std::fs::read_dir(dir) else { return };
    for e in entradas.flatten() {
        let p = e.path();
        if p.is_dir() {
            snd_files(&p, v);
        } else if p.extension().is_some_and(|x| x == "snd") {
            v.push(p);
        }
    }
}

#[test]
fn toda_cabecera_publicada_la_parsea_el_compilador_que_la_escribio() {
    let raiz = repo_root();
    let lib = raiz.join("lib");
    let mut ficheros = Vec::new();
    snd_files(&lib, &mut ficheros);
    ficheros.sort();
    assert!(
        ficheros.len() > 100,
        "se esperaba la libreria entera bajo {}, y salieron {}",
        lib.display(),
        ficheros.len()
    );

    // Se emite y se lee con EL binario, el mismo comando que corre `tools/install.sh`. Una
    // reconstruccion de la tuberia aqui dentro se saltaria la resolucion de includes, que es
    // parte de lo que la cabecera acaba diciendo, y daria verde sobre algo que install.sh no
    // produce.
    let sondc = env!("CARGO_BIN_EXE_sondc");
    let temporal = sondc::scratch::Temporal::nuevo("sond_hdrs");
    let tmp = temporal.ruta();

    let mut rotas: Vec<String> = Vec::new();
    for (n, f) in ficheros.iter().enumerate() {
        let emitida = Command::new(sondc)
            .arg(f)
            .arg("--emit-header")
            .output()
            .expect("sondc arranca");
        if !emitida.status.success() {
            rotas.push(format!(
                "{}: no se pudo emitir su cabecera: {}",
                f.display(),
                String::from_utf8_lossy(&emitida.stderr).trim()
            ));
            continue;
        }
        // La cabecera se guarda con su extension real: es el fichero que se publica.
        let dst = tmp.join(format!("h{n}.snd"));
        std::fs::write(&dst, &emitida.stdout).expect("la cabecera se escribe");
        // `--entry-info` parsea el fichero y no escribe ningun binario.
        let leida = Command::new(sondc)
            .arg("--entry-info")
            .arg(&dst)
            .output()
            .expect("sondc arranca");
        if !leida.status.success() {
            let e = String::from_utf8_lossy(&leida.stderr);
            let e = e.trim().replace(&dst.display().to_string(), "<cabecera>");
            rotas.push(format!("{}: {e}", f.display()));
        }
    }
    assert!(
        rotas.is_empty(),
        "{} de {} cabeceras no las parsea el compilador que las escribe. Se publican en \
         dist/include/ y en ~/.local/include/sond/, o sea que se entregan rotas:\n{}",
        rotas.len(),
        ficheros.len(),
        rotas.join("\n")
    );
}

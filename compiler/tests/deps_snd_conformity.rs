//! Las dos orillas del manifiesto, cruzadas: escribe la de Sond, lee la de Rust.
//!
//! `lib/any/pkg/get.snd` ESCRIBE `deps.snd` y `compiler/src/deps.rs` lo LEE. Cada una tiene sus
//! propios tests y las dos estaban verdes el 01/09/2026 mientras hablaban idiomas distintos: una
//! ponia una linea plana de cuatro campos y la otra esperaba un bloque `use … = sond:sha256:…`.
//! Un `sond get` habria dejado un fichero que el compilador rechaza, y ningun test lo veia porque
//! ninguno cruzaba los artefactos.
//!
//! Este test no congela un literal —que envejeceria en cuanto una de las dos cambie— sino que
//! hace ESCRIBIR al lado Sond, de verdad, y le da el resultado al parser de Rust. Si alguien mueve
//! una orilla sin la otra, aqui se ve.

use std::path::PathBuf;
use std::process::Command;

const HASH: &str = "923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d";
const SHA: &str = "1111222233334444555566667777888899990000";
const SRC: &str = "github.com/fredyk/sond-strings";

/// La raiz del repo: este fichero vive en `compiler/tests/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ siempre tiene padre")
        .to_path_buf()
}

#[test]
fn lo_que_get_snd_escribe_lo_lee_deps_rs() {
    let raiz = repo_root();
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let deps = tmp.join(format!("deps-cruce-{}.snd", std::process::id()));
    let _ = std::fs::remove_file(&deps);

    // El programa se escribe DENTRO del arbol y bajo un subdirectorio: `include` resuelve
    // subiendo hasta el `lib/` del proyecto, asi que desde /tmp no encontraria nada, y desde la
    // raiz misma tampoco hay hacia donde subir.
    let dir_fuente = raiz.join("examples");
    let fuente = dir_fuente.join(format!("cruce-deps-{}.snd", std::process::id()));
    let bin = tmp.join(format!("cruce-deps-{}", std::process::id()));
    std::fs::write(
        &fuente,
        format!(
            r#"include "any/pkg/get.snd"

fn main() -> i64 {{
    let p: string = "{}"
    pkg_deps_path = p.utf8
    let src: string = "{SRC}"
    let sha: string = "{SHA}"
    let h: string = "{HASH}"
    let nm: string = "strings"
    let ok = pkg_deps_write(src.utf8, sha.utf8, h.utf8, nm.utf8)
    if ok == 1 {{ return 0 }}
    1
}}
"#,
            deps.display()
        ),
    )
    .expect("la fuente del cruce debe poder escribirse");

    let out = Command::new(env!("CARGO_BIN_EXE_sondc"))
        .arg(&fuente)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("sondc debe poder ejecutarse");
    let _ = std::fs::remove_file(&fuente);
    assert!(
        out.status.success(),
        "el escritor de deps.snd debe compilar, y sondc dijo: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let corrida = Command::new(&bin).output().expect("el binario debe correr");
    assert!(
        corrida.status.success(),
        "pkg_deps_write fallo: {}",
        String::from_utf8_lossy(&corrida.stderr)
    );
    let _ = std::fs::remove_file(&bin);

    let escrito = std::fs::read_to_string(&deps).expect("get.snd debe haber escrito el fichero");
    let _ = std::fs::remove_file(&deps);

    // Y ahora la otra orilla, sobre esos bytes exactos.
    let leido = sondc::deps::parse_deps(&escrito)
        .unwrap_or_else(|e| panic!("deps.rs no sabe leer lo que get.snd escribe.\nEscrito:\n{escrito}\nError: {e}"));
    let e = leido
        .entries
        .get("strings")
        .unwrap_or_else(|| panic!("falta la entrada `strings`.\nEscrito:\n{escrito}"));
    assert_eq!(e.hash, HASH, "el hash no sobrevive el viaje:\n{escrito}");
    assert_eq!(
        e.from.as_deref(),
        Some(format!("{SRC}@{SHA}").as_str()),
        "el origen no sobrevive el viaje:\n{escrito}"
    );
    assert_eq!(e.grants, "{}", "el hueco de las capacidades:\n{escrito}");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jhon Magdalena Vila and the Sond contributors
//
// The source tree is source. `sond build program.snd` writes its binary to the working directory,
// the way every compiler has since cc(1) -- so building anything from inside the checkout leaves
// an executable next to the sources, and a `git add -A` puts it in the history. That is how a
// 19 KiB ELF named `compiler/testing` reached a commit here.
//
// A .gitignore cannot catch this: the binary of `foo.snd` is called `foo`, with no extension and
// no prefix to match on. What CAN catch it is asking what the files actually ARE, so this reads
// the first four bytes of every tracked file and refuses the ones the kernel would execute.
//
// A hook was the other candidate and is weaker: hooks are not cloned, are not installed by
// anything, and `--no-verify` walks past them. A test runs for everyone who runs the suite.

use std::path::PathBuf;
use std::process::Command;

/// The magic numbers of the formats an operating system loads and runs. ELF is what this project
/// emits; the rest are here because a tracked binary is wrong whatever machine it was built for.
const EJECUTABLES: [(&[u8], &str); 4] = [
    (b"\x7fELF", "ELF"),
    (b"MZ\x90\x00", "PE/COFF"),
    (b"\xcf\xfa\xed\xfe", "Mach-O"),
    (b"\xca\xfe\xba\xbe", "Mach-O universal"),
];

fn raiz() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the compiler crate lives inside the repository")
        .to_path_buf()
}

#[test]
fn ningun_binario_ejecutable_esta_versionado() {
    let raiz = raiz();
    let salida = Command::new("git")
        .arg("-C")
        .arg(&raiz)
        .args(["ls-files", "-z"])
        .output()
        .expect("git ls-files runs inside a checkout");
    assert!(salida.status.success(), "git ls-files failed");

    let mut culpables: Vec<String> = Vec::new();
    for bruto in salida.stdout.split(|b| *b == 0) {
        if bruto.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(bruto).into_owned();
        let ruta = raiz.join(&rel);
        // A tracked path that is not on disk is a deletion staged elsewhere, not a binary.
        let Ok(datos) = std::fs::read(&ruta) else { continue };
        for (magico, nombre) in EJECUTABLES {
            if datos.starts_with(magico) {
                culpables.push(format!("{rel} ({nombre})"));
                break;
            }
        }
    }

    assert!(
        culpables.is_empty(),
        "these tracked files are compiled executables, and the tree holds sources:\n  {}\n\
         Build output belongs outside the checkout, or under a path .gitignore already covers.",
        culpables.join("\n  ")
    );
}

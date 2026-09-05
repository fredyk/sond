//! A scratch directory that removes itself.
//!
//! `env::temp_dir()` is tmpfs here: what a test leaves in it is RAM that never comes back. The
//! resolve tests built a directory per case out of pid and a counter and deleted none of them,
//! and by the time anyone counted there were 6.947 holding 610 MB, the oldest twelve days old,
//! on a machine that had begun killing processes for want of memory. The suite was green
//! throughout -- nothing about a leak makes a test fail, which is why it ran for twelve days.
//!
//! Naming a scratch path and cleaning it up are therefore the same act. `Temporal` derefs to
//! `Path`, so it is used exactly where the hand-built path was, and its `Drop` is what deletes
//! the tree. `compiler/tests/scratch_hygiene.rs` refuses a test that builds one by hand again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static CONTADOR: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp dir, deleted when this value is dropped.
///
/// What it derefs to is not always what it deletes: `nuevo_en` hands out a path INSIDE the
/// scratch tree while still owning (and removing) the root. That is what lets a fixture return
/// `<root>/sub` without the caller having to carry two values around and remember which one keeps
/// the tree alive -- forgetting that is precisely how the first version of this leaked.
pub struct Temporal {
    raiz: PathBuf,
    expuesta: PathBuf,
}

/// Every scratch directory this module makes starts with this, so a sweep can tell what is its
/// to remove from what merely happens to sit in the same directory.
const MARCA: &str = "sondtmp-";

/// Clears the scratch of processes that are gone, once per process.
///
/// `Drop` is not a guarantee. It does not run on SIGKILL, and a `cargo test` here is killed from
/// outside routinely: a deadline, the OOM killer, Ctrl-C. Two directories from runs killed
/// earlier today were still in tmpfs when this was written -- the same shape as the leak that
/// reached 6.947 directories and 610 MB before anyone counted. So whoever makes the mess is not
/// the only one who clears it: the next run does too.
fn barrer_una_vez() {
    static UNA_VEZ: std::sync::Once = std::sync::Once::new();
    UNA_VEZ.call_once(barrer);
}

/// Removes every `sondtmp-...-<pid>-<n>` whose pid has no process behind it.
///
/// A live pid is left alone, and so is a name this module did not make: the fleet runs several
/// test binaries at once, and a sweep that cannot tell them apart deletes the fixture the run
/// next door is reading.
pub fn barrer() {
    let base = std::env::temp_dir();
    let Ok(entradas) = std::fs::read_dir(&base) else { return };
    for e in entradas.flatten() {
        let nombre = e.file_name().to_string_lossy().into_owned();
        if !nombre.starts_with(MARCA) {
            continue;
        }
        // `sondtmp-<prefijo>-<pid>-<n>`: the prefix may hold dashes, the last two fields may not.
        let mut campos = nombre.rsplit('-');
        let _n = campos.next();
        let Some(pid) = campos.next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

impl Temporal {
    /// Creates a fresh directory whose name starts with `prefijo`.
    ///
    /// Created rather than merely named: `create_dir` fails if the name is taken, so two runs
    /// never share one, and that holds without trusting the counter to be unique across
    /// processes.
    pub fn nuevo(prefijo: &str) -> Temporal {
        barrer_una_vez();
        let base = std::env::temp_dir();
        for intento in 0..64u64 {
            let n = CONTADOR.fetch_add(1, Ordering::Relaxed);
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let dir = base.join(format!(
                "{MARCA}{prefijo}-{}-{:x}",
                std::process::id(),
                t ^ (n << 32) ^ intento
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    return Temporal { raiz: dir.clone(), expuesta: dir };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create a scratch dir in {}: {e}", base.display()),
            }
        }
        panic!("cannot create a scratch dir in {}", base.display());
    }

    /// A directory with EXACTLY this name under the system temp dir, deleted on drop.
    ///
    /// For the tests that have to reproduce a path some other tool chose -- the leftovers of a
    /// run that was killed, named after its pid. The name is fixed, so the cleanup cannot be:
    /// this is the only way to build one and it takes its own removal with it.
    pub fn con_nombre(nombre: &str) -> Temporal {
        let dir = std::env::temp_dir().join(nombre);
        std::fs::create_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        Temporal { raiz: dir.clone(), expuesta: dir }
    }

    /// A fresh scratch tree with `sub` created inside it; the value points at `sub` and still
    /// deletes the whole tree when dropped.
    pub fn nuevo_en(prefijo: &str, sub: &str) -> Temporal {
        let mut t = Temporal::nuevo(prefijo);
        let dentro = t.raiz.join(sub);
        std::fs::create_dir_all(&dentro)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dentro.display()));
        t.expuesta = dentro;
        t
    }

    /// Points this value at `sub` inside its own tree, without creating anything: for a fixture
    /// that writes several files and hands back one of them. The tree still goes on drop.
    pub fn exponiendo(mut self, sub: &str) -> Temporal {
        self.expuesta = self.raiz.join(sub);
        self
    }

    /// The path, for the rare caller that needs it owned.
    pub fn ruta(&self) -> &Path {
        &self.expuesta
    }
}

impl std::ops::Deref for Temporal {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.expuesta
    }
}

impl AsRef<Path> for Temporal {
    fn as_ref(&self) -> &Path {
        &self.expuesta
    }
}

impl AsRef<std::ffi::OsStr> for Temporal {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.expuesta.as_os_str()
    }
}

impl Drop for Temporal {
    fn drop(&mut self) {
        // The ROOT, not the exposed path: a `nuevo_en` value points inside the tree it owns.
        let _ = std::fs::remove_dir_all(&self.raiz);
    }
}

/// A single scratch FILE, in a directory of its own that goes with it.
///
/// Derefs to the file's path, so it stands where a hand-built `PathBuf` stood, and dropping it
/// removes the directory holding it. A file written straight into the system temp dir has no
/// owner: nothing deletes it, and the next run of the same test writes beside it.
pub struct Fichero {
    _dir: Temporal,
    ruta: PathBuf,
}

impl Fichero {
    /// Creates `nombre` inside a fresh scratch directory, holding `contenido`.
    pub fn nuevo(prefijo: &str, nombre: &str, contenido: &str) -> Fichero {
        let dir = Temporal::nuevo(prefijo);
        let ruta = dir.join(nombre);
        std::fs::write(&ruta, contenido)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", ruta.display()));
        Fichero { _dir: dir, ruta }
    }
}

impl std::ops::Deref for Fichero {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.ruta
    }
}

impl AsRef<Path> for Fichero {
    fn as_ref(&self) -> &Path {
        &self.ruta
    }
}

impl AsRef<std::ffi::OsStr> for Fichero {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.ruta.as_os_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_gone_once_the_value_is_dropped() {
        let ruta;
        {
            let t = Temporal::nuevo("sond-scratch-prueba");
            ruta = t.to_path_buf();
            std::fs::write(t.join("algo.txt"), "contenido").unwrap();
            assert!(ruta.is_dir(), "it exists while it is held");
            assert!(ruta.join("algo.txt").exists());
        }
        assert!(!ruta.exists(), "and it is gone, with its contents, once dropped: {ruta:?}");
    }

    #[test]
    fn a_scratch_file_takes_its_directory_with_it() {
        let ruta;
        {
            let f = Fichero::nuevo("sond-scratch-prueba", "entrada.snd", "fn main() -> i64 { 0 }");
            ruta = f.to_path_buf();
            assert_eq!(std::fs::read_to_string(&*f).unwrap(), "fn main() -> i64 { 0 }");
        }
        assert!(!ruta.exists(), "the file goes when its owner goes: {ruta:?}");
        assert!(!ruta.parent().unwrap().exists(), "and so does the directory around it");
    }

    #[test]
    fn a_path_inside_the_tree_still_takes_the_whole_tree_with_it() {
        let dentro;
        let raiz;
        {
            let t = Temporal::nuevo_en("sond-scratch-prueba", "proyecto/src");
            dentro = t.to_path_buf();
            raiz = dentro.parent().unwrap().parent().unwrap().to_path_buf();
            std::fs::write(t.join("main.snd"), "fn main() -> i64 { 0 }").unwrap();
            assert!(dentro.is_dir(), "the exposed path is the one inside");
        }
        assert!(!raiz.exists(), "and dropping it removes the root, not just the leaf: {raiz:?}");
    }

    #[test]
    fn two_scratch_dirs_are_never_the_same_directory() {
        let a = Temporal::nuevo("sond-scratch-prueba");
        let b = Temporal::nuevo("sond-scratch-prueba");
        assert_ne!(a.ruta(), b.ruta());
        assert!(a.is_dir() && b.is_dir());
    }
}

#[cfg(test)]
mod nombre_exacto {
    use super::Temporal;

    #[test]
    fn con_nombre_uses_the_name_it_was_given_and_still_cleans_up() {
        let ruta;
        {
            let t = Temporal::con_nombre(".sond-4000001-00000000feedface");
            ruta = t.ruta().to_path_buf();
            assert_eq!(
                ruta.file_name().unwrap(),
                ".sond-4000001-00000000feedface",
                "the name has to be the one asked for: another tool is going to read it"
            );
            assert!(ruta.is_dir(), "and the directory exists while the value does");
        }
        assert!(!ruta.exists(), "and it is gone when the value is");
    }
}

//! A scratch directory that removes itself.
//!
//! `env::temp_dir()` is tmpfs here: what a test leaves in it is RAM that never comes back. The
//! resolve tests built a directory per case out of pid and a counter and deleted none of them,
//! and by the time anyone counted there were 6.947 holding 610 MB, the oldest twelve days old,
//! on a machine that had begun killing processes for want of memory. The suite was green
//! throughout -- nothing about a leak makes a test fail, which is why it ran for twelve days.
//!
//! Naming a scratch path and cleaning it up are therefore the same act. `Scratch` derefs to
//! `Path`, so it is used exactly where the hand-built path was, and its `Drop` is what deletes
//! the tree. `compiler/tests/scratch_hygiene.rs` refuses a test that builds one by hand again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp dir, deleted when this value is dropped.
///
/// What it derefs to is not always what it deletes: `new_in` hands out a path INSIDE the
/// scratch tree while still owning (and removing) the root. That is what lets a fixture return
/// `<root>/sub` without the caller having to carry two values around and remember which one keeps
/// the tree alive -- forgetting that is precisely how the first version of this leaked.
pub struct Scratch {
    root: PathBuf,
    exposed: PathBuf,
}

/// Every scratch directory this module makes starts with this, so a sweep can tell what is its
/// to remove from what merely happens to sit in the same directory.
const MARK: &str = "sondtmp-";

/// Clears the scratch of processes that are gone, once per process.
///
/// `Drop` is not a guarantee. It does not run on SIGKILL, and a `cargo test` here is killed from
/// outside routinely: a deadline, the OOM killer, Ctrl-C. Two directories from runs killed
/// earlier today were still in tmpfs when this was written -- the same shape as the leak that
/// reached 6.947 directories and 610 MB before anyone counted. So whoever makes the mess is not
/// the only one who clears it: the next run does too.
fn sweep_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(sweep);
}

/// Removes every `sondtmp-...-<pid>-<n>` whose pid has no process behind it.
///
/// A live pid is left alone, and so is a name this module did not make: the fleet runs several
/// test binaries at once, and a sweep that cannot tell them apart deletes the fixture the run
/// next door is reading.
pub fn sweep() {
    let base = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&base) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(MARK) {
            continue;
        }
        // `sondtmp-<prefix>-<pid>-<n>`: the prefix may hold dashes, the last two fields may not.
        let mut fields = name.rsplit('-');
        let _n = fields.next();
        let Some(pid) = fields.next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

impl Scratch {
    /// Creates a fresh directory whose name starts with `prefix`.
    ///
    /// Created rather than merely named: `create_dir` fails if the name is taken, so two runs
    /// never share one, and that holds without trusting the counter to be unique across
    /// processes.
    pub fn new(prefix: &str) -> Scratch {
        sweep_once();
        let base = std::env::temp_dir();
        for attempt in 0..64u64 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let dir = base.join(format!(
                "{MARK}{prefix}-{}-{:x}",
                std::process::id(),
                t ^ (n << 32) ^ attempt
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    return Scratch { root: dir.clone(), exposed: dir };
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
    pub fn with_name(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        Scratch { root: dir.clone(), exposed: dir }
    }

    /// A fresh scratch tree with `sub` created inside it; the value points at `sub` and still
    /// deletes the whole tree when dropped.
    pub fn new_in(prefix: &str, sub: &str) -> Scratch {
        let mut t = Scratch::new(prefix);
        let inside = t.root.join(sub);
        std::fs::create_dir_all(&inside)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", inside.display()));
        t.exposed = inside;
        t
    }

    /// Points this value at `sub` inside its own tree, without creating anything: for a fixture
    /// that writes several files and hands back one of them. The tree still goes on drop.
    pub fn exposing(mut self, sub: &str) -> Scratch {
        self.exposed = self.root.join(sub);
        self
    }

    /// The path, for the rare caller that needs it owned.
    pub fn path(&self) -> &Path {
        &self.exposed
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.exposed
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.exposed
    }
}

impl AsRef<std::ffi::OsStr> for Scratch {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.exposed.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // The ROOT, not the exposed path: a `new_in` value points inside the tree it owns.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A single scratch FILE, in a directory of its own that goes with it.
///
/// Derefs to the file's path, so it stands where a hand-built `PathBuf` stood, and dropping it
/// removes the directory holding it. A file written straight into the system temp dir has no
/// owner: nothing deletes it, and the next run of the same test writes beside it.
pub struct ScratchFile {
    _dir: Scratch,
    path: PathBuf,
}

impl ScratchFile {
    /// Creates `name` inside a fresh scratch directory, holding `contents`.
    pub fn new(prefix: &str, name: &str, contents: &str) -> ScratchFile {
        let dir = Scratch::new(prefix);
        let path = dir.join(name);
        std::fs::write(&path, contents)
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        ScratchFile { _dir: dir, path }
    }
}

impl std::ops::Deref for ScratchFile {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for ScratchFile {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_gone_once_the_value_is_dropped() {
        let path;
        {
            let t = Scratch::new("sond-scratch-test");
            path = t.to_path_buf();
            std::fs::write(t.join("something.txt"), "contents").unwrap();
            assert!(path.is_dir(), "it exists while it is held");
            assert!(path.join("something.txt").exists());
        }
        assert!(!path.exists(), "and it is gone, with its contents, once dropped: {path:?}");
    }

    #[test]
    fn a_scratch_file_takes_its_directory_with_it() {
        let path;
        {
            let f = ScratchFile::new("sond-scratch-test", "input.snd", "fn main() -> i64 { 0 }");
            path = f.to_path_buf();
            assert_eq!(std::fs::read_to_string(&*f).unwrap(), "fn main() -> i64 { 0 }");
        }
        assert!(!path.exists(), "the file goes when its owner goes: {path:?}");
        assert!(!path.parent().unwrap().exists(), "and so does the directory around it");
    }

    #[test]
    fn a_path_inside_the_tree_still_takes_the_whole_tree_with_it() {
        let inside;
        let root;
        {
            let t = Scratch::new_in("sond-scratch-test", "project/src");
            inside = t.to_path_buf();
            root = inside.parent().unwrap().parent().unwrap().to_path_buf();
            std::fs::write(t.join("main.snd"), "fn main() -> i64 { 0 }").unwrap();
            assert!(inside.is_dir(), "the exposed path is the one inside");
        }
        assert!(!root.exists(), "and dropping it removes the root, not just the leaf: {root:?}");
    }

    #[test]
    fn two_scratch_dirs_are_never_the_same_directory() {
        let a = Scratch::new("sond-scratch-test");
        let b = Scratch::new("sond-scratch-test");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && b.is_dir());
    }
}

#[cfg(test)]
mod exact_name {
    use super::Scratch;

    #[test]
    fn with_name_uses_the_name_it_was_given_and_still_cleans_up() {
        let path;
        {
            let t = Scratch::with_name(".sond-4000001-00000000feedface");
            path = t.path().to_path_buf();
            assert_eq!(
                path.file_name().unwrap(),
                ".sond-4000001-00000000feedface",
                "the name has to be the one asked for: another tool is going to read it"
            );
            assert!(path.is_dir(), "and the directory exists while the value does");
        }
        assert!(!path.exists(), "and it is gone when the value is");
    }
}

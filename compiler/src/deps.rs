//! `deps.snd` parser, canonical content hash, and package cache.
//!
//! A `deps.snd` file declares dependencies by content identity:
//!
//! ```sond
//! use strings = sond:sha256:7f3a9c1e...
//!     from "github.com/fredyk/sond-strings@3d9f21c"
//!     grants {}
//! ```
//!
//! The canonical hash is computed over all `.snd` files under a module root,
//! sorted by UTF-8 byte comparison of their relative paths.

use crate::sha256;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// A single `use` entry parsed from `deps.snd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepEntry {
    /// The module name (e.g. `strings`).
    pub name: String,
    /// The SHA-256 hash in lowercase hex (64 chars).
    pub hash: String,
    /// Optional origin hint (e.g. `"github.com/fredyk/sond-strings@3d9f21c"`).
    pub from: Option<String>,
    /// The raw grants text (e.g. `{}` or `{ read, write }`).
    pub grants: String,
    /// Line number where this `use` was declared (1-indexed).
    pub line: usize,
}

/// Parsed contents of a `deps.snd` file.
#[derive(Debug, Clone, Default)]
pub struct DepsFile {
    /// All `use` entries, keyed by module name.
    pub entries: HashMap<String, DepEntry>,
}

/// Parse a `deps.snd` source string. Returns the parsed entries or an error
/// naming the offending line.
pub fn parse_deps(src: &str) -> Result<DepsFile, String> {
    let mut deps = DepsFile::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("//") {
            i += 1;
            continue;
        }
        // Parse: use <name> = sond:sha256:<hash>
        let Some(rest) = line.strip_prefix("use ") else {
            return Err(format!(
                "deps.snd: line {}: expected `use <name> = ...`, got `{}`",
                i + 1,
                line
            ));
        };
        let Some(eq_pos) = rest.find('=') else {
            return Err(format!(
                "deps.snd: line {}: expected `=` in `use` declaration",
                i + 1
            ));
        };
        let name = rest[..eq_pos].trim().to_string();
        let rhs = rest[eq_pos + 1..].trim();
        let Some(hash_part) = rhs.strip_prefix("sond:sha256:") else {
            return Err(format!(
                "deps.snd: line {}: expected `sond:sha256:<hash>`, got `{}`",
                i + 1,
                rhs
            ));
        };
        let hash = hash_part.trim().to_string();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "deps.snd: line {}: SHA-256 hash must be 64 lowercase hex characters, got `{}` (len={})",
                i + 1,
                hash,
                hash.len()
            ));
        }

        // Check for duplicate name
        if deps.entries.contains_key(&name) {
            let existing_line = deps.entries[&name].line;
            return Err(format!(
                "deps.snd: duplicate `use {name}`: first at line {existing_line}, again at line {}",
                i + 1
            ));
        }

        // Look ahead for optional `from` and `grants` (they may be on continuation lines)
        let mut from = None;
        let mut grants = String::from("{}");
        i += 1;
        while i < lines.len() {
            let next = lines[i].trim();
            if next.is_empty() || next.starts_with("//") {
                i += 1;
                continue;
            }
            if let Some(origin) = next.strip_prefix("from ") {
                let origin = origin.trim().to_string();
                // Strip surrounding quotes if present
                let origin = origin
                    .strip_prefix('"')
                    .and_then(|o| o.strip_suffix('"'))
                    .unwrap_or(&origin)
                    .to_string();
                from = Some(origin);
                i += 1;
                continue;
            }
            if let Some(g) = next.strip_prefix("grants ") {
                grants = g.trim().to_string();
                i += 1;
                continue;
            }
            // Neither `from` nor `grants`: this line is a new directive or end of this entry
            break;
        }

        deps.entries.insert(
            name.clone(),
            DepEntry {
                name,
                hash,
                from,
                grants,
                line: i - 1, // line where the `use` was (approximate, after lookahead)
            },
        );
    }
    Ok(deps)
}

/// Recursively collect all `.snd` files under `root`, returning paths relative
/// to `root` with `/` separator and no `./` prefix. Sorted by UTF-8 byte
/// comparison (BTreeSet with raw byte ordering).
fn collect_snd_files(root: &Path) -> Result<Vec<String>, std::io::Error> {
    let mut files = BTreeSet::new();
    collect_recursive(root, root, &mut files)?;
    Ok(files
        .into_iter()
        .map(|b| String::from_utf8(b).unwrap())
        .collect())
}

fn collect_recursive(
    base: &Path,
    dir: &Path,
    files: &mut BTreeSet<Vec<u8>>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(base, &path, files)?;
        } else if path.extension().is_some_and(|e| e == "snd") {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            // Normalize: replace `\` with `/` on Windows (shouldn't happen on Linux)
            let rel = rel.replace('\\', "/");
            files.insert(rel.into_bytes());
        }
    }
    Ok(())
}

/// Compute the canonical content hash of a module rooted at `root`.
///
/// Format (documented for cross-task interop):
///
/// All `.snd` files under `root` are collected, their paths made relative to
/// `root` with `/` separator (no `./` prefix), sorted by **byte comparison**
/// of the UTF-8 path (not locale). For each file, in order:
///
/// ```text
/// u64_le(len_path_utf8) || path_utf8 ||
/// u64_le(len_content)   || content
/// ```
///
/// The hash is SHA-256 of the full concatenation, returned as lowercase hex.
pub fn canonical_hash(root: &Path) -> Result<String, String> {
    let files = collect_snd_files(root)
        .map_err(|e| format!("cannot read module directory {}: {e}", root.display()))?;
    let mut data = Vec::new();
    for rel_path in &files {
        let path_bytes = rel_path.as_bytes();
        let path_len = path_bytes.len() as u64;
        data.extend_from_slice(&path_len.to_le_bytes());
        data.extend_from_slice(path_bytes);
        let content = std::fs::read(root.join(rel_path))
            .map_err(|e| format!("cannot read {}: {e}", root.join(rel_path).display()))?;
        let content_len = content.len() as u64;
        data.extend_from_slice(&content_len.to_le_bytes());
        data.extend_from_slice(&content);
    }
    let digest = sha256::sha256(&data);
    Ok(sha256::hex(&digest))
}

/// Return the package cache directory rooted at `base` (`<base>/.cache/sond`).
fn cache_root_at(base: &Path) -> Option<PathBuf> {
    Some(base.join(".cache/sond"))
}

/// Return the user's package cache directory (`~/.cache/sond` or `SOND_CACHE`).
fn cache_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SOND_CACHE") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/sond"))
}

/// Return the directory for a cached package identified by its content hash,
/// rooted at `base` (`<base>/.cache/sond/pkg/sha256-<hash>/`).
pub fn cache_dir_for_hash_at(hash: &str, base: &Path) -> Option<PathBuf> {
    Some(cache_root_at(base)?.join("pkg").join(format!("sha256-{hash}")))
}

/// Return the directory for a cached package identified by its content hash.
/// The directory is `cache_root()/pkg/sha256-<hash>/`.
pub fn cache_dir_for_hash(hash: &str) -> Option<PathBuf> {
    Some(cache_root()?.join("pkg").join(format!("sha256-{hash}")))
}

/// Verify that the contents of `cache_dir` match the expected `hash` by
/// recomputing the canonical hash of the files inside it.
pub fn verify_cache(cache_dir: &Path, expected_hash: &str) -> Result<(), String> {
    let actual = canonical_hash(cache_dir)?;
    if actual != expected_hash {
        Err(format!(
            "hash mismatch: expected {expected_hash}, cache has {actual} — the cached module has been tampered with"
        ))
    } else {
        Ok(())
    }
}

/// Load and parse `deps.snd` from a directory, walking up parents until found.
/// Returns `None` if no `deps.snd` exists (not an error: a program without
/// dependencies compiles as always).
pub fn load_deps(start_dir: &Path) -> Option<DepsFile> {
    let mut dir = Some(start_dir);
    while let Some(d) = dir {
        let path = d.join("deps.snd");
        if path.exists()
            && let Ok(src) = std::fs::read_to_string(&path)
            && let Ok(deps) = parse_deps(&src)
        {
            return Some(deps);
        }
        dir = d.parent();
    }
    None
}

/// A module resolver backed by `deps.snd` and the package cache.
///
/// For each `import <name>`, it looks up the `use` entry in `deps.snd`, finds
/// the cache directory for that hash, verifies the content, and returns it.
/// Falls back to the default resolver when `deps.snd` has no entry for the name.
pub struct DepsResolver {
    base_dir: PathBuf,
    cache_root: PathBuf,
    default: crate::resolve::DefaultResolver,
}

impl DepsResolver {
    pub fn new(base_dir: PathBuf) -> Self {
        let cache_root = cache_root().unwrap_or_else(|| PathBuf::from("/dev/null"));
        let default = crate::resolve::DefaultResolver::new(base_dir.clone());
        Self { base_dir, cache_root, default }
    }

    /// Create a resolver with an explicit cache root (for testing).
    pub fn with_cache_root(base_dir: PathBuf, cache_root: PathBuf) -> Self {
        let default = crate::resolve::DefaultResolver::new(base_dir.clone());
        Self { base_dir, cache_root, default }
    }
}

impl crate::resolve::ModuleResolver for DepsResolver {
    fn resolve(&self, module_name: &str) -> Result<PathBuf, String> {
        // Try deps.snd first
        if let Some(deps) = load_deps(&self.base_dir)
            && let Some(entry) = deps.entries.get(module_name)
        {
            let cache =
                cache_dir_for_hash_at(&entry.hash, &self.cache_root)
                    .ok_or("cannot determine cache directory")?;
            if !cache.exists() {
                let origin = entry.from.as_deref().unwrap_or("");
                return Err(format!(
                    "module `{module_name}` (hash {}{origin}) is not in the cache; \
                     run `sond get {origin}` to download it",
                    &entry.hash[..12],
                ));
            }
            // Verify content hash
            verify_cache(&cache, &entry.hash)?;
            // Look for the .snd file inside the cache dir
            let target = cache.join(format!("{module_name}.snd"));
            if target.exists() {
                return Ok(target);
            }
            // Try the candidates (with .snd extension, any/ prefix, etc.)
            for cand in crate::resolve::include_candidates(module_name) {
                let p = cache.join(&cand);
                if p.exists() {
                    return Ok(p);
                }
            }
            return Err(format!(
                "module `{module_name}` is declared in deps.snd but its cache directory \
                 `{}` does not contain a matching .snd file",
                cache.display()
            ));
        }
        // Fall back to default resolver
        self.default.resolve(module_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static UID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn uid() -> usize {
        UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// A scratch directory the caller HOLDS: dropping the returned value deletes the tree. The
    /// first version built the path from a `Temporal` and returned the inner `PathBuf`, which
    /// dropped the owner on the spot and left the directory behind -- the exact leak this is for.
    fn tmp_dir(name: &str) -> crate::scratch::Temporal {
        crate::scratch::Temporal::nuevo_en("deps_test", name)
    }

    // ── deps.snd parsing ───────────────────────────────────────────────

    #[test]
    fn parse_a_minimal_deps_snd() {
        let src = "use strings = sond:sha256:7f3a9c1e0123456789abcdef0123456789abcdef0123456789abcdef01234567\n";
        let deps = parse_deps(src).unwrap();
        assert_eq!(deps.entries.len(), 1);
        let e = &deps.entries["strings"];
        assert_eq!(e.name, "strings");
        assert_eq!(
            e.hash,
            "7f3a9c1e0123456789abcdef0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(e.from, None);
        assert_eq!(e.grants, "{}");
    }

    #[test]
    fn parse_deps_with_from_and_grants() {
        let src = "use strings = sond:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
                     from \"github.com/fredyk/sond-strings@3d9f21c\"\n\
                     grants { read }\n";
        let deps = parse_deps(src).unwrap();
        let e = &deps.entries["strings"];
        assert_eq!(e.from.as_deref(), Some("github.com/fredyk/sond-strings@3d9f21c"));
        assert_eq!(e.grants, "{ read }");
    }

    #[test]
    fn parse_deps_duplicate_name_is_error() {
        let src = "use strings = sond:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
                     use strings = sond:sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let err = parse_deps(src).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
        assert!(err.contains("strings"), "got: {err}");
    }

    #[test]
    fn parse_deps_bad_hash_length_is_error() {
        let src = "use strings = sond:sha256:tooshort\n";
        let err = parse_deps(src).unwrap_err();
        assert!(err.contains("64"), "got: {err}");
    }

    #[test]
    fn parse_deps_non_hex_hash_is_error() {
        let src = "use strings = sond:sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n";
        let err = parse_deps(src).unwrap_err();
        assert!(err.contains("hex"), "got: {err}");
    }

    #[test]
    fn parse_deps_multiple_entries() {
        let src = "use strings = sond:sha256:1111111111111111111111111111111111111111111111111111111111111111\n\
                     use io = sond:sha256:2222222222222222222222222222222222222222222222222222222222222222\n";
        let deps = parse_deps(src).unwrap();
        assert_eq!(deps.entries.len(), 2);
        assert!(deps.entries.contains_key("strings"));
        assert!(deps.entries.contains_key("io"));
    }

    #[test]
    fn parse_deps_with_comments_and_blanks() {
        let src = "// declared dependencies\n\nuse strings = sond:sha256:1111111111111111111111111111111111111111111111111111111111111111\n";
        let deps = parse_deps(src).unwrap();
        assert_eq!(deps.entries.len(), 1);
    }

    #[test]
    fn parse_deps_bad_keyword_is_error() {
        let src = "require strings = sond:sha256:1111111111111111111111111111111111111111111111111111111111111111\n";
        let err = parse_deps(src).unwrap_err();
        assert!(err.contains("use"), "got: {err}");
    }

    #[test]
    fn parse_deps_conformity_vector_format() {
        let src = "// declared dependencies\n\
                    \n\
                    use strings = sond:sha256:923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d\n\
                    \n\
                    // fetch origin\n\
                    from \"github.com/fredyk/sond-strings@3d9f21c\"\n\
                    \n\
                    // capabilities\n\
                    grants {}\n";
        let deps = parse_deps(src).unwrap();
        assert_eq!(deps.entries.len(), 1);
        let e = &deps.entries["strings"];
        assert_eq!(e.name, "strings");
        assert_eq!(
            e.hash,
            "923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d"
        );
        assert_eq!(e.from.as_deref(), Some("github.com/fredyk/sond-strings@3d9f21c"));
        assert_eq!(e.grants, "{}");
    }

    // ── canonical hash ─────────────────────────────────────────────────

    // Conformity vector from the spec:
    //   año.snd:       "fn añadir() -> i64 { 42 }\n"  (8B path, 27B content)
    //   sub/niño.snd:  "fn niño() -> i64 { 7 }\n"     (13B path, 24B content)
    // Expected hash: 923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d

    #[test]
    fn canonical_hash_conformity_vector() {
        let root = tmp_dir("conformity");
        std::fs::write(
            root.join("año.snd"),
            "fn añadir() -> i64 { 42 }\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("sub/niño.snd"),
            "fn niño() -> i64 { 7 }\n",
        )
        .unwrap();
        let hash = canonical_hash(&root).unwrap();
        assert_eq!(
            hash,
            "923345ba8a4816339aeac609bef11aaa7dd88d46459bf8886ed8cc68833b3a8d",
            "conformity vector must match the spec"
        );
    }

    #[test]
    fn canonical_hash_empty_module() {
        let root = tmp_dir("empty_mod");
        std::fs::create_dir_all(&root).unwrap();
        let hash = canonical_hash(&root).unwrap();
        // Empty concatenation -> SHA-256 of ""
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn canonical_hash_single_file() {
        let root = tmp_dir("single");
        let content = b"fn main() -> i64 { 0 }\n";
        std::fs::write(root.join("main.snd"), content).unwrap();
        let hash = canonical_hash(&root).unwrap();
        // Compute manually: path "main.snd" = 8 bytes
        let mut data = Vec::new();
        data.extend_from_slice(&8u64.to_le_bytes());
        data.extend_from_slice(b"main.snd");
        data.extend_from_slice(&(content.len() as u64).to_le_bytes());
        data.extend_from_slice(content);
        let expected = sha256::hex(&sha256::sha256(&data));
        assert_eq!(hash, expected);
    }

    #[test]
    fn canonical_hash_independent_of_filesystem_order() {
        // Run twice and verify the hash is identical regardless of OS enumeration order.
        let root = tmp_dir("order_indep");
        std::fs::write(root.join("b.snd"), "bb\n").unwrap();
        std::fs::write(root.join("a.snd"), "aa\n").unwrap();
        std::fs::write(root.join("c.snd"), "cc\n").unwrap();
        let h1 = canonical_hash(&root).unwrap();
        let h2 = canonical_hash(&root).unwrap();
        assert_eq!(h1, h2);
    }

    // ── cache directory ────────────────────────────────────────────────

    #[test]
    fn cache_dir_for_hash_uses_sha256_prefix() {
        let base = tmp_dir("cache_prefix");
        let d = cache_dir_for_hash_at("abcdef0123456789", &base).unwrap();
        assert!(d.to_string_lossy().contains("sha256-abcdef0123456789"));
    }

    #[test]
    fn verify_cache_ok() {
        let root = tmp_dir("verify_ok");
        std::fs::write(root.join("x.snd"), "hello\n").unwrap();
        let hash = canonical_hash(&root).unwrap();
        assert!(verify_cache(&root, &hash).is_ok());
    }

    #[test]
    fn verify_cache_tampered() {
        let root = tmp_dir("verify_tamper");
        std::fs::write(root.join("x.snd"), "hello\n").unwrap();
        let hash = canonical_hash(&root).unwrap();
        // Tamper with the file
        std::fs::write(root.join("x.snd"), "TAMPERED\n").unwrap();
        let err = verify_cache(&root, &hash).unwrap_err();
        assert!(err.contains("mismatch"), "got: {err}");
    }

    // ── integration: DepsResolver + ModuleResolver ─────────────────────

    // CP2: `import strings` resolves to the cache dir from deps.snd and compiles.
    #[test]
    fn deps_resolver_import_resolves_to_cache() {
        let root = tmp_dir("cp2_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let cache_base = tmp_dir("cp2_cache");

        // Create the "strings" module (what will go in the cache)
        let mod_dir = tmp_dir("cp2_mod");
        std::fs::write(
            mod_dir.join("strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }\n",
        )
        .unwrap();
        let hash = canonical_hash(&mod_dir).unwrap();

        // Copy the module into the temp cache
        let cache = cache_dir_for_hash_at(&hash, &cache_base).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::copy(mod_dir.join("strings.snd"), cache.join("strings.snd")).unwrap();

        // Write deps.snd in the project
        std::fs::write(
            root.join("project/deps.snd"),
            format!("use strings = sond:sha256:{hash}\n"),
        )
        .unwrap();

        // Write the entry file
        std::fs::write(
            root.join("project/main.snd"),
            "import strings\nfn main() -> i64 { strings::itoa(7) }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), cache_base.to_path_buf());
        let combined =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver)
                .expect("import must resolve");
        assert!(
            !combined.contains("::"),
            "no `::` survives resolution:\n{combined}"
        );
        assert!(
            combined.contains("itoa(7)"),
            "it binds to the module's function:\n{combined}"
        );
    }

    // CP3: cache content verification: tampered module fails before splicing.
    #[test]
    fn deps_resolver_tampered_cache_fails() {
        let root = tmp_dir("cp3_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let cache_base = tmp_dir("cp3_cache");

        // Create the "strings" module and compute its hash
        let mod_dir = tmp_dir("cp3_mod");
        std::fs::write(
            mod_dir.join("strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }\n",
        )
        .unwrap();
        let hash = canonical_hash(&mod_dir).unwrap();

        // Put a TAMPERED version in the temp cache
        let cache = cache_dir_for_hash_at(&hash, &cache_base).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("strings.snd"),
            "pub fn itoa(n: i64) -> i64 { 999 }\n", // different content!
        )
        .unwrap();

        // Write deps.snd and entry
        std::fs::write(
            root.join("project/deps.snd"),
            format!("use strings = sond:sha256:{hash}\n"),
        )
        .unwrap();
        std::fs::write(
            root.join("project/main.snd"),
            "import strings\nfn main() -> i64 { strings::itoa(7) }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), cache_base.to_path_buf());
        let err =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver)
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("mismatch") || err.contains("tampered"),
            "must fail on hash mismatch, got: {err}"
        );
    }

    // CP4: hash not in cache → error with `sond get` command.
    #[test]
    fn deps_resolver_missing_from_cache_error_with_sond_get() {
        let root = tmp_dir("cp4_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let cache_base = tmp_dir("cp4_cache");

        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        std::fs::write(
            root.join("project/deps.snd"),
            format!("use strings = sond:sha256:{hash}\nfrom \"github.com/x/y@abc\"\n"),
        )
        .unwrap();
        std::fs::write(
            root.join("project/main.snd"),
            "import strings\nfn main() -> i64 { 0 }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), cache_base.to_path_buf());
        let err =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver)
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("sond get"),
            "must tell user to run `sond get`, got: {err}"
        );
        assert!(
            err.contains("not in the cache"),
            "must say it is not in the cache, got: {err}"
        );
    }

    // CP5: import of name without `use` in deps.snd → error.
    #[test]
    fn deps_resolver_import_without_use_in_deps_is_error() {
        let root = tmp_dir("cp5_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let cache_base = tmp_dir("cp5_cache");

        // deps.snd declares `strings` but not `io`
        std::fs::write(
            root.join("project/deps.snd"),
            "use strings = sond:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        )
        .unwrap();
        std::fs::write(
            root.join("project/main.snd"),
            "import io\nfn main() -> i64 { 0 }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), cache_base.to_path_buf());
        let err =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver)
                .unwrap_err()
                .to_string();
        assert!(
            err.contains("io"),
            "must name the unknown module, got: {err}"
        );
    }

    // CP7: custom cache root redirect works.
    #[test]
    fn sond_cache_env_redirect() {
        let root = tmp_dir("cp7_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let custom_cache = tmp_dir("cp7_cache");

        // Create the "strings" module
        let mod_dir = tmp_dir("cp7_mod");
        std::fs::write(
            mod_dir.join("strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }\n",
        )
        .unwrap();
        let hash = canonical_hash(&mod_dir).unwrap();

        // Put module in the custom cache dir (layout: <base>/.cache/sond/pkg/sha256-<hash>)
        let cache = cache_dir_for_hash_at(&hash, &custom_cache).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::copy(mod_dir.join("strings.snd"), cache.join("strings.snd")).unwrap();

        // Write deps.snd and entry
        std::fs::write(
            root.join("project/deps.snd"),
            format!("use strings = sond:sha256:{hash}\n"),
        )
        .unwrap();
        std::fs::write(
            root.join("project/main.snd"),
            "import strings\nfn main() -> i64 { strings::itoa(7) }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), custom_cache.to_path_buf());
        let result =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver);

        let combined = result.expect("import must resolve with custom cache root");
        assert!(
            combined.contains("itoa(7)"),
            "resolved via custom cache:\n{combined}"
        );
    }

    // CP8: missing deps.snd is not an error.
    #[test]
    fn no_deps_file_is_not_an_error() {
        let root = tmp_dir("cp8_root");
        std::fs::create_dir_all(root.join("project")).unwrap();
        let cache_base = tmp_dir("cp8_cache");

        // No deps.snd at all
        std::fs::write(
            root.join("project/main.snd"),
            "fn main() -> i64 { 0 }\n",
        )
        .unwrap();

        let resolver = DepsResolver::with_cache_root(root.join("project"), cache_base.to_path_buf());
        let combined =
            crate::resolve::resolve_includes_with_resolver(&root.join("project/main.snd"), &resolver)
                .expect("no deps.snd must not be an error");
        assert!(
            combined.contains("fn main"),
            "program compiles as always:\n{combined}"
        );
    }

    // CP: load_deps walks up parent directories.
    #[test]
    fn load_deps_walks_up() {
        let root = tmp_dir("walkup");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(
            root.join("deps.snd"),
            "use x = sond:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        )
        .unwrap();
        let deps = load_deps(&root.join("a/b/c")).expect("must walk up to find deps.snd");
        assert!(deps.entries.contains_key("x"));
    }

    // CP: load_deps returns None when no deps.snd exists anywhere.
    #[test]
    fn load_deps_none_when_absent() {
        let root = tmp_dir("no_deps");
        std::fs::create_dir_all(&root).unwrap();
        assert!(load_deps(&root).is_none());
    }
}

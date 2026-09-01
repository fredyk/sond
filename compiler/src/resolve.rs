//! Source-level `include` resolution: the pragmatic, Go-simple package on-ramp.
//!
//! A program can pull in a reusable library with a top-level line
//! `include "path.snd"` (path relative to the including file). The compiler splices
//! that file's declarations in before parsing, so a `chacha20.snd` can be shared by
//! every file that needs it. Includes are resolved recursively and DEDUPED by
//! canonical path, so pulling the same library in from two places defines its
//! functions only once (no duplicate-definition errors). This is a stepping stone
//! toward the RFC-0006 package format, not a full module system: one flat namespace,
//! no visibility, but it makes real multi-file programs (an SSH stack) buildable.

use crate::lexer::tokenize_located;
use crate::token::Token;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Resolves a module name (from `import <name>`) to a file path on disk.
/// The default implementation searches `lib/` the same way `include` does.
pub trait ModuleResolver {
    fn resolve(&self, module_name: &str) -> Result<PathBuf, String>;
}

/// Default resolver: searches for `<name>.snd` under the project's `lib/` and the
/// system `~/.local/lib/sond/`, exactly like `include` does today. The `base_dir`
/// is the parent directory of the importing file, so the search starts there.
pub struct DefaultResolver {
    base_dir: PathBuf,
}

impl DefaultResolver {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }
}

impl ModuleResolver for DefaultResolver {
    fn resolve(&self, module_name: &str) -> Result<PathBuf, String> {
        for cand in include_candidates(module_name) {
            if let Some(found) = find_in_roots(&self.base_dir, &cand) {
                return Ok(found);
            }
            if let Some(home) = std::env::var_os("HOME") {
                let home_path = Path::new(&home);
                if let Some(found) = find_in_roots(home_path, &cand) {
                    return Ok(found);
                }
            }
        }
        Err(format!(
            "`import {module_name}`: module `{module_name}` not found; declare it or add an `include` of the file that defines it"
        ))
    }
}

/// The PRIVATE (non-`pub`) FREE functions a file defines, i.e. its file-local helpers. A free
/// function's `fn` keyword sits at brace-depth 0; a method's is inside its `impl { .. }` (depth 1)
/// and is already mangled `Type_m`, so it is left alone. Uses the lexer, so strings and comments
/// never false-match. Modifiers (`wrapping`/`interrupt`) between the visibility and `fn` are
/// skipped when checking for `pub`.
fn private_free_fn_names(src: &str) -> HashSet<String> {
    let (toks, _) = tokenize_located(src);
    let mut names = HashSet::new();
    let mut depth: i32 = 0;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Token::LBrace => depth += 1,
            Token::RBrace => depth -= 1,
            Token::Fn if depth == 0 => {
                let mut j = i;
                while j > 0 && matches!(toks[j - 1], Token::Wrapping | Token::Interrupt) {
                    j -= 1;
                }
                let is_pub = j > 0 && matches!(toks[j - 1], Token::Pub);
                if !is_pub && let Some(Token::Ident(n)) = toks.get(i + 1) {
                    names.insert(n.clone());
                }
            }
            _ => {}
        }
    }
    names
}

/// The PRIVATE (non-`pub`) module-level `let` names a file defines, i.e. its file-local global
/// constants and state. A `let` at brace-depth 0 is a module global; a `let` inside a function
/// body (depth > 0) is a local and is ignored. An optional `mut` (lexed as an identifier) between
/// `let` and the name is skipped. These are file-scoped on collision exactly like private helper
/// functions, so two bignum modules can each define `let BN_MASK16 = 0xFFFF`.
fn module_global_names(src: &str) -> HashSet<String> {
    let (toks, _) = tokenize_located(src);
    let mut names = HashSet::new();
    let mut depth: i32 = 0;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Token::LBrace => depth += 1,
            Token::RBrace => depth -= 1,
            Token::Let if depth == 0 => {
                let is_pub = i > 0 && matches!(toks[i - 1], Token::Pub);
                let mut j = i + 1;
                if let Some(Token::Ident(m)) = toks.get(j)
                    && m == "mut"
                {
                    j += 1;
                }
                if !is_pub && let Some(Token::Ident(n)) = toks.get(j) {
                    names.insert(n.clone());
                }
            }
            _ => {}
        }
    }
    names
}

/// The PUBLIC (`pub fn` / `pub let`) names a file exports: its module surface. These are what a
/// `module::name` may reach, and what has to stay reachable unqualified while the tree migrates.
fn public_names(src: &str) -> HashSet<String> {
    let (toks, _) = tokenize_located(src);
    let mut names = HashSet::new();
    let mut depth: i32 = 0;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Token::LBrace => depth += 1,
            Token::RBrace => depth -= 1,
            Token::Fn if depth == 0 => {
                let mut j = i;
                while j > 0 && matches!(toks[j - 1], Token::Wrapping | Token::Interrupt) {
                    j -= 1;
                }
                if j > 0
                    && matches!(toks[j - 1], Token::Pub)
                    && let Some(Token::Ident(n)) = toks.get(i + 1)
                {
                    names.insert(n.clone());
                }
            }
            Token::Let if depth == 0 => {
                if i > 0 && matches!(toks[i - 1], Token::Pub) {
                    let mut j = i + 1;
                    if let Some(Token::Ident(m)) = toks.get(j)
                        && m == "mut"
                    {
                        j += 1;
                    }
                    if let Some(Token::Ident(n)) = toks.get(j) {
                        names.insert(n.clone());
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// The module name a file goes by: its own name without the extension, with `-` written `_` so it
/// is a legal identifier. `x86_64/linux/http-server.snd` is the module `http_server`. Deriving it
/// from the file rather than declaring it inside keeps one source of truth -- a declared name can
/// disagree with the file it is in, and then two things claim to be the same module.
fn module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .replace('-', "_")
}

/// What a single file's line rewriting needs to know: which colliding names bind to which file
/// (`map`), which modules it can see, what each of those exports, and which names are ambiguous
/// unqualified. Grouped rather than passed as six arguments, because every one of them is needed
/// for a decision that must be made in one place: what does this identifier refer to?
/// Everything the emit pass needs to turn names into symbols, gathered from the first pass and
/// keyed by file. Each entry answers the same question for a different file: given a line of
/// THIS file, what does each name in it refer to?
struct Namespaces<'a> {
    resolve_map: &'a std::collections::HashMap<PathBuf, std::collections::HashMap<String, u32>>,
    module_maps: &'a std::collections::HashMap<PathBuf, std::collections::HashMap<String, PathBuf>>,
    ambiguous_maps:
        &'a std::collections::HashMap<PathBuf, std::collections::HashMap<String, Vec<String>>>,
    exports: &'a std::collections::HashMap<PathBuf, HashSet<String>>,
    ids: &'a std::collections::HashMap<PathBuf, u32>,
    colliding: &'a HashSet<String>,
}

struct LineCtx<'a> {
    map: &'a std::collections::HashMap<String, u32>,
    modules: &'a std::collections::HashMap<String, PathBuf>,
    exports: &'a std::collections::HashMap<PathBuf, HashSet<String>>,
    ids: &'a std::collections::HashMap<PathBuf, u32>,
    ambiguous: &'a std::collections::HashMap<String, Vec<String>>,
    colliding: &'a HashSet<String>,
    aliases: &'a std::collections::HashMap<String, String>,
}

/// Rewrites one line: qualified `module::name` references into the symbol they denote, and
/// whole-identifier occurrences of a colliding name into `name__f<id>` of the file that defines
/// it. Strings, char literals and line comments are copied verbatim, so a name inside a message
/// is never touched. Preserves the line count, so source-line info stays accurate.
///
/// `module::name` is resolved HERE, before the lexer, so nothing downstream ever sees a `::`:
/// the parser, the type checker and the code generator keep working on the flat namespace they
/// already understand, and the module system is a rewriting of names rather than a second
/// meaning for them.
fn rewrite_line(line: &str, ctx: &LineCtx) -> Result<String, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' => {
                // Copy the literal verbatim (respecting `\` escapes) without rewriting inside it.
                let quote = c;
                out.push(c);
                i += 1;
                while i < chars.len() {
                    out.push(chars[i]);
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        out.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                out.extend(&chars[i..]); // rest of the line is a comment
                break;
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                // `module::name`?
                if i + 2 < chars.len()
                    && chars[i] == ':'
                    && chars[i + 1] == ':'
                    && (chars[i + 2].is_alphabetic() || chars[i + 2] == '_')
                {
                    let ns = i + 2;
                    let mut j = ns;
                    while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                        j += 1;
                    }
                    let name: String = chars[ns..j].iter().collect();
                    out.push_str(&resolve_qualified(&ident, &name, ctx, ctx.aliases)?);
                    i = j;
                    continue;
                }
                if let Some(mods) = ctx.ambiguous.get(&ident) {
                    return Err(format!(
                        "`{ident}` is exported by more than one module ({}); say which one, \
                         e.g. `{}::{ident}`",
                        mods.join(", "),
                        mods[0]
                    ));
                }
                if let Some(id) = ctx.map.get(&ident) {
                    out.push_str(&format!("{ident}__f{id}"));
                } else {
                    out.push_str(&ident);
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// The symbol `module::name` denotes, or why it denotes nothing.
///
/// A module has to be INCLUDED to be named -- qualifying is a way to say which of the things you
/// already pulled in you meant, not a way to reach something you did not. And only its `pub`
/// surface is reachable: if `::` could see a private helper, the visibility the language already
/// has would stop meaning anything the moment somebody typed a colon.
fn resolve_qualified(
    module: &str,
    name: &str,
    ctx: &LineCtx,
    _aliases: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    // The module name is looked up directly in the module map. For aliased imports,
    // the alias replaces the original name in the map (e.g., `str` instead of `strings`).
    let Some(file) = ctx.modules.get(module) else {
        return Err(format!(
            "module `{module}` is not included here: `{module}::{name}` needs an \
             `include` of the file that defines it"
        ));
    };
    let exported = ctx.exports.get(file).is_some_and(|e| e.contains(name));
    if !exported {
        return Err(format!(
            "`{name}` is not public in module `{module}`: only `pub` names can be reached \
             from outside the file that defines them"
        ));
    }
    match ctx.ids.get(file) {
        Some(id) if ctx.colliding.contains(name) => Ok(format!("{name}__f{id}")),
        _ => Ok(name.to_string()),
    }
}

/// Reads `entry` and inlines every `include "..."` it (transitively) names, returning
/// the combined source ready for the lexer.
pub fn resolve_includes(entry: &Path) -> std::io::Result<String> {
    let base = entry
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    resolve_includes_with_resolver(entry, &DefaultResolver::new(base))
}

/// Like [`resolve_includes`], but uses a custom [`ModuleResolver`] for `import` resolution.
pub fn resolve_includes_with_resolver(
    entry: &Path,
    resolver: &dyn ModuleResolver,
) -> std::io::Result<String> {
    resolve_includes_scoped_with_resolver(entry, resolver).map(|(s, _)| s)
}

/// Like [`resolve_includes`], but also returns the set of function names that COVERAGE should
/// measure, the code the entry is responsible for testing. If the entry defines functions of
/// its own (a program), those are the scope; if it only holds `test` blocks (a library's test
/// file), the scope is the functions of its DIRECT includes (the library under test). Deeper,
/// transitively-included dependencies are assumed tested by their own suites, so they do not
/// drag down the entry's coverage. (The testing library is filtered out downstream.)
pub fn resolve_includes_scoped(entry: &Path) -> std::io::Result<(String, HashSet<String>)> {
    let base = entry
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    resolve_includes_scoped_with_resolver(entry, &DefaultResolver::new(base))
}

/// Like [`resolve_includes_scoped`], but uses a custom [`ModuleResolver`] for `import` resolution.
pub fn resolve_includes_scoped_with_resolver(
    entry: &Path,
    resolver: &dyn ModuleResolver,
) -> std::io::Result<(String, HashSet<String>)> {
    // Make the entry ABSOLUTE first. The search for a project's own `lib/` walks up from the
    // entry's PARENT, and the parent of a relative path like `prog.snd` is the empty path, so the
    // walk never starts and the only root left is the one under $HOME. The result is a compiler
    // that resolves the same source differently depending on how the path was typed, and that is
    // invisible on a machine with the standard library installed in the home directory: it only
    // shows up in a container with an empty HOME, as a missing include that names the file rather
    // than the reason. `absolute` rather than `canonicalize`: this needs the walk-up to start, not
    // symlinks resolved, and it must not fail on a path that does not exist yet.
    let owned = std::path::absolute(entry).unwrap_or_else(|_| entry.to_path_buf());
    let entry: &Path = &owned;
    // First pass: walk the include graph, recording each file's private helper names, its direct
    // include edges, and a mangling id for the files that define private helpers. From that graph
    // we precompute, per file, how each COLLIDING private name (defined privately in more than one
    // file) resolves within THAT file's include closure.
    let mut g = IncludeGraph::default();
    let mut gseen = HashSet::new();
    build_graph(entry, 0, &mut gseen, &mut g, resolver);

    // Validate aliases: an alias must not clash with any module name already visible.
    let visible_modules: HashSet<&String> = g.modules.values().collect();
    for (alias, _original) in &g.aliases {
        if visible_modules.contains(alias) {
            // Find which file introduced this alias to report the line.
            for (file, lines) in &g.source_lines {
                for (i, line) in lines.iter().enumerate() {
                    if parse_import(line.trim()).is_some() && parse_import_alias(line.trim()).as_deref() == Some(alias.as_str()) {
                        g.import_errors.push((
                            file.clone(),
                            i + 1,
                            format!(
                                "alias `{alias}` clashes with the module name `{alias}`; \
                                 rename the import or the module"
                            ),
                        ));
                        break;
                    }
                }
            }
        }
    }

    // A name is colliding if two or more DISTINCT files define it -- privately as before, and
    // now publicly too: two modules are allowed to export the same name, and telling them apart
    // is exactly what the mangling already does for privates.
    let mut name_files: std::collections::HashMap<&String, HashSet<&PathBuf>> =
        std::collections::HashMap::new();
    for (path, names) in &g.defines {
        for n in names {
            name_files.entry(n).or_default().insert(path);
        }
    }
    for (path, names) in &g.exports {
        for n in names {
            name_files.entry(n).or_default().insert(path);
        }
    }
    let colliding: HashSet<String> = name_files
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(name, _)| (*name).clone())
        .collect();

    // Per file, resolve each colliding name to the single definition visible in its include
    // closure (the file itself plus everything it includes, transitively). If the closure sees
    // more than one definer, the name is genuinely ambiguous from that file and is left alone.
    let mut resolve_map: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, u32>,
    > = std::collections::HashMap::new();
    // Per file, the modules it can NAME (its include closure, by module name) and the names that
    // are genuinely ambiguous from there: exported by two of the modules it can see, so an
    // unqualified use has to be turned back rather than guessed at.
    let mut module_maps: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, PathBuf>,
    > = std::collections::HashMap::new();
    let mut ambiguous_maps: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, Vec<String>>,
    > = std::collections::HashMap::new();
    for f in g.edges.keys() {
        let closure = include_closure(f, &g.edges);
        let mut map = std::collections::HashMap::new();
        let mut mods = std::collections::HashMap::new();
        let mut amb: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for name in &colliding {
            let definers: Vec<&PathBuf> = closure
                .iter()
                .filter(|c| {
                    g.defines.get(*c).is_some_and(|d| d.contains(name))
                        || g.exports.get(*c).is_some_and(|e| e.contains(name))
                })
                .collect();
            // A file that defines the name itself sees its OWN definition, not the one in a file
            // it includes: file-local names are the author's to reuse (serial.snd and io.snd both
            // define O_RDWR and that is legal). That definition appears under the file's own id
            // (the depth-0 entry has no id and stays bare), so bind the name to it here -- NOT to
            // the collapsed single id of the other definers. Before this, when the entry (no id)
            // collided with an include, `ids` below shrank to one value and BOTH definitions were
            // renamed `name__f<id>`: a duplicate global at codegen time.
            if definers.contains(&&f) {
                if let Some(id) = g.ids.get(f) {
                    map.insert(name.clone(), *id);
                }
                continue;
            }
            let ids: HashSet<u32> = definers
                .iter()
                .filter_map(|c| g.ids.get(*c).copied())
                .collect();
            if ids.len() == 1 {
                map.insert(name.clone(), *ids.iter().next().unwrap());
                continue;
            }
            // Ambiguity is only reported for a PUBLIC clash. Two files with a private helper or
            // a private constant of the same name are not ambiguous to anybody: neither is
            // visible outside its file, and that case is already file-scoped. Reporting it would
            // turn a private name -- something the author is entitled to choose freely -- into a
            // compile error caused by a file they may never have read.
            let mut exporters: Vec<String> = definers
                .iter()
                .filter(|c| g.exports.get(**c).is_some_and(|e| e.contains(name)))
                .filter_map(|c| g.modules.get(*c).cloned())
                .collect();
            if exporters.len() > 1 {
                exporters.sort();
                amb.insert(name.clone(), exporters);
            }
        }
        for c in &closure {
            if let Some(m) = g.modules.get(c) {
                // If this module name is hidden by an alias in this file, add the
                // alias instead of the original name.
                if let Some(hidden) = g.hidden_modules.get(f) {
                    if hidden.contains(m) {
                        for (alias, orig) in &g.aliases {
                            if orig == m {
                                mods.insert(alias.clone(), c.clone());
                            }
                        }
                        continue;
                    }
                }
                mods.insert(m.clone(), c.clone());
            }
        }
        if !map.is_empty() {
            resolve_map.insert(f.clone(), map);
        }
        module_maps.insert(f.clone(), mods);
        if !amb.is_empty() {
            ambiguous_maps.insert(f.clone(), amb);
        }
    }

    let mut seen = HashSet::new();
    let mut out = String::new();
    let mut depth0 = HashSet::new();
    let mut depth1 = HashSet::new();
    let ns = Namespaces {
        resolve_map: &resolve_map,
        module_maps: &module_maps,
        ambiguous_maps: &ambiguous_maps,
        exports: &g.exports,
        ids: &g.ids,
        colliding: &colliding,
    };
    resolve_into(
        entry,
        &mut seen,
        &mut out,
        0,
        &mut depth0,
        &mut depth1,
        &ns,
        &g.import_errors,
        &g.aliases,
    )?;
    // Which functions is this entry on the hook for covering?
    //
    // A file named `*_test.snd` exists to test what it includes, so the subject is its direct
    // includes and never its own top-level functions, which are test scaffolding.
    //
    // Deciding this by "does the entry define any functions of its own" instead, as this did
    // before, is a trap: adding one helper to a test file reclassified it as a program, and the
    // library under test dropped out of the denominator. A suite covering 1 of 5 library
    // functions then reported 1/1 and sailed through the 80% gate. The gate is the whole point,
    // so the rule it runs on has to be stated rather than inferred.
    let is_test_file = entry
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with("_test.snd"));
    let scope = if is_test_file {
        if depth1.is_empty() { depth0 } else { depth1 }
    } else if depth0.is_empty() {
        depth1
    } else {
        depth0
    };
    Ok((out, scope))
}

/// The include graph gathered in the first pass, keyed by canonical file path: each file's
/// private free-function `defines`, its direct include `edges`, and a mangling `id` for the
/// files that define private helpers (assigned in visit order, at depth >= 1). The entry file
/// (depth 0) gets no id, so a program's own functions are never mangled.
#[derive(Default)]
struct IncludeGraph {
    ids: std::collections::HashMap<PathBuf, u32>,
    defines: std::collections::HashMap<PathBuf, HashSet<String>>,
    /// Each file's `pub` surface: what `module::name` may reach.
    exports: std::collections::HashMap<PathBuf, HashSet<String>>,
    /// Each file's module name, taken from the file name itself.
    modules: std::collections::HashMap<PathBuf, String>,
    edges: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
    ctr: u32,
    /// Errors from failed `import` resolution: (file, line_number, error_message).
    import_errors: Vec<(PathBuf, usize, String)>,
    /// Module aliases from `import X as Y`: maps alias -> original module name.
    aliases: std::collections::HashMap<String, String>,
    /// Per-file: original module names hidden by aliases. When `import X as Y` is in
    /// file F, `X` is hidden from F's namespace so only `Y` works.
    hidden_modules: std::collections::HashMap<PathBuf, HashSet<String>>,
    /// Source lines per file, for line-number reporting in import errors.
    source_lines: std::collections::HashMap<PathBuf, Vec<String>>,
}

/// First pass: walk the include tree once and populate [`IncludeGraph`]. Each visited file records
/// its private helper names and the canonical paths it directly includes, so the emit pass can
/// resolve a colliding private to the definition visible in each file's include closure. Read
/// errors are ignored here; the emit pass reports them.
fn build_graph(
    path: &Path,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    g: &mut IncludeGraph,
    resolver: &dyn ModuleResolver,
) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canon.clone()) {
        return;
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return;
    };
    let mut names = private_free_fn_names(&src);
    // Module-level private globals are file-scoped on collision too (e.g. two bignum modules
    // both defining `let BN_MASK16 = 0xFFFF`), so fold them into the same defined-name set.
    names.extend(module_global_names(&src));
    let exports = public_names(&src);
    // A file needs a mangling id if anything it defines might have to be told apart from
    // somebody else's name of the same spelling -- which now includes its PUBLIC surface, since
    // two modules are allowed to export the same name and be separated by qualification.
    if depth >= 1 && !(names.is_empty() && exports.is_empty()) {
        g.ctr += 1;
        g.ids.insert(canon.clone(), g.ctr);
    }
    g.modules.insert(canon.clone(), module_name(path));
    g.exports.insert(canon.clone(), exports);
    g.defines.insert(canon.clone(), names);
    // Store source lines for line-number reporting on import errors.
    g.source_lines
        .insert(canon.clone(), src.lines().map(String::from).collect());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut es = Vec::new();
    let mut brace_depth: i32 = 0;
    for (line_idx, line) in src.lines().enumerate() {
        // Track brace depth to detect imports inside functions.
        for c in line.chars() {
            match c {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                _ => {}
            }
        }
        if let Some(rel) = parse_include(line.trim()) {
            let target = resolve_target(dir, rel);
            let tcanon = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
            es.push(tcanon);
            build_graph(&target, depth + 1, seen, g, resolver);
        } else if let Some(mod_name) = parse_import(line.trim()) {
            // `import` inside braces (inside a function) is invalid.
            if brace_depth > 0 {
                g.import_errors.push((
                    canon.clone(),
                    line_idx + 1,
                    "`import` is only valid at the top level, not inside a function".to_string(),
                ));
                continue;
            }
            // Resolve the module name to a file path using the injectable resolver.
            match resolver.resolve(mod_name) {
                Ok(target) => {
                    let tcanon =
                        std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
                    es.push(tcanon);
                    // Register alias if present (validation of clashes happens after graph building).
                    if let Some(alias) = parse_import_alias(line.trim()) {
                        g.aliases.insert(alias.to_string(), mod_name.to_string());
                        g.hidden_modules
                            .entry(canon.clone())
                            .or_default()
                            .insert(mod_name.to_string());
                    }
                    build_graph(&target, depth + 1, seen, g, resolver);
                }
                Err(msg) => {
                    g.import_errors
                        .push((canon.clone(), line_idx + 1, msg));
                }
            }
        }
    }
    g.edges.insert(canon, es);
}

/// The set of files reachable from `start` through include edges, including `start` itself: the
/// file's include closure, i.e. everything it can see in Sond's single flat namespace.
fn include_closure(
    start: &PathBuf,
    edges: &std::collections::HashMap<PathBuf, Vec<PathBuf>>,
) -> HashSet<PathBuf> {
    let mut seen = HashSet::new();
    let mut stack = vec![start.clone()];
    while let Some(p) = stack.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        if let Some(es) = edges.get(&p) {
            stack.extend(es.iter().cloned());
        }
    }
    seen
}

#[allow(clippy::too_many_arguments)]
fn resolve_into(
    path: &Path,
    seen: &mut HashSet<PathBuf>,
    out: &mut String,
    depth: usize,
    depth0: &mut HashSet<String>,
    depth1: &mut HashSet<String>,
    ns: &Namespaces,
    import_errors: &[(PathBuf, usize, String)],
    aliases: &std::collections::HashMap<String, String>,
) -> std::io::Result<()> {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(canon) {
        return Ok(()); // already included, skip to avoid duplicate definitions
    }
    // Name the file that actually failed. Without this the caller reports the ENTRY path for
    // every read error, so a missing NESTED include (`any/json.snd` not on any root) is blamed on
    // the top file, which is deeply confusing to debug.
    let src = std::fs::read_to_string(path).map_err(|e| {
        std::io::Error::new(e.kind(), format!("cannot read `{}`: {e}", path.display()))
    })?;
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    // File-scope the COLLIDING private helpers this file can see, renaming each occurrence to
    // `name__f<id>` where `<id>` is the defining file in THIS file's include closure. That binds a
    // cross-file CALLER to the same mangled name as the definition it includes, not just the file
    // that defines the name. A unique private name is not in the map and is left alone. The entry
    // file (depth 0) has no id, so `main` and the program's own functions are never renamed.
    let empty_map = std::collections::HashMap::new();
    let empty_mods = std::collections::HashMap::new();
    let empty_amb = std::collections::HashMap::new();
    let ctx = LineCtx {
        map: ns.resolve_map.get(&canon).unwrap_or(&empty_map),
        modules: ns.module_maps.get(&canon).unwrap_or(&empty_mods),
        exports: ns.exports,
        ids: ns.ids,
        ambiguous: ns.ambiguous_maps.get(&canon).unwrap_or(&empty_amb),
        colliding: ns.colliding,
        aliases,
    };
    // Report import errors for THIS file before processing lines, so they surface
    // before any `module::name` rewrite error on a subsequent line.
    for (err_file, line_num, msg) in import_errors {
        if err_file == &canon {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: line {line_num}: {msg}", path.display()),
            ));
        }
    }
    for line in src.lines() {
        match parse_include(line.trim()) {
            Some(rel) => resolve_into(
                &resolve_target(dir, rel),
                seen,
                out,
                depth + 1,
                depth0,
                depth1,
                ns,
                import_errors,
                aliases,
            )?,
            None if let Some(mod_name) = parse_import(line.trim()) => {
                // Resolve the import: find the module's canonical path and inline it.
                // For aliased imports, the alias replaces the original name in module_maps,
                // so we must look up by the alias if present.
                let lookup_name = parse_import_alias(line.trim()).unwrap_or(mod_name);
                if let Some(file) = ns.module_maps.get(&canon).and_then(|m| m.get(lookup_name)) {
                    resolve_into(
                        file,
                        seen,
                        out,
                        depth + 1,
                        depth0,
                        depth1,
                        ns,
                        import_errors,
                        aliases,
                    )?;
                }
                // If the import had an error, it was already reported above.
            }
            None => {
                // A name that cannot be resolved is reported HERE, naming the file it was
                // written in. Left to the code generator it would surface as an undefined
                // mangled symbol, which names nothing anybody typed.
                let owned = rewrite_line(line, &ctx).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{}: {e}", path.display()),
                    )
                })?;
                let line: &str = &owned;
                if let Some(name) = extract_fn_name(line) {
                    match depth {
                        0 => {
                            depth0.insert(name);
                        }
                        1 => {
                            depth1.insert(name);
                        }
                        _ => {}
                    }
                }
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(())
}

/// The name declared by a top-level `fn`/`pub fn`/`priv fn` line, if any (methods and
/// `test` blocks are not free functions and are ignored, the coverage scope of a library is
/// its free functions).
fn extract_fn_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub ")
        .or_else(|| t.strip_prefix("priv "))
        .unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Resolves an include path, trying a short list of CANDIDATES in order until one
/// exists, so a program may write the short `include "serial"` and reach
/// `any/serial.snd`. For a given `rel` the candidates are: the path as written, the
/// same with `.snd` appended (so the extension is optional), then both again under
/// the `any/` prefix (the target-agnostic library root, tried only when the exact
/// path is not found). Each candidate is searched through the roots below.
fn resolve_target(dir: &Path, rel: &str) -> PathBuf {
    for cand in include_candidates(rel) {
        if let Some(found) = find_in_roots(dir, &cand) {
            return found;
        }
    }
    dir.join(rel) // fall back to the path as written (error surfaces on read)
}

/// The ordered candidate paths for an include of `rel`: exact first (so an explicit
/// `any/serial.snd` resolves immediately and unchanged), then `.snd`-appended, then
/// the same two under `any/`. The `any/` prefix makes `"serial"` an alias of
/// `"any/serial"` when the exact path is not found.
pub(crate) fn include_candidates(rel: &str) -> Vec<String> {
    let has_ext = rel.ends_with(".snd");
    let mut cands = vec![rel.to_string()];
    if !has_ext {
        cands.push(format!("{rel}.snd"));
    }
    // Only alias into `any/` when the path is not already an `any/` path.
    if !rel.starts_with("any/") {
        cands.push(format!("any/{rel}"));
        if !has_ext {
            cands.push(format!("any/{rel}.snd"));
        }
    }
    cands
}

/// Searches for one candidate path through the roots, in order: relative to the
/// including file, then a `lib/` root found by walking up the tree, then the system
/// library `~/.local/lib/sond/`. The system root lets an isolated program (e.g. a
/// sandboxed AI worker) pull in the shared libraries by their canonical path, while
/// seeing only the `.snd` libraries, never the compiler's own source.
fn find_in_roots(dir: &Path, rel: &str) -> Option<PathBuf> {
    let direct = dir.join(rel);
    if direct.exists() {
        return Some(direct);
    }
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let candidate = d.join("lib").join(rel);
        if candidate.exists() {
            return Some(candidate);
        }
        cur = d.parent();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let sys = Path::new(&home).join(".local/lib/sond").join(rel);
        if sys.exists() {
            return Some(sys);
        }
    }
    None
}

/// If `line` is a top-level `include "..."` directive, returns the quoted path.
fn parse_include(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("include")?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// If `line` is a top-level `import <name>` directive, returns the module name.
/// Also matches `import <name> as <alias>` (returns the original name, not the alias).
fn parse_import(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("import")?.trim_start();
    let end = rest
        .chars()
        .position(|c| c.is_whitespace())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

/// If `line` is a top-level `import <name> as <alias>` directive, returns the alias.
fn parse_import_alias(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("import")?.trim_start();
    let after_name = rest.chars().position(|c| c.is_whitespace())?;
    let rest = rest[after_name..].trim_start();
    let rest = rest.strip_prefix("as")?.trim_start();
    let end = rest
        .chars()
        .position(|c| c.is_whitespace() || c == '/')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // One counter for the WHOLE test module, so every fixture path is unique across all tests,
    // which run in parallel. A per-test counter (each starting at 0) made two different tests build
    // the same temp path and race on it, a flaky failure.
    static UID: AtomicUsize = AtomicUsize::new(0);
    fn uid() -> usize {
        UID.fetch_add(1, Ordering::Relaxed)
    }

    // The walk-up that finds a project's own `lib/` starts at the entry file's PARENT. For a
    // RELATIVE entry (`sondc prog.snd`, which is how everyone types it) that parent is the empty
    // path, so the walk never starts and the only root left is the one in $HOME. On a developer
    // machine with the standard library installed there, nothing is ever noticed; in a container
    // with an empty HOME the same source stops compiling, and the error names the include rather
    // than the reason. Same compilation must resolve the same way however the path is written.
    #[test]
    fn a_relative_entry_finds_the_project_lib_like_an_absolute_one_does() {
        let dir = std::env::temp_dir().join(format!("sond_rel_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(dir.join("lib/mylib")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("lib/mylib/helper.snd"),
            "fn helper_answer() -> i64 { 42 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/prog.snd"),
            "include \"mylib/helper.snd\"\nfn main() -> i64 { helper_answer() }\n",
        )
        .unwrap();

        let from_absolute = resolve_includes_scoped(&dir.join("src/prog.snd"))
            .expect("an absolute entry resolves the project lib");

        // The relative form, resolved from the same directory the author would be sitting in.
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.join("src")).unwrap();
        let from_relative = resolve_includes_scoped(Path::new("prog.snd"));
        std::env::set_current_dir(prev).unwrap();

        let from_relative = from_relative.expect("a relative entry must resolve it too");
        assert!(
            from_relative.0.contains("helper_answer"),
            "the relative form pulled in the include: {}",
            from_relative.0
        );
        assert_eq!(
            from_absolute.0, from_relative.0,
            "both spellings of the same entry must produce the same program"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("{}_{}", uid(), name));
        writeln!(std::fs::File::create(&p).unwrap(), "{body}").unwrap();
        p
    }

    // A fresh temp directory that is a `lib/` root: `root/lib/<subpath>` plus a `root/main.snd`
    // entry that includes `include_line`. Returns the entry path. Used to exercise include
    // resolution against a walk-up `lib/` root without touching the real ~/.local/lib/sond.
    fn lib_fixture(subpath: &str, body: &str, include_line: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sond_res_{}_{}", std::process::id(), uid()));
        let lib_file = root.join("lib").join(subpath);
        std::fs::create_dir_all(lib_file.parent().unwrap()).unwrap();
        std::fs::write(&lib_file, body).unwrap();
        let entry = root.join("main.snd");
        std::fs::write(&entry, format!("{include_line}\nfn main() -> i64 {{ 0 }}")).unwrap();
        entry
    }

    #[test]
    fn a_bare_name_resolves_under_any_with_the_snd_extension_added() {
        // `include "serial"` must reach `any/serial.snd`: the extension is optional and the
        // `any/` prefix is tried when the exact path is not found (JMV's alias request).
        let entry = lib_fixture(
            "any/serial.snd",
            "fn serial_getc() -> i64 { 7 }",
            "include \"serial\"",
        );
        let combined = resolve_includes(&entry).unwrap();
        assert!(
            combined.contains("fn serial_getc"),
            "`include \"serial\"` pulled in any/serial.snd"
        );
    }

    #[test]
    fn the_snd_extension_is_optional_on_an_explicit_any_path() {
        let entry = lib_fixture(
            "any/frame.snd",
            "fn frame_encode() -> i64 { 1 }",
            "include \"any/frame\"",
        );
        let combined = resolve_includes(&entry).unwrap();
        assert!(
            combined.contains("fn frame_encode"),
            "`any/frame` resolved to any/frame.snd"
        );
    }

    #[test]
    fn an_explicit_full_path_still_resolves_exactly() {
        // The alias must not change behaviour for the canonical, extension-carrying paths the
        // whole stdlib already uses.
        let entry = lib_fixture(
            "x86_64/linux/io.snd",
            "fn close_fd(fd: i64) -> i64 { 0 }",
            "include \"x86_64/linux/io.snd\"",
        );
        let combined = resolve_includes(&entry).unwrap();
        assert!(combined.contains("fn close_fd"), "explicit path unchanged");
    }

    #[test]
    fn a_bare_and_a_full_include_of_the_same_file_are_deduped() {
        // `include "serial"` and `include "any/serial.snd"` name the same file; it must appear
        // once, not twice (which would be duplicate definitions).
        let root = std::env::temp_dir().join(format!("sond_resd_{}_{}", std::process::id(), uid()));
        let lib_file = root.join("lib").join("any").join("serial.snd");
        std::fs::create_dir_all(lib_file.parent().unwrap()).unwrap();
        std::fs::write(&lib_file, "fn serial_putc(b: i64) { }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"serial\"\ninclude \"any/serial.snd\"\nfn main() -> i64 { 0 }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert_eq!(
            combined.matches("fn serial_putc").count(),
            1,
            "included once despite two spellings"
        );
    }

    #[test]
    fn a_missing_nested_include_names_the_nested_file_not_the_entry() {
        // When an include cannot be resolved on any root, the error must name the file that was
        // not found, NOT the entry file. Before this, every read failure was blamed on the entry,
        // so a missing `any/json.snd` read as "cannot read main.snd", which is deeply misleading.
        let main = tmp(
            &format!("sond_missinginc.snd"),
            "include \"any/does_not_exist_xyz.snd\"\nfn main() -> i64 { 0 }",
        );
        let err = resolve_includes(&main).unwrap_err().to_string();
        assert!(
            err.contains("does_not_exist_xyz"),
            "the error names the missing include, got: {err}"
        );
        assert!(
            !err.contains("sond_missinginc"),
            "the error does not blame the entry file, got: {err}"
        );
    }

    #[test]
    fn a_colliding_private_helper_is_file_scoped_a_unique_one_is_left_alone() {
        let root =
            std::env::temp_dir().join(format!("sond_fscope_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        // Two included libs each define a PRIVATE `secret` (a collision); libb also has a unique
        // private `only_b`. The entry includes both.
        std::fs::write(
            root.join("a.snd"),
            "fn secret() -> i64 { 1 }\npub fn a() -> i64 { secret() }",
        )
        .unwrap();
        std::fs::write(
            root.join("b.snd"),
            "fn secret() -> i64 { 2 }\nfn only_b() -> i64 { 9 }\npub fn b() -> i64 { secret() + only_b() }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"a.snd\"\ninclude \"b.snd\"\nfn main() -> i64 { a() + b() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();
        // The colliding `secret` is renamed per file: no bare `fn secret(` survives.
        assert_eq!(
            combined.matches("fn secret(").count(),
            0,
            "the colliding private `secret` is file-scoped in both files:\n{combined}"
        );
        assert_eq!(
            combined.matches("__f").count(),
            4,
            "two definitions + two call sites of `secret` are mangled:\n{combined}"
        );
        // The unique private `only_b` is NOT touched (nothing that calls it across files breaks).
        assert!(
            combined.contains("fn only_b()"),
            "a unique private helper is left alone"
        );
    }

    #[test]
    fn a_cross_file_call_to_a_colliding_private_binds_to_the_definer_in_its_closure() {
        // The real bug behind the bignum `rbn_` workaround: two libs define the same PRIVATE
        // name, and a THIRD file that includes only ONE of them CALLS it. That caller does not
        // itself define the name, so the old emit pass (which mangled only names a file defines)
        // left its call BARE, while both definitions were mangled per file. Result: the bare
        // call resolved to nothing. A caller must bind to the definition visible in ITS include
        // closure.
        let root =
            std::env::temp_dir().join(format!("sond_xfile_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.snd"), "fn shared() -> i64 { 1 }").unwrap();
        std::fs::write(root.join("b.snd"), "fn shared() -> i64 { 2 }").unwrap();
        // c includes ONLY b, and calls `shared` without defining it: it must bind to b's `shared`.
        std::fs::write(
            root.join("c.snd"),
            "include \"b.snd\"\npub fn c_call() -> i64 { shared() }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"a.snd\"\ninclude \"c.snd\"\nfn main() -> i64 { c_call() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();

        // Both definitions are file-scoped: no bare `fn shared(` survives.
        assert_eq!(
            combined.matches("fn shared(").count(),
            0,
            "both colliding definitions are mangled:\n{combined}"
        );
        // c's cross-file call is NOT left bare (the bug): it is mangled.
        assert!(
            !combined.contains("{ shared() }"),
            "c's cross-file call must be mangled, not left bare:\n{combined}"
        );
        // And it binds to the SAME mangled name as b's definition (`{ 2 }`), the definer in c's
        // closure, NOT a's (`{ 1 }`).
        let b_mangled = mangled_name_on_line_with(&combined, "{ 2 }");
        assert!(
            combined.contains(&format!("{{ {b_mangled}() }}")),
            "c must call b's `{b_mangled}` (the definer it includes):\n{combined}"
        );
    }

    // Extracts the `shared__f<id>` token from the (single) line that also contains `marker`.
    fn mangled_name_on_line_with(combined: &str, marker: &str) -> String {
        let line = combined
            .lines()
            .find(|l| l.contains("shared__f") && l.contains(marker))
            .unwrap_or_else(|| {
                panic!("no mangled `shared__f` line with `{marker}` in:\n{combined}")
            });
        let start = line.find("shared__f").unwrap();
        line[start..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    }

    #[test]
    fn a_colliding_module_level_global_is_file_scoped() {
        // The same file-scoping must cover module-level `let` globals, not only functions: two
        // bignum libraries each define a private `let MASK = ...` and use it internally, and a
        // program that pulls in both would otherwise fail with "declared more than once".
        let root = std::env::temp_dir().join(format!("sond_glob_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a.snd"),
            "let MASK = 255\npub fn a() -> i64 { 7 & MASK }",
        )
        .unwrap();
        std::fs::write(
            root.join("b.snd"),
            "let MASK = 255\npub fn b() -> i64 { 9 & MASK }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"a.snd\"\ninclude \"b.snd\"\nfn main() -> i64 { a() + b() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert_eq!(
            combined.matches("let MASK =").count(),
            0,
            "the colliding global `MASK` is file-scoped in both files:\n{combined}"
        );
        // two definitions + two uses of MASK are mangled
        assert_eq!(
            combined.matches("MASK__f").count(),
            4,
            "both definitions and both uses of `MASK` are mangled:\n{combined}"
        );
    }

    #[test]
    fn a_uniquely_named_global_shared_across_files_is_left_alone() {
        // A global defined in ONE file and used in another (a shared constant) must NOT be
        // mangled: it does not collide, so cross-file use keeps working.
        let root =
            std::env::temp_dir().join(format!("sond_globshare_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("k.snd"), "let SHARED = 42").unwrap();
        std::fs::write(
            root.join("u.snd"),
            "include \"k.snd\"\npub fn u() -> i64 { SHARED }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(&entry, "include \"u.snd\"\nfn main() -> i64 { u() }").unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert!(
            combined.contains("let SHARED = 42"),
            "a unique shared global is untouched:\n{combined}"
        );
        assert!(
            !combined.contains("SHARED__f"),
            "a non-colliding global is never mangled:\n{combined}"
        );
    }

    #[test]
    fn inlines_an_included_file_once() {
        let lib = tmp(&format!("sond_lib.snd"), "fn helper() -> i64 { 42 }");
        let libname = lib.file_name().unwrap().to_str().unwrap();
        // main includes the lib TWICE; it must appear only once.
        let main = tmp(
            &format!("sond_main.snd"),
            &format!(
                "include \"{libname}\"\ninclude \"{libname}\"\nfn main() -> i64 {{ helper() }}"
            ),
        );
        let combined = resolve_includes(&main).unwrap();
        assert_eq!(combined.matches("fn helper").count(), 1, "included once");
        assert!(combined.contains("fn main"), "keeps the main file");
    }

    #[test]
    fn a_test_files_coverage_scope_is_the_library_it_includes_not_its_own_helpers() {
        // The gate that makes a Sond program refuse to build without 80% coverage is only
        // worth anything if it measures the code under test. A `_test.snd` file that happens
        // to define a helper of its own must not shrink the denominator to that helper: that
        // turns "1 of 5 library functions covered" into a green "1 of 1".
        let lib = tmp(
            &format!("sond_covlib.snd"),
            "fn a1() -> i64 { 1 }\nfn a2() -> i64 { 2 }\nfn a3() -> i64 { 3 }",
        );
        let libname = lib.file_name().unwrap().to_str().unwrap();
        let entry = tmp(
            &format!("sond_covscope_test.snd"),
            &format!("include \"{libname}\"\nfn helper() -> i64 {{ 7 }}\ntest \"t\" {{ }}"),
        );
        let (_, scope) = resolve_includes_scoped(&entry).unwrap();
        assert!(scope.contains("a1"), "the library under test is the scope");
        assert!(scope.contains("a3"), "all of it, not a sample");
        assert!(
            !scope.contains("helper"),
            "a helper in the test file is scaffolding, not the subject"
        );
    }

    #[test]
    fn a_program_is_still_measured_on_its_own_functions() {
        // The counterpart: a program that includes the standard library is not on the hook
        // for covering the standard library.
        let lib = tmp(&format!("sond_progdep.snd"), "fn dep() -> i64 { 1 }");
        let libname = lib.file_name().unwrap().to_str().unwrap();
        let entry = tmp(
            &format!("sond_prog.snd"),
            &format!("include \"{libname}\"\nfn mine() -> i64 {{ dep() }}\ntest \"t\" {{ }}"),
        );
        let (_, scope) = resolve_includes_scoped(&entry).unwrap();
        assert!(scope.contains("mine"), "its own functions");
        assert!(!scope.contains("dep"), "not its dependencies");
    }

    #[test]
    fn a_line_that_only_starts_with_include_is_not_a_directive() {
        // `included` / `include_x` are ordinary identifiers, not directives.
        assert_eq!(parse_include("included = 5"), None);
        assert_eq!(parse_include("let include_flag = 1"), None);
        assert_eq!(parse_include("include \"lib.snd\""), Some("lib.snd"));
    }

    #[test]
    fn a_qualified_name_reaches_a_module_by_its_file_name() {
        // The first step of RFC namespaces: every included file is a MODULE named after itself,
        // and `module::name` says which one you mean. Resolved here, at include time, into the
        // symbol that module actually defines -- so the lexer, the parser and the code generator
        // never see a `::` and nothing downstream changes.
        let root = std::env::temp_dir().join(format!("sond_ns1_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("dns.snd"), "pub fn resolve() -> i64 { 7 }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"dns.snd\"\nfn main() -> i64 { dns::resolve() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert!(
            !combined.contains("::"),
            "no `::` survives resolution:\n{combined}"
        );
        assert!(
            combined.contains("resolve()"),
            "it binds to the module's function:\n{combined}"
        );
    }

    #[test]
    fn two_modules_may_export_the_same_name_when_each_use_says_which() {
        // The point of the whole exercise. Today two libraries that both export `read` cannot be
        // used by one program at all: it is a hard "defined more than once". Qualified, they can.
        let root = std::env::temp_dir().join(format!("sond_ns2_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("disk.snd"), "pub fn read() -> i64 { 1 }").unwrap();
        std::fs::write(root.join("net.snd"), "pub fn read() -> i64 { 2 }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"disk.snd\"\ninclude \"net.snd\"\nfn main() -> i64 { disk::read() + net::read() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).unwrap();
        assert_eq!(
            combined.matches("fn read(").count(),
            0,
            "both definitions are renamed apart:\n{combined}"
        );
        assert!(
            !combined.contains("::"),
            "and both uses are resolved:\n{combined}"
        );
    }

    #[test]
    fn an_unqualified_name_two_modules_export_says_which_two() {
        // The diagnostic is the feature. Left alone, the ambiguous name would compile to
        // something undefined and the error would land in the code generator, naming a mangled
        // symbol nobody wrote.
        let root = std::env::temp_dir().join(format!("sond_ns3_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("disk.snd"), "pub fn read() -> i64 { 1 }").unwrap();
        std::fs::write(root.join("net.snd"), "pub fn read() -> i64 { 2 }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"disk.snd\"\ninclude \"net.snd\"\nfn main() -> i64 { read() }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("read"),
            "it names the ambiguous name, got: {err}"
        );
        assert!(
            err.contains("disk") && err.contains("net"),
            "and both modules that export it, got: {err}"
        );
    }

    #[test]
    fn a_qualified_name_cannot_reach_a_module_that_is_not_included() {
        let root = std::env::temp_dir().join(format!("sond_ns4_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("dns.snd"), "pub fn resolve() -> i64 { 7 }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(&entry, "fn main() -> i64 { dns::resolve() }").unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(err.contains("dns"), "it names the module, got: {err}");
    }

    #[test]
    fn a_qualified_name_cannot_reach_a_private_helper() {
        // `pub` is what a module offers; qualifying must not be a way around it, or the
        // visibility that already exists would stop meaning anything the moment `::` landed.
        let root = std::env::temp_dir().join(format!("sond_ns5_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("dns.snd"),
            "fn helper() -> i64 { 7 }\npub fn resolve() -> i64 { helper() }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"dns.snd\"\nfn main() -> i64 { dns::helper() }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("helper") && err.contains("dns"),
            "it says which name in which module, got: {err}"
        );
    }

    #[test]
    fn an_entry_that_redefines_an_included_global_keeps_its_own() {
        // The repro behind io.snd/serial.snd: the entry (depth 0) has NO mangling id, so when it
        // defines a module-level global that an included file also defines, the only id that
        // survives is the include's -- and the old code renamed BOTH definitions to
        // `name__f<id>`, i.e. a `global `O_RDWR__f1` is declared more than once` duplicate. The
        // entry's own definition must stay bare; only the include's is file-scoped.
        let root = std::env::temp_dir().join(format!("sond_entglob_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.snd"), "let O_RDWR = 2\npub fn a() -> i64 { O_RDWR }").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"a.snd\"\nlet O_RDWR = 99\nfn main() -> i64 { O_RDWR }",
        )
        .unwrap();
        let combined = resolve_includes(&entry)
            .expect("the collision must resolve, not become a duplicate definition");
        assert_eq!(
            combined.matches("let O_RDWR =").count(),
            1,
            "exactly one BARE definition survives (the entry's own):\n{combined}"
        );
        assert!(
            combined.contains("let O_RDWR = 99"),
            "the entry keeps its own global unmangled:\n{combined}"
        );
        assert!(
            combined.contains("let O_RDWR__f1 = 2"),
            "the include's definition is file-scoped:\n{combined}"
        );
        assert!(
            combined.contains("fn main() -> i64 { O_RDWR }"),
            "the entry's use binds to its own bare global:\n{combined}"
        );
        assert!(
            combined.contains("O_RDWR__f1 }"),
            "the include's use binds to its mangled global:\n{combined}"
        );
    }

    #[test]
    fn two_nested_includes_each_keep_their_own_colliding_global() {
        // The exact shape of the tree KO: `serial.snd` includes `io.snd`, and BOTH define a
        // private module-level global of the same name (both depth >= 1, both with ids). Each
        // must be file-scoped to its own definition and its own uses -- no duplicate, no
        // rebinding where one file starts reading the other's value.
        let root = std::env::temp_dir().join(format!("sond_nestglob_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("io.snd"), "let O_ROLE = 1\npub fn io_role() -> i64 { O_ROLE }").unwrap();
        std::fs::write(
            root.join("serial.snd"),
            "include \"io.snd\"\nlet O_ROLE = 2\npub fn serial_role() -> i64 { O_ROLE }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"serial.snd\"\nfn main() -> i64 { serial_role() + io_role() }",
        )
        .unwrap();
        let combined = resolve_includes(&entry)
            .expect("the nested collision must resolve, not become a duplicate definition");
        assert_eq!(
            combined.matches("let O_ROLE =").count(),
            0,
            "both definitions are file-scoped (none left bare):\n{combined}"
        );
        // io.snd's definition and use bind to io's id; serial.snd's to serial's id; each function
        // reads its own file's constant (1 and 2), not both reading one or the other.
        let io_def_line = combined
            .lines()
            .find(|l| l.contains("let O_ROLE__") && l.contains("= 1"))
            .unwrap_or_else(|| panic!("io's file-scoped definition must survive:\n{combined}"));
        let io_id = &io_def_line[io_def_line.find("O_ROLE__").unwrap() + 8..]
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect::<String>();
        let serial_def_line = combined
            .lines()
            .find(|l| l.contains("let O_ROLE__") && l.contains("= 2"))
            .unwrap_or_else(|| panic!("serial's file-scoped definition must survive:\n{combined}"));
        let serial_id = &serial_def_line[serial_def_line.find("O_ROLE__").unwrap() + 8..]
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect::<String>();
        assert_ne!(io_id, serial_id, "the two files must NOT share a mangled id:\n{combined}");
        assert!(
            combined.contains(&format!("serial_role() -> i64 {{ O_ROLE__{serial_id} }}")),
            "serial reads its own {serial_id}-mangled constant:\n{combined}"
        );
        assert!(
            combined.contains(&format!("io_role() -> i64 {{ O_ROLE__{io_id} }}")),
            "io reads its own {io_id}-mangled constant:\n{combined}"
        );
    }

    #[test]
    fn a_qualified_name_for_a_module_without_exports_fails_resolution() {
        // A module that exports nothing cannot be queried for any name via `::`.
        let root = std::env::temp_dir().join(format!("sond_ns6_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("empty.snd"), "// no functions, no exports").unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "include \"empty.snd\"\nfn main() -> i64 { empty::thing() }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(err.contains("thing") && err.contains("empty"), "error names both, got: {err}");
    }

    // ── import directive tests ────────────────────────────────────────────

    // CP1: `import strings` empalma el módulo y `strings::itoa(7)` resuelve.
    #[test]
    fn an_import_empalmes_the_module_and_qualified_name_resolves() {
        let root = std::env::temp_dir().join(format!("sond_imp1_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "import strings\nfn main() -> i64 { strings::itoa(7) }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).expect("import must resolve");
        assert!(
            !combined.contains("::"),
            "no `::` survives resolution:\n{combined}"
        );
        assert!(
            combined.contains("itoa(7)"),
            "it binds to the module's function:\n{combined}"
        );
    }

    // CP2: `import strings as str` alias renombra el módulo en ese fichero.
    #[test]
    fn an_import_as_alias_renames_the_module_and_original_name_is_gone() {
        let root = std::env::temp_dir().join(format!("sond_imp2_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "import strings as str\nfn main() -> i64 { str::itoa(7) }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).expect("import as must resolve");
        assert!(
            !combined.contains("::"),
            "no `::` survives resolution:\n{combined}"
        );
        assert!(
            combined.contains("itoa(7)"),
            "alias resolves to the function:\n{combined}"
        );
        // The original name `strings` must NOT resolve any more.
        let entry2 = root.join("main2.snd");
        std::fs::write(
            &entry2,
            "import strings as str\nfn main() -> i64 { strings::itoa(7) }",
        )
        .unwrap();
        let err = resolve_includes(&entry2).unwrap_err().to_string();
        assert!(
            err.contains("strings"),
            "original name is gone, error names it: {err}"
        );
    }

    // CP3: A private function of the imported module is NOT reachable via `::`.
    #[test]
    fn a_private_fn_of_an_imported_module_is_not_reachable() {
        let root = std::env::temp_dir().join(format!("sond_imp3_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/math.snd"),
            "fn secret() -> i64 { 42 }\npub fn answer() -> i64 { secret() }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "import math\nfn main() -> i64 { math::secret() }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("secret") && err.contains("math"),
            "error names the private name and the module, got: {err}"
        );
    }

    // CP4: `import` of an unknown name is a hard error naming the import, its line, and
    // saying it must be declared.
    #[test]
    fn an_import_of_an_unknown_name_is_a_hard_error_with_line() {
        let root = std::env::temp_dir().join(format!("sond_imp4_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(&root).unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "fn dummy() -> i64 { 0 }\nimport nonexistent\nfn main() -> i64 { 0 }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("import"),
            "error names the `import` keyword, got: {err}"
        );
        assert!(
            err.contains("nonexistent"),
            "error names the unknown module, got: {err}"
        );
        assert!(
            err.contains("line 2"),
            "error includes the line number (line 2), got: {err}"
        );
    }

    // CP5: Importing the same module twice does not duplicate definitions.
    #[test]
    fn importing_the_same_module_twice_does_not_duplicate() {
        let root = std::env::temp_dir().join(format!("sond_imp5_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "import strings\nimport strings\nfn main() -> i64 { strings::itoa(7) }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).expect("double import must not fail");
        assert_eq!(
            combined.matches("fn itoa(").count(),
            1,
            "included once despite two import lines:\n{combined}"
        );
    }

    // CP6: An `import` and an `include` of the same file do not define twice.
    #[test]
    fn import_and_include_of_the_same_file_dedup() {
        let root = std::env::temp_dir().join(format!("sond_imp6_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "import strings\ninclude \"lib/strings.snd\"\nfn main() -> i64 { strings::itoa(7) }",
        )
        .unwrap();
        let combined = resolve_includes(&entry).expect("import+include must not fail");
        assert_eq!(
            combined.matches("fn itoa(").count(),
            1,
            "included once despite import + include:\n{combined}"
        );
    }

    // CP7: `import` only valid at top level; inside a function is an error with line.
    #[test]
    fn an_import_inside_a_function_is_an_error_with_line() {
        let root = std::env::temp_dir().join(format!("sond_imp7_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        std::fs::write(
            &entry,
            "fn main() -> i64 {\n  import strings\n  strings::itoa(7)\n}",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("import"),
            "error names the `import` keyword, got: {err}"
        );
        assert!(
            err.contains("2"),
            "error includes the line number (line 2), got: {err}"
        );
    }

    // CP8: An alias that clashes with an already-visible module name is an error.
    #[test]
    fn an_alias_that_clashes_with_a_visible_module_name_is_an_error() {
        let root = std::env::temp_dir().join(format!("sond_imp8_{}_{}", std::process::id(), uid()));
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/strings.snd"),
            "pub fn itoa(n: i64) -> i64 { n }",
        )
        .unwrap();
        std::fs::write(
            root.join("lib/io.snd"),
            "pub fn read() -> i64 { 1 }",
        )
        .unwrap();
        let entry = root.join("main.snd");
        // Import `strings` as `io` — but `io` is already a visible module.
        std::fs::write(
            &entry,
            "import strings as io\ninclude \"lib/io.snd\"\nfn main() -> i64 { io::read() }",
        )
        .unwrap();
        let err = resolve_includes(&entry).unwrap_err().to_string();
        assert!(
            err.contains("io"),
            "error names the clashing name, got: {err}"
        );
    }
}

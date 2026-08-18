//! The RFC-0016 line-to-locals symbol table (§3.3), synthesized as Sond globals.
//!
//! The debugger needs to name the debuggee's locals: given the current source line and the frame
//! base, which words in the frame are which variables. The compiler already knows this (the frame
//! layout of every function), so a `--debug` build folds it into the program as ordinary Sond data
//! globals that the agent reads with raw `peek`, keeping the whole thing self-contained (no sidecar
//! file, no external symbol the agent could not reference):
//!
//! - `__dbg_nfuncs`: scalar, the number of functions in the table.
//! - `__dbg_fn[F*4]`: per function, `[line_lo, line_hi, local_start, local_count]`. A function owns
//!   the half-open line span up to the next function's first line, so a checkpoint line falls in
//!   exactly one function (functions are disjoint text regions).
//! - `__dbg_loc[K*3]`: per local, `[frame_offset, name_start, name_len]`, grouped by function.
//! - `__dbg_names`: every local name concatenated (no separators); a local's bytes are
//!   `__dbg_names[name_start .. name_start + name_len]`.
//!
//! Offsets are rbp-relative (negative), taken from [`crate::codegen::frame_layout`] so they match the
//! prologue exactly. Only scalar params and locals are listed; arrays and the internal return slot
//! are skipped (they are not single-word values to print). The agent walks this to answer `locals`.

use crate::ast::{Expr, Function, Global, Visibility};
use crate::codegen::frame_layout;

/// Builds the symbol-table globals for a `--debug` program. Pass the whole function list; the debug
/// agent (the `__dbg_` namespace) and test blocks are skipped, leaving the user's own code.
pub fn build_symtab_globals(funcs: &[Function]) -> Vec<Global> {
    // Keep the user functions that actually carry checkpoints (at least one statement line), in the
    // order they appear in the resolved source. Their first statement line orders them, and each
    // owns the span up to the next function's first line, so nested lines are covered too.
    let mut entries: Vec<(u32, &Function)> = funcs
        .iter()
        .filter(|f| !f.is_test && !f.name.starts_with("__dbg_"))
        .filter_map(|f| f.body.stmt_lines.iter().copied().min().map(|lo| (lo, f)))
        .collect();
    entries.sort_by_key(|(lo, _)| *lo);

    let mut names = String::new();
    let mut loc_words: Vec<i64> = Vec::new(); // flat [off, name_start, name_len] * K
    let mut fn_words: Vec<i64> = Vec::new(); // flat [lo, hi, local_start, local_count] * F

    for (i, (lo, func)) in entries.iter().enumerate() {
        // The function owns lines up to (but not including) the next function's first line; the last
        // function runs to the end. A checkpoint line therefore resolves to exactly one function.
        let hi = if i + 1 < entries.len() {
            entries[i + 1].0 as i64 - 1
        } else {
            i64::MAX
        };
        let layout = frame_layout(func);
        let array_names: std::collections::HashSet<&str> =
            layout.arrays.iter().map(|(n, _, _)| n.as_str()).collect();

        let local_start = loc_words.len() as i64 / 3;
        // Params first, then plain scalar locals (array pointer slots and the return slot are not
        // single values to show). The order is stable, matching how a reader expects them.
        let scalars = layout
            .params
            .iter()
            .map(|(n, off, _)| (n.as_str(), *off))
            .chain(layout.locals.iter().map(|(n, off)| (n.as_str(), *off)))
            .filter(|(n, _)| !array_names.contains(n));
        let mut count = 0i64;
        for (name, off) in scalars {
            loc_words.push(off as i64);
            loc_words.push(names.len() as i64);
            loc_words.push(name.len() as i64);
            names.push_str(name);
            count += 1;
        }
        fn_words.push(*lo as i64);
        fn_words.push(hi);
        fn_words.push(local_start);
        fn_words.push(count);
    }

    // A zero-length `.data` array is awkward; pad the flat arrays and the name blob so they always
    // have a body. The agent never reads past the counts, so the padding is inert.
    if loc_words.is_empty() {
        loc_words.push(0);
    }
    if fn_words.is_empty() {
        fn_words.push(0);
    }

    let int_array = |name: &str, words: Vec<i64>| Global {
        vis: Visibility::Priv,
        mutable: false,
        name: name.to_string(),
        array_len: Some(words.len() as i64),
        ty: None,
        init: Expr::List(words.into_iter().map(Expr::Int).collect()),
    };

    vec![
        Global {
            vis: Visibility::Priv,
            mutable: false,
            name: "__dbg_nfuncs".to_string(),
            array_len: None,
            ty: None,
            init: Expr::Int(entries.len() as i64),
        },
        int_array("__dbg_fn", fn_words),
        int_array("__dbg_loc", loc_words),
        Global {
            vis: Visibility::Priv,
            mutable: false,
            name: "__dbg_names".to_string(),
            array_len: None,
            ty: None,
            init: Expr::Str(names),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::build_symtab_globals;
    use crate::ast::Expr;
    use crate::lexer::tokenize;
    use crate::parser::Parser;

    fn global<'a>(gs: &'a [crate::ast::Global], name: &str) -> &'a crate::ast::Global {
        gs.iter().find(|g| g.name == name).expect("global present")
    }

    fn ints(e: &Expr) -> Vec<i64> {
        match e {
            Expr::List(items) => items
                .iter()
                .map(|i| match i {
                    Expr::Int(n) => *n,
                    _ => panic!("non-int in the table"),
                })
                .collect(),
            _ => panic!("not a list"),
        }
    }

    #[test]
    fn the_table_names_a_functions_params_and_locals_with_their_offsets() {
        // `add` has a param and two locals; the table must record all three, in order, with the same
        // rbp-relative offsets the codegen prologue assigns (params first: -8, then locals -16, -24).
        let src = "fn add(x: i64) -> i64 {\n  let a = x\n  let b = 1\n  a + b\n}\n";
        let funcs = Parser::new(tokenize(src)).parse_program();
        let g = build_symtab_globals(&funcs);

        assert!(matches!(global(&g, "__dbg_nfuncs").init, Expr::Int(1)));

        // Names are concatenated in order: the param `x`, then locals `a`, `b`.
        let names = match &global(&g, "__dbg_names").init {
            Expr::Str(s) => s.clone(),
            _ => panic!("names is a string"),
        };
        assert_eq!(names, "xab");

        // __dbg_loc is [offset, name_start, name_len] per local. Offsets match the prologue.
        let loc = ints(&global(&g, "__dbg_loc").init);
        assert_eq!(
            loc,
            vec![-8, 0, 1, -16, 1, 1, -24, 2, 1],
            "x@-8, a@-16, b@-24"
        );

        // __dbg_fn is [line_lo, line_hi, local_start, local_count]; the sole function owns 3 locals.
        let f = ints(&global(&g, "__dbg_fn").init);
        assert_eq!(f[2], 0, "locals start at index 0");
        assert_eq!(f[3], 3, "three named slots");
    }

    #[test]
    fn arrays_and_the_debug_agent_are_excluded() {
        // A stack array is not a single-word value, so it is left out; the __dbg_ agent is not user
        // code, so it contributes no rows either.
        let src = "fn f() -> i64 {\n  let s = 7\n  let buf[4]\n  s\n}\n\
                   fn __dbg_helper() -> i64 {\n  let q = 1\n  q\n}\n";
        let funcs = Parser::new(tokenize(src)).parse_program();
        let g = build_symtab_globals(&funcs);
        assert!(
            matches!(global(&g, "__dbg_nfuncs").init, Expr::Int(1)),
            "only `f` counts"
        );
        let names = match &global(&g, "__dbg_names").init {
            Expr::Str(s) => s.clone(),
            _ => panic!(),
        };
        assert_eq!(
            names, "s",
            "the array `buf` and the agent's `q` are excluded"
        );
    }
}

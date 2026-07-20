# lib/compiler: Self-hosted Sond compiler

The Sond compiler written in Sond itself. Each piece is a faithful port of a subsystem from the Rust compiler (`compiler/src/`), verified against the reference by differential testing.

## Map

| File | Status | Description |
|------|--------|-------------|
| `token.snd` | DONE | Token kind constants (all kinds from Rust lexer.rs) |
| `lexer.snd` | DONE | Lexer / tokenizer, converts source bytes to token stream |
| `lexer_test.snd` | DONE | Tests for the lexer, 39 tests, all passing |
| `README.md` | this | This file |

## Bug fixes in lexer.snd port (differential with Rust reference)

- **Hex parsing**: Added `0x` prefix check before calling hex parsing, matching Rust lexer behavior of detecting `0x`/`0X` prefix before treating digits as hex
- **Hex character values**: Fixed uppercase A-F hex digit value computation: `d-55` for uppercase (65-70) instead of `d-87` which underflows for uppercase
- **Keyword length buckets**: Fixed misassigned keywords in `_kw_ident`: moved `break` (5 chars) from len==4 to len==5 bucket, moved `priv` (4 chars) from len==5 to len==4 bucket, added `implies` (7 chars) to len==7 bucket, fixed `_kw_implies` function (was 8-byte check for 7-char word)
- **`>>>` operator**: Moved `>>>` (TK_LSHR) check before `>>` (TK_SHR) check in the multi-char operator block
- **Include paths**: Fixed include from `"lib/compiler/lexer.snd"` → `"lexer.snd"` (relative to this directory)

## How it works

The lexer (`tokenize`) accepts a **Bytes handle** (via `x86_64/linux/bytes.snd`) and returns a **Vec** of kind-value pairs:

- `vec_get(tokens, 2*i)` = token kind (one of the `TK_*` constants)
- `vec_get(tokens, 2*i+1)` = token value (for value-carrying kinds like Int, Str, Ident)

Token kinds are defined in `token.snd`. The mapping is faithful to the Rust lexer's `Token` enum.

## Running tests

```
sond test lib/compiler/lexer_test.snd    # build and run test suite
```

## Coverage

39 tests exercise all keyword, operator, and literal tokenization paths. The lexer.snd functions are covered at 100%.
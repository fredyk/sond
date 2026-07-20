# Case: integer-overflow-defeated bounds check (CWE-190 -> CWE-787)

## CVE

**CVE-2017-7529**, nginx range filter module, integer overflow leading to
out-of-bounds read / memory disclosure. Affects nginx 0.5.6 through 1.13.2;
fixed in 1.13.3 / 1.12.1 (2017-07-11). CWE-190 (Integer Overflow or
Wraparound), which enables a CWE-787-class out-of-bounds read.
- NVD: https://nvd.nist.gov/vuln/detail/cve-2017-7529
- Official advisory: https://mailman.nginx.org/pipermail/nginx-announce/2017/000200.html
- Official patch: http://nginx.org/download/patch.2017.ranges.txt

## The real bug

nginx's range filter (`src/http/modules/ngx_http_range_filter_module.c`,
`ngx_http_range_parse()`) parses an HTTP `Range:` header that can request
several byte ranges of a cached response (a multipart request). For each
parsed range it accumulates the total response size it will need to send:

```c
range->start = start;
range->end = end;

size += end - start;      // <-- the culprit line (pre-patch)

if (ranges-- == 0) {
```

`size` is an `off_t` (signed, effectively i64 on 64-bit builds). A client
that crafts enough ranges, or ranges with extreme `start`/`end` values, can
make this running sum wrap around `NGX_MAX_OFF_T_VALUE`, turning it into a
small or negative number. Later logic that trusts `size` to reflect "how
much response body this request needs" is then working from a wrapped,
wrong value, and nginx serves a multipart response that walks past the
cached response body into adjacent cache-file memory (the header/metadata
region), leaking its bytes to the client, the advisory's "memory
disclosure" impact.

The official fix (`patch.2017.ranges.txt`) inserts an explicit
pre-addition overflow guard, moving the overflow-prone `+` onto the safe
side of a comparison instead of trusting the raw sum:

```c
// after the patch
range->start = start;
range->end = end;

if (size > NGX_MAX_OFF_T_VALUE - (end - start)) {
    return NGX_HTTP_RANGE_NOT_SATISFIABLE;
}

size += end - start;
```

This is the textbook CWE-190 shape: `if (a + b > LIMIT)` can overflow and
falsely pass; `if (a > LIMIT - b)` cannot (given `a`, `b`, `LIMIT` are
already within representable range). The task description also names this
exact shape (`offset + length > buffer_size` vs. `offset > buffer_size -
length`), and it's the same idiom behind Stagefright (CVE-2015-1538) and
innumerable other "does the requested slice fit?" guards in C.

## The port

`vuln.snd` / `fixed.snd` distill the bug to its essence rather than
reproducing nginx's multi-range accumulator verbatim (that would require
porting the whole range-list parser for no added signal): a single
`range_copy(src_ptr, src_len, offset, length, dst_ptr)` function that (1)
checks whether `[offset, offset+length)` fits inside a `src_len`-byte
buffer, then (2) copies the bytes with raw pointer arithmetic, exactly like
nginx's underlying `ngx_output_chain`/`memcpy`-style copy of the requested
range into the response.

- `vuln.snd` keeps the buggy guard `if offset + length > src_len`, the
  direct analogue of nginx's `size += end - start` overflowing before the
  patch (both are "the sum of two untrusted quantities, compared/accumulated
  without checking it didn't wrap").
- `fixed.snd` uses `if offset > src_len - length`, the direct analogue of
  nginx's patched `if (size > NGX_MAX_OFF_T_VALUE - (end - start))`, which
  also moves the subtraction onto the trusted/bounded side.
- Neither file adds any `requires`/`where` bounding `offset`/`length`: the
  real C code doesn't bound them either (they come straight off the wire),
  and adding such a contract would hide exactly the bug this case exists to
  test.
- The buffer access is `peek8`/`poke8` on computed addresses
  (`src_ptr + offset + i`), i.e. raw pointer arithmetic, the same idiom
  Sond's own `lib/any/strings.snd` uses for `memcpy`. This is the faithful
  translation of C's `memcpy(dst, src->pos + offset, length)`; there is no
  "safe slice" abstraction anywhere in the original bug.

## Result: NOT caught: and why, with evidence

Both files were compiled with the full pipeline (`sondc <file>.snd --shared
-o probe.so`, contract prover included, run under `tools/sandbox.sh`):

```
$ sondc vuln.snd  --shared -o /tmp/probe_vuln.so
sondc: wrote native x86-64 binary `/tmp/probe_vuln.so`
$ sondc fixed.snd --shared -o /tmp/probe_fixed.so
sondc: wrote native x86-64 binary `/tmp/probe_fixed.so`
```

`vuln.snd` compiles cleanly, the compiler does **not** flag the
overflow-prone guard, and would happily ship the vulnerable binary. This is
the expected, honest result for this case, not a bug in the demo.

**Why it isn't caught, this is a real, evidence-backed gap, not a hand-wave:**

Sond's compiler (`compiler/src/check.rs`, `bounds_expr`) *does* have a
genuinely overflow-aware, checked-by-default bounds checker, but it only
fires on the language's sugared indexing syntax, `a[i]` (fixed arrays) and
`s[i]` (`Slice`). Confirmed empirically with a two-line probe:

```
$ cat probe_slice_check.snd
fn touch(s: Slice, i: i64) -> i64 { s[i] }

$ sondc probe_slice_check.snd --shared -o probe.so
sondc: function `touch`: cannot prove this index is in bounds for slice `s`
- bound it with a `where`/`requires`/loop condition to 0..s.len
```

An *unproven* variable index into a `Slice` is rejected outright, Sond's
indexing really is "checked by default," and (per `compiler/src/verify.rs`)
the underlying prover even reasons about whether the arithmetic forming the
index itself can overflow before trusting it.

But `vuln.snd`/`fixed.snd` never index a `Slice` or array, they use
`peek8`/`poke8` on a manually computed `i64` address, exactly as real
low-level buffer code (nginx, Stagefright, and Sond's own `strings.snd`
`memcpy`) does. `peek8`/`poke8` are documented as operating on "any
address" with no bounds semantics of their own (`Sond.md`: "load/store...
`a` is any address"), and structurally they are ordinary `Call` expressions
in the AST, `bounds_expr`'s special-casing keys specifically on
`Expr::Index`, so it never even looks at them. There is currently no static
or runtime check tying a `peek8`/`poke8` address back to any buffer's size.

**Conclusion, stated honestly:** Sond's checked-by-default array/slice
indexing is a real, overflow-aware guarantee, but its scope is the sugared
indexing syntax only. Any code written in the raw-pointer/`peek`/`poke`
style that real systems C code (and Sond's own stdlib) actually uses for
buffer manipulation sits entirely outside that guarantee today. This CVE
class, an integer-overflow-defeated "does this range fit?" guard followed
by a raw memory copy, survives untouched in the current compiler,
regardless of whether the guard is written correctly or not (`fixed.snd`
compiles for the same reason `vuln.snd` does: peek/poke aren't checked
either way, not because the fix was "verified"). Closing this gap would
mean either (a) extending bounds-checking to `peek`/`poke` when the address
is provably derived from a sized buffer, or (b) discouraging/gating raw
pointer builtins in favor of `Slice` indexing for buffer code, which *is*
covered. Both are real, scoped follow-up items for the language, not
something this case's result should be spun to hide.

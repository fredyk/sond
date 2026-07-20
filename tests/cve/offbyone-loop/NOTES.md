# CVE-2021-3156 "Baron Samedit": off-by-one copy loop (CWE-193)

## The CVE

CVE-2021-3156, discovered by the Qualys Research Team and disclosed January 26,
2021, is a heap-based buffer overflow in `sudo` (all versions from legacy
1.8.2-ish up through the fixed 1.9.5p2 / 1.8.31p2), reachable by any local
user via `sudoedit -s` or `sudo -s` with a crafted final command-line
argument, leading to root privilege escalation. The bug lives in
`set_cmnd()` in `plugins/sudoers/sudoers.c`, in the loop that concatenates
`argv` into the `user_args` string sudo later logs/parses, unescaping
backslash-escaped characters along the way:

```c
for (to = user_args, av = NewArgv + 1; (from = *av); av++) {
    while (*from) {
        if (from[0] == '\\' && !isspace((unsigned char)from[1]))
            from++;
        *to++ = *from++;
    }
    *to++ = ' ';
}
```

`user_args` (`to`) is heap-allocated with exactly enough room for a 1:1 copy
of the concatenated `argv` strings, no slack is reserved for the unescape
step. `from` walks a NUL-terminated `argv` string. **The culprit line** is:

```c
if (from[0] == '\\' && !isspace((unsigned char)from[1]))
    from++;
```

When the *last* character of an argument is a lone backslash, `from[1]` is
that string's own NUL terminator. `isspace('\0')` is false, so
`!isspace(from[1])` is true, and the branch fires: `from++` steps `from`
past the terminator. The very next statement, `*to++ = *from++`, then reads
and copies whatever byte sits one past the end of *this* argument's
allocation, and advances `from` again. The outer `while (*from)` re-checks
against that out-of-bounds byte; if it's non-zero (overwhelmingly likely -
it's adjacent heap data, e.g. the next `argv` string or allocator metadata),
the loop keeps walking off the end of the source buffer, copying arbitrary
heap bytes into `to` and overrunning `to`'s allocation right along with it.
That is the CWE-193 off-by-one at the root of the bug: the stop condition
(`!isspace(from[1])`) fails to account for `from[1]` being the terminator,
so the loop runs one logical step too far, and that one step cascades into
an attacker-influenced heap read+write.

**The real fix** (upstream sudo patch):

```diff
- if (from[0] == '\\' && !isspace((unsigned char) from[1]))
+ if (from[0] == '\\' && from[1] != '\0' && !isspace((unsigned char) from[1]))
```

A backslash is only an escape character if there is a next character to
escape.

## Mapping to the Sond port

Files: `vuln.snd` (faithful port of the bug), `fixed.snd` (same program with
the real fix applied).

**Idiom choice, peek8/poke8, not Slice.** Sond has two ways to work with
buffers: the high-level, bounds-carrying `Slice { ptr, len }` / `let a[N]`
array-indexing API described in `Sond.md`, and raw `peek8`/`poke8` on `i64`
addresses. I checked how this codebase itself ports C string functions of
the exact same shape (`lib/any/strings.snd`: `strlen`, `memcpy`, `streq`,
`starts_with`, `find_byte`), every one of them is written with
`peek8`/`poke8` pointer arithmetic over `i64` addresses, matching C's own
pointer style almost line for line, and `tests/strings_test.snd` builds its
test buffers with `let mut buf[N]` + `poke8`. I followed that established
idiom rather than the `Slice` API: `set_cmnd()` itself is raw C pointer
arithmetic over a NUL-terminated buffer, and `peek8`/`poke8` is the direct,
honest Sond equivalent, no bounds machinery is grafted on that the
original C code didn't have either. Porting to `Slice` instead would test a
different, easier question ("does `s[i]` get bounds-checked against
`s.len`?") rather than the actual bug ("does a hand-rolled pointer-walking
copy loop get any protection at all?").

**The loop.** `unescape_arg(from_buf, to_buf)` in `vuln.snd` is a
near-literal transcription of `set_cmnd()`'s inner `while` loop:
`from_i`/`to_i` play the role of `from`/`to`, `peek8`/`poke8` play `*from`/
`*to`, and `is_space_byte()` reimplements the whitespace classes `isspace()`
covers in the "C" locale. The buggy condition is copied verbatim in
structure: `if c == BACKSLASH && not is_space_byte(c1) { from_i = from_i + 1 }`
- it does **not** check whether `c1` is the terminator, exactly like the
real code.

**Buffers.** `Sond.md`'s `let a[N]` allocates *N words* (8 bytes each), not
N bytes, and `tests/strings_test.snd` uses that generously (`let mut
buf[20]` for a 3-byte string), so a naive small demo buffer would give
many bytes of slack and never cross a real allocation boundary even when
the bug fires. To keep the demo a *genuine* out-of-bounds access rather
than a logic-only bug that happens to stay inside slack space, I sized the
argument so its **correct** unescaped output is exactly one word (8 bytes):
the argument is `"ABCDEFG\"` (7 letters + a trailing backslash = 8 real
characters, terminated by NUL at index 8). `to_buf` is declared as `let mut
to_buf[1]` (exactly 8 real bytes of backing storage), matching how the
real `user_args` allocation is sized exactly for the correct output, with
no slack for the bug.

Bytes 9–10 of `from_buf` (`'X'`, `'Y'`) stand in for whatever the allocator
happened to place right after this argument's chunk on the real heap (the
next `argv` string, allocator metadata, …), in the real exploit those
bytes are attacker-influenced heap layout; here they are just fixed,
non-zero filler so the demo is deterministic. Byte 11 is a NUL that stops
the runaway loop inside our own declared memory, so the demo can't wander
into unrelated stack frames even in the worst case.

I traced both versions by hand against these exact buffer contents:

- **`vuln.snd`**: iterations for `'A'..'G'` copy straight through
  (`to_i`/`from_i` reach 7). At `from_i=7` (the backslash), `c1 =
  from_buf[8] = NUL`; `is_space_byte(0)` is false, so the buggy branch
  fires and `from_i` jumps to 8 (the terminator index). The write at that
  point (`to_buf[7] = from_buf[8] = NUL`) is still in bounds. `from_i`
  then advances to 9, past the terminator, into the `'X'`/`'Y'` filler.
  The loop's stop condition re-checks `from_buf[9] = 'X' != NUL` and keeps
  going for two more iterations, writing `to_buf[8]` and `to_buf[9]` -
  **both genuinely past `to_buf`'s declared 8-byte capacity**, before
  finally hitting the safety NUL at index 11 and stopping.
- **`fixed.snd`**: same buffers, but the added `c1 != NUL` guard means the
  branch does not fire at `from_i=7`; the trailing backslash is copied
  through literally. The loop writes exactly 8 bytes (`to_buf[0..7]`,
  filling `to_buf` exactly) and stops at the real terminator, the `'X'`/
  `'Y'` filler is never read.

## Annotation added to `fixed.snd`

**None**, beyond making the port syntactically valid Sond. This is a
deliberate, documented non-choice, not an oversight: per `Sond.md`, the
current `sondc` contract prover only verifies **straight-line integer
bodies** ("no loops or calls"), so there is currently no way to attach a
`requires`/`ensures` that would let the compiler *prove* "this loop never
writes past `to_buf`'s capacity", the prover simply doesn't look at loop
bodies yet. `peek8`/`poke8` are also unchecked builtins with no bounds
contract of their own to refine with a `where` clause. So the honest answer
to "what's the minimal annotation Sond needs" is: the fix here is a pure
logic correction (add `c1 != NUL` to the condition), and it is not yet
something `sondc` can be asked to verify, only to compile.

## Compilation check (syntax only, not a verdict)

I compiled both files myself with the exact command given
(`sondc <file>.snd --shared -o ...`, under `tools/sandbox.sh --time 60`)
purely to confirm they are syntactically valid Sond, `fixed.snd` was
required to compile per the task instructions, and I checked `vuln.snd` the
same way only as a sanity check, not to select for a particular outcome.
Both compiled (`sondc: wrote native x86-64 binary ...`, exit 0) with no
changes needed to either file. I did not execute the resulting `.so`s, and
I am not drawing a conclusion here about what that does or doesn't say
about the compiler's ability to catch CWE-193 in this idiom, that
measurement and its write-up belong to the parent task.

## Alternative considered: `for i <= len` instead of `for i < len`

The task allowed substituting a cleaner classic off-by-one (`for i <= len`
writing into a buffer of size `len`) if it made for an easier or more
natural port. I did not need it: the faithful, unmodified translation of
`set_cmnd()`'s real branch condition (skip the escape-check only when
`from[1]` is checked against "is it whitespace", without checking "does it
exist") *already* produces exactly that shape as an emergent consequence -
the buggy branch pushes `from_i`/`to_i` one step further than the buffer
was sized for, on its own, without any artificial `<=` inserted by hand.
Keeping the real branch condition, rather than swapping in a synthetic
`<=`, is what makes this a faithful CVE port rather than a made-up loop
bug that merely resembles one.

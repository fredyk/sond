# Sond: Implemented Language Surface (what the compiler accepts TODAY)

> **⭐ Prime Directive (permanent):** Sond is a language written by AI and used by AI.
> Hard-to-implement features are never avoided, the goal is a language in which it is
> *almost impossible* to make a programming or logic error: extremely hard for a human
> to write, yet extraordinarily hard for an AI to be wrong. Correctness guarantees
> always win over ease of authorship. (Full statement in `docs/rfc-0000-vision.md`.)

<!-- SOND-MD-BEGIN -->
> This is the practical, ground-truth reference for the CURRENT `sondc` compiler.
> The design RFCs describe features (contracts, effects, modules, capabilities)
> that are mostly NOT implemented yet. **To write programs that compile and run,
> use ONLY the constructs on this page.**

## Reusable libraries (include)

A top-level `include "path.snd"` (path relative to the including file) pulls in another
file's functions and globals. Includes are recursive and de-duplicated; one flat namespace.

```sond
include "mathlib.snd"          // brings in square()
fn main() -> i64 { square(6) }
```

Libraries in `lib/` (include by relative path):
- crypto `any/crypto/`: cipher/chacha20, aead/chacha20poly1305, hash/{sha256,sha512},
  mac/poly1305, dh/x25519, sign/ed25519-sign, kdf/hkdf-sha256, noise/noise-ik, ssh-kex.
- data `x86_64/linux/`: heap (arena alloc), bytes, vec, map, lexer, calc; testing (see below).
- os/net `x86_64/linux/`: io, proc, net, poll, http, capture; `x86_64/net/{tcp-server,dhcp}`,
  drivers/net/virtio-net; `any/strings.snd` (strlen/memcpy/streq/itoa/atoi).

## Program shape

A program is one or more functions:

```sond
fn efi_main() -> i64 {
    print("hello from Sond")
    0
}
```

- `fn name(params) -> Type { ... }`. Parameters are `name: Type`, comma-separated. The
  only value type is `i64` (also written `bool`, `Money`, all 64-bit); arrays/buffers are
  i64 ADDRESSES (`[]`/`[i64]` also accepted, mean `i64`). Omitting a type defaults to i64.
- The **entry** is `main` (hosted/freestanding) or `efi_main` (UEFI); if neither is present,
  the LAST function. Its integer value is the exit code (hosted) or EFI status.
- Functions call each other by position, at most 6 args. Statements are newline-terminated
  (no `;`); `//` is a line comment. A block's value is its tail expression (the `0` above).
- `(pub|priv)? extern fn name(params) -> Type`, with no body, DECLARES a function this
  program calls and does not define; the linker resolves it. It takes contracts like any
  other signature. This is the shape of a `.snd` header: `sondc --emit-header` writes the
  public interface of a module as `extern` declarations, so the header is a file this
  compiler can read.

## Types

- `i64`: 64-bit signed integer. This is also used for pointers and characters.
- `bool`: `true` / `false`.
- A string literal `"..."` is a pointer (`i64`) to its NUL-terminated bytes; use it where an
  address is wanted (`peek8("hi")`, `f("hi")`). No `cstr`: the literal already is one.

Decimals (`10.25`) compile as `f64`, NOT as exact `Money` yet: do not use them for money.

## Expressions

- Integer literals: `42`, `0`, `1000000`; hex `0x3F8`; character literals
  `'A'` (== 65), `'0'` (== 48), `'\n'` (== 10), all are just integers.
- Booleans: `true`, `false`.
- Arithmetic: `+`, `-`, `*`, `/` (integer division), `%` (remainder).
- Bitwise/shift: `&`, `|`, `^`, `<<`, `>>`, `>>>`, and prefix `~` (bitwise NOT).
  `>>` is ARITHMETIC (sign-preserving), `>>>` is LOGICAL (zero-fill); for crypto
  rotations on high-bit-set values use `>>>`: `(x >>> n) | (x << (64 - n))`.
  These bind TIGHTER than arithmetic/comparisons, so `reg & 0x20 == 0x20` means
  `(reg & 0x20) == 0x20`.
- Unary prefixes: `-x` (arithmetic negation), `~x` (bitwise NOT), `not b` (boolean).
- Comparisons: `==`, `!=`, `<`, `>`, `<=`, `>=` → `bool`.
- Logical: `&&`, `||` (short-circuit), and prefix `not` for boolean negation
  (`not` is BOOLEAN only; for integer bit-complement use `~`).
- Parentheses group as usual: `(a + b) * c`.
- Function/builtin calls: `name(arg, arg)`.

## Statements and control flow

```sond
let x = 10          // immutable binding
let mut i = 0       // mutable binding
let mut n: i64      // no initializer -> defaults to 0
i = i + 1           // assignment (only to `let mut`)

if cond {           // if / else; also usable as an expression
    ...
} else {
    ...
}

for i < 10 {        // while-style loop
    ...
    i = i + 1
}

for i in 0..10 { .. }   // counting loop, half-open; bounds may be variables (0..n)
for let mut i = 0; i < n; i++ { .. }   // three-part loop; step may be any assignment
for x in s { .. }       // foreach over a Slice/array element by element

for {               // infinite loop
    if done { break }
    continue
}

return 0            // early return
```

`if` is also an expression: `let m = if a > b { a } else { b }`; `else if` chains to any
depth (`if a { } else if b { } else if c { } else { }`).

**`for i in 0..n` is the way to walk a buffer**: the index is PROVEN to live in `0..n`, so
`px[i]` inside needs no `where`/guard. `0..n` is half-open (n excluded); there is no `..=`,
no step and no descending form — count up, or use the three-part loop for anything else.

### Constraints (common gotchas)
- **Functions are TOP-LEVEL only**: no `fn` inside another `fn`; factor helpers out.
- **Parameters are IMMUTABLE**: to mutate, copy into a local: `let mut v = x`.
- A stack array `let a[N]` is written with `a[i] = ...`; it needs NO `let mut` (the
  name is an address). Only scalar bindings you reassign need `let mut`.
- No `println`; use `print`, `print_int`, `print_char`, `serial_print`.

## Module-level globals

At the top level you may declare globals; the initializer must be a CONSTANT integer
expression. Read like any variable; a `let mut` one keeps state between calls.

```sond
let POLY = 0xEDB88320       // a named constant
let mut counter = 0         // mutable global state, persists across calls
```


## Contracts (requires / ensures)

A function can carry a contract, PROVEN AT COMPILE TIME for every input (not checked at
runtime). If it cannot prove a postcondition the compiler REJECTS the program.

The prover handles straight-line bodies, loops whose `invariant` it VERIFIES (init and
preservation, not merely trusts), and calls: a caller must prove the callee's `requires`, and
its `ensures` flows back.
Rules of thumb that WILL verify:
- constant results: `ensures { r == 42 } { 6 * 7 }`;
- arithmetic identities: `ensures { r == x + x } { x + x }`;
- passing a precondition through: `requires { x > 0 } ensures { r > 0 } { x }`.

Syntax: `fn f(x: i64) -> (q: i64) requires { x >= 0 } ensures { q >= 0 } { x }`.
`requires`/`ensures` hold propositions (newline or `;` separated, implicit AND); `&&` inside
one is split into conjuncts. `-> (name: Type)` binds the result so
`ensures` can name it; parameters are in scope in both.

A parameter can carry a **`where` refinement**: `fn get(i: i64 where 0 <= i && i < 10)`
is a `requires` on that param (`&&` splits into conjuncts), proves bounds/overflow.

## Builtins: provided by the COMPILER itself (no `include` needed)

Everything below is emitted by the compiler directly and is ALWAYS available. Everything
else (crypto, strings, drivers) is a `.snd` library you `include`.

### All targets
- `print("literal")`: write a STRING LITERAL to the console EXACTLY as given, NO
  automatic newline (hosted `write(1,…)`; UEFI `ConOut`). May contain `\n`; `print("")`
  writes nothing.
- `print_int(n)`: print the integer `n` in decimal (no newline).
- `peek64/32/16/8(ptr) -> i64`: load a 64/32/16/8-bit value from `ptr` (zero-extended).
- `poke64/32/16/8(ptr, val)`: store the low 64/32/16/8 bits of `val` at `ptr`.
  **⛔ Raw memory is the LAST resort, never the default.** `peek*`/`poke*` are for genuine
  low-level access to an address the program does not own as data: memory-mapped device
  registers, a syscall's out-parameter, an ABI/kernel structure layout, an FFI buffer.
  Everything else (buffers, strings being built, records, tables) goes through `Slice`,
  typed views `T[]` and `struct`s, whose every index the compiler PROVES. A peek/poke is
  invisible to the bounds prover: a mistake there is silent memory corruption, the exact
  class of bug Sond exists to make impossible. Bytes: `let b: u8[n]`, `b[i] = v`.
- `let a[N]`: fixed-size array of N words on the stack (no allocator, every target);
  index with `a[i]`; `a.len` is the size N (`for i < a.len`). A constant index outside
  `[0, N)` is a compile error. Tables: `K[64]`.
- **`Slice`**: built-in length-carrying array `{ptr, len}`. `let s = Slice { ptr: a, len:
  a.len }`; pass so a fn knows the length (`fn sum(s: Slice)`); `s.len`, index `s[i]` /
  `s[i] = v`. Sub-slice `s[lo:hi]` is a VIEW, no copy (`s[:hi]`/`s[lo:]`/`s[:]`). Built in.
- **Typed views** `T[]`: a base address, indexed at the ELEMENT's width, one word at
  runtime. Elements `u8`/`i8`, `u16`/`i16`, `u32`/`i32`, `f32`, `i64`; the SIGN follows the
  element (`0xFF` is 255 through `u8[]`, −1 through `i8[]`), so pixels want `u32`.
  `let ar: i32[n]` RESERVES n elements, zeroed, no allocator call (UEFI: use `alloc`);
  `= {10, 20, 30}` also writes them; `= <address>` binds that address instead
  (`heap_alloc(..)`, a measurable array, or `base + K` for constant K — a sub-view checked
  against the capacity that is really left).
  **A view says how long it is** — `u32[n]` names the parameter/local holding its length,
  `u32[16]` writes it down — and every index is PROVEN against it: `for i in 0..n` needs no
  annotation, another buffer's length is rejected, and indexing a view with no declared
  length is a compile error naming what is missing. Declaring one inside a function reserves
  on EVERY call and the arena never gives it back: reserve outside hot loops (`heap_used()`
  shows the total).
- **Structs** (all fields i64, one word each). `struct Point { x: i64, y: i64 }`;
  `let p = Point { x: 1, y: 2 }` or `let p: Point` (zeroed); read/write `p.x` / `p.x = 5`.
  **Methods**: `impl Point { fn sum(self) -> i64 { self.x + self.y } }`, called `p.sum()`.
  **Value vs pointer**: a value struct is read-only once passed to a function; to let a
  callee mutate, declare a pointer `*Point` (no `&`): `fn inc(c: *Point) { c.x = c.x + 1 }`.
  Mutating a value-struct PARAMETER is a compile error.
- `let a[N] = [e0, e1, ...]` INITIALIZES the words in order. At MODULE level it is the shared
  crypto table (`K[64]`, S-boxes) in `.data`, readable everywhere.
- `a[i]` reads / `a[i] = v` writes the 8-byte WORD at `a + i*8` (sugar over peek64/poke64);
  for BYTE buffers use a `u8[]` view, not peek8/poke8. `a` is any address.
- `write(fd, "literal") -> i64`: raw `write` syscall (hosted/freestanding only).

### Platform builtins: port I/O, UEFI, kernel interrupts

I/O ports are a SEPARATE address space: for a device register use `inb`/`outb`, NOT `peek8`/`poke8`
(those hit RAM). Hex literals allowed: `0x3F8`. The full per-target reference (UEFI boot
services, `exit_boot_services`, interrupt handlers) is served at
**<https://sond.dev/docs/builtins.md>**, fetched on demand so this quickref stays small.


## Testing (MANDATORY: a program without tests does not build)

`sond build`, `sond run` and `sond test` all enforce a coverage gate: a program is
REJECTED unless its `test "..." { }` blocks exercise ≥80% of its own functions' basic
blocks (the compiler lists the uncovered ones).

```sond
include "x86_64/linux/testing.snd"
fn double(x: i64) -> i64 { x + x }
test "double works" { assert_eq(double(21), 42) }
```

- `test "name" { ... }`: a test block. Each runs in a FORKED child.
- `assert(cond)` / `assert_eq(actual, expected)` / `assert_ne(a, b)` (from `testing.snd`)
  abort the test on failure. Only i64 values are compared.
- `sond test prog.snd` runs the suite and prints per-test pass/fail.

<!-- SOND-MD-END -->

## How programs are compiled (for reference)

```
sondc prog.snd                        # hosted ELF; entry main, value = exit code
sondc prog.snd --freestanding         # static libc-free ELF /init; entry main
sondc prog.snd --uefi -o BOOTX64.EFI  # PE32+ EFI app; entry efi_main
sondc prog.snd --shared -o libNAME.so # C-ABI .so; pub fn exported (C/Go/Rust/Python)
```


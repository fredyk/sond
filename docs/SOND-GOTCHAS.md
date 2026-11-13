# Sond: constraints for writing code that COMPILES (read before writing .snd)

Sond is a new language; its syntax is NOT Rust/Go/C even where it looks similar. These are the
mistakes a model makes every time.

**The compiler is the only authority.** This page ages faster than the language does, so before
you conclude that something is impossible, write five lines with a `test` block and run
`sond test f.snd`. Everything below was checked that way; anything that is not listed, check it
yourself rather than assuming.

## What does NOT exist (won't compile)
- **Overloading**: one name = ONE function. Do NOT define `fn f(a)` and `fn f(a, b)`; the compiler
  answers `function 'f' is defined more than once`. Use distinct names.
- **`&x` as "address of x"**: `&` is ONLY bitwise AND, and `&x` is a parse error. There is no
  address-of operator for a variable. For an out-param (e.g. the status of `wait4`): use a local
  array `let st[1]`, pass `st` (it is an address), and read it back with `peek32(st)` / `peek64(st)`.
- **Nested functions**: `fn` inside `fn` is a parse error. `fn` is TOP-LEVEL only; lift helpers out.

## What compiles but bites
- **Returning the address of a LOCAL array**: `let a[3] ... return a` compiles and leaves a
  DANGLING pointer (the stack frame is freed on return). Build buffers/argv in the CALLER and
  pass them by address. The compiler will not catch this one for you.

## What IS there
- Types: `i64` (also pointer and char) and `bool`. **Decimals compile, as f64** — so they are not
  exact and must not carry money; keep money in cents, in `i64`.
- **String values exist**: `let s: string = "abc"` is a value and `s.utf8` is the address of its
  bytes. A bare `"..."` literal is already a pointer to NUL-terminated bytes, so it goes straight
  into anything wanting an address (`open`, `execve`). Build one at runtime in a `u8[]` view
  (`let b: u8[n]`, `b[i] = code`) with a trailing NUL.
- **`pub` exists**: `pub fn` marks what a module exports (and what `--shared` puts in the `.so`).
  Plain `fn` is fine too; `include` gives one flat namespace either way.
- `struct` with `impl` and methods, built-in `Slice` (`{ptr, len}`), typed views `T[]`, and
  `extern fn` declarations (`sondc --emit-header` writes a module's public interface as those).
- `let x = ...` (immutable), `let mut i = ...` (reassignable), `let mut n: i64` (= 0).
  **Parameters ARE reassignable inside the body**: `fn h(a: i64) -> i64 { a = a + 1  a }` compiles
  and returns 2 for 1. The caller's value is untouched, since everything is passed by value.
- `let a[N]` stack array; `a[i]` reads/writes word i; `a` (no index) is the base ADDRESS. It takes
  `let mut` without complaint, and does not need it: the name is already an address.
  For bytes, a `u8[]` view (`let b: u8[n]`, `b[i] = v`); for dwords, `u32[]`; for records, a `struct`.
- **⛔ `peek*`/`poke*` only for real low-level pointer access** (device memory, a syscall
  out-param, an ABI layout, FFI). Never for your own buffers, strings or records: the bounds
  prover cannot see through them, so the error they allow is silent memory corruption.
  Use `Slice`, typed views `T[]` and `struct`s, which the compiler proves index by index.
- Module-level globals: `let NAME = <const>` (constant integer). E.g. `let SYS_read = 0`.
- Control: `if/else` (also an expression), `for cond { }` (while), `for i in 0..n`, the three-part
  `for`, `for { break/continue }`, `return`.
- Builtins: `print("lit")`, `print_int(n)`, `write(fd, "lit")`, `peek*/poke*`, `outb/inb`,
  and **`syscall(num, a1..a6) -> i64`** (Linux x86-64 ABI). The numbers live in `lib/x86_64/linux/syscall-nums.snd`.
- Shifts: `>>` arithmetic (sign), `>>>` logical (zero-fill). Bitwise `& | ^ ~`. Logical `&& || not`.
- `include "path.snd"` (relative to the including file; recursive; de-duplicated; flat namespace).

## Tests are not optional
A program whose `test "..."` blocks do not cover 80% of its basic blocks is REJECTED. A file with
no test block does not build at all, and one whose tests measure nothing (all the work sitting in
`impl` methods, say) fails with `nothing measured: this entry answers for no instrumented code`.

## Reference pattern (run a command, already in `lib/x86_64/linux/proc.snd`)
```sond
include "syscall-nums.snd"
fn run(path: i64, argv: i64) -> i64 {   // the CALLER builds argv (an i64 array terminated by 0)
    let pid = syscall(SYS_fork)
    if pid == 0 { syscall(SYS_execve, path, argv, 0)  syscall(SYS_exit, 127)  return 127 }
    if pid < 0 { return -1 }
    let st[1]
    let ret = syscall(SYS_wait4, pid, st, 0, 0)
    if ret < 0 { return -1 }
    (peek32(st) >>> 8) & 0xff            // WEXITSTATUS
}
```
The caller builds argv and path (with poke8) in ITS OWN arrays and calls `run(path, argv)`.

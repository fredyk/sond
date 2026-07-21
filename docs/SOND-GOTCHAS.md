# Sond: constraints for writing code that COMPILES (read before writing .snd)

Sond is a new language; its syntax is NOT Rust/Go/C even where it looks similar. These are the
mistakes a model makes every time. ALWAYS verify by compiling: `./compiler/target/debug/sondc f.snd -o /tmp/x`.

## What does NOT exist (won't compile)
- **`pub`**: there is no visibility. Just `fn name(...)`. Flat namespace (everything is visible via `include`).
- **Overloading**: one name = ONE function. Do NOT define `fn f(a)` and `fn f(a, b)`. Use distinct names.
- **`&x` as "address of x"**: `&` is ONLY bitwise AND. There is no address-of operator for a variable.
  For an out-param (e.g. the status of `wait4`): use a local array `let st[1]`, pass `st` (it is an
  address), and read it back with `peek32(st)` / `peek64(st)`.
- **Returning the address of a LOCAL array**: `let a[3] ... return a` leaves a DANGLING pointer (the
  stack frame is freed on return). Build buffers/argv in the CALLER and pass them by address.
  - **Strings as values**: a `"..."` literal IS a pointer (`i64`) to its NUL-terminated bytes in static
    memory. Pass it straight to anything wanting an address (`open`, `execve`, `peek8`). No `cstr`;
    the literal already is one. Build one at runtime with `poke8(buf+i, code)` and a trailing NUL.
- **Nested functions**: `fn` is TOP-LEVEL only. Lift helpers out to the top level.
- **Mutable parameters**: parameters are immutable. To mutate: `let mut v = x`.
- **`let mut a[N]`** for arrays: arrays take no `mut` (the name is already an address). Use `let a[N]`.

## What IS there
- Types: `i64` (also pointer and char) and `bool`. No decimals, no string values.
- `let x = ...` (immutable), `let mut i = ...` (mutable, reassignable), `let mut n: i64` (= 0).
- `let a[N]` stack array; `a[i]` reads/writes word i; `a` (no index) is the base ADDRESS.
  For bytes: `poke8(a+i, v)` / `peek8(a+i)`. For dwords: `peek32`. For words: `peek64/poke64`.
- Module-level globals: `let NAME = <const>` (constant integer). E.g. `let SYS_read = 0`.
- Control: `if/else` (also an expression), `for cond { }` (while), `for { break/continue }`, `return`.
- Builtins: `print("lit")`, `print_int(n)`, `write(fd, "lit")`, `peek*/poke*`, `outb/inb`,
  and **`syscall(num, a1..a6) -> i64`** (Linux x86-64 ABI). The numbers live in `lib/x86_64/linux/syscall-nums.snd`.
- Shifts: `>>` arithmetic (sign), `>>>` logical (zero-fill). Bitwise `& | ^ ~`. Logical `&& || not`.
- `include "path.snd"` (relative to the including file; recursive; de-duplicated; flat namespace).

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

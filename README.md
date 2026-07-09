# The AI Language (Sond)

A programming language optimized for verifiability in the era of AI code generation.

## ⭐ Prime Directive (permanent)

**Sond is a language written by AI and used by AI.** It is no longer necessary to
avoid features that are hard to implement, even ones that look herculean. What is
needed is a language in which it is *almost impossible* to make a programming or logic
error: **extremely hard for a human to write, yet in which it is also extraordinarily
hard for an AI to be wrong**. Every design decision serves that end; when a choice
trades more implementation effort for a stronger correctness guarantee, take the
harder implementation. This directive stands above all others and does not expire.
Put plainly: *a language so brutally hard to write that no human will use it, but when
an AI writes in it, it never has to come back to the code to fix a bug later.*

**The one bound on the difficulty:** the language must always remain writable by a
**qwen3.6-class model** working in the compiler's red→green oracle loop. That is the
difficulty ceiling, push every guarantee as hard as possible up to the point where a
modest AI can still produce valid programs against the oracle, and no harder.

## Thesis

When AI types at full speed, the bottleneck shifts from *writing* to *verifying*.
Sond reorients the language's ergonomics toward **verifiability**: the human writes
and approves contracts; the AI fills in and proves bodies; the compiler acts as an
oracle in a tight loop.

## The four pillars

1. **The spec is the primary artifact.** The unit of program is intent + contract +
   implementation. The human reviews intents, not lines.
2. **The compiler as oracle.** A sound system that gives a correct verdict: a
   postcondition it cannot prove is a *compile error*, not a runtime surprise.
3. **Local reasoning.** Immutability by default, explicit effects. A diff is verified
   against its interface, not against the universe.
4. **Security by capabilities.** If you did not give a function a capability, it is
   physically incapable of using it.

## What works today

There is a real compiler, `sondc` (Rust, built bottom-up with strict TDD):

- **Static contract verification**, `requires`/`ensures` are proven at compile
  time over a decidable fragment; **sound by default**: a contract the prover cannot
  verify is rejected, never trusted to a runtime check. (`src/verify.rs`.)
- **A capable systems language**, `i64`/`bool`, functions and calls (stack-passed
  args beyond 6), `if`/`else`/`else if`, `for` loops, module-level globals, word-array
  indexing `a[i]`; the full operator set (`+ - * / % & | ^ << >> ~ && ||`, comparisons);
  hex/char/string literals; raw memory (`peek`/`poke`, `alloc`), port I/O
  (`inb`/`outb`), and inline assembly (`asm`).
- **Three code-generation targets**, a hosted ELF (`sondc x.snd`), a freestanding
  libc-free `/init` (`--freestanding`), and a **UEFI** PE32+ application (`--uefi`)
  that **boots on bare hardware with no OS underneath** (verified in QEMU/OVMF), all
  the way through `ExitBootServices` into a bare-metal kernel talking to COM1.
- Ground-truth language reference: [`docs/implemented-language-surface.md`](docs/implemented-language-surface.md).

## How it is validated

The compiler is stress-tested by having a deliberately weak model (qwen3.6) write
Sond programs in isolation, it knows only the language reference, researches the
relevant RFCs online, and must compile (and, for verifiable outputs, *run and check*
against known answers) its own code before delivering it. The compiler-oracle proves
safety; an executable oracle catches algorithm errors. See [`examples/`](examples/)
for the resulting bare-metal drivers (UART, PIT), a network stack (Ethernet/ARP/IPv4/
ICMP/TCP/DHCP/DNS), and bit-exact checksums/crypto (CRC-32, Adler-32, FNV-1a, ...).

## Documentation

- [RFC-0000: Language vision](docs/rfc-0000-vision.md), the foundational design.
- [Implemented language surface](docs/implemented-language-surface.md), what `sondc`
  accepts today.
- Further RFCs (grammar, Go idiom, target profiles, package format, FFI) under `docs/`.

## Build

```
cargo test  --manifest-path compiler/Cargo.toml   # the test suite
cargo build --manifest-path compiler/Cargo.toml   # builds sondc
tools/install.sh                                   # installs sondc to ~/.local/bin
```

All compilation/execution during development runs under `tools/sandbox.sh` (cgroup
limits); UEFI binaries are booted in QEMU/OVMF via `tools/boot-uefi.sh`.

## Installing Sond as a toolchain

`tools/install.sh` installs Sond as a self-contained toolchain (in the spirit of Go's
`go` + `GOROOT`), so an external project depends on the *installed* Sond, not on this repo:

- `sondc` → `~/.local/bin/sondc` (on `PATH`)
- the standard library → `~/.local/lib/sond/` (a **copy** of `lib/`, not a symlink)
- header interfaces → `~/.local/include/sond/`
- the language reference `Sond.md` → `~/.local/share/sond/Sond.md`

An `include` is resolved in order: relative to the including file → a `lib/` root found by
walking up the tree → the system library `~/.local/lib/sond/`. So an external program
writes `include "any/crypto/cipher/chacha20.snd"` and it resolves from the installed
copy, with no path back to this repository.

**Pinning:** the installed copy *is* the pinned version. To move to a specific Sond
version, re-run `install.sh` from the desired commit/tag (formal versioning is ahead).

**[`Sond.md`](docs/Sond.md)** is the compact language reference, the single document an
AI (or human) needs to write correct Sond. Point your model or editor at it.

## Status

**Working compiler, active development.** The language and its native backend are
real and growing by TDD; the design RFCs describe features (full effects, capabilities,
modules) still ahead. Part of a running experiment: building an OS tower in Sond,
from bare UEFI upward.

## Author

JMV + agents (2026).

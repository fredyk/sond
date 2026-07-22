# RFC-0014: Ports --- one interface, one implementation per target

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0014                                                                  |
| Status      | Implemented (all six slices landed 2026-07-22)                        |
| Date        | 2026-07-22                                                            |
| Authors     | Jhon Magdalena Vila                                                    |
| Motivation  | A caller that needs "a serial line" cannot say so: the include model resolves to one concrete file per target, so on a target with no stdlib implementation you must hand-stub every primitive |
| Related     | RFC-0001 (grammar, `include`), RFC-0003 (target profiles), RFC-0006 (package format), RFC-0011 (effects + capabilities) |

## 1. The evidence: the same driver, imported on one target and stubbed on another

Two programs drive the same serial protocol. The host (x86-64/Linux) writes:

```
include "x86_64/linux/serial.snd"   // real: termios + read/write syscalls
```

and gets a working implementation. The firmware for the same cluster (PIC16F684) writes,
at the top of the file, this:

```
fn serial_init() { }
fn serial_getc() -> i64 { 0 }
fn serial_putc(b: i64) { }
```

Empty stubs. Not because the author wanted stubs, but because:

1. **No implementation exists for the target.** There is no `lib/pic16/serial.snd`; the only
   `serial.snd` is the Linux one, and Linux syscalls are meaningless on bare metal (no OS, no file
   descriptors). PIC serial has to be bit-banged on GPIO with cycle-accurate timing: a *different*
   implementation of the *same* interface.
2. **Sond is whole-program with no linker.** Every function you *call* must be *defined* in the
   translation unit, or it is an "unknown function" compile error. There is no `extern`, no symbol
   to resolve at link time.

So the author is forced to write bodies for `serial_init`/`serial_getc`/`serial_putc` just to make
the program type-check and codegen, even though the real work lives elsewhere (or, in a skeleton,
nowhere yet). The buffer-based framing library (`any/pic/frame.snd`) is *also* not imported, for a
related reason: it builds frames in a RAM buffer with `poke8`, and a 64-byte MCU streams bytes
instead, so the firmware re-implements the protocol inline. `any/` means "target-agnostic in
principle", but the buffer and allocator assumptions still do not fit an 8-bit part.

The through-line: **two targets want the same interface and legitimately need different
implementations, and the language has no way to say so.** You either `include` a concrete file that
happens to exist for your target, or you hand-write it. This RFC gives the interface a name.

## 2. Why the current model cannot express it

- **`include` is textual splicing of one concrete file** (RFC-0001, `resolve.rs`: "the pragmatic,
  Go-simple package on-ramp"). It resolves a path to exactly one source and inlines its bodies.
- **The stdlib is siloed by target directory** (`x86_64/linux/`, and a *missing* `pic16/`). For the
  newest and smallest target the stdlib is nearly empty.
- **Whole-program + no linker** turns "no implementation for my target" into "declare the body or it
  does not compile", i.e. hand-stubbing.

Generated headers (`~/.local/include/sond/.../*.snd`, the `pub` surface with no bodies) do not help:
there is no separate compilation, so a header cannot stand in for an implementation. They are for
interface checking, documentation, and coverage scoping, not for linking against.

None of these is a bug; each is a deliberate simplification. Together they leave a hole exactly where
Sond claims its widest reach (kernel to MCU): the target with the least stdlib is the one where you
are most on your own.

## 3. The mechanism: `port` and `provides`

A **port** is a named interface: a set of function signatures (with contracts and effects), no
bodies. A **provider** is a per-target implementation of a port. The compiler selects the one
provider whose target matches the active build, entirely at compile time.

```
// port/serial.snd  --- the interface, target-agnostic, no bodies
port Serial {
    fn serial_init()
    fn serial_getc() -> (b: i64) ensures { b >= -1 && b <= 255 }   // -1 = no byte
    fn serial_putc(b: i64) requires { b >= 0 && b <= 255 }
}
```

```
// lib/x86_64/linux/serial.snd  --- one provider
provides Serial for x86_64_linux {
    fn serial_init() { /* termios raw 8N1 via ioctl */ }
    fn serial_getc() -> i64 { /* read() one byte, -1 on timeout */ }
    fn serial_putc(b: i64) { /* write() one byte */ }
}
```

```
// lib/pic16/serial.snd  --- another provider, same interface
provides Serial for pic16 {
    fn serial_init() { /* CMCON0 = 0x07, TRIS setup, bit-bang timing */ }
    fn serial_getc() -> i64 { /* sample RX at bit centers */ }
    fn serial_putc(b: i64) { /* bit-bang 9600 8N1 */ }
}
```

The caller depends on the *port*, never on a concrete file:

```
use Serial            // brings the port's names into scope

fn main() {
    serial_init()
    let c = serial_getc()
    ...
}
```

At build time the compiler, knowing the active target (RFC-0003 profile), splices the matching
provider's bodies. The firmware and the host now share one line (`use Serial`) and each gets the
right implementation. **The stub disappears, and so does the accidental re-implementation.**

## 4. Resolution and its error, the whole point

Resolution is static and total:

1. A `use P` requires exactly **one** provider of `P` for the active target. Zero providers is a
   compile error; two providers for the same target is a compile error (ambiguity, never silent
   "last wins").
2. The error when none exists is the deliverable that replaces hand-stubbing:

   ```
   sondc: no implementation of port `Serial` for target `pic16`.
     `use Serial` needs a `provides Serial for pic16 { ... }`.
     found providers for: x86_64_linux
   ```

   That single message is the difference between "unknown function serial_getc, go figure out why"
   and "write the bit-bang provider here". It turns a missing port into a task, named, with the
   target that lacks it.
3. A provider must implement **every** signature of the port, with a **matching** signature. A
   missing or mistyped function in a provider is a compile error against the port, not a mystery at
   the call site.

## 5. Interaction with contracts (the reason this is not just a header)

A port's signatures carry `requires`/`ensures`. These are **obligations on every provider and
guarantees to every caller**:

- Each provider must prove it satisfies the port's `ensures` (and may *assume* the `requires`),
  exactly as an ordinary function proves its own contract. A bit-bang `serial_getc` that could
  return 256 fails to compile against `ensures { b <= 255 }`.
- Each caller may rely on the port's `ensures` and must discharge the port's `requires`, without
  seeing any provider. The prover reasons about `Serial` from the port alone, so host and firmware
  callers are verified identically, and swapping the provider cannot change what the caller proved.

This is what a plain header cannot give: a header is a signature; a port is a **verified contract
boundary** that every target implementation is held to. It is the same move as effects and
capabilities (RFC-0011), one level up: the port is the interface, the provider is the authorized
implementation, and the contract is the proof that ties them.

## 6. Fit with the rest of the series

- **Target profiles (RFC-0003).** "For `<target>`" reuses the profile names RFC-0003 already
  defines (`x86_64_linux`, `pic16`, `uefi`, `freestanding`). A port is how a profile exposes its
  capabilities to portable code; RFC-0003's continuity claim ("same mental model across targets")
  becomes concrete here.
- **Effects and capabilities (RFC-0011).** A port method may declare `effects`, and a provider's
  body must stay within them. A capability can *be* the token that admits `use Serial` (holding
  `Cap[Io]` is what lets a body call the port), so a pure function still cannot smuggle in serial IO.
- **Package format (RFC-0006).** A package may ship a port and any subset of providers; a downstream
  target with no provider gets the total error of section 4, not a link failure at the end.
- **No dynamic dispatch.** Resolution is compile-time and monomorphic: one provider per build, spliced
  in. No vtables, no function pointers, no runtime cost. This is mandatory, not a preference: the
  PIC16F684 cannot afford indirection, and Sond's AOT verification wants the concrete body in view.

## 7. What this deliberately is NOT

- **Not runtime polymorphism.** You cannot pick a provider at runtime; a build is one target and one
  provider. (Runtime strategy selection is a normal value/enum, not a port.)
- **Not multiple providers linked together.** Exactly one provider per port per build. "Two serials
  at once" is two ports (`SerialA`, `SerialB`), not two providers of one.
- **Not a full module/namespace system.** Ports name *interfaces to swap by target*, nothing more.
  The Go-simple `include` stays for everything that does not vary by target. This RFC adds the one
  axis that `include` cannot express, and no more.

## 8. Migration: serial, ported

The `port Serial` interface has landed at `lib/any/serial.snd` (commit `6aef049`), shaped by the
target that constrains it hardest: an 8-bit microcontroller (PIC16F684) with no integer wider than a
byte.

```
pub port Serial {
    fn serial_init()
    fn serial_getc() -> u8   // blocks until a byte arrives; no wider "-1 for none" fits on 8 bits
    fn serial_putc(b: u8)
}
```

Everything is `u8`. An earlier draft had `serial_getc` return an `i16` so it could signal "-1 for no
byte", but the smallest target has no `i16`, so the interface would not be implementable there. The
resolution is the classic bare-metal one: `serial_getc` **blocks** and returns a byte; a non-blocking
read, if a provider ever needs it, is a separate `serial_ready` predicate, never a wider return. (The
`u8` range is not yet type-enforced, that is RFC-0012; the interface already speaks in `u8` so the
enforcement is free when it lands.)

The remaining steps, downstream product/backend work, not language work:

1. `provides Serial for pic16` with the bit-bang body **in Sond**, under `lib/pic/16F/684/serial.snd`
   (the maintainer's placement: reusable per-target libraries live in the language repo, the product only uses
   them). This requires the pic16 backend primitives currently in flight (a read-`peek8`, a
   variable-value `poke8`, and a cycle-exact delay), because today the bit-bang is a **backend
   intrinsic emitted by name** in `pic16.rs` (the `serial_getc:`/`serial_putc:` asm), not Sond, and
   the firmware's empty stubs are placeholders the backend ignores.
2. Removing that name-special-casing from `pic16.rs`, so the provider's Sond body *is* the
   implementation (compiled normally). This deletes the hidden asm and the firmware stubs in one move.
3. The same treatment retires the other bare-metal stubs (GPIO, PWM) as `lib/pic/16F/684/` providers
   grow, so the PIC16 stdlib fills in behind stable, verified interfaces.

## 9. Honest limits and open questions

- **Grammar cost.** Two new keywords (`port`, `provides`) and one binding form (`use`). Small, but
  real surface; the alternative (a target-conditional `include "pic16/serial.snd"` resolved by
  profile) is *smaller* and needs no keywords, but gives no interface and no contract boundary, so it
  does not solve section 5. The port is the version worth its weight.
- **Where does a port's contract get proved against hardware?** The provider proves it against the
  Sond model; whether the bit-bang actually meets timing is outside the type system (the same honest
  limit as any `asm`/`poke` primitive, RFC-0011 debt #4). The port bounds *what* the function
  promises, not *that the silicon obeys*.
- **Default providers.** Should a port allow a `provides Serial for any { ... }` fallback used when no
  target-specific provider exists? Convenient, but it re-opens the silent-wrong-implementation door
  the total error in section 4 closes. Deferred; leaning against.
- **Interaction with generated headers.** A port *is* a header that the language understands. The
  header generator should emit ports verbatim and providers as their signatures only.

## 10. Implementation plan (TDD slices)

1. **Parse `port P { sigs }`** into the AST (signatures only), red-green with a parser test.
2. **Parse `provides P for T { fns }`** and bind it to its port and target.
3. **Resolve `use P`** against providers filtered by the active target; the three errors of section 4
   (zero, ambiguous, signature-mismatch) each land as a failing test first.
4. **Splice the selected provider** into the translation unit at lowering, so codegen is unchanged.
5. **Contract check**: a provider is verified against the port's `requires`/`ensures`; a caller
   against the port. Reuse the existing prover; the new work is threading the port's clauses to both
   sides.
6. **Port `Serial`** end to end (section 8) as the first real user, deleting the firmware stubs.

Each slice is a compile-time-only change (no runtime), so the whole feature can land behind failing
tests without touching a single generated instruction until step 6 proves it on real hardware.

## 11. Status: implemented (2026-07-22)

The mechanism landed in six commits, strict TDD throughout, and matches this design with one honest
adjustment: the concrete target identifiers are the compiler's own tags (`host`, `pic16`, `uefi`,
`freestanding`, `shared`), not the `x86_64_linux` of the examples above. What is built:

- **Grammar** (`port`, `provides ... for`, `use`): parsed into `PortDef` (bodyless `Signature`s),
  `ProviderDef` (per-target functions), and `UseDecl`, collected off the function list like structs
  and enums. The function-head parser is shared (`parse_sig_tail`) so a port method and a function
  signature are one grammar.
- **Resolution** (`ports::resolve_ports`, run in the driver after parse, before lowering): each `use`
  binds to exactly one provider for the active target. Zero (message names the target and lists those
  that do have a provider), ambiguous, missing-method, wrong-arity, and renamed-parameter are all
  compile errors; using an undeclared port is too.
- **Splicing**: the selected provider's methods are added to the translation unit (one target's
  implementation, providers for other targets dropped, a repeated `use` spliced once), so codegen is
  unchanged and a call to a port method resolves to the active target's code.
- **Verification**: each spliced method carries the port signature's contract, so the existing prover
  proves the body meets the port's `ensures`. A provider returning `x + 1` against `ensures { r == x +
  x }` is rejected with a counterexample; `x + x` compiles and runs.

Header emission of ports (section 9) is also done: a public port is rendered into a module's
generated header verbatim, its providers omitted like function bodies, so a consumer can `include`
the header and check against the interface. There is a runnable example at
`examples/host/language/ports/`.

What remains open (not blocking): the `default provider` question (section 9, leaning against), and
slice-plan step 6, migrating the real `Serial` to a port and deleting the firmware stubs, which is
downstream product work, not language work.

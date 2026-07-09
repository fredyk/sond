# RFC-0007: Flexible C interop --- beyond Go's all-or-nothing CGO

| Field       | Value                                  |
|-------------|----------------------------------------|
| RFC         | 0007                                   |
| Status      | Draft                                  |
| Date        | 2026-07-09                             |
| Authors     | JMV + agents                          |
| Motivation  | JMV's pain with Go's global `CGO_ENABLED` |
| Related     | RFC-0003 (profiles), RFC-0004 (trust frontier), RFC-0006 (packages) |

## 1. The problem: Go's CGO is a global, binary switch

Go couples all its C interop to a single build variable, `CGO_ENABLED`:
- **`CGO_ENABLED=0`**: pure-Go static binary, trivial cross-compile --- but **you give up C entirely** (you cannot use, e.g., a C memory allocator/cache that requires dynamic linking).
- **`CGO_ENABLED=1`**: you can use C, but you inherit **dynamic linking**, painful cross-compilation, slower builds, dependency on a C compiler, and a **toll per call** at the Go↔C boundary (stack transition, GC safepoints, scheduler).

There is no middle ground: you cannot say *"static in almost everything, but link THIS specific library dynamically"*. It is **global**, not per-boundary. That is the pain this RFC solves.

## 2. Diagnosis: why CGO hurts

1. **Global granularity**: a single flag decides for the entire binary.
2. **Runtime toll**: the cost per call comes from coordinating Go's runtime (GC safepoints, goroutine scheduler), not from the fact of "calling C" per se.
3. **Two runtimes / two allocators**: Go's GC vs C's `malloc`, different stacks.
4. **Toolchain**: requires a C compiler, breaking pure-Go simplicity.

## 3. Sond's design: three orthogonal knobs, all PER-BOUNDARY

### 3.1 Knob 1 --- linking per dependency (`link`)

Each C package, pinned by hash in the manifest (RFC-0006), declares **how** it links:

```sond
require {
    // Almost everything is static pure-Sond...
    jemalloc = sond:c9a1...  link dynamic   // ...but THIS lib, dynamic
    fastcache = sond:2b7f... link dlopen    // ...and THIS, lazy optional loading
    zlib      = sond:88de... link static    // ...and THIS, embedded static
}
```

A binary can be **95% static-pure-Sond with one dynamic lib and one `dlopen`**. The all-or-nothing era ends: linking is a **per-dependency** property, not a build flag.

### 3.2 Knob 2 --- the boundary cost is set by the PROFILE (RFC-0003), not a global runtime

CGO's toll comes from coordinating Go's runtime. In Sond that cost depends on the **caller's profile**:
- **GC-free profile** (`kernel`, `systems`): a call to C is a **direct call** --- zero overhead, no runtime to coordinate. The boundary is free.
- **GC profile** (`script`, `server`): the call inserts the safepoint transition (like CGO), **but only there**.

> Consequence: **you pay Go's cost exactly where you have Go's problem (a GC to coordinate), and nowhere else.** The toll is opt-in via profile, not a general binary tax.

### 3.3 Knob 3 --- wrapper with contract + capability (RFC-0004, RFC-0006)

Every `extern` is a **typed trust boundary**:
```sond
// Binding to a C syscall/lib, wrapped in Sond contract.
extern "C" fn c_socket(domain: i32, kind: i32, proto: i32) -> i32
requires { domain >= 0 }
ensures  { result >= -1 }        // -1 = error; >=0 = valid fd
caps     [CapRed]                // the manifest declares what capability it needs
link     dynamic
```
- The compiler verifies that the **wrapper** satisfies its contract (even though C's internals are unverified).
- The **capability manifest** makes it auditable that a "cache" lib does not secretly ask for `CapRed`.
- The **hash pin** prevents hallucinated dependencies or ones that "move".

## 4. "The best of both worlds" (what JMV was missing)

- **Standalone by default**: without `extern`, a **fully static** binary and trivial cross-compile (the `CGO_ENABLED=0` world) --- no flag, simply by absence of C dependencies.
- **Granular C opt-in**: add that dynamic cache/allocator lib **without losing the static nature of the rest** or touching a global switch.
- **`dlopen` for optional acceleration**: a binary can go pure-Sond and **attempt** to load a C lib from the system at runtime if present, with a **clean fallback** if not. A **single artifact** that is both standalone **AND** capable of leveraging the system lib when it exists. (JMV's "use the memcache if present" scenario.)

## 5. The honest truth: real limits (without glossing)

- **Two allocators**: Sond's GC + C's `malloc` is a real problem. Who frees what is expressed in the **boundary ownership contract** (`owns`/`borrows` in the `extern` signature). It is not free.
- **C->Sond callbacks** into a GC profile still need runtime coordination --- you cannot escape it entirely.
- **Symbol/ABI conflicts** when mixing static and dynamic are real engineering, not magic.
- **Complexity**: granularity gives more power but also more rope to hang yourself. Go's switch is ugly but **simple**; Sond bets on **power-with-discipline** (and the verifier imposes the discipline).

## 6. Fit with qwen3.6 validation

This is **step 0** of `validation-qwen36-https-stack.md`: qwen3.6 writes the `extern` for socket syscalls (`socket`/`bind`/`connect`/`read`/`write`) with their contracts, `link dynamic` to libc, `caps [CapRed]`. The compiler verifies the wrappers; the rest of the stack (TCP->HTTP->TLS) is built on top in pure Sond. If Sond nails this flexible FFI, step 0 is solid and the rest of the ladder rests on it.

## 7. Open questions

1. **Default linking**: static if the lib allows it, dynamic if not? Or always explicit?
2. **Ownership semantics at the boundary**: how to express in the contract who allocates/frees (avoid double-free and leaks between GC and `malloc`).
3. **`dlopen` optional × verification**: a lib loaded at runtime cannot be verified statically -> trust level "unverified-dynamic" (RFC-0006 Sec.3.5) and what capabilities are allowed for it.
4. **Cross-compilation with dynamic deps**: keep the no-C case trivial and contain the friction when there is `link dynamic`.
5. **ABI**: only C ABI (System V) at first, or also C++/Rust `extern`?

---

*Conclusion: where Go puts a global switch (`CGO_ENABLED`), Sond puts three per-boundary knobs
---linking per dependency, cost per profile, and wrapper with contract+capability--- that truly give "the best
of both worlds": standalone by default, granular dynamic C when you want it, and optional `dlopen` acceleration. The price is more complexity, tamed by the verifier.*

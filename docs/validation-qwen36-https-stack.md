# The litmus test: qwen3.6 builds a complete HTTPS stack in Sond

> JMV's decision (2026-07-09): Sond's **definitive** validation is not filling in bodies,
> but that **the weakest model (qwen3.6) builds an HTTPS REST API with self-signed certificate and
> its client**, which forces writing **the entire stack (TCP -> HTTP -> TLS) as reusable libraries
> first**, plus a basic **stdlib** (strings, bytes, collections, regex…). Sond's compiler
> is the oracle that catches every error from the weak model. If qwen3.6 pulls it off, the thesis is proven
> at real scale, not on a toy.

## Why this test and not another

Sond's thesis (RFC-0000) is: *AI-generated code is cheap to DISTRUST because the compiler
proves it satisfies its contracts*. A "fill in this function" test validates the thesis at small scale. But the
real question is: **does it hold when a clumsy AI builds an entire system, by layers, where each
layer depends on the one before?** An HTTPS network stack is the ideal case because:
- It is **deep** (syscalls -> TCP -> HTTP -> TLS -> app): a failure in a low layer propagates.
- It is **critical in correctness and security** (bad TLS = hole; bad HTTP parser = RCE).
- It is **verifiable by contracts** at every boundary (an HTTP parser has clear invariants; a
  TLS handshake has a state machine with pre/postconditions).
- It has an **objective and binary success criterion**: the client does an https GET to the server with a self-signed cert and **receives the correct body**, or it does not.

The adversary is **qwen3.6 precisely for being weak**: if a clumsy model cannot slip a bug
past the verifier in a system of this depth, Sond's structural containment
works. Each bug the compiler *does not* catch is a hole in the contract design -> gold for iteration.

## The bootstrap ladder (bottom-up, each step = Sond libraries with verified contracts)

### Step 0 --- World access: C bindings / FFI (`extern`)

Writing the stack "from silicon" is infeasible. The realistic start is the **trust frontier**
from RFC-0004: wrapping the minimal syscalls / libc (`socket`, `bind`, `connect`, `read`, `write`,
`close`) behind Sond contracts. qwen3.6 writes the `extern` with their `requires`/`ensures`; the compiler
verifies that the *wrapper* satisfies the contract even though the internals (C) are unverified. More pure alternative: raw syscalls via a `kernel`-like profile, but C bindings are the pragmatic on-ramp
(and validate the migration strategy of RFC-0004).

### Step 1 --- Primitive stdlib

Before networking, qwen3.6 must write (with contracts) the base pieces that everything else uses:
- **bytes and buffers** (`Bytes`, slicing, append) with length invariants.
- **strings** (UTF-8, split, trim, find) --- leveraging the already-supported Unicode identifiers.
- **collections** (`List`, `Map`) if not native.
- **regex** --- a small engine (Thompson NFA) with contract: "accepts ⇔ the string is in the language".
  This is a beautiful test because a poorly implemented regex is a classic subtle bug, exactly what the
  contracts should catch.
- **encoding** (hex, base64) with the postcondition `decode(encode(x)) == x` (verified round-trip).

### Step 2 --- TCP

On top of Step 0 sockets: a `tcp` library with `Listener`/`Conn`, state contracts
(do not read from a closed connection), and `[Red]` effects + `CapRed`. An echo server as the first verifiable milestone.

### Step 3 --- HTTP/1.1

On top of TCP: **HTTP parser and serializer**. Here contracts shine:
- request line / headers / body with invariants (`Content-Length` matches the body, headers well
  formed, no CRLF injection --- security postcondition).
- The parser is Turing-unnecessary -> can live in a total sub-language (tower, RFC-0000 Sec.8).

### Step 4 --- TLS (with self-signed certificate)

On top of TCP: the TLS 1.3 handshake as a **state machine with contracts** (each transition with
pre/postcondition), crypto primitives (probably via `extern` to a libcrypto wrapped with
contract in Step 0, to avoid reimplementing AES/curves --- realism), and self-signed cert generation/loading. The correctness of the handshake state is exactly what the verifier must guarantee.

### Step 5 --- The application: HTTPS REST API + client

On top of HTTP+TLS: a REST server (routing, handlers with request->response contracts) listening on
HTTPS with the self-signed cert, and a **client** that makes the request and validates the response.

**Success criterion (binary):** the client runs a `GET https://localhost/...`, negotiates TLS with the
self-signed cert, and receives the expected body with status 200. Green = the thesis holds at scale.

## Experiment metrics

While qwen3.6 climbs the ladder, we measure:
- **Capture rate**: of the incorrect bodies it generates, what % does the compiler reject before
  execution? (target: ->100%).
- **Leaks**: bad bodies that pass the verifier and fail at runtime/e2e. Each leak = a contract
  too weak -> it is hardened. The leak list is the improvement map for contract design.
- **Iterations per step**: how many AI-proposes/compiler-rejects rounds does qwen3.6 need per layer
  (the "oracle loop" from Pillar 2 in real action).
- **Depth reached**: how far the weak model gets without human help.

## What we need from the compiler to run this

This test needs, in addition to the current lexer+parser:
1. **Function bodies** parsed (in progress) and a **type/effect/capability checker**.
2. The **contract verifier** (the oracle) --- translating `requires`/`ensures` to conditions and
  checking them (SMT or bounded symbolic execution).
3. The **`extern` trust frontier** (RFC-0004) for the C bindings of Step 0.
4. The **package format** (RFC-0006) so that each step's libraries are reusable by
  those above without dependency hallucination.
5. An **execution backend** (AST interpreter or codegen) to be able to *run* the final e2e.

This is a long-term north; the ladder defines the build order of the compiler and stdlib.

## The harness: total isolation of the AI under test (JMV)

For the experiment to be honest, qwen3.6 must learn Sond **exactly like a real user would, from the
docs, never from the compiler source**. Reading the source would be cheating: it could reverse-engineer
what the compiler accepts instead of following the specification. The harness enforces this:

1. **The only language knowledge given to qwen3.6 is `~/.local/share/sond/Sond.md`**, the full docs
   bundle produced by `tools/build-docs.sh`. Nothing else. It never sees `compiler/` or any Rust source.
2. **qwen3.6 runs in an isolated scratch dir `/tmp/<example>/`**, with `--dir` pointing there and no
   read access to the repository. It writes its Sond program into that temp folder.
3. **Opus (the operator) collects** the produced `.snd` files from `/tmp/<example>/` into the repo's
   `examples/`, then compiles and runs them with `sondc` **under `tools/sandbox.sh`** (resource limits:
   qwen3.6 code may loop/OOM/fork-bomb).
4. **Audit the isolation, don't assume it.** For the first workers launched, inspect opencode's session
   store `~/.local/share/opencode/opencode.db` (SQLite) and confirm that every file the worker read /
   every path in its tool calls is **only `~/.local/share/sond/Sond.md`**, no `compiler/`, no repo
   source, nothing else. Query the session messages/tool-calls and grep for any path outside the
   allowed one. If anything else leaked into its context, the run is invalid and is redone with tighter
   `--dir`/permission isolation.
5. The oracle verdict is recorded: how many of qwen3.6's wrong bodies the compiler rejects (capture
   rate), and each **leak** (a wrong body the verifier let through) becomes a contract to harden.

This isolation guarantees the result reflects "can a weak AI build correct Sond software guided only by
the spec and the compiler's verdicts?", the real question, and not "can it copy the compiler's tests?".

---

*Recorded as Sond's reference validation objective. Related: RFC-0004 (trust frontier /
migration), RFC-0006 (package format), RFC-0000 (the 4 pillars and the tower).*

---

## Experimental results (2026-07-09/10)

The experiment was run: qwen3.6, isolated (knowing only the language reference, researching
the RFCs online), writing Sond programs and, for verifiable outputs, compiling and running
its own code against known answers before delivering. Twelve rounds, dozens of programs.
Two oracles were used: the **compiler-oracle** (contracts + static verification) for safety,
and an **executable oracle** (run and compare to a known vector) for algorithm correctness -
the second turned out to be essential and was added mid-experiment.

**What the weak model built correctly, on bare UEFI or on the host:**
- **Protocol framing**, Ethernet/ARP (RFC 826), IPv4 header + Internet checksum (RFC 791),
  ICMP echo (RFC 792), TCP segment + handshake state machine (RFC 793), DHCP DISCOVER
  (RFC 2131), DNS query (RFC 1035).
- **Drivers**, a 16550 UART driver, an 8254 PIT timer, first-class port I/O.
- **Data structures / tools**, a ring buffer over mutable globals, a hexdump, a FAT16
  directory-entry parser, UTF-8 decoding, Base64 encode/decode, hex encode.
- **Simple cryptography / checksums, bit-exact**, CRC-32, CRC-16, Adler-32, FNV-1a, djb2,
  RC4 (a real stream cipher), Luhn.

**Where it hit a hard ceiling:**
- **Large cryptographic hash primitives (SHA-256, SHA-1)**, even with fixed-size stack
  arrays for the tables, the exact constants provided, and the executable oracle available,
  qwen3.6 did not converge to a correct 64/80-round hash. The algorithms are too long and
  bit-exact for the weak model to get entirely right, and it did not reliably complete the
  self-test loop on the largest programs.

**Where the ceiling actually is (corrected twice).** The one confirmed-beyond piece is
the hash functions **SHA-256 and SHA-1**, and, crucially, the full **ChaCha20 block**
(80 quarter-rounds + a 16-word state) came out CORRECT, so the limit is NOT sequence
length. What defeats the weak model is SHA's structural VARIETY (ch, maj, four distinct
sigma functions, 64 distinct round constants), not raw length: ChaCha's single uniform
operation repeated 80 times is fine. SHA-256/SHA-1 (64/80 rounds) never matched the standard
test vectors (`ba7816bf...`, `a9993e...`), even with stack arrays, the constants
provided, the executable oracle, and explicit debugging guidance. But the boundary is
*narrower* than first recorded: the **ChaCha20 quarter-round**, a real modern ARX cipher
primitive, came out **correct** (`3928658676 ...`), as did RC4, the Poly1305 clamp, and
every checksum. (An earlier note that ChaCha failed was *my* error: a hand-mistyped
expected value; the same false-negative bit the ARP-cache MAC and the MAC parser, where
qwen was right and my arithmetic wrong, compute expected values programmatically, never
by hand.) AES was never tested, so no claim is made about it.

**Conclusion.** The pipeline (weak model + research + self-compile + executable oracle)
reliably produces *correct systems and protocol code and cryptography up to a bounded
per-function complexity*, and the compiler-oracle catches the safety mistakes it makes.
The one demonstrated hard limit is the very long, exact, interdependent bit-op sequences
of the SHA family, which stayed wrong across every aid tried. So the thesis holds at real
scale for verifiable, contract-shaped code, and the executable-oracle loop extends it to
algorithm correctness, up to a genuine, robust complexity ceiling for the *specific*
longest primitives, where a stronger model would be needed for that one piece.

Each round also drove the compiler forward: every qwen mistake that was a *language* gap
(logical `&&`/`||`, `else if`, hex/char literals, string escapes, >6 arguments, module
globals, array indexing, stack arrays, uninitialised `let`, host `print_char`) was closed
with a TDD cycle, so the language grew to meet the code the weak model naturally wanted to
write.

## Update (2026-07-10): the SHA-256 ceiling is MODEL-bound, not language-bound

Running the same isolated harness with a **stronger model** (`deepseek-v4-flash`) instead
of qwen3.6, **SHA-256 came out correct on the first delivered program**: the digest of
`"abc"` matched the FIPS 180-4 vector exactly (`ba7816bf 8f01cfea 414140de 5dae2223
b00361a3 96177a9c b410ff61 f20015ad`, checked programmatically). So the "hard limit" above
is a property of **qwen3.6's** capacity, not of Sond or of the pipeline: the language, the
oracle and the self-compile loop were sufficient all along; only the writer needed more
capacity for that one class of long, structurally-varied primitive. This confirms the
operating strategy for the SSH crypto wall (M4): use qwen3.6 for the tractable bulk and a
stronger model (`deepseek`/`mimo`) for the few hard primitives (SHA-256, X25519), with the
executable oracle validating each against its published test vector. The thesis, *AI code
is cheap to distrust because the oracle decides*, holds regardless of which model writes;
a stronger writer simply clears a taller primitive in fewer rounds.

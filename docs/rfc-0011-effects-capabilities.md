# RFC-0011: Effects and capabilities --- the keystone that makes "Class A" unrepresentable

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0011                                                                  |
| Status      | Draft (the next real implementation frontier)                        |
| Date        | 2026-07-18                                                            |
| Authors     | JMV + agents                                                         |
| Motivation  | Everything downstream, the graph core, concurrency, on-chain safety, and closing debt #4/#5/#7, rests on one unbuilt mechanism |
| Related     | RFC-0000 (§5.3 effects, §6 capabilities), RFC-0008 (graph), RFC-0010 (concurrency), RFC-0009 (on-chain), bootstrap-debt-registry (#4/#5/#7) |

## 1. Why this is the keystone

Three later RFCs and half the debt registry all defer to the same unbuilt thing:

- **The graph core (RFC-0008)** decides parallelism by *effect conflict*, it cannot work
  without an effect algebra that names resources.
- **Concurrency (RFC-0010)** proves race-freedom by *capability disjointness*, it cannot work
  without capabilities.
- **On-chain safety (RFC-0009)** models account-write authority and re-entrancy as capabilities
  and effect ordering.
- **Debt #4** (`peek`/`poke`/`asm`/`syscall` are free), **#5** (kernel primitives free), **#7**
  (mutable globals) are exactly "ambient authority that should be a capability."

So effects+capabilities is not one feature among many; it is the mechanism RFC-0000 §6 calls
*"the thing that turns Class A into unrepresentable."* Until it lands, every guarantee above is
design, not fact. This RFC specifies it.

## 2. The two halves and the one link

- **Effect**: an *observable interaction*, reading/writing a resource, IO, nondeterminism.
  The *dynamic* fact of touching the outside world or shared state.
- **Capability**: an unforgeable *authority* to perform a class of effect on a resource. The
  *static* right. If you were not handed it, you physically cannot use it.
- **The link**: *a capability is the compile-time proof that an effect is permitted; the effect
  is the runtime act it authorizes.* Holding `Cap[FS, f]` is what lets a body carry
  `effects [FS(f)]`. No capability in scope ⟹ that effect cannot appear ⟹ the primitive that
  needs it does not compile.

**Resource identity = capability identity.** A resource is named by the capability instance
that governs it. Two effects conflict iff they name the *same capability instance* and at least
one writes. This collapses "how do we name a resource for conflict analysis" into "which
capability value", no separate resource-naming machinery.

## 3. Effects: the surface

The `effects [ ... ]` clause already exists in the grammar (RFC-0001). This RFC gives it
meaning. Each entry is `Class` or `Class(resource)`:

```
fn transfer(from: Account, to: Account, amt: u64, db: Cap[Db])
    requires { balance(from) >= amt }
    ensures  { balance(from) == old(balance(from)) - amt }
    effects  [ Db(from), Db(to) ]
{ ... }
```

- **Purity is the empty set.** A function with no `effects` clause is *pure*: memoizable,
  freely reorderable, trivially parallel (RFC-0008 §5.3), safe to call from any thread.
- **Conflict lattice.** `read` vs `read` on the same resource: commute (concurrent). `write`
  vs anything on the same resource: ordered. Disjoint resources: always concurrent. This *is*
  the graph-core edge rule (RFC-0008 §5.2) and the thread-disjointness rule (RFC-0010 §3).
- **Resource granularity is mandatory, not optional** (the delta surfaced in RFC-0010 §7.2):
  `Db(users)` and `Db(orders)` must be distinguishable, or the prover serializes all database
  work. The resource is a capability instance, so `db_users` and `db_orders` are different
  capabilities, granularity falls out of capability attenuation (§5).

## 4. Effect inference and polymorphism (so qwen need not annotate everything)

- **Private functions: inferred.** A non-`pub` function's effect set is the union of the
  effects of everything it calls plus the primitive effects it performs. The author writes
  nothing; the compiler computes it. (Ergonomics: a modest model should not hand-annotate every
  effect.)
- **Public functions: declared and checked.** A `pub fn`'s `effects` clause is part of its
  interface; the compiler proves the *inferred* effects are a subset of the *declared* ones.
  This is checked exceptions done right: internal inference, boundary declaration, the header
  (RFC-0006) exposes the effect signature so a caller reasons locally (RFC-0000 §3.3).
- **Effect polymorphism.** Higher-order functions carry an effect *variable*:
  `fn map(xs: [T], f: fn(T) -> U effects [E]) -> [U] effects [E]`. `map` over a pure `f` is
  pure; over an effectful `f` inherits `E`. Without this, `map`/`fold`/combinators would be
  either always-impure (useless for the graph core) or unable to call effectful bodies.

## 5. Capabilities: the surface

- **Zero-cost by default (phantom).** A capability is a compile-time token; proven present,
  erased at runtime (no runtime check, no allocation). It exists only to make the *type* of a
  function demand it. `peek`/`poke`/`syscall`/`asm` gain a required capability parameter; a
  function that lacks it cannot name them. **This closes debt #4 and #5**: raw memory, port
  IO, syscalls and kernel primitives become `Cap[Mem]` / `Cap[IO]` / `Cap[Kernel]`-gated -
  physically unusable without the grant.
- **The powerbox.** `fn main(root: Powerbox)` is the sole source of authority. Everything else
  receives capabilities *explicitly*; there is no ambient `open()` / global allocator / free
  syscall. Least authority by construction.
- **Attenuation (the workhorse).** From a capability you derive a *weaker* one:
  `readonly(c)` (drops the write effect), `scope(fs, "/tmp")` (narrows the resource),
  `split(c) -> (c1, c2)` for disjoint sub-resources. Attenuation is what feeds concurrency:
  `split` yields two capabilities the prover *knows* are disjoint, so the two threads that hold
  them cannot race (RFC-0010 §3). It is also least-privilege: hand a subsystem exactly the
  narrow capability it needs.
- **Runtime capabilities (rare).** When authority is decided dynamically (a plugin, a received
  handle), a capability may be a runtime value, but the *effect* it authorizes is still
  checked against the *type*; the value only decides *which* resource instance.

## 6. What this closes on the debt registry

- **#4 (free `peek`/`poke`/`asm`/`outb`/`syscall`)** → each requires the matching capability;
  ungated raw power becomes a compile error. The `unsafe` region (RFC-0003 §5) is where a
  capability is *minted* from nothing, audited once.
- **#5 (free kernel primitives)** → `interrupt fn`, `swtch`, `lidt`, port IO require
  `Cap[Kernel]`, available only under `@profile kernel`.
- **#7 (mutable globals)** → a module-level `let mut` is ambient mutable authority; it becomes
  a capability that must be threaded explicitly, or is forbidden. Non-local reasoning
  (action-at-a-distance) disappears because the mutation now appears in the effect signature.

## 7. Interaction with contracts

Effects and contracts compose: an effect can carry a contract on the *world*. `Db(users).write`
can `ensure` the row is persisted; the graph core and the caller reason about the post-state via
that ensures, not by tracing execution. Contracts describe the *value* result; effects +
their contracts describe the *world* result. Together they are the full spec RFC-0000 §3.1
means by "the unit of program is intent + contract + implementation."

## 8. Implementation plan (staged, and this is Opus work, not fleet work)

The subtlety here (inference, polymorphism, the capability/effect link, soundness) is exactly
the "genuinely critical/subtle" work RFC-0000's delegation rule reserves for careful review.

1. **Effect lattice + inference over the existing IR**, compute a private function's effect
   set; a first primitive effect (`Mem` for `peek/poke`). No surface change; proves the
   machinery. TDD in `verify.rs`/a new `effects.rs`.
2. **Declared-effect checking on `pub fn`**, inferred ⊆ declared, surfaced in the header.
3. **Capability gating of the raw primitives** (#4), `peek/poke/syscall` demand `Cap[...]`;
   `main` gets the powerbox; thread it. This is the first *observable* guarantee.
4. **Attenuation + `split`**, unblocks concurrency's disjointness proof (RFC-0010).
5. **Effect polymorphism**, unblocks combinators and the graph core at scale.
6. **Resource-parameterized effects + the conflict lattice**, unblocks the graph core's real
   parallelism and #7.

## 9. Open questions

1. **Effect syntax for resources**, `Db(users)` names a capability *value* in a *type*
   position; does that need dependent-ish types, or a lighter "effect labels" scheme keyed to
   capability identity?
2. **Inference termination**, effect inference over mutual recursion is a fixpoint; is the
   lattice finite/fast enough for the qwen loop's speed requirement (RFC-0000)?
3. **Erasure vs. dynamic caps**, where exactly is the line between phantom (erased) and
   runtime capabilities, and does crossing it ever cost soundness?
4. **`old()` in effect contracts**, expressing "the world before vs. after" for an effect's
   ensures needs a pre-state snapshot notion; how heavy is that for the prover?
5. **Backwards fit**, the current corpus uses free `peek/poke/syscall` everywhere; gating them
   is a breaking change staged like the mandatory-coverage gate (`sondc` primitive stays
   ungated for bootstrap; the gate lives in the profile).

---

*This is the frontier. The graph core, race-free concurrency, and provably-safe on-chain code
are all cashing a cheque this RFC writes: an effect algebra that names resources, and
capabilities that gate them. It is the mechanism that finally makes RFC-0000's "impossible to be
wrong" mean "the authority to be wrong was never in scope."*

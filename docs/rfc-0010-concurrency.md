# RFC-0010: Concurrency --- data races are unrepresentable, so there is no memory model to get wrong

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0010                                                                  |
| Status      | Draft (exploratory)                                                   |
| Date        | 2026-07-18                                                            |
| Authors     | JMV + agents                                                         |
| Motivation  | Concurrency's hard part is not spawning threads; it is shared mutable state, the memory model, and deadlock, all of which a sound oracle can make impossible |
| Related     | RFC-0008 (graph core), RFC-0000 (§3.3 immutability, §5.3 effects, §6 capabilities), RFC-0003 (`swtch`) |

## 1. The real problem

Nobody struggles to *start* a thread. Concurrency is hard for three reasons, and all three
are correctness problems a prover can own:

1. **Shared mutable state** → data races (two threads, one location, one write).
2. **The memory model** → the semantics of racy access (sequential consistency vs. relaxed,
   acquire/release, fences). C++/Java spent decades here; almost no human reasons about it
   correctly.
3. **Synchronization bugs** → deadlock, lost wakeups, priority inversion.

Sond's stance is the same one it takes on nil (`Option`) and out-of-bounds: **do not make the
error catchable, make it unrepresentable.** If a data race cannot be *written*, then (2), the
memory model, evaporates, because there is no racy access whose behaviour needs defining.
That is the headline: **Sond has no relaxed memory model to reason about, because
unsynchronized shared mutable access is a compile error.**

## 2. Two layers: declarative parallelism over an explicit substrate

Concurrency in Sond is two layers of one story:

- **Layer A --- the graph core (RFC-0008): the model.** Most parallelism is *not written* at
  all; it is the partial order of a `graph { }`'s dependency edges, and the compiler proves it
  schedule-independent. The programmer never mentions threads. This is where 90% of concurrency
  should live.
- **Layer B --- explicit threads: the mechanism.** The runtime that *executes* Layer A, and the
  escape hatch for genuinely imperative concurrency (a server's accept loop feeding workers, a
  kernel's per-core run queues). Layer A **compiles down to** Layer B; they are not rivals.

The rest of this RFC specifies Layer B, the substrate, because Layer A's safety proof
(confluence) ultimately rests on it.

## 3. The core rule: a thread sees only the capabilities it is given

Spawning is capability-scoped, structured, and scoped in lifetime:

```
spawn caps [handle] {
    // body: may touch ONLY the capabilities in `caps`
}
```

The compiler enforces, at compile time:
1. **Disjointness.** The child's *mutable* capabilities are disjoint from every other
   concurrently-live thread's mutable capabilities. Two threads that hold no overlapping
   mutable capability **cannot** race, there is no shared writable location. This is the
   whole game, and it is a capability set-disjointness check (RFC-0000 §6), not a runtime
   guard.
2. **Read sharing is free.** Immutable capabilities (the default, §3.3) may be shared by any
   number of threads, immutable data has no race. So most sharing needs no synchronization
   *at all*.
3. **Structured lifetime.** `spawn` is lexically scoped and joined at scope exit (structured
   concurrency): no detached thread outlives the data it borrowed, no leak, no
   use-after-free of shared state. A thread cannot escape with a capability whose owner's
   scope has ended.

Consequence: the *default* concurrent program, pure/immutable-sharing threads, is race-free
**by the same proof that admitted the spawn**, with zero locks and zero fences.

## 4. When you must share mutable state: through a synchronization capability

Sometimes threads genuinely need shared mutable state (a work queue, a counter, a cache). Then
the mutation must pass through a **synchronization capability**, which is the *only* place an
inter-thread memory ordering exists:

- **`Mutex<T>`**, a capability that *serializes*. Acquiring it grants the inner `&mut T`
  capability for one lexical critical section; the type system ensures the inner capability
  cannot escape the section. Two writers are ordered by the lock, not racing.
- **`Channel<T>`**, message passing with *move* semantics: `send(v)` transfers ownership of
  `v` (the sender no longer holds it), so there is never shared access to the sent value. The
  actor model, made a capability.
- **`Atomic<i64>`**, the lock-free primitive (compare-and-swap, fetch-add) for the runtime
  and the scheduler; a capability with defined acquire/release ordering. The *only* place the
  hardware memory model surfaces, confined to a tiny audited core.

Because these are the **only** conduits for shared mutation, the compiler inserts the required
fences *exactly* at them and nowhere else. There is no user-visible relaxed ordering: outside a
sync capability, there is no shared mutable access to order.

## 5. Deadlock-freedom, proven

Locks bring deadlock. Sond makes it a compile-time obligation, not a testing exercise:

- Each lock capability carries a **rank** (a position in a partial order, declared or
  inferred). A thread may acquire locks only in strictly increasing rank. The prover rejects
  any path that could acquire two locks out of rank order, the classic cycle that causes
  deadlock cannot form. (This is the well-known lock-hierarchy discipline, but *checked*
  instead of documented.)
- With ranks enforced there is no acquisition cycle, hence no deadlock, the same shape of
  argument as the graph core's acyclic effect edges (RFC-0008 §5.2). A program whose locking
  cannot be shown acyclic does not compile.

## 6. Reconciliation with the existing surface

- **`swtch`** (RFC-0003, already implemented) is Layer B for the *freestanding/kernel*
  profile: a cooperative context switch between stacks. The concurrency model builds on it -
  a kernel scheduler is a `graph`/thread runtime over `swtch`, not a separate mechanism.
- **hosted** profile: Layer B is OS threads (`clone` + `futex`), which the arena allocator and
  the sync capabilities target. The graph-core scheduler (RFC-0008 §5.5) is a worker pool here.
- **on-chain** profiles (RFC-0009): single-threaded VMs, Layer B is trivial (one worker), and
  the graph core degrades to a topological linear run with identical result. Concurrency
  guarantees cost nothing there but still forbid the effect-ordering bugs (re-entrancy).

## 7. What was overlooked before this RFC (design deltas)

Writing this surfaced three things the earlier RFCs implied but never stated:

1. **The graph core (RFC-0008) has no runtime without this.** Its confluence proof presumes a
   scheduler that respects the DAG's edges; that scheduler *is* Layer B. RFC-0008 should
   reference this RFC as its execution substrate.
2. **Effect granularity gates all provable parallelism.** "This touches `Db`" is too coarse;
   the conflict analysis (graph edges *and* thread disjointness) needs "touches `Db` resource
   *users*" vs "`orders*`". The effect system (RFC-0000 §5.3) must name *resources*, not just
   effect *classes*, or the prover serializes far more than necessary. This is a concrete
   requirement on the effect design, not a nicety.
3. **The memory-model sidestep is a first-class selling point, not a footnote.** "No relaxed
   memory model because races are unrepresentable" is as strong a claim as "no null", it
   should sit next to the four pillars, not be discovered here.
4. **The allocator is the race the guarantee forgets (surfaced building the stdlib, 2026-07-18).**
   "Races unrepresentable" presumes *all* shared mutable state flows through tracked mutable
   capabilities (§4). But the stdlib's foundation, the bump **arena** that `Bytes`/`Vec`/`Map`
   (and thus every crypto/HTTP/PQ program) allocate from, is a raw `i64` bump pointer in a
   module global, mutated with `poke64`. That is *below* the capability system (registry #4 free
   `peek/poke` + #7 mutable globals), so two Layer-B threads that both allocate race on the bump
   pointer, and the race is **invisible** to the analysis, it is not a typed capability, so
   nothing flags it. The guarantee has a hole at the very bottom of the tower. Why nothing breaks
   *today*: the only shipped concurrency is **fork-per-connection** (the HTTP/secure-echo demos) -
   separate address spaces, so each process has its own arena. Real threading (`clone`, shared
   address space) races on the first two concurrent `alloc`s. **Direction: thread-local arenas** -
   each Layer-B thread gets its own bump region seeded at spawn, so allocation needs no
   synchronization and stays race-free *by construction* (no shared writable location), keeping
   the claim true down to the allocator; a handle crosses threads only by `Channel<T>` move, which
   must copy/relocate the bytes (or the arena is split into a shared-immutable pool + per-thread
   scratch). The alternative, one global arena behind a lock, reintroduces a serialization point
   on every allocation and a real (if audited) synchronized-mutable core. Either way this is a
   **precondition** for turning on hosted threads, not an afterthought: the allocator must pick its
   story before Layer B ships, or the headline guarantee is false at the foundation.

## 8. Honest limits

- This model **presupposes the effect + capability system** (RFC-0000 §5–6), which is *not yet
  implemented* (it is on the bootstrap debt registry as the mechanism that turns Class A into
  "unrepresentable"). Until it lands, concurrency in Sond is only as safe as manual discipline
 , the guarantees here are the target, not today's reality.
- A tiny **trusted core** remains: the `Atomic` primitive and the scheduler's lock-free queue
  are the irreducible place where the hardware memory model is real; they are audited once, and
  everything above them inherits the safety. (You cannot bootstrap synchronization out of
  nothing, but you can confine it to a few dozen proven lines.)
- **Fairness and progress** (starvation, livelock) are *liveness*, not safety; the prover
  guarantees safety (no race/deadlock) but scheduling fairness is a runtime property, stated
  but not proven in v1.
- Proving disjointness across dynamically-created capabilities (not just lexical ones) is the
  hard research edge, v1 targets lexically-scoped structured concurrency, where disjointness
  is decidable.

## 9. Open questions

1. **Rank inference vs. declaration** for locks, can the compiler infer a sound lock order
   from the call graph, or must the programmer declare ranks?
2. **`Mutex<T>` as a capability transform**, is "acquire grants inner `&mut` for a scope"
   expressible in the current value/pointer receiver model, or does it need new surface?
3. **Cancellation.** Structured concurrency needs scoped cancellation/timeouts, is that an
   effect (`Cancel`) the child observes, and how does it interact with holding a lock?
4. **Async/await vs. threads.** Do we need stackless coroutines (for millions of connections)
   as a *third* Layer-B form, or does the graph core + a thread pool subsume them?
5. **Relaxed atomics.** Do we ever expose acquire/release/relaxed to the user (for a
   hand-rolled lock-free structure), or keep `Atomic` sequentially-consistent-only and force
   everything else through `Mutex`/`Channel`?

---

*Exploratory. It defines the destination: a language where a data race, a deadlock, and a
memory-ordering bug are compile errors, because the only shared mutation is through capabilities
the compiler can order, and everything else is immutable or disjoint. It rests on the effect +
capability system, which is the next real implementation frontier.*

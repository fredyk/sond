# RFC-0008: The graph core --- a program is a proven dependency graph, not a line of statements

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0008                                                                  |
| Status      | Draft (exploratory)                                                   |
| Date        | 2026-07-18                                                            |
| Authors     | Jhon Magdalena Vila                                                         |
| Motivation  | Sequential source text is a HUMAN constraint; an AI can author the graph directly |
| Related     | RFC-0000 (vision: local reasoning, effects, contracts), RFC-0001 (grammar), RFC-0003 (profiles) |

## 1. The thesis: linearity is an artifact of the human writer, not of computation

Programming languages are lines of statements (`a; b; c`) because a human reads and writes
one thing at a time, top to bottom. But **nothing that actually runs is linear**:

- The CPU executes **out-of-order**, superscalar, speculative: it reorders your instructions
  and runs many at once, preserving only the *illusion* of your order (the "as-if" rule).
- The compiler, internally, **discards your linear text** and builds a **dependency graph**
  (the *sea-of-nodes* IR of HotSpot/V8). It optimizes over the graph and re-serializes to
  linear instructions at the very end, only because the ISA and the human expect it.

So the sequence lives in exactly two places: the **source text** and the **illusion of one
thread**. Both are conveniences for a human. When the author is an AI (which holds a whole
graph in view at once and does not read top-to-bottom), the sequential source is pure
overhead, and worse: **it imposes a FALSE order.** You write `a; b; c` even when `b` does not
depend on `a`. Every such line **over-serializes**: it invents a dependency that does not
exist in the data, purely so a human can follow a single thread.

This RFC proposes the AI-native alternative: **the program is the dependency graph itself.**
The only order is *real* data- and effect-dependency; everything else runs concurrently; and
the compiler **proves** the result is independent of schedule. It is not "add parallel
instructions"; it is *stop pretending computation is a line*.

## 2. Prior art (and why it is evidence, not competition)

This paradigm exists in fragments, and every one stayed niche **for the reason this RFC
turns into an advantage**: too hard for a human to write or reason about.

- **Dataflow languages** (SISAL, Id, Lucid): no order, only data dependencies; massively
  parallel; abandoned because humans could not follow them.
- **Synchronous languages** (Lustre, Esterel, Signal): everything reacts "at once"; used in
  avionics because they are *verifiable*, but written by a handful of specialists.
- **Hardware description** (Verilog/VHDL): describing hardware *is* writing pure concurrency;
  notoriously hard precisely because there is no single thread to trace.
- **Content-addressed code** (Unison): a definition is identified by the hash of its AST, not
  by name or file position: no order, no files, no naming conflicts. A graph of definitions.

Common trait: **machine-friendly, human-hostile.** Their failure with humans is the strongest
argument that the paradigm is right for an AI author, *if* the missing piece is supplied.

## 3. The missing piece all of them lacked: a sound oracle

Concurrency never failed because it was hard to *type*. It failed because it is impossible
for a human to *reason about*: races, deadlocks, memory models, the combinatorial explosion
of interleavings. Humans do not fail at writing parallel code; they fail at **verifying** it.

Verifying is exactly what Sond does. A sound prover that can show *"these two nodes touch
disjoint state, run them in any order"* converts a humanly-inviable paradigm into one that is
**correct by construction**. RFC-0000's whole thesis (*when the AI writes, the bottleneck is
verification*) **is the enabling condition for this paradigm.** The graph core is the natural
destination of the vision, not a departure from it.

## 4. Sond is already halfway there

Three features Sond already commits to remove most sequential order *today*:

1. **Immutability by default** (RFC-0000 §3.3): with no mutable state, the order between two
   expressions **cannot** be observed: it is already semantically irrelevant.
2. **Explicit effects** (RFC-0000 §5.3): the *only* places where order can matter are marked.
   Two pure nodes never conflict; only declared effects can.
3. **Contracts + local reasoning**: every node already carries `requires`/`ensures`, so it
   composes against its interface, not against a global timeline.

Strip mutable state and hidden effects (which Sond already does) and **~90% of the
sequential order you would write is false**; it was never a real dependency. The graph core
simply *stops writing the false order down.*

## 5. Design

### 5.1 The `graph` block

A `graph { .. }` is an expression whose body is a set of **nodes**. A node is a named binding;
its dependencies are inferred from the values it references. Order among nodes is **only** the
partial order of those references. `yield` names the graph's result.

```
let result = graph {
    node fx = heavy_transform(input)       // depends on: input
    node fy = other_transform(input)       // depends on: input     (independent of fx)
    node z  = merge(fx, fy)                // depends on: fx, fy
    yield z
}
```

`fx` and `fy` share no dependency, so they are concurrent; `z` waits for both. There is **no
line order**: reordering the three `node` lines changes nothing. The author (human or AI)
states *what depends on what*, never *when*.

Sequential code is the **degenerate case**: a chain `node b = f(a); node c = g(b)` is a graph
that happens to be a single path. So the graph core *subsumes* the linear surface rather than
replacing it: existing linear functions are graphs of one path and keep working unchanged.

### 5.1a The inline `par` form: horizontal peers

A text file is a 2D medium (rows and columns), but conventional syntax spends the whole
horizontal axis on token spelling and pours *all* meaning (sequence) into the vertical one. The
graph core reclaims the horizontal axis: **going down asserts a dependency, going across asserts
concurrency.** For a small fan-out, a full `graph { node .. }` block is heavier than the idea
deserves; the same set of independent nodes is written inline with `par` and the flat peer
delimiter `~~`:

```
let a ~~ b ~~ c = par loadUser(id) ~~ loadCart(id) ~~ loadPrices()
render(a, b, c)
```

`~~` is **not an infix operator**: it is a FLAT, n-ary delimiter of the `par` construct, so
`a ~~ b ~~ c` is a set of three peers, never the tree `((a ~~ b) ~~ c)`. There is no
associativity, hence no left-before-right: **the horizontal position of a peer is pure layout, it
does not order execution** (the horizontal analogue of §5.1's "reordering the `node` lines changes
nothing"). The binding `a ~~ b ~~ c` names each result positionally, a naming convenience that
likewise implies no order.

The two axes now carry exactly one meaning each, and the compiler checks both:

- **A newline (going down) asserts a dependency** on what is above. Assuming a dependency is
  always sound (at worst it serializes needlessly), so this is the conservative default.
- **`~~` (going across) asserts independence**: run these in any order. This assertion must be
  PROVEN (disjoint effects or purity, §5.3). If two peers conflict on an effect, `f() ~~ g()` is
  a COMPILE ERROR naming the shared resource, not a race to discover at runtime. `~~` is a
  *verified claim*, not a scheduling hint (unlike `go f()` / `async`, which say "trust me").

This also repairs the original sin §1 names: today a newline is nearly free, so you
over-serialize `a; b; c` by default without meaning to. Here, piling everything vertically
becomes a visible, honest statement ("I declare this a chain of dependencies"), usually false,
and the reader now *sees* the false serialization.

So `~~` = same layer (concurrent); a newline = the next layer (may depend). The inline form is
the linear projection of the layered layout: `par` fans out one row, the next line joins it.
`par` is the proven descendant of Occam's `PAR` (CSP): the parallel composition Occam could
express but not verify, Sond both expresses and proves.

**Why `~~` and not `~`.** A single `~` is already prefix bitwise-NOT. A parser can tell prefix
`~a` from an infix `a ~ b` by position (as it does for unary vs binary `-`), so this is not a
grammar ambiguity; the problem is human. Overloading one glyph across a *tight* bitwise unary and
a *loose* scheduling delimiter, at opposite ends of the precedence spectrum, reads badly (the
lesson `port`-as-a-keyword taught the same week: reuse that steals a common meaning is paid for
forever). Two further points settle it: (1) as a flat delimiter of `par`, `~~` never enters the
expression precedence table at all, so any residual ambiguity is localized to the `par` context;
(2) `~~` is its own two-character token, never bitwise-NOT, and its weight reads as structural,
like `&&` or `||`. (`||`, the canonical CSP parallel-composition symbol, is already the
contract disjunction, so it is taken.)

### 5.2 Effects impose the only other edges

Pure nodes are ordered solely by data. **Effectful** nodes carry their effect set (RFC-0000
§5.3). The compiler adds an **effect edge** between two nodes only when their effects
*conflict* on the same capability/resource (write–write, or read–write). Non-conflicting
effects (two writes to disjoint capabilities; two reads) stay concurrent.

```
graph {
    node a = db.read(users)        effects [Db]     // read
    node b = db.read(orders)       effects [Db]     // read     (concurrent with a: two reads)
    node c = log.write(a)          effects [Log]    // disjoint capability: concurrent with a,b
    node d = db.write(users, a)    effects [Db]     // write on `users` -> ordered AFTER a
    yield d
}
```

Conflict is decided by capabilities and the resources they name, the same machinery RFC-0000
§6 already uses for security. A function that was not *given* a capability cannot conflict on
it, so capability-scoping directly widens provable parallelism.

### 5.3 The determinism obligation (what the oracle must prove)

A graph is admissible only if its observable result is **the same under every valid schedule**
(confluence). This is the core verification obligation, and it decomposes cleanly:

- **Pure nodes**: confluence is *free*: referential transparency means value depends only on
  inputs, never on timing.
- **Conflicting effects**: the prover must either (a) show the effects **commute** (order does
  not change the observable outcome), or (b) accept the **serializing edge** it inserted
  (§5.2) as the resolution. If neither holds (a genuine race whose result depends on timing),
  the graph is **rejected at compile time**, exactly as an unprovable contract is today.

So nondeterminism is not a runtime hazard to test for; it is a **compile error**. This is the
graph-core analogue of "a postcondition it cannot prove does not compile."

### 5.4 No data races, by construction

A data race requires two concurrent accesses to the same location, one a write. In the graph
core: pure nodes touch no shared location; effectful nodes that *could* race on a resource
receive a serializing effect edge (§5.2) before they are allowed to be concurrent. Therefore a
scheduled graph is race-free **by the same proof that admitted it**: the runtime needs no
locks for correctness, only for the effect edges the compiler already made explicit.

### 5.5 Execution model

The admitted graph is a DAG (data edges + effect edges). The backend schedules it as a
**work list over a fixed worker pool** (`@profile` picks the width; a single worker degrades
gracefully to a topological linear run, identical result, guaranteed by §5.3). Because every
ordering constraint is explicit in the DAG, the scheduler is trivial and lock-free: a node
runs when its in-edges are satisfied. Targets:

- **hosted**: worker threads (clone/futex) over the DAG.
- **freestanding / kernel** (RFC-0003): the DAG *is* a task graph, a natural fit for the
  cooperative `swtch` primitive already in the surface, or for per-core run queues.
- **`--shared`**: a graph compiles to a C-ABI entry that runs the DAG internally; the boundary
  stays sequential.

## 6. Grammar sketch (extends RFC-0001)

```
Expr    ::= ... | GraphExpr
GraphExpr ::= "graph" "{" { NodeDecl } "yield" Expr "}"
NodeDecl  ::= "node" Ident "=" Expr [ "effects" "[" IdentList "]" ]

Stmt    ::= ... | ParStmt | ParLet | ParReduce
ParStmt  ::= "par" Call { "~~" Call }
ParLet   ::= "let" Ident { "~~" Ident } "=" "par" Call { "~~" Call }
ParReduce ::= "let" Ident "=" "par" "(" CommOp ")" Call { "~~" Call }
CommOp   ::= "+" | "*" | "&" | "|" | "^"
```

`node`, `graph`, `yield` and `par` are new keywords. `~~` is the flat peer delimiter (its own
two-character token, not an infix operator; it does not enter the expression precedence table).
A `NodeDecl` binds an SSA-style value; dependency edges are the free variables of each node's
`Expr` that are other nodes; effect edges are computed from the `effects` clauses.

### 6a. `par` status: IMPLEMENTED (the inline form)

The three inline `par` forms are implemented over the RFC-0010 Layer B thread substrate
(`clone` + `futex`, thread-local arena), verified with real profiling at ~290% CPU on three
compute-bound peers (a 2.8x speedup over the same work in series):

- **`par f() ~~ g() ~~ h()`** (statement): run the peers concurrently, join before the next line.
- **`let a ~~ b ~~ c = par f() ~~ g() ~~ h()`**: bind each result BY SOURCE POSITION (deterministic
  regardless of finish order), so the caller may combine them with any operator.
- **`let s = par(+) f() ~~ g() ~~ h()`**: fold the results with a COMMUTATIVE-ASSOCIATIVE operator
  (`+ * & | ^`) into one value. A non-commutative reducer is a compile error, because the peers
  finish in an unspecified order and only a commutative monoid keeps the result schedule-independent
  (§5.3 applied to the reduction). This is the streaming-reduce case where §5.3 bites; the positional
  bind above buffers by source and so needs no such restriction.

The `graph { node .. yield e }` block (§5.1) is IMPLEMENTED, including its parallelism: nodes are
grouped into topological levels, and a level's independent CALL nodes run concurrently on the Layer
B thread substrate, joined before the next level (verified at ~290% CPU on a level of three
independent nodes). The author writes only the data dependencies; the compiler extracts the
concurrency. A dependency cycle is a compile error. EFFECT EDGES (§5.2) are implemented: two same-level nodes
whose footprints conflict on a mutable global get a serializing edge, so they run in sequence
instead of racing, while independent nodes stay parallel (verified: conflicting nodes serialize
deterministically with no thread spawned; disjoint nodes still spawn). Still ahead for the graph:
effect edges through pointer/heap aliasing (needs RFC-0011 capabilities, as for `par`), and explicit
`effects` clauses on nodes.

The independence/confluence PROOF (§5.3) has begun: `par` peers are verified confluent over the
module's MUTABLE GLOBALS. A per-function read/write footprint over the mutable globals is computed
transitively across the call graph, and a `par` whose peers write the same global (or one writes
while another reads) is a COMPILE ERROR: a data race whose result depends on the schedule. Pure
peers and peers on disjoint globals are admitted. So `~~` is now a verified claim w.r.t. global
state. Still ahead: extend the proof to coupling through raw pointers / shared heap (needs the
effect/capability system, RFC-0011), the full `graph { node .. }` block, and the effect-conflict
edges (§5.2).

## 7. Worked example: a fiscal batch, honestly parallel

```
// Compute VAT and IRPF for a batch, write an audit line, then persist, with the maximum
// parallelism the data allows, proven, not hoped.
fn settle(batch: Invoices) -> (r: Report)
    requires { batch.count > 0 }
    ensures  { r.vat >= 0 }
{
    graph {
        node vat   = sum_vat(batch)                    // pure
        node irpf  = sum_irpf(batch)                   // pure, concurrent with vat
        node audit = log.write(batch.id) effects [Log] // disjoint capability, concurrent
        node rep   = build_report(vat, irpf)           // waits for vat, irpf (not audit)
        node saved = db.write(rep)       effects [Db]  // waits for rep
        yield saved
    }
}
```

`vat`, `irpf` and `audit` run at once (no shared data, disjoint effects); `rep` joins the two
pure results; `saved` follows `rep`. The `ensures { r.vat >= 0 }` is proven over the graph the
same way it is over a linear body: the contract does not care about schedule, because §5.3
guarantees the schedule is irrelevant.

## 8. Fit with the qwen3.6 ceiling (RFC-0000 acceptance test)

The graph core is *easier* for a weak model to author than linear code, not harder: the model
emits `node name = expr` bindings and never has to choose an order or reason about
interleavings, the two things models (and humans) get wrong. The oracle does the hard part
(the determinism proof). This is the ideal division of labour of the Prime Directive: the AI
states intent + dependencies; the compiler proves the concurrency sound. A `graph` block that
cannot be proven confluent is rejected with a counterexample (RFC-0000 feedback mandate: *the
two nodes that race, and the input that shows it*), so the red→green loop converges.

## 9. The honest limits (no hand-waving)

- **Amdahl is real.** The critical path (the longest chain of *true* dependencies) is an
  irreducible floor. The graph core removes **false** serialization, not the real kind; a
  computation that genuinely depends step-by-step gains nothing, and should not.
- **Effects still serialize.** Two writes to the same resource are ordered, period. The win is
  that *only* real conflicts serialize, and the conflict set is provable, not guessed.
- **This is not automatic parallelization of legacy code.** It is a *source paradigm*: the
  parallelism is semantic and authored, and its correctness is proven. Auto-parallelizing
  arbitrary mutable code remains undecidable; the graph core sidesteps that by building on
  immutability + explicit effects, where the analysis is tractable.
- **Scheduling overhead is not free.** For tiny graphs the worker-pool hand-off can cost more
  than it saves; a `@profile`/threshold decides when to actually fan out vs. run the DAG on
  one worker (same result either way).

## 10. Open questions

1. **Cycles.** Pure data cycles are forbidden (no fixpoint semantics in v1). Do we ever want
   feedback nodes (synchronous-language style `pre`/`fby`) for streaming/reactive code, or is
   that a separate RFC?
2. **Loops as graphs.** A `for` over independent iterations is an implicit fan-out (a `map`).
   Should `for` gain a `parallel` form that lowers to a graph, or should the compiler infer
   it from the (proven) independence of iterations?
3. **Effect commutativity proofs (§5.3a).** How much commutativity can the current prover show
   (e.g. two increments of disjoint counters) before we need a richer effect algebra?
4. **Granularity.** Node = expression is fine for coarse work; do we want to *fuse* small pure
   nodes to amortize scheduling, and is fusion a compiler concern or a `@profile` knob?
5. **Diagnostics.** What is the most legible way to show a rejected graph, the conflicting
   node pair and a witnessing schedule, so qwen3.6 fixes it in one iteration?
6. **Interaction with `--shared` bindings (RFC-0007).** A graph behind a C-ABI boundary runs
   its DAG internally; is any of the parallelism observable/controllable from the C caller, or
   is the boundary always "one opaque call"?

---

*This RFC is exploratory. It does not change the implemented surface; it describes the
destination the existing guarantees (immutability, effects, contracts, a sound oracle) were
already pointing at: the point where Sond stops being a better line of statements and becomes
the language whose programs are proven graphs.*

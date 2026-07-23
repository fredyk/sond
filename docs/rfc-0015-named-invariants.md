# RFC-0015: Named invariants: the change you cannot forget

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0015                                                                  |
| Status      | Draft (exploratory)                                                   |
| Date        | 2026-07-23                                                           |
| Authors     | Jhon Magdalena Vila                                                         |
| Motivation  | A codebase grows until no one holds all the sites a meaning touches; the change that compiles but is wrong is the dominant failure |
| Related     | RFC-0000 (local reasoning, contracts, effects), RFC-0008 (the graph core), RFC-0011 (effects and capabilities) |

## 1. The problem: implicit semantic coupling

The bug that survives to production in a large codebase is almost never a type error. It is
this: you change an assumption in one place, fix "the two or three sites that obviously depend
on it", ship, and a month later a fourth site, two thousand lines away, misbehaves through a
side effect. The compiler never complained, because it was never a *compilation* problem: the
coupling between the site you changed and the site you missed lived in **your head, not in the
code.**

Call it **implicit semantic coupling**: two points of a program share an assumption ("this list
is sorted", "an id is always positive", "this buffer is NUL-terminated", "money is never
negative") that is real, load-bearing, and written down nowhere the machine can check. As the
program grows, the number of such couplings grows with it, and the human ceases to hold them
all. The characteristic symptom is the sentence *"I'll just change this here and there, and I
think that's all the places"*, followed weeks later by *"it turned out there was one more."*

This RFC is about making that class of error a **compile error**, the same way an added enum
variant makes every non-exhaustive `match` a compile error today.

## 2. Why the usual tools only half-help

- **Types catch coupling of *shape*, not of *meaning*.** Change a signature and the compiler
  shows every call site. Change the *meaning* of a value while keeping its type (`i64` that used
  to be non-negative now can be negative) and the type checker is silent: nothing about the type
  changed.
- **Tests catch only the coupling you already foresaw.** The site two thousand lines away is,
  by construction, the one you did not think to test. A test suite is a set of hypotheses about
  what matters; the surprise site is outside the hypotheses.
- **`grep` is search-and-pray.** It finds the sites that are *spelled* similarly, not the sites
  that *depend on the meaning*. The dangerous site often shares no token with the change.

The one mechanism in common languages that genuinely forces completeness is the **exhaustive
`match`**: add a variant to a sum type and every incomplete `match` fails to compile, with the
full list of sites that must react. That is exactly the shape of the solution, and it works for
one reason: the dependency ("these matches depend on this enum's set of cases") is **explicit in
the type and the closure is computed by the compiler.** This RFC generalizes that single working
idea from "the set of a sum type's cases" to "any named semantic assumption."

## 3. The thesis: turn the coupling into a proof, and completeness comes for free

The reason the compiler cannot find the missed site is that **the assumption is not in the
proof.** If it were, breaking it would break the proof at that exact site, at compile time. So
the move is not to invent a new whole-program analysis: it is to let the author **name the
assumption once**, attach it where it is relied on and where it is preserved, and let Sond's
existing oracle do what it already does: verify each site locally against the contracts in scope.
Change the assumption, and every proof that depended on it stops going through. The set of
affected sites is then not guessed; it is the set of proofs that now fail, which the compiler
enumerates completely.

"It was not a compilation problem" becomes a compilation problem.

## 4. Sond is already halfway there

Four things Sond already commits to are the substrate this needs:

1. **Contracts (`requires`/`ensures`, RFC-0000).** A named invariant is a shared, reusable
   proposition; a contract is the same machinery, per function. The prover already discharges
   propositions of exactly this form.
2. **Local reasoning (RFC-0000).** Every function composes against its interface, not a global
   timeline. That is precisely what lets the impact of a change stay bounded (§6).
3. **Explicit effects and capabilities (RFC-0011).** The couplings that flow through *shared
   state* (not through a value's type) are visible only if every access to that state is
   declared. Sond already forces that, so the "what touches this state" map is total.
4. **The graph core (RFC-0008).** A content-addressed dependency graph makes "who depends on
   this definition" an exact query, not a `grep`. That is the enabling data structure for
   enumerating impact completely.

## 5. Design

### 5.1 A named invariant

An `invariant` is a named, parameterized proposition, declared once at module level:

```
invariant Sorted(xs: Slice) { forall i in 0 .. xs.len - 1 : xs[i] <= xs[i + 1] }
invariant NonNeg(x: i64)    { x >= 0 }
```

It is not code that runs. It is a proposition the prover can instantiate, exactly like the body
of a `requires`/`ensures`, but referred to by name from many places.

### 5.2 Subscribing: `assumes` and `maintains`

A function states its relationship to an invariant in its contract:

```
fn binary_search(xs: Slice, k: i64) -> (r: i64)
    assumes Sorted(xs)            // I rely on it: the prover may use it as a fact
{ .. }

fn insert(xs: Slice, x: i64) -> (r: Slice)
    maintains Sorted(r)           // I promise it: the prover checks my body re-establishes it
{ .. }
```

- `assumes P` seeds `P` into the function's assumptions (like a `requires`, but by name).
- `maintains P` adds `P` to the obligations the body must discharge (like an `ensures`, but by
  name), *and* records that this function is a maintainer of `P`.

These are ordinary contract clauses; they ride the same verifier. The only new thing is the
*name*, which is what lets the compiler build the "who touches this meaning" graph.

### 5.3 The invariant rides the data (the missed site cannot hide)

The key against the "two thousand lines away" mutation: an invariant may be **attached to a
type** as a refinement, so it travels with the value.

```
type SortedSlice = Slice where Sorted(self)
```

Now any function that produces or mutates a `SortedSlice` carries `maintains Sorted` as an
obligation *by virtue of the type*, not by remembering to write it. A new function, written a
month later, that takes a `SortedSlice` and returns one but breaks the order **does not compile**:
its body fails to discharge the `Sorted` obligation the type imposes. The 2km site cannot silently
violate the invariant, because touching a value that carries it obliges re-establishing it. This
is typestate/refinement applied to exactly the assumptions that used to be tacit.

### 5.4 Change propagation, and why contracts are firewalls

When the definition of an invariant changes, the compiler invalidates and re-verifies **every
subscriber** (`assumes`/`maintains`) and every refinement type that names it, and reports the
complete list. Because the subscribers are an explicit, named set (not inferred by search), the
list is *complete*: there is no "one more site" left to discover in production.

Crucially, this does **not** cascade across the whole program, because a contract is a **cut in
the dependency graph**:

- Change a function's *implementation* but keep its contract (and the invariants it maintains):
  **nothing propagates.** Callers depended on the contract, not the body.
- Change a *contract or an invariant*: only the **direct frontier** re-verifies. If those
  neighbours still satisfy their own contracts, propagation stops there.

The blast radius is "where the meaning actually changed", not the transitive closure. This is the
**frame rule** of separation logic: a function's footprint (its contract plus its declared
effects) is explicit and local, so everything outside the footprint is provably unaffected, and
the compiler need not look at it.

### 5.5 State coupling: the effect graph is the complete map

Not all coupling flows through a value's type; some flows through shared state. Here RFC-0011 is
already the answer: a piece of shared state is a capability, and **every** function that touches
it carries it in its effect signature, by construction. So "who is coupled to state X" is a total
query over the effect graph, never a `grep`. An invariant over shared state
(`invariant CacheConsistent(cache: Cap[Cache]) { .. }`) is maintained by exactly the functions
that hold the capability, and the compiler knows all of them. The 2km site that mutates X cannot
exist without declaring `Cap[X]`, so it cannot hide.

## 6. Why it does not explode into millions of lines

The natural first objection: "so I have to declare the whole call graph?" No. **You declare
nodes, never edges.** The author writes one local fact per function (its contract, its
`assumes`/`maintains`, its effects): that is `O(N)` in the number of functions, linear. The
**edges are derived by the compiler**, and traversing a graph is linear in its edges, not
exponential; the exponential blow-up is only in *enumerating paths by hand*, which no one does.

This is already how three familiar mechanisms work: you write each function's *type* (local) and
the compiler computes the whole compatibility graph; you never annotate a call site as
type-compatible. Same for exhaustive `match`, same for effect inference. The map with many edges
exists, but it lives in the compiler, not in your source.

And the frame rule (§5.4) means the map is rarely even traversed far: most changes are
implementation-only and propagate to zero sites; a contract change propagates to a frontier and
stops. Named invariants do not add lines proportional to edges; they **remove the need to trace
edges by hand.** The only cost is `O(N)`: naming, once, the assumption you were already carrying
in your head.

## 7. Grammar sketch (extends RFC-0001)

```
Item         ::= ... | InvariantDecl
InvariantDecl ::= "invariant" Ident "(" ParamList ")" "{" Prop "}"
Contract     ::= { "requires" "{" Prop "}" | "ensures" "{" Prop "}"
                 | "assumes" InvokeInv | "maintains" InvokeInv | EffectClause }
InvokeInv    ::= Ident "(" ArgList ")"
Refinement   ::= "type" Ident "=" Type "where" InvokeInv
```

`invariant`, `assumes` and `maintains` are new keywords. `Prop` is the existing contract
proposition grammar (it may quantify with `forall .. in .. :` where the prover supports it,
RFC-0012). An `InvokeInv` instantiates a named invariant at concrete arguments, expanding to its
proposition with the parameters substituted, so no new proof machinery is required, only naming
and substitution.

## 8. Worked example

```
invariant NonNeg(x: i64) { x >= 0 }
type Balance = i64 where NonNeg(self)

// A withdrawal keeps the balance non-negative. Written once, the invariant is now the account's
// law: any later function that returns a Balance must re-prove it, or it does not compile.
fn withdraw(b: Balance, amount: i64) -> (r: Balance)
    requires  { amount >= 0 }
    maintains NonNeg(r)
{
    if amount > b { b } else { b - amount }   // returning `b - amount` unguarded would fail NonNeg(r)
}

// Six months later, a hurried refactor adds a fee. It forgets the guard. It DOES NOT COMPILE:
// `b - fee` cannot be proven >= 0, and `Balance` demands it. The 2km bug is caught at the keystroke.
fn withdraw_with_fee(b: Balance, amount: i64, fee: i64) -> (r: Balance)
    requires  { amount >= 0; fee >= 0 }
    maintains NonNeg(r)
{
    let after = if amount > b { b } else { b - amount }
    after - fee            // rejected: prove `after - fee >= 0` (needs a guard) or it will not build
}
```

The invariant `NonNeg` was declared once. Nothing about `withdraw_with_fee` referenced `withdraw`,
yet the shared assumption is enforced at both, and the omission is a compile error, not a
production incident.

## 9. Fit with the qwen3.6 ceiling (RFC-0000 acceptance test)

Named invariants make a weak author *more* likely to be correct, not less. The model does not have
to remember the tacit rule or hunt for its sites: the rule is named, attached to the type or the
capability, and the compiler rejects any body that violates it with a local counterexample
(RFC-0000's feedback mandate: the value and the site that break it). The hard part (finding all
affected sites) is done by the compiler; the model only states intent. That is the Prime
Directive's division of labour: the author declares the assumption, the oracle guarantees
completeness.

## 10. The honest limits

- **You must name the invariant.** This is not automatic discovery of tacit assumptions; it is a
  discipline of writing the assumption down once. The invariant you never name will still surprise
  you. The trade is deliberate: `O(N)` up-front naming buys away the whole class of "one more
  site" bugs.
- **The proposition must be in the prover's fragment.** An invariant the oracle cannot decide
  (rich quantification, non-linear arithmetic beyond RFC-0012) cannot be enforced statically, only
  documented. The set of expressible invariants grows with the prover, not faster.
- **Refinements need the type system work of RFC-0012.** `type T = U where P(self)` presupposes
  refinement types; this RFC depends on that track for §5.3. Without it, `assumes`/`maintains` on
  functions still work (they are pure contract sugar); only the "rides the data" guarantee waits.
- **It is not free re-verification.** Changing a widely-maintained invariant re-checks its whole
  frontier. That is the point (the frontier is exactly what might now be wrong), but it is real
  compile-time cost, bounded by the frame rule to the frontier, not the closure.

## 11. Open questions

1. **Quantifier support.** How much of `forall i in 0 .. n : P(i)` can the current linear prover
   discharge (with loop invariants, RFC-0012), and which invariants must wait for a richer
   fragment or an SMT backend?
2. **Invariant composition.** Should invariants be able to reference each other
   (`Sorted(xs) and Distinct(xs)`), and how does changing one propagate through the other?
3. **Global vs data-carried.** When is an invariant better attached to a type (§5.3) versus to a
   capability (§5.5)? Is there a single surface that covers both, or are they genuinely two forms?
4. **Migration ergonomics.** When a maintained invariant changes and the frontier is large, what
   is the most legible way to present "these N sites must be revisited, here is the failing
   obligation at each", so the red-green loop converges site by site?
5. **Interaction with the graph core (RFC-0008).** In a content-addressed graph, an invariant is
   itself a node; is "who subscribes to this invariant" just another edge query, and does that
   subsume the special-casing here?

---

*This RFC is exploratory. It does not change the implemented surface; it names the destination the
existing guarantees (contracts, effects, local reasoning, a sound oracle) already point at: the
point where the assumption you used to keep in your head becomes a thing the compiler keeps for
you, completely.*

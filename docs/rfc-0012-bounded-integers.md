# RFC-0012: Bounded integers and refinement types --- the mechanism that closes overflow (#3) and bounds (#2)

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0012                                                                  |
| Status      | **DECIDED (JMV 2026-07-18): (A) checked-by-default, reached via (C) as the non-breaking on-ramp.** #3 and #2 are unblocked, staged implementation (§6). |
| Date        | 2026-07-18                                                            |
| Authors     | JMV + agents                                                         |
| Motivation  | #3 (silent overflow) and #2 (unchecked `a[i]`) are the two demonstrated Class-A holes still open; both are blocked on *the same* missing mechanism, not on effort |
| Related     | RFC-0000 (§5.2 dependent types), bootstrap-debt-registry (#2, #3), verify.rs (the prover) |

## 1. Why #3 and #2 are the same problem

The registry lists them separately, but they are one mechanism away from closed, and it is the
*same* mechanism:

- **#3 (overflow)**: `a + b` is safe iff `MIN ≤ a + b ≤ MAX`. The prover can discharge that
  obligation, *if the operands carry bounds*. With bare `i64`, they do not, so a strict check
  rejects the whole corpus (the registry's exact blocker).
- **#2 (bounds)**: `arr[i]` is safe iff `0 ≤ i < len(arr)`. Same shape, the prover can
  discharge it *if `i` carries a bound relative to `len`*. Today an array is a bare address with
  no `len`, so there is nothing to compare against.

Both need the same thing: **a value that carries a proven bound in its type**. That is a
*refinement type*: `i64 where 0 <= self < len`. Give the prover bounded operands and both
obligations become the div-by-zero closure (#3b) it already does, raise `lo ≤ e ≤ hi`, discharge
from the operands' refinements, reject what it cannot prove.

The demonstrated overflow break (`x>0 ⊢ x+1>x`, false at `x=MAX`) is exactly a *missing
refinement*: the proof needs `x < MAX`, which a refinement type would carry.

## 2. The design: refinements as contracts on a value's type

A refinement is a predicate attached to a type, in the same decidable fragment the prover
already handles (linear arithmetic):

```
type Idx(len: i64) = i64 where 0 <= self and self < len      // an index into a len-array
type u8b          = i64 where 0 <= self and self <= 255      // a byte
fn get(a: Arr, i: Idx(a.len)) -> i64 { a[i] }                // no runtime bounds check: proven
```

- A refinement is **just a contract on a variable**, so it rides the *existing* `verify.rs`
  machinery (Fourier–Motzkin over `Σcᵢ·vᵢ + k ≥ 0`). No new solver.
- **Flow refinement** (occurrence typing): after `if i < len { .. }`, inside the branch `i` has
  the refinement `i < len` (the branch guard, which the path evaluator already tracks, RFC's
  §5.3 of the prover). So most bounds come *for free* from the guard the programmer already
  writes; refinements are the *type-level* name for what the path analysis already proves.
- **Length-carrying arrays** (closes #2's representation gap): an array/slice is `{ptr, len}`
  (a fat pointer). `a.len` is a value the refinement of an index can mention. `a[i]` with
  `i: Idx(a.len)` needs no runtime check; `a[i]` with an unrefined `i` is a *compile error*
  telling you to prove `0 ≤ i < a.len` (branch guard, precondition, or a checked `at(a, i) -> Opt`).

## 3. How overflow (#3) closes with this

Two coherent policies, both expressible; the choice is §5.

- **Ranged-by-default**: integer types carry ranges (`i64` is `i64 where MIN <= self <= MAX`);
  every `+`/`−`/`*` raises `MIN ≤ result ≤ MAX`, discharged from the operands' ranges. Where
  ranges are tight (a byte + a byte fits in i64), it is free; where operands are full-range and
  the result could overflow, it is a *compile error* unless the author narrows (a precondition,
  a guard, or a wrapping/checked opt-in). This is "impossible to be wrong" at full strength.
- **Wrap-by-default + `checked`**: `+` wraps (defined, like today's machine), and overflow is
  only an error inside a `checked { }` region or on a refined type. Pragmatic, non-breaking,
  matches Rust-release/C.

Either way, the *prover* stops reasoning over ℤ and reasons over the value's *refined range*,
so it never again proves `x+1 > x` without `x < MAX` in scope, the demonstrated break is gone.

## 4. The division corollary (finishes #3b)

The same range machinery finishes the `i64::MIN / -1` gap (a demonstrated SIGFPE the current
div-by-zero obligation misses): with ranges, signed `/` and `%` raise
`¬(dividend == MIN ∧ divisor == -1)` alongside `divisor != 0`, discharged from the operands'
refinements (a divisor refined `!= -1`, or a dividend refined `> MIN`). Needs the signedness the
prover must track anyway once integers carry ranges.

## 5. THE DECISION (this is what unblocks #3 --- JMV's call)

Everything above is mechanism. One policy choice gates turning it on, and it is a language-
identity decision, not an implementation detail:

> **Q: What is the default semantics of `a + b` on an unrefined integer?**
>
> **(A) Ranged/checked-by-default**, overflow is a compile error unless proven impossible.
> Strongest ("impossible to be wrong"), most on-brand for Sond, but forces refinements/bounds
> pervasively (a bigger ask of the qwen ceiling; more annotation).
>
> **(B) Wrap-by-default + opt-in `checked`/refined**, `+` wraps (defined), overflow-checking is
> opt-in where it matters. Pragmatic, non-breaking, but concedes that the *default* can be
> "wrong" (silently wraps) unless you asked for the check.
>
> **(C) Ranged-by-default but only PROVABLE overflows are errors** (the weaker first step from
> the registry), reject only when the prover *proves* it overflows (e.g. constant folding past
> MAX); everything else compiles and wraps. Non-breaking today, a strict subset of (A) later.

My read: Sond's whole thesis argues for **(A)** as the destination, reached via **(C)** as a
non-breaking on-ramp (ship constant-overflow rejection now; grow refinements toward (A) as the
type system matures). But this is the language's soul, it is yours to set. Once you pick, #3
and #2 are no longer blocked; they are staged implementation (refinement contracts → length-
carrying arrays → the raise-and-discharge in verify.rs, which is the div-by-zero pattern).

> **DECISION (JMV 2026-07-18): (A) via (C).** Checked-by-default is the destination; ship (C)
>, reject only *provable* (constant) overflows, now as the non-breaking first step, and grow
> refinements toward full (A). Intended wrapping becomes explicit (`wrapping_add` / a `u64w`
> type). Before committing to full (A), run the qwen3.6 gate on an accumulator loop and an
> integer parser to measure the annotation burden the counterexample-guided loop must carry.

## 6. Staged implementation (after the decision)

1. **Length-carrying arrays** (`{ptr, len}` fat pointer + `a.len`), closes #2's representation
   gap and gives refinements something to mention. New surface → qwen3.6 gate. **✅ SHIPPED
   (2026-07-18):** `a.len` on fixed arrays (the constant N) and the built-in `Slice { ptr, len }`
   (construct `Slice { ptr: a, len: a.len }`, pass so a fn carries the length, `s.len`, `s[i]`
   through `.ptr`), riding the struct machinery, no new backend. Passed the qwen3.6 gate.

### 6a. Rich slice operations (JMV's request, Python-style ergonomics on `Slice`)

The `Slice` foundation makes these staged, each a self-contained increment (parser + a desugar
that produces a sub-`Slice` view, no copy, over the same backing store):

- **Sub-slice ranges** `s[lo:hi]` → `Slice { ptr: s.ptr + lo*8, len: hi - lo }` (a *view*, no
  allocation). Omitted bounds default: `s[:hi]` (lo=0), `s[lo:]` (hi=`s.len`), `s[:]` (whole).
  Parser: accept `:` inside `[ ]`; a new `Expr::SliceRange(base, lo?, hi?)`. **← next increment.**
- **Negative indices** `s[-1]` = last element, `s[-k]` = `s[s.len - k]`, from the end. Applies
  to both `s[i]` and range bounds. Constant `-k` folds; a variable negative lowers to a runtime
  `i < 0 ? i + s.len : i` normalization. (A future bounds obligation then proves `0 ≤ i < len`.)
- **Step / reverse** `s[::-1]`, `s[lo:hi:step]`, a strided view. Needs `Slice` to gain a `step`
  (a 3-word `{ptr, len, step}`, or a reversed materialization for `[::-1]`). More semantics
  (negative stride, iteration order) → its own design + gate.
- **Comprehensions** `[expr for x in src if cond]`, build a new `Slice` by iterating `src`,
  filtering by `cond`, mapping by `expr`. Needs a growable backing store (an arena/`alloc`, or a
  two-pass count-then-fill into a fixed buffer). The richest step; lowers to a loop + append.

The bounds obligation (step 3 below) then applies to slice indexing too: `s[i]` raises
`0 ≤ i < s.len`, discharged from a guard/precondition or a compile error, closing #2's variable
case for slices without a separate `len` argument to reason about.
2. **`where` refinement clauses on `let`/params**, parsed, lowered to prover assumptions (reuse
   the contract path). Flow refinement from branch guards is *already* in the path evaluator.
3. **The bounds obligation** for `a[i]`, raise `0 ≤ i < a.len`, discharge like div-by-zero.
   Unrefined index → compile error with a counterexample (the feedback machinery already exists).
4. **The overflow obligation** per the chosen policy (§5), raise `MIN ≤ e ≤ MAX`, discharge
   from ranges. Reuses div-by-zero end to end.
5. **The MIN/-1 division obligation** (§4).

## 7. Open questions

1. **`self` vs. a bound name** in refinement syntax, `i64 where 0 <= self < len` reads well;
   does it collide with anything in RFC-0001?
2. **Dependent-ish `len`**, `Idx(a.len)` mentions a *value* (`a.len`) in a *type*. How much
   dependent typing does that pull in, and can we keep it in the linear-arithmetic fragment
   (values that are affine in lengths) to stay decidable and fast for the qwen loop?
3. **Erasure**, refinements are proven and then *erased* (no runtime tag); confirm every
   refinement is discharged at compile time or is a compile error (never a runtime check),
   consistent with "release carries zero runtime contract checks" (registry).
4. **Wrapping opt-in surface**, if (A)/(C), what is the ergonomic escape for genuinely-wanted
   wrapping (a hash, a CRC)? `wrapping_add`, a `wrap { }` region, or a `u64w` type? See §8 -
   the crypto stdlib turns this from a corner case into a blocker for (A).

## 8. The wrapping substrate: crypto is not a corner case (overlooked when deciding §5)

The §5 decision (A-via-C) framed genuine wrapping as a rare escape hatch, "a hash, a CRC."
Building out the stdlib shows that framing is wrong, and it changes what (A) must ship with.

**The evidence.** The crypto/hash tower, ChaCha20, Poly1305, SHA-256/512, **SHA-3/SHAKE
(Keccak-f[1600])**, X25519, Ed25519, is built *entirely* on intentional mod-2⁶⁴ (and mod-2³²)
arithmetic. Every `+`, `*`, `<<` and rotation in Keccak's θ/ρ/π/χ, in ChaCha's quarter-round,
in Poly1305's accumulator, is *defined* to wrap; overflow is not a bug there, it is the
specification. A single `keccak_f` round has ~100 wrapping operations. This is not a handful
of CRCs, it is an entire subsystem, and it is the subsystem the PQ layer (ML-KEM/ML-DSA, all
SHAKE-based) and the whole secure-channel stack sit on.

**Why this matters for (A).** Under (C) (shipped) variable overflow still wraps, so this code
compiles today. Under the *destination* (A), every one of those thousands of operations becomes
a compile error unless individually proven bounded (they are not, they genuinely overflow) or
individually rewritten as `wrapping_add`/`wrapping_mul`/`wrapping_shl`. Annotating per-operation
would (a) triple the size of already dense crypto, (b) bury the *one* place a non-wrapping bug
could hide under thousands of "yes I meant to wrap" tokens, destroying the signal (A) exists to
create, and (c) be exactly the kind of pervasive-annotation burden §5 flagged as (A)'s risk to
the qwen ceiling.

**The design consequence.** (A) cannot land with only a per-operation `wrapping_*` surface. The
wrapping context must be declarable **once over a region**, so an entire crypto module reads as
ordinary arithmetic and the *checked* default is what is unusual there, the inverse of the
default elsewhere. Concretely, prefer a **lexical `wrap { … }` region** and/or a **module-level
`#![wrapping]` attribute**, under which `+ - * << >>` are the wrapping ops and the *checked*
forms are the ones you name (`checked_add`). A `u64w` *type* is the weaker option: it colours
values, but mixed-width crypto (u32 lanes packed into u64 state, byte↔word) forces constant
casts, re-introducing the noise. The region/attribute keeps the type system out of it and maps
cleanly onto how the code is already organised (one file = one primitive).

**Sequencing.** This means the staged plan (§6) gains a step-0 that is a *precondition* for
step-4 (the overflow obligation): **define and implement the wrapping region/attribute first**,
migrate the crypto stdlib into it (a no-op under (C): the region just documents intent), and only
then turn on checked-by-default outside such regions. Without step-0, flipping to (A) breaks the
entire crypto/PQ tower on the same day it lands. New surface (`wrap {}` / `#![wrapping]`) → runs
the qwen3.6 gate. Recorded here so the §5 decision is not mistaken for "just add `wrapping_add`."

---

*This RFC does not decide; it removes the excuse for #3 and #2 staying "blocked." The mechanism
is one design, refinement contracts + length-carrying arrays, riding the prover Sond already
has. It waits on exactly one call (§5): the default overflow semantics, which is a statement
about what kind of language Sond is.*

# Bootstrap Debt Registry, where today's compiler contradicts the Prime Directive

> Companion to [RFC-0000](rfc-0000-vision.md) (the **Prime Directive**). The implemented
> `sondc` surface is a **bootstrap**: it was kept deliberately simple (Go-like, `i64`,
> raw memory) to build the compiler and the OS tower fast, with a modest model (qwen3.6)
> in the loop. Several of those choices **contradict the Prime Directive**. They are
> listed here so they can **never normalize silently** as the stable language. Each is
> temporary debt with a directive-correct target; each closes as the designed guarantee
> layer (RFC-0000 §3–6, capabilities/effects/contracts) is implemented.
>
> The bound is the same as the directive's: the target must stay writable by a
> qwen3.6-class model in the red→green oracle loop.

## Classification by failure mode (this sets the order of attack)

Not all contradictions are equally dangerous. Sorted by how they let an AI be wrong:

- **Class A, the compiler ACCEPTS silent wrongness** (the worst; directly violates
  "impossible to be wrong"): code that **compiles green and is incorrect/unsafe**.
  i64-collapse (#1), unchecked `a[i]` (#2), silent overflow (#3), div-by-zero (#3b),
  `>>`/`>>>` by hand (#8), free `peek/poke/asm/outb/syscall` (#4), free kernel prims
  (#5), mutable globals (#7), the `.so` boundary (#6). **These are the priority** -
  and the qwen **three-attempt test does NOT catch them**, because the code compiles:
  qwen "succeeds", a naive test passes, the bug ships (the `null`/overflow class from
  the history of PLs). *The three-attempt test bounds difficulty from above (qwen can
  write it); making Class A unrepresentable bounds correctness from below (even a wrong
  qwen program does not compile). Both are required, without Class A closed, qwen's
  green lies.*
- **Class B, the guarantee EXISTS but is unreachable**: ~~the sound-but-vacuous oracle~~
  **✅ LARGELY CLOSED** (#9, below): a real prover now verifies branch/call/loop code
  (linear-arithmetic Fourier–Motzkin core + path branches + inter-procedural summaries +
  loop invariants). The remaining reach is a full SMT backend (non-linear, arrays).
- **Class C, the tool does not exist**: ~~no sum types + exhaustive `match`~~ **✅ sum
  types + exhaustive `match` LANDED**, and ~~`Option`/`Result`~~ **✅ payloaded enums
  (`Option`/`Result`, nil-safety) LANDED**, a variant carries a value (`Some(i64)`),
  and the payload is readable ONLY through `match` binding, so "forgot to check for
  absence" is unrepresentable. Lowered to a 2-word `{__tag, __payload}` struct (no new
  backend); construct via `let`, `match` with binding, pass by parameter; verified
  (`examples/shared/enums/option.snd`) and **passed the qwen gate on the first attempt**
  (qwen3.6 wrote an idiomatic `Reading = Value(i64) | Missing` with `or_zero`/`double_or`,
  independently verified). **✅ now fully usable:** a constructor works in any expression
  position (`f(Some(42))`, `return Some(x)`, `if c { None } else { Some(y) }`, via a
  hoist-to-temp desugar pass), and an enum is **returned by value** (`fn safe_div -> Opt`,
  System V RAX:RDX for ≤16-byte aggregates), passed a second qwen gate first try
  (`checked_sub -> Opt`, independently verified). Deferred within the feature: >2-word
  returns (MEMORY-class sret), a struct-returning call directly in expression position
  (bind to a `let` first), non-word payloads, generics (`Option<T>`). Still missing in Class C:
  refinement types, effects, capabilities (all RFC). These are the *mechanism* that turns
  Class A into "unrepresentable".

Order of attack: **(1) typed codegen** (the wedge, `f64` first) → **(2) close Class A**
by making each hole unrepresentable → **(3) Class B** (a real prover: loop invariants +
inter-procedural + SMT) → **(4) Class C** (sum types/Option/effects/capabilities).

## Headline finding (code-grounded): the oracle is SOUND but VACUOUS

Sond's central promise is "the compiler proves the contract." The prover is genuinely
**sound**, `verify.rs:75-82` REJECTS any contract it cannot prove (never accepts on
faith). But its verifiable fragment is straight-line integer arithmetic only:
`eval_block` bails to `None` on `For`/`Return`/`Break`/`Continue`/field-assign and any
call or non-arithmetic expression (`verify.rs:129,147,151`). So **a contract on any
function with a loop or a call is rejected**, i.e. on essentially all real code (kernel,
drivers, crypto). And functions *without* `ensures` are not verified at all
(`verify.rs:58`). Net effect today: **~100% of the codebase runs with zero verification;
the central pillar is aspirational.** (Even the prover's own arithmetic wraps -
`verify.rs:165-167` uses `i64::wrapping_*`, so it cannot reason about overflow either.)
This is debt item #9, and it is the highest-leverage contradiction of all.

## Implementation debt (ranked by leverage)

| # | Debt in the compiler TODAY | Violates (error it lets an AI make) | Directive-correct target |
|---|---|---|---|
| 1 | **Everything is `i64`** (bool, pointer, char, array-address all 64-bit; `parse_type` defaults omitted/`[]` types to i64) | type-category confusion (ptr + bool + char compiles) | distinct nominal types. **✅ WEDGE LANDED: `f64` (complete + passed the qwen gate).** The first non-i64 type; typed codegen (a `Ty` Int/Float pass drives INTEGER-vs-SSE): params/return in the System V SSE class (`%xmm0..`), `+ − * /` via SSE, int→f64 promotion (`cvtsi2sd`), float locals & mutable reassignment (`movsd`), comparisons (`ucomisd`), unary neg, float-valued `if`. Verified from Rust and Python (`examples/shared/math/floats.snd`), zero regression on i64 (146 tests). **Directive acceptance test PASSED:** qwen3.6, in one attempt, wrote a physics `.so` in f64 (vlen2/lerp/c_to_f/absf + Newton's hypot2) that compiled clean and was independently verified correct, f64's difficulty is within the ceiling. **✅ `u64` landed** (signed/unsigned split, see #8; passed the qwen gate). **✅ `i32`/`u32` landed:** integer widths, Ty carries a width, narrow ints wrap at 32 bits (canonicalized after each op / on store / on param entry), and sign+width select shift/divide/compare. Verified across the C ABI; passed the qwen gate (a u32 FNV-1a checksum, independently verified). **✅ all widths landed** (`i8/u8/i16/u16/i32/u32/i64/u64`): each wraps at its width, sign+width select the instruction, no implicit promotion. **Next of #1:** newtypes, sum types, `Option`/`Result`. |
| 2 | **Arrays are raw addresses; `a[i]` unchecked** (`shlq $3`/`addq`/`movq (%rax)`, no `i<len` compare) | out-of-bounds read/write, silent | length-carrying arrays + static bounds via the prover (`0 ≤ i < len`, discharged like div-by-zero). **BLOCKED on the representation**: an array is a bare base address today with no `len` to compare against. Prerequisite: a length-carrying array/slice type. Once `len` exists, the bounds obligation rides the same prover machinery as #3b. **Mechanism specified: [RFC-0012](rfc-0012-bounded-integers.md)** (refinement types + `{ptr, len}` fat pointers), same design as #3; no longer blocked once JMV picks the overflow policy (RFC-0012 §5). **✅ (C) ON-RAMP SHIPPED (2026-07-18):** `check_constant_bounds` (check.rs) rejects `a[K]` into a fixed-size `let a[N]` when K is a compile-time CONSTANT outside `[0, N)`, a provable stack overrun (off-by-one, wrong literal) is now a compile error naming the index and size, not a silent segfault. The index folds through `Int`/unary-neg/`+`/`-`/`*` of constants; sizes tracked per scope. Non-breaking (no valid program indexes a known-size array with a constant out of range; stdlib suite + all examples still compile), the bounds analog of #3's (C). Remaining for full (A): the VARIABLE-index case (a loop writing `a[i]`), which needs the length-carrying array representation + the prover's `0 ≤ i < len` obligation with loop invariants (RFC-0012 §6). |
| 3 | **Integer arithmetic wraps silently** (`addq`, no overflow check) | overflow bugs | checked arithmetic: overflow a compile error unless *proven* impossible. **The prover can now do this** (raise `MIN ≤ a+b ≤ MAX`, discharge from bounds), mechanically identical to the div-by-zero closure. **BLOCKED, though, on a policy/type decision:** unlike divisors (usually constant or guarded → safe), most `a+b`/`a*b` are over UNBOUNDED values, so a strict check would reject the whole corpus. Needs either **refinement types** (`i64 where 0 <= self < N`) used pervasively so operands carry bounds, or a **wrap-by-default + opt-in checked** policy. A weaker, non-breaking first step: flag only PROVABLE (disproven) overflows (constant folding past MAX). Decision needed before turning it on. **Mechanism + THE decision specified: [RFC-0012](rfc-0012-bounded-integers.md) §5**, refinement types close this the same way as #2; JMV picks the default overflow semantics (A checked / B wrap / C provable-only) and it is unblocked. **✅ DECIDED (A)-via-(C) + (C) SHIPPED (2026-07-18):** `check_constant_overflow` (check.rs) rejects any `+`/`-`/`*` of compile-time constants whose result overflows i64 (`9e18 + 9e18` is now a compile error). Non-breaking (corpus has none; out-of-range decimal literals already fail at lexing; hex `i64::MIN` is a single token). The remaining path to full (A) is the staged refinement work (RFC-0012 §6): length-carrying arrays → `where` clauses → the general obligation. **⚠️ DEMONSTRATED SOUNDNESS BREAK (2026-07-18):** `fn f(x: i64) -> (r: i64) requires { x > 0 } ensures { r > x } { x + 1 }` **compiles**, the prover proves `r > x` over ℤ (`(x+1) − x = 1 > 0`). But `f(i64::MAX)` satisfies `x > 0`, and `MAX + 1` wraps to `i64::MIN`, so `r > x` is FALSE at runtime. Today the always-on `ud2` contract guard traps it (SIGILL), a proven-correct program crashes on valid input, itself a directive violation. With the guard removed for proven props (the intended release behaviour, see "Reconciled design-doc clauses" below), `f(MAX)` **silently returns `i64::MIN`** with the "proven" `r > MAX` false (verified by temporarily disabling `check_contracts`: printed `-9223372036854775808`). **This puts #3 on a collision course with release guard-erasure: erasing a contract proven only by unsound ℤ reasoning turns a loud trap into a silent wrong answer. #3 must be closed before proven-contract guards are erased in release.** The prover must reason over wrapping i64 (or carry `MIN ≤ e ≤ MAX` obligations), not over ℤ, for any `+`/`−`/`*` whose operands are not bounded. Same class via **linear multiply**: `requires { x > 0 } ensures { r > x } { x * 2 }` compiles and traps at `x = MAX`. **⚠️ NARROW TYPES ARE WORSE, a SILENT break even with the guard on (2026-07-18):** `fn f(x: i32) -> (r: i32) requires { x > 0 } ensures { r > x } { x + 1 }` called with `x = i32::MAX` compiles, **returns `-2147483648`** (verified by printing r), and the `ensures` guard **does NOT fire** (exit 0), the runtime guard evaluates the postcondition on the *un-canonicalized* (pre-32-bit-truncation) value `2147483648 > MAX` = true, while the caller receives the truncated `i32::MIN`. So a false postcondition ships silently even in debug. Fix: narrow-int canonicalization must happen *before* the contract guard reads the value (and the prover must reason at the declared width, not ℤ). |
| 3b | ~~**Division has no divide-by-zero check**~~ **✅ RESOLVED (by proof, the Sond way)** (`idivq`/`divq` would #DE on `b==0`) | div-by-zero crash | **Done via the prover:** every integer `/` and `%` raises the obligation `divisor != 0`, discharged (Fourier–Motzkin + syntactic match) under the assumptions live at the site, a nonzero constant is trivial, a `requires { b != 0 }` or a branch guard (`if b == 0 {..} else { a/b }`, the `safe_div` shape) proves it, and an unguarded divisor is a COMPILE ERROR (checked on every function, not only contracted ones). Float `/` is exempt (IEEE-754 gives ±inf/NaN, never traps). Not a runtime trap, impossibility is PROVED, as the directive prefers. Gap: a precondition-bearing call in bare statement position isn't analysed (the division primitive itself always is). Verified: all 60 examples compile (unsigned.snd's `udiv`/`umod` now carry `requires { b != 0 }`). **✅ OVERFLOW OBLIGATION NOW CLOSED (2026-07-18):** signed `/` and `%` also raise the disjunctive obligation `divisor != -1 ∨ dividend != i64::MIN` (proving either rules out the `i64::MIN / -1` quotient overflow / SIGFPE). Raised for SIGNED division only, a `u64` operand makes codegen emit `divq` (no overflow), so unsigned still needs only the nonzero guard. Discharged the same way: `y > 0`, `x > 0`, a branch guard, or a non-(-1) constant; otherwise a COMPILE ERROR with a counterexample (`x = i64::MIN, y = -1`). `fn q(x,y) requires { y != 0 } { x / y }` is now correctly rejected. Root cause of a latent panic fixed en route: the linear prover did its arithmetic in i64, so reasoning about `i64::MIN` overflowed (debug panic / release silent-wrap = unsound); it now works in **i128 with checked arithmetic**, bailing conservatively on i128 overflow (aborted proof, never a wrapped one). Narrow signed types (i32) are safe in this backend, their values sit sign-extended in 64-bit registers, so `i32::MIN / -1` = `2^31` does not overflow the i64 `idivq`. |
| 4 | **`peek/poke/asm/outb/inb/inl/syscall` are free** | arbitrary memory/IO, no authorization | gate behind **capabilities** (`CapMMIO`/`CapIO`) + declared **effects** + `unsafe` region. **Mechanism specified: [RFC-0011](rfc-0011-effects-capabilities.md)**, these primitives demand a `Cap[Mem]`/`Cap[IO]` in scope; a body without it cannot name them. |
| 5 | **Kernel primitives free** (`interrupt fn`, `swtch`, `lidt`, `addr_of`, `read_cs`) | full hardware power with no gating | enclose in `@profile kernel` + hardware capabilities (`CapIRQ`/`CapMMIO`). **Mechanism: [RFC-0011](rfc-0011-effects-capabilities.md)** (`Cap[Kernel]`, available only under `@profile kernel`). |
| 6 | **`--shared` .so (C-ABI) voids all guarantees at the boundary** | callers (C/Rust/Py) bypass contracts, caps, effects | **Guarantee-preserving bindings, not just a raw symbol.** Emit alongside the .so a typed binding for the priority targets, **Rust first** (it shares Sond's vocabulary: `Option`/`Result`/sum types/ownership/no-null → near-1:1 mapping that re-imposes the contracts at the boundary), **Go second** (weaker: has errors/interfaces but `nil`, no sum types). C/C++/Python remain the raw C-ABI **`unsafe`** escape hatch (RFC-0007 trust boundary; crossing drops the guarantees). Prerequisite: rich types must exist first (Class C), so this rides on the typed-codegen wedge (#1). Priority per JMV: Go+Rust are the ones that matter. |
| 7 | **Module-level `let mut` globals** | action-at-a-distance; non-local reasoning | forbid, or pass state explicitly (RFC-0000 §3.3.2). **[RFC-0011](rfc-0011-effects-capabilities.md)** subsumes this: a mutable global is ambient mutable authority → a capability threaded explicitly (so the mutation appears in the effect signature) or forbidden. |
| 8 | ~~**`>>` (arithmetic) vs `>>>` (logical) chosen by hand**~~ **✅ RESOLVED** | pick the wrong one → silent corruption | **Done via `u64`:** `>>` now follows the LEFT operand's type, `sar` (arithmetic) for `i64`, `shr` (logical) for `u64`, so it is correct by construction; the AI never picks. Same for `/ %` (idivq vs divq) and `< > <= >=` (signed vs unsigned setcc). `>>>` remains only as an explicit escape until code migrates to `u64`. Verified on a value that is negative-as-i64 / huge-as-u64. **Passed the qwen3.6 gate:** qwen wrote a correct u64 FNV-1a hash + rotate-right `.so` (one attempt, independently verified); a type annotation `let h: u64` is authoritative for the local's signedness. |
| 9 | ~~**Contracts verified only on straight-line bodies**~~ **✅ LARGELY RESOLVED** (was: no branches, no calls, no loops → ~0% of real code verified) | ~0% of real code was actually verified | **Done via a real prover, in four increments.** (a) **Linear-arithmetic core**: propositions normalize to `Σ cᵢ·vᵢ + k ≥ 0` and entailment is decided by **Fourier–Motzkin** elimination (UNSAT/ℚ ⟹ UNSAT/ℤ, sound; a size cap bails conservatively), proves shifted/weaker bounds, transitivity, linear combinations. Self-contained, no external Z3. (b) **Branches**: path-based VC generation, each `if` forks, the guard (and its negation) entering that path's assumptions; one bad path rejects. (c) **Inter-procedural (modular)**: a call is reasoned about via the callee's contract, `ensures` (params→args, return→fresh var) become facts, `requires` become a discharged obligation; sound because every contract is independently verified (recursion = partial correctness). (d) **Loop invariants**: `for cond invariant { .. } { body }` verified by Hoare's while rule (init + preservation over havoc'd modified vars), continuation assumes `invariant ∧ ¬cond`. Still open: a full SMT backend (non-linear, arrays, quantifiers), and `>2`-var elimination blow-up beyond the cap. **✅ diagnostics, path naming landed:** a rejected postcondition or safety obligation now names the exact case it fails on (`… on the path where \`x < 0\``), built from the path's branch conditions, so the red→green loop sees WHICH path to fix, not merely which contract; a straight-line body carries no spurious clause. **✅ whole-body bail now names the construct:** when the body leaves the verifiable fragment the evaluator records the first (deepest) reason, so the diagnostic states it precisely, `a \`for\` loop without an \`invariant { .. }\` clause`, `an early \`return\``, `an unsupported expression (a comparison used as a value, or a field/index read)`, `a loop invariant not preserved by the body`, etc., instead of a generic laundry list, so qwen knows whether to add an invariant or restructure. **✅ counterexamples landed (Prime Directive feedback mandate, "actionable: a counterexample input"):** a rejected postcondition or safety obligation now exhibits a concrete breaking input, `cannot prove postcondition \`x >= 0\` (counterexample: x = -1)`, `the divisor \`b\` is nonzero (counterexample: b = 0)`, found by a bounded, VERIFIED search (evaluated with wrapping i64, so the witness is faithful to the machine; ≤3 free vars, seeded from the literals in play + type extremes; omitted rather than overclaimed when none is found in budget, and never a false positive). Still open here: witnesses for >3 vars / non-linear goals (the search is deliberately bounded). But the oracle is **no longer vacuous**, it verifies real branch/call/loop code. |
| 10 | **`asm("…")` without the RFC-0003 §5 guards** (no size cap, mandatory comment, audit) | unbounded unsafe with no isolation | apply RFC-0003 §5 `unsafe` isolation rules to the implemented `asm` |
| 11 | ~~**`print_int` renders a negative `i64` as unsigned**~~ **✅ RESOLVED** (`0 - 5` printed `18446744073709551611`) | silent wrong output (a −5 result read as a huge positive) | **Done via typed dispatch:** the callsite reads the argument's type, a SIGNED integer calls `sond_print_int_signed` (leading `-` + magnitude, correct even for i64::MIN), an UNSIGNED integer keeps `sond_print_int` (full magnitude). So `print_int(0 - 5)` prints `-5`, a `u64` still prints unsigned. Verified end-to-end and unit-tested (both directions). |

## Reconciled design-doc clauses (superseded by the Prime Directive)

- **RFC-0003 `script` profile** no longer relaxes *soundness*, only *ceremony* (optional
  contracts/effect annotations). The earlier line "the verifier will not require total
  soundness" is **void**. Soundness holds in every profile.
- **The `ud2` runtime contract guard** (`emit_contract_guard`) is **debug/oracle-only** and
  must NOT be emitted in a release build: a proven contract is erased, an unprovable one is
  a compile error (memory doc 09). Release carries zero runtime contract checks. *(Today it
  is already gated on `check_contracts`; this registry fixes the intent so it stays that way.)*

## Design tension to resolve: the coverage gate undervalues proof (2026-07-18)

The mandatory ≥80% **coverage gate** (`sond build/run/test` refuse to build under-tested code)
counts *basic blocks executed by tests*. But Sond ranks its two correctness mechanisms
unequally: a **proof** (a discharged `ensures`, true for ALL inputs) is strictly stronger than a
**test** (one example). The gate ignores that ranking, a fully *proven* function with no test
drags coverage down and can **block the build**, even though it is the most trustworthy code in
the file. Left as-is this creates a perverse incentive: to clear the gate, an author pads
coverage with vacuous tests, or **deletes a contract** the prover was checking so the function
reads as ordinary (uncovered-but-not-special) code, i.e. the gate can push toward *weaker*
correctness, a directive violation of the same kind this registry guards against.

**Direction (to decide):** coverage should be satisfied by **proof OR test**, composing
additively, a basic block inside a function with a discharged postcondition that pins its
result counts as covered without a test (the proof *is* the coverage; it dominates any test).
Concretely, either exclude proven blocks from the denominator or auto-credit them in the
numerator. Latent today (the prover only fully proves straight-line integer bodies, so few
functions qualify, Class B), but the gate's definition must be set coherently NOW so it never
normalizes "tested-only" as if proof did not count. New surface only in the driver's coverage
accounting → no language change; runs against the existing example suite.

## What is already directive-aligned and implemented (the seeds)
- Contracts are proven at compile time; unprovable ⇒ **does not compile** (never a runtime
  surprise)., the core of the directive, already live.
- **Value/pointer struct model**: value-structs are read-only when passed, `*T` declared up
  front, value-XOR-pointer methods. Catches Go's alias footgun *at compile time*. The proof
  that "harder to write → impossible to be wrong" is buildable within the qwen3.6 ceiling.
- Immutability by default (`let` vs `let mut`); immutable parameters.

*This registry is itself a guarantee: nothing on it is allowed to become "just how Sond is".*

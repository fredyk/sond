# RFC-0013: Dynamic memory without a collector and without lifetimes

Status: draft. Supersedes nothing. Depends on RFC-0011 (effects and capabilities).

## The problem this exists to solve

Sond today has no dynamic memory. Fixed arrays and the stack, and that is all. It is enough for a
kernel and for an 8-bit microcontroller, and it is the reason the language can honestly say it has
no collector and no borrow checker: it has nothing to collect and nothing to borrow.

That is not a position, it is an absence. The language cannot reach anything that outlives a
stack frame, which is most of what people write.

So this is the design that decides how far Sond goes. The bet the project is making is that the
useful gap is Rust's guarantees without the borrow checker and Go's simplicity without the garbage
collector. Reaching it requires an answer here that is neither.

## What the answer must satisfy

1. **No collector.** Not because collectors are bad, but because a pause is not available on a
   timer interrupt, and a runtime is not available in 2 KB of flash.
2. **No lifetime annotations threaded through signatures.** This is the specific thing that makes
   Rust hard, and the evidence says so: the annual Rust surveys and the usability literature put
   the difficulty on explicit lifetimes and on the self-referential structures that `async` forces
   through `Pin`, not on ownership itself, which people report learning in days. Copying ownership
   is fine. Copying `'a` is not.
3. **Deterministic, and local.** Where a byte is freed must be answerable by reading the function
   you are looking at.
4. **Reaches the metal.** Whatever this is, it has to work with no allocator underneath.

## What was considered and rejected, with the reason

### Automatic region inference (MLKit)

MLKit compiles Standard ML with a stack of regions inferred by the compiler. The source is
ordinary ML: no annotations, no new syntax, nothing to learn. It is the most attractive proposal in
the space and it is the one to learn the most from, because it failed in a specific way.

Inference decides placement from types and syntax alone, so it regularly puts values of very
different lifetimes in the same region. A list built inside a loop lands in the enclosing
function's region and is not released until that function returns. Nothing is leaked in the sense a
collector would recognise, and memory grows without bound anyway. The project documents this as
"space leaks", ships a profiling mode for it, and devotes a chapter of its manual to reading
compiler warnings like *the program might space leak in region r10*.

The ending is the argument: MLKit now integrates tracing garbage collection alongside its regions,
to reclaim what inference strands. A design that set out to replace the collector added one to
cover its own gaps.

The lesson is not "regions do not work". It is that **inference without a way for the programmer
to say otherwise trades a predictable cost for an unpredictable one**, and does it invisibly.
Changing a line in one place changes memory behaviour somewhere else with no diagnostic at the
point you touched. For a language competing on determinism that is worse than an honest collector,
because a collector is predictably slow sometimes and a region space leak is unbounded memory
somewhere that does not look suspicious.

**Sond will not infer placement.** Where a value lives is written down.

### Reference counting eliminated at compile time (Perceus, Lobster, Nim, Swift, Roc)

Koka's Perceus is precise rather than naive: it works out at each use whether this is the last live
reference, and its reuse analysis turns a matching drop and allocation into an in-place write, so
functional-looking code executes as mutation. The published benchmarks are strong.

Three reasons it is not the primary mechanism here.

None of these systems has been demonstrated on a target measured in kilobytes. Every one assumes a
heap, a per-object header, and an allocator. Sond's smallest target has 2 KB of flash and 128 bytes
of RAM.

None of them reuses anything Sond already has. The compiler carries a Fourier-Motzkin prover for
linear arithmetic. Refcount elimination is a different kind of analysis entirely, either use-typing
or whole-program alias analysis, so adopting it means taking on a second compiler-research bet
rather than extending the first.

And cycles are the shared blind spot: they leak in Lobster and Swift, need a non-deterministic
collector in Nim's ORC, and are unresolved in Perceus. Cyclic structures are ordinary in systems
code.

**The reuse analysis is worth stealing later** as a codegen optimisation on top of whatever this
RFC settles. The counting is not.

### Copying Rust

Rust solved this and the solution works. But the thing this project is betting on is that the
solution costs more ergonomically than it needs to, and the evidence points at a specific part of
it. Reproducing lifetimes reproduces the cost and leaves Sond with no reason to exist. Vale spent
years on generational references and paused; Hylo hides the analysis from the syntax but its own
community notes the compiler-internal work is as hard as a borrow checker either way; Pony's
reference capabilities are real but Pony is not collector-free, it uses per-actor reference
counting.

## The design

Two mechanisms that compose, both explicit, neither inferred.

### 1. Linear types, in the shape Austral gave them

A type may be declared linear. A value of a linear type must be used **exactly once**: not zero
times, which is a leak or an unreleased resource, and not twice, which is a use after move or a
double free. The check is a syntactic count of uses per variable, handling branches and loops. It
is not a solver and not a constraint system; Austral's implementation of it is under six hundred
lines.

```sond
linear type File

fn read_config(path: i64) -> i64 {
    let f: File = file_open(path)      // f is now live and owed
    let n = file_read(f, buf, 256)     // consumed, and returned renamed
    file_close(f)                      // last use; the debt is settled
    n
}
```

Forgetting `file_close` does not compile. Using `f` after `file_close` does not compile. No
lifetime is named anywhere, because no reference outlives the block it was taken in.

This also gives RFC-0011 its mechanism for free: a capability is a linear value. It cannot be
duplicated, so a function can only touch what its signature was handed, and a compromised
dependency cannot widen its own reach.

### 2. Named lexical arenas

Linearity alone cannot express a value that outlives the frame that made it, which is exactly what
a zero-copy parser or a per-connection buffer needs. Arenas cover that, without inference:

```sond
arena request {                    // one bump allocator, one lifetime
    let buf = alloc(request, 4096)
    let hdr = parse_headers(request, buf)
    respond(hdr)
}                                  // the whole arena is released here, at once
```

An arena is a bump pointer over a block the program already owns, so it works with no allocator
underneath and on a microcontroller. Arenas nest strictly, last in first out, which makes "does
`r1` outlive `r2`" a question about lexical nesting rather than about values.

That last point deserves an honest correction to an earlier assumption in this project: **the
prover does not help here.** Whether one arena encloses another is a scope check, the same kind
any compiler already does for locals. Fourier-Motzkin has nothing to contribute, and pretending
otherwise would be dressing up a simple check as a proof. The rule is that an allocation cannot be
stored anywhere whose arena encloses it, and that is enforced structurally.

## What this does not solve

Structures with irregular lifetimes: a graph whose nodes reference each other with no nesting to
lean on, a cache whose entries outlive the request that created them and die in an order nobody
can predict at compile time.

Nothing in this family solves that. Cyclone gave up and reached for a conservative collector for
its heap region, which meant the programmer had to know which half of the language they were
standing in. MLKit stranded memory and then added a collector. Rust solves it, with the machinery
this RFC is written to avoid.

So it is stated as an open problem rather than answered badly. A language that covers stack, arena
and linear resources honestly, and says the graph case is unsolved, is more useful than one that
claims to cover everything and leaks in the fourth case.

## Incremental plan

1. **Linear kinds and the use count.** No allocator, no arenas. Applies immediately to file
   descriptors, sockets and the capabilities RFC-0011 wants, all of which exist today and are all
   currently leakable.
2. **Arenas with structural escape checking**, and `alloc` against a named arena.
3. **Borrows**, read-only and mutable, restricted to lexical scope so no lifetime is ever named.
4. **Measure it.** The same gate the rest of the language is held to: a weak model writing against
   the reference alone, and a corpus that has to compile. If step 1 cannot be used without an
   expert, it is wrong and gets redesigned rather than documented.

## The open question, stated plainly

Whether linear types plus lexical arenas cover enough real programs to be worth the restriction.
If they do not, the honest options are to accept a collector for a clearly marked part of the
language, which Cyclone tried and which cost it its identity, or to accept lifetimes, which is
Rust. Both of those are known-good and known-costly. This RFC is the bet that a third position
exists and is reachable. It has not been proven yet.

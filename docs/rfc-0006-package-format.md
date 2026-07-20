# RFC-0006: Sond package format --- Go taken to its logical conclusion

| Field       | Value                                  |
|-------------|----------------------------------------|
| RFC         | 0006                                   |
| Status      | Draft                                  |
| Date        | 2026-07-09                             |
| Authors     | JMV + agents                          |
| Based on    | Go package model (JMV's strong preference) |
| Related     | RFC-0004 (trust frontier), RFC-0000 (capabilities) |

## 0. Starting point: Go's model, which JMV loves

Sond starts from Go's **package distribution model**, considered by the author one of the best that exists, and rightly so. Its virtues are exactly what the AI era needs:

- **Decentralized**: no central registry. You import **from where the code lives** (its repository URL). No central chokepoint, no mutable namespace that is an object of *name-squatting* or fraudulent publication.
- **Transparent provenance**: reading the import you know where the package comes from.
- **Cryptographic integrity**: `go.sum` stores checksums; `sum.golang.org` is a tamper-proof *transparency log*.
- **Minimum Version Selection (MVS)**: **deterministic and reproducible** version resolution, no SAT solver, no npm "resolver roulette".
- **Minimal ceremony**: `go.mod` is small; no baroque manifest.

This RFC **keeps all of that** and asks: *is there one more step for when the one writing imports is an AI?* The answer is yes, and curiously **it is the direction Go is already heading** with its checksums --- you just need to take it to its logical conclusion.

## 1. The specific problem of the AI era (with evidence)

Three problems of AI with dependencies, all **measured in real 2024-2025 studies**:

1. **Package hallucination** (USENIX Security 2025, arXiv:2406.10279): LLMs **invent package names** that do not exist in a systemic way (~1 in 5 depending on the model), and **43% of invented names repeat** -> *slopsquatting* (the attacker registers the hallucinated package).
2. **Use of deprecated/outdated APIs** (arXiv, 7 LLMs, 145 API mappings, 28,125 prompts): LLMs call **old** versions of libraries because their training is out of date -> *API drift*.
3. **Non-reproducibility** of AI-generated code (2025 empirical study): the same prompt produces different builds.

> **No study exists (as of Jul 2026) that ranks package distribution formats by "AI usability."** It is a gap. But the three problems above, cross-referenced, **determine** the ideal form: you must attack hallucination + drift + non-reproducibility all at once.

## 2. Honest survey of the state of the art (with AI lens)

| System | Package identity | Strength | Weakness for AI |
|--------|-----------------|----------|-----------------|
| **npm / PyPI / crates.io** | name + semver range, **central mutable** registry | huge ecosystem, discoverable | **worst case**: central namespace hallucinable -> slopsquatting; ranges -> drift |
| **Go modules** | `path@version` (URL) + checksum | **decentralized, checksummed, MVS reproducible, minimal ceremony** | identity coupled to **location** (moving/forking breaks the path); the AI can still hallucinate a plausible `github.com/x/y` |
| **Cargo** | name + semver, central registry | ergonomic, `Cargo.lock` | same class as npm: central + ranges |
| **Deno** | direct **URL** + lockfile | no central registry, integrity by lock | hallucinable URL / 404; identity = location |
| **Nix** | **hash of the derivation** | total reproducibility | heavy; at build level, not at code definition level |
| **Unison** | **hash of each definition's content** | **no dependency hell, codebase never broken, supply-chain resistant, no imports to break** | tiny ecosystem; big mental shift |

**Reading**: Go is one of the best because it already brings **decentralization + checksum + reproducibility**.
Its only weak point is that **identity = location+version**, not **content**. Unison solves
exactly that (identity = hash of content) but loses ergonomics. **The ideal is the union: the
Go philosophy, with Unison's content-based identity.**

## 3. The ideal format for Sond: Go + content-addressing + contracts

### 3.1 Identity = hash of content (the Unison piece)

Each **definition** (function, type, with its contracts included) is identified by the **hash of its
content** (its normalized AST + its contract). Not by a mutable name or by a URL.

This attacks the three problems in one stroke:
- **Hallucination -> impossible by construction**: a hash cannot be "invented" to resolve. And a
  **hallucinated friendly name simply does not resolve** (compilation error), never installs a malicious
  package in silence. The slopsquatting surface disappears.
- **API drift -> eliminated**: a hash fixes the exact definition and its contract. There is no "version
  range" that can drift. Updating is **explicitly re-pointing** to a new hash whose contract the
  compiler **re-verifies** against your usages -> if the new version breaks the contract, it does not compile.
- **Non-reproducibility -> eliminated**: content-addressed is bit-identical by definition. The "codebase
  is never broken" (Unison) comes for free.

### 3.2 Everything good from Go is preserved

- **Decentralized**: hashes are served from **any** mirror/repo/CDN --- location becomes a
  **mere cache hint**, not the identity. Moving or forking no longer **breaks anything** (the hash is
  the same wherever it lives). This is *better* than Go: it fixes its only weak point without losing
  decentralization.
- **Checksum by default**: the hash **is** the checksum. `go.sum` stops being a separate file:
  integrity is in the reference itself.
- **Reproducible**: stronger than MVS --- there is no version resolution at all; the hashes
  are the lockfile themselves.
- **Minimal ceremony**: the spirit of `go.mod` is maintained --- a small manifest.

### 3.3 Contracts ARE the package interface (the Sond piece)

What a package **exports** is its **contract** (`requires`/`ensures`/types/effects), not just its signature. Why this is key for AI:
- The AI **reasons about the contract** (small, fits in context) instead of the implementation
  (large, out of context) -> mitigates the documented "context rot".
- When re-pointing to a new version, the compiler checks **contract against contract**: an
  update that changes observable behavior **does not compile** until you review it. This is the
  structural response to *API drift*.

### 3.4 Capability manifest (the security piece)

Each package declares the **capabilities it needs** (RFC-0000 Pillar 4). A package that claims to be a
"JSON parser" but asks for `CapRed` is **suspicious and auditable at a glance**. The AI cannot inject
a dependency that exfiltrates without its manifest declaring it and the compiler propagating it.

### 3.5 Friendly name layer -> resolves to hashes (Go ergonomics, Unison security)

To avoid typing hashes, there is a **name layer** (like Unison aliases or Go paths) that is
**pure metadata pointing to hashes**. You write `import http` (friendly); it resolves to the hash pinned in your
manifest. A name that the AI hallucinated and is not mapped to a hash **fails at compilation** --- never
installs anything. The best of both worlds: Go's convenience, Unison's security.

## 4. What it looks like (syntax draft)
```sond
// Package manifest: friendly names -> content hashes.
// Preserves Go's minimalist spirit.
package app

require {
    http  = sond:7f3a9c...    // content hash; location is just cache
    regex = sond:b21e08...
}

// In code: friendly names, hash resolution.
import http
import regex
```
Updating `http` = changing its hash in `require {}`; the compiler re-verifies all usages against the
new contract. If it breaks, it does not compile.

## 5. Comparison: why this is "Go, but better for AI"

| Property | Go | Sond (RFC-0006) |
|----------|-----|-----------------|
| Decentralized | Yes | Yes (better: location = cache only) |
| Cryptographic integrity | Separate `go.sum` | The hash **is** the identity |
| Reproducible | MVS | Total (hashes = lockfile) |
| Moving/forking a package | breaks the path | no impact (identity = content) |
| Package hallucination | possible (inventable path) | **impossible** (hash / unmapped name fails) |
| API drift | ranges can drift | **impossible** (hash fixes contract; upgrade re-verified) |
| Package interface | type signature | **contract** (AI-reasonable in context) |
| Dependency security | by convention | **verified capability manifest** |

## 6. Open questions

1. **Hash granularity**: per definition (Unison, maximum reuse but many hashes) or per module/package (closer to Go, more manageable)? Proposal: per definition internally, exposing friendly package names on top. **Pending** to decide the exact point.
2. **Discovery**: without a central registry, how does the AI (or the human) find a package that solves a need? Go solves this with known search engines/URLs; Unison with Unison Share. **Pending**: a discovery index that is NOT a mutable hallucinable namespace.
3. **Contract hashing**: the hash includes the contract, but does it include the implementation? If two different impls satisfy the same contract, are they the same package? Proposal: hash of the **contract** for the interface + hash of the **impl** for the artifact; allows swapping verified-equivalent impls.
4. **Interop with the non-content-addressed world** (npm, Go, C): via the trust frontier of RFC-0004
  (wrapper with contract + hash of the wrapper). **Pending** the exact import mechanism.
5. **Renaming and refactoring**: if identity is content, renaming a function does not change its hash
  (the name is metadata) --- great for AI, but how do name changes propagate in the friendly
  layer? (Unison solves this with `alias`/`update`.) **Pending** to define the flow.

## 7. Implementation plan (incremental, each slice shippable and gate-verified)

The full format is large; it lands in slices, each one useful on its own and each keeping the
existing flat `include` working (additive, never a breaking migration). The FIRST slice delivers
the thing a reader notices immediately: **qualified referencing** (`module.function` instead of a
flat C-style global namespace), which is exactly the friendly-name layer of §3.5. Content
hashing (§3.1), contract re-verification (§3.3) and the capability manifest (§3.4) ride on top of
it in later slices; none of them changes the call-site surface, so the namespacing designed here
is stable.

### 7.1 Increment 1 --- namespaced `import` and qualified calls (the §3.5 friendly-name layer)

**Surface.** A new top-level directive, additive alongside `include`:

```sond
import "any/strings"            // namespace = the file's basename -> strings.itoa(x)
import "any/strings" as str     // explicit alias                 -> str.itoa(x)
```

A call through a namespace is ordinary field-call syntax the parser already produces
(`str.itoa(x)` parses as `Call(Field(Var("str"), "itoa"), [x])`), so **no new call syntax** is
introduced --- only a resolution step that rewrites it.

**Visibility finally means something.** Only `pub` functions of the imported module are reachable
through its namespace; a `priv` function is invisible to importers (today `include` flattens
everything into one namespace with no enforcement --- RFC-0001 §4.2's module privacy becomes real).

**Resolution model (the mechanism, no new backend).** A namespaced module's functions are
*mangled* to globally-unique names, and calls are rewritten to them. The split is deliberate:
**definition names are mangled in the spliced text** (a safe, unambiguous edit --- the identifier
right after `fn`), while **call sites are rewritten on the AST** (robust --- text call-rewriting
would hit variables, strings and comments):

1. `resolve.rs`, on an `import "P" as ns`, splices `P` but rewrites every **definition** line
   (`fn`/`pub fn`/`priv fn`) so `f` becomes the mangled `ns__f`. The separator is `__` because the
   mangled name must itself lex as an identifier (the spliced text goes back through the lexer,
   which admits only `[A-Za-z0-9_]` --- so a sigil like `$` is NOT usable here). Collisions are
   made impossible by a **check**: if a mangled name coincides with any other function name in the
   program, that is a hard error (in practice `strings__itoa` never clashes). Two modules that both
   define `f` therefore coexist as `a__f` and `b__f` --- the flat-namespace collision that plagues
   `include` disappears. resolve.rs records the map `ns.f -> ns__f` with each function's
   visibility.
2. A post-parse **resolution pass** rewrites calls on the AST:
   - a qualified `ns.f(args)` where `ns` is a known import alias -> `Call(Var("ns__f"), args)`,
     erroring if `f` is not `pub` in `ns` (or does not exist);
   - inside a namespaced module's own function `ns__g`, a bare sibling call `f(args)` for which
     `ns__f` exists -> `ns__f(args)` (intra-module calls resolve to the module's mangled names ---
     the module's bodies were text-mangled only at their DEFINITION sites, never at their calls);
   - an import alias `ns` is tracked so it is never mistaken for a struct value in the existing
     `x.m()` method-call desugar (an alias colliding with a local variable name is an error).

Because definition mangling is a text-safe edit and call rewriting is on the AST, the model is
sound and needs no lexer/parser change beyond recognizing the `import` directive. `include` (flat
splice) stays for backward compatibility; `import` is the namespaced path forward.

**TDD checkpoints:** an `import ... as ns` mangles the module's defs and exposes only `pub`;
`ns.f()` resolves and runs; a `priv` function is unreachable through the namespace; an unaliased
`import "a/b"` namespaces under `b`; two modules defining the same `f` no longer collide. New
language surface, so it passes the qwen3.6 gate before merge.

### 7.2 Later increments (ride on Increment 1, do not change the call surface)

2. **Manifest + content hashing (§3.1):** a `package`/`require { name = sond:<hash> }` block;
   `import name` resolves the friendly name to the pinned hash; the module's identity becomes the
   hash of its normalized AST+contract. Open question #1 (per-definition vs per-module granularity)
   is decided here --- proposal stands: per definition internally, friendly package names on top.
3. **Contract re-verification on update (§3.3):** re-pointing a hash re-checks contract-against-
   contract at every usage; an observable-behavior change does not compile.
4. **Capability manifest (§3.4):** each package declares its capabilities; the compiler propagates
   and audits them (RFC-0000 Pillar 4).

Increment 1 is the one that answers "there is no way to reference by namespace/module.function" ---
it is the friendly-name layer, usable immediately with path resolution, and the stable surface the
hash/contract/capability layers attach to.

---

*Conclusion: Sond's package format is the Go model --- decentralized, checksummed,
reproducible, minimal --- with identity moved from **where it lives** to **what it is** (Unison's content-addressing)
and with **the contract as interface**. It keeps what JMV loves about Go and closes its
cracks exactly where AI opens them.*

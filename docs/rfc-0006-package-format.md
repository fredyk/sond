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

---

*Conclusion: Sond's package format is the Go model --- decentralized, checksummed,
reproducible, minimal --- with identity moved from **where it lives** to **what it is** (Unison's content-addressing)
and with **the contract as interface**. It keeps what JMV loves about Go and closes its
cracks exactly where AI opens them.*

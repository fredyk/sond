# State of the art: new programming languages (2016-2026)

> Context research for the design of **Sond** (RFC-0000). Catalog of languages whose
> **first public appearance** or whose **1.0 milestone / identity leap** falls within the 2016-2026 window.
> Sources: Wikipedia (*Timeline of programming languages*), Built In, Stack Overflow Developer
> Survey 2023-2025, C3/Alopex *Survey of Systems Languages 2024*, and official pages of each
> project (verified via SearXNG, 2026-07-09).

## Methodology note

- "Appeared in the window" = first public release, first serious announcement, or the **1.0 / rebranding**
  that marked its current identity (e.g. Gleam has existed since ~2016 but its 1.0 was in 2024; ReScript
  is the 2020 rebranding of BuckleScript/Reason).
- Languages whose current identity predates 2016 are **excluded** even if they kept growing
  (Rust 1.0=2015, Swift=2014, TypeScript=2012, Julia=2012, Kotlin 1.0=2016 borderline, Elixir=2011,
  Crystal=2014, Nim=2008, Pony=2015). They are cited as **boundary references** where relevant.
- Years marked with `~` are approximate (pre-release development).

---

## 1. C successors / systems languages

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Zig** | 2016 | Andrew Kelley | C without surprises: no hidden allocs, `comptime` as metaprogramming, `defer`, first-class cross-compilation. No GC or borrow checker. |
| **Odin** | 2016 | Bill "gingerBill" Hall | Pragmatic modern C oriented to gamedev/gfx; Pascal-style syntax, SIMD arrays, implicit `context`. |
| **Jai** | ~2016 (dev, not yet public) | Jonathan Blow | High-performance gamedev language with arbitrary compile-time metaprogramming. Closed/beta. |
| **V (Vlang)** | 2019 | Alexander Medvednikov | Compiled, statically typed, Go-style syntax, no dependencies, very fast compilation, `-autofree`. Controversial marketing. |
| **C3** | 2019 (public ~2023) | Christoffer Lernö | Incremental evolution of C (near-superset), modules, semantic macros, `fault` types for errors. |
| **Carbon** | 2022 | Google (Chandler Carruth) | "Experimental C++ successor" with full bidirectional interop with C++; migration from massive C++ codebases. |
| **Hare** | 2022 | Drew DeVault | Minimalist C, no dependencies, tiny toolchain, open-source platforms only. Deliberately small. |
| **Beef** | 2020 | Brian Fielder | Systems-oriented gamedev with its own IDE and hot-reload; C# style. |
| **Nelua** | ~2019 | Eduardo Bart | Typed Lua compiled to C, for systems/gamedev with Lua metaprogramming. |
| **Vale** | ~2020 | Evan Ovadia | Research in *generational references* and *region borrow checking*: memory safe without GC or classic borrow checker. |
| **Onyx** | 2022 | Brendan Hansen | Modern language that compiles to WebAssembly as its primary target. |
| **MoonBit** | 2023 | IDEA / Hongbo Zhang (China) | Optimized for **WASM** (also JS and native), fast toolchain, oriented to the AI/edge era. |

> **Boundary:** *Rust* (2015) is the catalyst for this entire generation: it proved in 2016 that you
> can have memory safety without GC. Almost all these languages are defined *in relation to Rust*.

---

## 2. Functional / advanced typing / research

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Gleam** | ~2016 (1.0 in 2024) | Louis Pilfold | Functional, sound static typing, on the **BEAM** (Erlang/OTP) and also JS. Actor concurrency. |
| **Roc** | ~2019 | Richard Feldman | Pure functional, fast, with **platforms** (the host defines effects); spiritual heir of Elm for backend. |
| **Unison** | ~2019 | Paul Chiusano, Rúnar Bjarnason | **Content-addressed** code (functions identified by hash): no broken builds, no dependency hell, distributed computation. |
| **Koka** | ~2016+ | Microsoft Research (Daan Leijen) | **Algebraic effects** with handlers and *effect typing* as a central feature; evidence passing. |
| **Hylo** (ex-**Val**) | 2022 (renamed 2023) | Dave Abrahams, Dimi Racordon | **Mutable value semantics**: safety without pointers or borrow checker, powerful generics. High-level systems. |
| **Austral** | 2022 | Fernando Borretti | **Linear types + capabilities** for safety: "Rust: the good parts", simple and sound. **Highly relevant for Sond.** |
| **Flix** | ~2016+ | Aarhus University (Magnus Madsen) | Functional-logical with integrated Datalog, effects, global type inference ML-style. |
| **Grain** | ~2017 (~2020 public) | Oscar Spencer, Blaine Bublitz | Functional-academic that compiles to **WASM**, with its own types and GC. |
| **Inko** | ~2021+ | Yorick Peterse | Actor-concurrent with memory safety without GC (lightweight ownership), error handling by types. |
| **Lean 4** | 2021 | Leonardo de Moura (Microsoft/AWS) | Proof assistant **and** general-purpose language; compiles to itself. Formal verification. |
| **Idris 2** | 2020 | Edwin Brady | **Dependent types** + *quantitative type theory* (linearity). Type-driven programming. |
| **Effekt** | ~2020 | Uni Tübingen | Algebraic effects with *effect polymorphism* without explicit monads. Research. |
| **Fennel** | 2016 | Phil Hagelberg | Lisp that compiles to Lua with no runtime, hostable in any Lua environment. |
| **Janet** | 2018 | Calvin Rose | Modern embeddable Lisp (like Lua) with PEGs and coroutines; single binary. |

> **Boundary / formal verification:** *Dafny*, *F\**, *SPARK/Ada* are older but thrive in
> this window due to interest in verification. *Move Prover*, *Quint* and *P* (Microsoft)
> are new verifiers cited below.

---

## 3. AI / ML / GPU / massively parallel compute

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Mojo** | 2023 | Modular (Chris Lattner) | Python superset with C/C++ performance, types and `fn`/`struct`; MLIR underneath. For high-performance AI. |
| **Bend** | 2024 | HigherOrderCO (Victor Taelin) | High-level **massively parallel** (GPU-native) over the **HVM2** runtime (interaction combinators); parallelizes without annotations. |
| **Triton** | 2021 | OpenAI (Philippe Tillet) | Python DSL for writing efficient GPU kernels without bare CUDA; de facto standard for deep learning kernels. |
| **Futhark** | 2016+ | Uni Copenhagen (Troels Henriksen) | Pure functional oriented to arrays that compiles to GPU (CUDA/OpenCL); data-parallelism with fusion. |
| **Taichi** | 2019 | Yuanming Hu (MIT) | Embedded DSL in Python for parallel GPU gfx/physics/simulation. |
| **Slang** | ~2020+ | NVIDIA / Khronos | Modern shader language with generics and autodiff, toward gfx + ML. |

> Note: most are **embedded DSLs** (Triton, Taichi) rather than general-purpose languages;
> included for their huge impact in the AI era. *Mojo* and *Bend* do aspire to be full languages.

---

## 4. Blockchain / smart contracts / zero-knowledge proofs (ZK)

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Vyper** | 2017 | Ethereum Foundation | "Pythonic" alternative to Solidity, deliberately restricted for auditability (no inheritance, no unlimited recursion). |
| **Michelson** | 2018 | Tezos | Typed stack-machine language for formally verifiable contracts. |
| **Pact** | 2018 | Kadena | Contracts with integrated formal verification (SMT), Turing-incomplete by design. |
| **Move** | 2019/2020 | Meta (Diem/Libra) -> Aptos, Sui | Resources as **linear types** (cannot be copied or discarded): models digital assets securely. |
| **Clarity** | 2020 | Stacks / Hiro | Interpreted (not compiled), decidable, no recursion: the cost can be analyzed statically. |
| **Cairo** | 2020 | StarkWare | Language for **STARKs** (ZK): probabilistic computation. Cairo 1.0 (2023) with Rust-style syntax. |
| **Leo** | 2021 | Aleo | High-level ZK language for private apps; compiles to circuits. |
| **Fe** | 2021 | Ethereum | Rust/Python-style successor to Vyper for the EVM. |
| **Sway** | 2022 | Fuel Labs | Rust-style for the FuelVM; oriented to assets with explicit `storage`. |
| **Noir** | 2023 | Aztec | General-purpose DSL for **ZK proofs**, verification-system-agnostic; Rust-style syntax. |

> This entire family shares a pattern costly for Sond: **security by construction**
> (linear resource types, decidability, deliberate Turing-incompleteness, formal verification).

---

## 5. Web / frontend / full-stack / cloud

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Reason** | 2016 | Facebook (Jordan Walke) | OCaml-friendly syntax alternative for JS devs; base of ReasonReact. |
| **ReScript** | 2020 | (rebranding of BuckleScript) | Robustly typed OCaml/ML that compiles to readable JS; detached from Reason. |
| **Imba** | 2016 | Sindre van der Meer / Scrimba | Web language with DOM and reactivity as first-class syntax; memoized DOM. |
| **Ballerina** | 2017 | WSO2 | **Cloud-native** integration language: network, services, and data as language constructs; generates sequence diagrams. |
| **Dark (Darklang)** | 2019 | Ellen Chisa, Paul Biggar | Language + editor + infra unified ("deployless"): backends without managing deployment. |
| **Wing (Winglang)** | 2022 | Wing Cloud (Elad Ben-Israel) | **Cloud-oriented**: distinguishes *preflight* (infra) code from *inflight* (runtime) code in the language itself. |
| **Civet** | 2023 | Daniel Ehrenberg et al. | TypeScript superset with sugar (modern CoffeeScript style) that compiles to TS. |
| **Verse** | 2022 | Epic Games (Simon Peyton Jones et al.) | Multi-paradigm with **functional logic** and transactional semantics; for the Fortnite/UEFN metaverse. Ambition of a long-lived language. |

> **Boundary:** *Elm* (2012) remains the reference for "runtime-error-free functional web";
> it inspires Roc, Gleam, and Sond's reactivity.

---

## 6. Configuration / data / query / infrastructure

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Dhall** | 2016 | Gabriel Gonzalez | **Total** config (non-Turing, always terminates), typed, with secure imports; alternative to YAML/JSON. |
| **Starlark** | 2017 | Google (Bazel) | Deterministic Python dialect without I/O for builds (Bazel/Buck); hermetic by design. |
| **Rego** | 2016 | Styra / OPA | Declarative **policy** language (Open Policy Agent) based on Datalog. |
| **CUE** | 2018 | Marcel van Lohuizen (ex-Google) | Unifies types and values in a single lattice: config, validation, and data generation. |
| **Nickel** | 2020 | Tweag | Typed config with contracts, gradual typing, and merges; spiritual successor of Nix-lang. |
| **Bicep** | 2020 | Microsoft (Azure) | Declarative DSL over ARM for infra in Azure; transpiles to JSON. |
| **PRQL** | 2022 | community (Aljaž Mur Eržen) | *Pipelined Relational Query Language*: composable alternative to SQL that compiles to SQL. |
| **Malloy** | 2022 | Google | Semantic data modeling/query language; reusable relationships and aggregations. |
| **KCL** | ~2022/2023 | Ant Group / CNCF | Typed and constrained config for cloud-native/Kubernetes at scale. |
| **Flux** | 2018 | InfluxData | Functional language for time series query (InfluxDB). |
| **Pkl** | 2024 | **Apple** | Config as a language (pronounced "Pickle"): typed, with classes and validation, generates JSON/YAML/Java/Kotlin/Swift/Go code. |

> Pattern relevant for Sond's **sub-language tower**: almost all are
> *total-terminating and without I/O* by design (Dhall, Starlark, CUE), exactly the level 1-2 that Sond proposes.

---

## 7. Formal verification / specification (relevant for Sond)

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Q#** | 2017 | Microsoft | **Quantum** computing language with simulator and library. |
| **Silq** | 2020 | ETH Zürich | High-level quantum language with automatic *uncomputation*. |
| **Quint** | 2023 | Informal Systems | Executable **specification** language (on top of TLA+) for distributed systems. |
| **P** | ~2019+ (open source) | Microsoft / Berkeley | Modeling of asynchronous systems via state machines + verification. |
| **Lean 4** | 2021 | (see Sec.2) | Formal proofs + general-purpose programming. |

> Along with **Austral** (Sec.2), **Move** (Sec.4), **Pact/Clarity/Cairo** (Sec.4) and the total configs (Sec.6),
> this family is the corpus of ideas from which Sond draws: contracts, resource linearity,
> capabilities, decidability, and deliberate Turing-incompleteness by levels.

---

## 8. Scripting / niche / embeddable / shells

| Language | Year | Author / Org | Purpose and distinguishing trait |
|----------|------|--------------|----------------------------------|
| **Wren** | 2016 | Bob Nystrom | Class-oriented scripting, small and fast, embeddable (Lua style). |
| **Nushell (Nu)** | 2019 | Sophia June Turner, Jonathan Turner et al. | **Structured** shell: typed data pipelines instead of plain text. |
| **Luau** | 2019/2021 | Roblox | Lua dialect with gradual typing and sandbox; at massive scale in Roblox. |
| **Umka** | 2021 | Vasiliy Tereshkov | Statically typed embeddable scripting for gamedev. |
| **Amber** | 2023 | Paweł Karaś | Language that **compiles to Bash** with types and modern ergonomics. |
| **Ring** | 2016 | Mahmoud Fayed | Multi-paradigm general-purpose, flexible and natural syntax. |

---

## 9. Synthesis: the underlying currents 2016-2026

1. **The post-Rust "C successor" wave** (Zig, Odin, Hare, C3, Carbon, V, Hylo, MoonBit): safety
  and ergonomics without borrow checker complexity; Rust proved the market exists.
2. **Algebraic effects and linearity leave academia** (Koka, Effekt, Roc, Austral, Idris 2,
  Move): controlling effects and resources in the type system stops being exotic.
3. **The AI era creates its own layer** (Mojo, Bend, Triton, Slang): the bottleneck is the GPU and
  parallelism, and languages appear that make it ergonomic.
4. **WASM as a first-class target** (Grain, MoonBit, Onyx, and backends of almost all): the
  browser and edge as a native destination, not an afterthought.
5. **Config-as-code matures into a serious language** (Dhall, CUE, Nickel, Pkl, Starlark, KCL): declarative,
  typed, total-terminating; untyped YAML falls short.
6. **Blockchain forces security by construction** (Move, Vyper, Cairo, Noir, Clarity, Pact):
  real money ⇒ decidability, resource linearity, and formal verification as a requirement.
7. **Full-stack / cloud as a language construct** (Ballerina, Dark, Wing, Verse): the network/infra
  frontier stops being a library and becomes syntax.

### Why this matters for Sond

Sond (RFC-0000) does not invent its pieces: it **recombines** them. It takes the contracts from the verification family (Sec.7, and Dafny/F*/SPARK at the boundary), the linearity/capabilities of Austral and Move
(Sec.2, Sec.4), the algebraic effects of Koka/Effekt (Sec.2), the total-terminating tower from the config-langs
(Sec.6), the full-stack-as-language of Ballerina/Wing/Verse (Sec.5) and the WASM target of MoonBit/Grain
(Sec.1-2). Sond's **novelty** is the *framing*: optimizing all of that for the **AI-proposes /
compiler-demonstrates** loop, something none of the above had as a guiding principle because all of
them were born in the human-typing era.

---

## 10. Success / failure / traction (honest verdict as of 2026)

Criterion: real adoption, active community, production use, and project/company health behind it ---
not "pretty ideas". Ordered by outcome.

### 🚀 Clear success (breakout or niche dominance)
- **Zig** --- the big winner of the C wave. Huge community, used in serious production (Bun, TigerBeetle) and as a cross-compilation *toolchain* even by C/C++ projects. Still pre-1.0, and still the reference.
- **Mojo** --- hype + strong funding (Modular) and growing real adoption in AI. Risk: it started semi-proprietary and its value depends on the Modular ecosystem. Success so far.
- **Gleam** --- 1.0 in 2024, very beloved (appears as a notable new language in the Stack Overflow Survey 2025). Healthy growth in the BEAM world.
- **Triton** --- **de facto standard** for writing GPU kernels in the deep learning world. Resounding success, even as an embedded DSL.
- **Move / Cairo / Noir / Sway / Clarity / Vyper** --- success **tied to their chain**: they thrive while Aptos/Sui, StarkNet, Aztec, Fuel, Stacks, Ethereum thrive. Vyper and Move are the most consolidated.
- **Starlark** --- massive silent success: it is the language of Bazel/Buck, runs in millions of builds. Nobody "learns" it but it is everywhere.
- **CUE / Pkl** --- serious config on the rise. Pkl brings **Apple**'s backing; CUE has solid adoption in cloud-native infra.
- **Luau** --- brutal and quiet scale: it is the typed Lua of **Roblox**, executed by millions. Absolute niche success.
- **Nushell** --- strong community; the structured shell with the most traction.
- **ReScript** --- stable and productive niche after detaching from Reason.

### 🱢 Solid / alive niche (not massive, but healthy and influential)
- **Odin** --- real favorite in indie gamedev/gfx; loyal community.
- **Roc** --- no 0.1 after years (rewrote the compiler in Zig in 2025), but **highly respected**; its ideas (platforms, fast functional) influence even as the language advances slowly.
- **Unison** --- alive and with **commercial** momentum (Unison Cloud, 2025); admired ideas (content-addressed). Adoption still small.
- **Koka / Effekt** --- enormous **academic and indirect industrial** impact: the algebraic effects they popularized are entering many languages. As a language of use, marginal.
- **Lean 4** --- strong growth in formal mathematics and verification (mathlib); growing community. Niche, but vibrant.
- **Austral** --- small, almost single-author, but **conceptually influential** (linear types + capabilities well explained). Proof of concept more than product.
- **Hylo** (ex-Val), **Idris 2**, **Flix**, **Grain**, **Inko**, **Nickel**, **Dhall** --- alive, researcher/enthusiast niche; none with mass adoption, all with ideas that get cited.
- **Ballerina** --- alive within the **WSO2** umbrella (enterprise integration, now with an AI layer); little life outside that ecosystem.
- **Bicep** --- real adoption as a layer over ARM in the **Azure** world.
- **MoonBit** --- **on the rise** (WASM + AI era); too young to declare victory, but with momentum and funding.
- **Janet / Fennel / Wren / Umka** --- beloved embeddables in their corner; healthy and stable without mass ambition.

### ⚠️ In trouble / uncertain / controversial
- **V (Vlang)** --- the most controversial case: promised too much (autofree, "compile any C") and suffered serious **reputational damage**; still developing and has users. Survives under suspicion.
- **Darklang** --- **stalled**: the OCaml->F# rewrite slowed them, they admitted that announcing backward compatibility was premature; pivoted ("Darklang GCP") without recovering traction. The "deployless" vision did not take hold.
- **C3, Beef, Onyx, Nelua, Vale** --- small projects, alive but with small communities; Vale is still mostly research in *generational references*.
- **Hare** --- not "in trouble" but **deliberately limited** (open-source platforms only, no mainstream ambition, "100-year language"). Niche by choice.

### 💀 Failure / abandoned / never arrived
- **Wing (Winglang)** --- the **Wing Cloud startup closed in 2025**. The language remains open source with "potential", but the commercial product failed.
- **Jai** --- ~a decade of development and **still no stable public release**; exists only in closed beta. Influential from Blow's talks, non-existent as a product.
- **Bosque** (Microsoft, 2019) --- regularization experiment that was **abandoned** in practice.
- **Silq** --- remained an academic paper; no ecosystem.

### ⏳ Too early to judge
- **Bend** (2024) --- very striking launch (parallelizes without annotations on HVM2); needs to demonstrate performance and utility **at real scale**. Promise without verdict.
- **Verse** (2022) --- currently trapped within Fortnite/UEFN; its universal language ambition remains to be seen.
- **Carbon** (2022) --- Google declares it an **experiment**, does not promise it will reach product; its future depends on whether massive interop with C++ convinces. Still in the air.
- **Quint, Amber, Civet, KCL, Fe, Leo, Umka** --- young, with incipient traction; insufficient data.

### Cross-cutting lesson for Sond

The clearest **failure** pattern is not technical but **product/traction**: Wing (bankrupt company),
Darklang (vision too big, slow execution), Jai (perfectionism without release), V (promising too
much). The quiet **successes** (Starlark, Luau, Triton, Move) triumphed by solving **a concrete, bounded problem** for an ecosystem that already existed and needed it. For Sond this is a warning: the "verified full-stack AI-era language" vision is *enormous* --- the risk is not that the ideas are bad, but dying of ambition like Wing or Darklang. The sensible path is **entering through a narrow, real wedge** (e.g. "the language in which AI writes fiscal/financial functions that are proven correct") and expanding from there, not going out promising the universe.

---

*Compilation date: 2026-07-09. Verified with local SearXNG + official project pages. Years are
the best public estimate; some projects (Jai) still have no stable release. Success/failure verdicts
reflect public signal as of 2026 (SO surveys, repo activity, status of the companies behind) and are,
by nature, a photo that ages.*

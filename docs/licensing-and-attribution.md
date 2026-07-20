# Licensing and attribution policy

Sond is licensed under **Apache License 2.0**. This document explains why, and
the rules every contributor (human or AI) follows so the project stays legally
clean as an open-source codebase that ships cryptography.

## Why Apache-2.0 (not MIT)

Both are permissive. The decisive difference is **patents**:

- **MIT** grants copyright permission only. It is *silent* on patents: a
  contributor could contribute code and later assert a patent against users.
- **Apache-2.0 §3** adds an **express, royalty-free patent grant** from every
  contributor to every user, plus a **retaliation clause**: anyone who starts
  patent litigation over the software loses their grant.

For a language/compiler that other people build on *and* that ships crypto,
that patent grant is the protection worth having. The one trade-off (Apache-2.0
is incompatible with GPLv2-*only* projects) is marginal for a new language, and
Apache-2.0 is fully compatible with GPLv3/LGPLv3.

## The idea/expression rule (why re-implementations are ours)

Copyright protects a *specific source expression*, never an **algorithm, idea,
method, or interface**. Re-implementing SHA-256, ChaCha20, CRC-32, the Kyber
Barrett reduction, or TCP/SSH framing **from its specification** produces an
original work owned by this project: it carries only Sond's Apache-2.0 license,
with **no third-party license obligation**. Legal basis: Directive 2009/24/EC
art. 1.2; Spain art. 96.4 TRLPI; U.S. 17 U.S.C. 102(b); CJEU *SAS v. WPL*
(C-406/10, 2012), which held functionality, programming languages, and data
formats are not copyright-protected expression.

Patents are separate from copyright and near-zero risk for our stack: ChaCha20
is public domain (Bernstein), AES/SHA-2 carry no patent, and **ML-KEM/Kyber is
royalty-free** under the patent licenses NIST negotiated in FIPS 203.

## Rules for writing algorithm code (applies to the qwen example-writers too)

1. **Unpatented algorithms only, verified by the worker itself.** Only implement
   algorithms that are **patent-free or royalty-free** (unpatented IETF RFCs, NIST
   FIPS, public domain). The AI worker **must research online first**: locate the
   official spec (RFC/FIPS/paper) *and* check the algorithm's patent status,
   before writing any code. At the slightest doubt an algorithm may be patented,
   it is **not implemented**. Copyright is never the risk when re-implementing; a
   **method patent** is, and a clean re-implementation does not evade it (cf. the
   Unisys GIF/LZW patent).
2. **Clean-room only.** Implement from the *prose* of the spec. **Never paste**
   a third-party implementation, nor an RFC's ABNF grammar / pseudocode block /
   test vector *verbatim*: RFC "Code Components" (post-2008) are Revised-BSD
   licensed via the IETF Trust, and copying them verbatim drags that attribution.
   Re-derive them instead.
2. **NIST/FIPS test vectors are free** (U.S. government work, public domain,
   17 U.S.C. 105), they may be embedded; a courtesy citation is good manners.
3. **Header every source file** with SPDX + the spec it implements:

   ```
   // SPDX-License-Identifier: Apache-2.0
   // Copyright 2026 Jhon Magdalena Vila and the Sond contributors
   //
   // Clean-room implementation of <ALGO> from <SPEC> (e.g. ChaCha20, RFC 8439).
   // Original work; no third-party source copied.
   ```

   In `.snd` sources the same header uses `//` comments.
4. **No regurgitated headers.** LLMs sometimes emit a third party's copyright
   header from memory. Before committing AI-written code, grep for any
   `Copyright` / `SPDX` line that is not ours and remove it.

## Pre-publish checklist

- [ ] `LICENSE` (Apache-2.0) and `NOTICE` present and consistent with file headers.
- [ ] No file contains verbatim third-party code, RFC ABNF, or RFC pseudocode.
- [ ] Reused RFC test vectors (if any) carry the IETF Revised-BSD notice; FIPS
      vectors are free.
- [ ] No foreign `Copyright (c)` lines survived from AI generation.

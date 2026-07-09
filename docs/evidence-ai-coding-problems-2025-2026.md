# Empirical evidence: the real problems of AI programming (2025-2026 studies)

> Collection of real studies (RCTs, large-scale analyses, surveys, academic papers) from 2025
> and 2026 on the limitations and failures of AI generating code. Serves as empirical basis for the
> **motivation of RFC-0000 for Sond** (is it true that the bottleneck is *verifying*, not *writing*?).
> Researched with local SearXNG + primary sources (arXiv, METR, Veracode, GitClear, Stack Overflow,
> Sonar, Apple). Date: 2026-07-09.

---

## 1. Productivity does NOT go up (and sometimes goes down) --- the study that broke the consensus

**METR --- "Measuring the Impact of Early-2025 AI on Experienced Open-Source Developers"** (RCT, July
2025). arXiv:2507.09089 * https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/
- **Randomized controlled trial** with **16 experienced open-source developers**, **246
  tasks** on their own mature repos, with Cursor Pro + Claude 3.5/3.7 Sonnet.
- The devs **predicted -24%** time (faster); after doing it, they **estimated -20%**. The actual
  result: **+19% more time --- AI SLOWED them down**.
- External experts also missed it: economists predicted -39%, ML specialists -38%.
- They investigated 20 factors; the dominant cause is the **overhead of reviewing, correcting, and integrating**
  AI code, which eats up the gain from typing faster.
- Honesty note: METR **revised the experimental design in Feb 2026**
  (https://metr.org/blog/2026-02-24-uplift-update/) --- the finding is about *experts on their own
  mature repos*, does not generalize to all scenarios (juniors, greenfield). But the key point holds:
  **the perception of speed is illusory and the cost is in post-processing, not writing.**

## 2. The "almost correct" code --- the near-miss that costs more than starting over

**Stack Overflow Developer Survey 2025** (49,000+ responses). https://survey.stackoverflow.co/2025/ai
- **51%** of professionals use AI daily.
- **66%** are frustrated by "**almost-right**" solutions (almost right but not quite).
- **45%** say that **debugging AI code takes MORE time** than writing it themselves.
- 2025 trend: devs are "**willing but reluctant**" --- trust in accuracy is falling.

**Sonar --- State of Code Developer Survey 2025**. https://www.sonarsource.com/state-of-code-developer-survey-report.pdf
- **38%** state that **reviewing AI-generated code requires MORE effort** than reviewing human code.
- **67%** spend more time **debugging** AI code; **68%** more time **resolving
  vulnerabilities** it introduces.

> This is the heart of Sond's thesis: the problem is not that AI does not type, it is the **"almost-right"**
> --- plausible code, that compiles, that passes a superficial test, but subtly incorrect. Finding it
> by eye costs more than having written it.

## 3. Systematic insecurity of generated code

**Veracode --- 2025 GenAI Code Security Report** (>100 LLMs; Java, JS, Python, C#).
https://www.veracode.com/resources/analyst-reports/2025-genai-code-security-report/
- **45% of AI-generated code introduces security vulnerabilities** (failure in ~half of
  tasks). Java is the worst. Some derived sources cite an increase of **~2.74×** vulnerabilities.

**"Security Vulnerabilities in AI-Generated Code: A Large-Scale Analysis"** (Oct 2025). arXiv:2510.26103
- Analysis of real code on GitHub from 4 tools (ChatGPT, Copilot, Tabnine, CodeWhisperer):
  **87.9% without identifiable CWE vulnerability**, but **significant patterns** in the remaining ~12%,
  with signatures per tool.

> This directly validates **Pillar 4 (capabilities)**: if the code is not trustworthy by default, the
> containment must be structural (it cannot call network/FS if it did not receive the capability), not a
  manual audit that fails 45% of the time.

## 4. Package hallucination -> new attack vector on the supply chain

**"We Have a Package for You! A Comprehensive Analysis of Package Hallucinations by Code Generating
LLMs"** (USENIX Security 2025). Cited by CSA and Trend Micro.
- LLMs **invent package/dependency names that do not exist** in a notable fraction of cases
  (on the order of ~20% depending on the model).
- **43% of hallucinated names REPEAT** consistently across queries -> predictability
  that enables **"slopsquatting"**: an attacker registers the invented package and waits for the AI to
  recommend it. (Trend Micro 2025, CSA 2026).
- **Re-evaluation in the 2026 frontier** (arXiv:2605.17062, "The Range Shrinks, the Threat Remains"): newer
  models hallucinate less, but **the problem does not disappear**.

> This validates the approach of **verified dependencies** (RFC-0004): a package with contract and explicit
  trust level makes it impossible for a hallucinated import to enter the chain without being flagged.

## 5. Maintainability erosion --- duplication and "code churn"

**GitClear --- AI Copilot Code Quality Research 2025** (analysis of **211 million lines**).
https://www.gitclear.com/ai_assistant_code_quality_2025_research
- **~4× growth in duplicated/cloned code blocks** in AI-assisted code.
- In 2024, **copy-pasted lines surpassed refactored ("moved") lines** for the first time.
- **Decline in refactoring** and "**code line inflation**": more code, worse factorization.

> Honest nuance: Sond does not *solve* duplication on its own. But canonical `sondfmt` + types that
  force reuse + contracts that make equivalence visible mitigate part of the churn. This is the
  problem where Sond has the WEAKEST response --- state it, do not oversell it.

## 6. Benchmarks lie --- contamination and memorization

**OpenAI abandons SWE-bench Verified** (2025-2026).
https://openai.com/index/why-we-no-longer-evaluate-swe-bench-verified/
- They audited 27.6% of the issues and found that **≥59.4% of "hard" issues have defective
  tests** (reject correct solutions) + **training contamination / data leakage**.
- They recommend migrating to SWE-bench Pro.

**"The SWE-Bench Illusion: When State-of-the-Art LLMs Remember Instead of Reason"** (arXiv:2506.12286).
- Top models **remember/memorize** solutions from the filtered git history, instead of reasoning.
  Benchmark scores **overestimate** actual coding capability.

> Meta-lesson for Sond: do not trust the "AI already programs beautifully" narrative. The real picture is more
  messy. This **reinforces** the principle "cheap to DISTRUST": the design cannot rely on the model's
  good faith.

## 7. Fragility of reasoning and long context

**Apple --- "The Illusion of Thinking"** (Jun 2025). arXiv:2506.06941 *
https://machinelearning.apple.com/research/illusion-of-thinking
- Reasoning models (LRMs) suffer a **total precision collapse** beyond a certain complexity
  threshold, and --- counterintuitively --- **reduce reasoning effort** just when the problem becomes
  more difficult. Three regimes: easy (better without "thinking"), medium (thinking helps), hard
  (collapse to zero).

**"Lost in the Middle" / "Context Rot"** (Chroma, 2025; base arXiv:2307.03172).
- **Measurable** performance degradation as context grows; information in the **middle gets
  lost**. Chroma replicated this in **18 frontier models** in 2025.

**"A Survey of Bugs in AI-Generated Code"** (arXiv:2512.05239) --- taxonomy of **8 categories** of bugs
(functional --- the most frequent --- syntactic, reliability, style, **hallucination**, system, test, unspecified). Confirmed as prevalent by practitioners in a parallel empirical study
(Springer, 10.1007/s10664-025-10614-4).

> This validates **Pillar 3 (LOCAL reasoning)**: if reasoning collapses with complexity and long context
  degrades, the only robust strategy is to **keep the unit small and locally verifiable**,
  not trust that the model can hold the entire system in its head.

---

## 8. Synthesis: what does the evidence say about Sond's thesis?

| Real problem (2025-26 evidence) | Sond pillar that addresses it | Validates? |
|---------------------------------|-------------------------------|------------|
| Review/correct overhead > typing gain (METR +19% slower) | Central thesis: verifying is the bottleneck | **Yes, strong** |
| Subtly broken "almost-right" code (SO 66%, Sonar 38% more review effort) | Pillar 1: specs/contracts that reject almost-correct | **Yes, strong** |
| 45% of code insecure (Veracode) | Pillar 4: capabilities (structural containment) | **Yes, strong** |
| Package hallucination / slopsquatting (43% recurring) | RFC-0004: verified dependencies | **Yes** |
| Reasoning collapse + context rot (Apple, Chroma) | Pillar 3: local reasoning | **Yes** |
| Contaminated benchmarks (SWE-bench) | Principle "cheap to distrust" | **Yes (meta)** |
| Maintainability erosion / duplication (GitClear 4×) | sondfmt + types (partial) | **Weak --- does not fully solve it** |

### The honest tension that must be acknowledged

The **METR** study **cuts in two directions**. On one hand it **validates** Sond's thesis (the cost is
in post-processing, not writing). On the other, it **tensions the optimism** of "AI rewrites all libraries in weeks": today, with 2025 tools, AI **slows down** an expert on mature code.
The correct reading is not "AI is useless", but: **AI's gain is blocked by the cost of
trusting its output** --- which is EXACTLY the cost that a verification-first language intends
to tear down. Sond does not bet on AI already being fast; it bets on **the current brake (manually verifying what
AI writes) being exactly what static verification eliminates.** If that bet is correct, a
language like Sond would turn METR's -19% into positive, because review stops being human and guesswork
and becomes a compiler verdict.

### Conclusion

The empirical evidence from 2025-2026 **strongly supports** Sond's framing: the problem of AI
programming is **not writing, but that its output is systematically insecure, subtly incorrect,
hallucinates dependencies, and verifying it by hand costs more than it saves**. The four pillars attack
exactly those documented failures. The only relative weakness is maintainability/duplication (GitClear),
where Sond mitigates but does not cure --- must be stated without glossing.

---

*All figures verified against primary sources (arXiv, METR, Veracode, GitClear, OpenAI, Apple,
Stack Overflow, Sonar) on 2026-07-09. The studies are a snapshot of 2025-2026 and will age; the frontier
improves (less hallucination, e.g.) but structural problems persist.*

# Published vulnerabilities, ported to Sond

Six real CVEs from OpenSSL, FreeBSD, WavPack, dnsmasq, nginx and sudo, each written in Sond the
way it was written in C, and each compiled.

Run them with `tools/test-cve.sh`.

## Why this exists

"The compiler proves your code" is easy to say and hard to believe. This corpus makes the claim
falsifiable: take bugs that shipped, that had CVE numbers, that were exploited or exploitable,
and see whether the compiler refuses them.

## The rules the corpus is held to

**The port is faithful.** `vuln.snd` reproduces the structure of the original defect. It carries
no contract the original C did not have, and nothing is arranged to make the checker fire. A port
tuned until it fails would prove only that the author knows how to trip a compiler.

**Every case has a control.** `fixed.snd` applies the change upstream actually made, and it has to
compile. Without that half, a compiler that rejected every program would score six out of six.
When a fix stops building, `tools/test-cve.sh` fails: the case has stopped meaning anything.

**Misses are part of the corpus.** Two of the six compile, bug and all. They are listed here, they
are not quietly dropped, and the script reports them.

## Results

| CVE | Project | Class | Result |
|---|---|---|---|
| CVE-2014-0160 (Heartbleed) | OpenSSL | Out-of-bounds read | rejected |
| CVE-2014-8612 | FreeBSD SCTP | Unvalidated attacker index | rejected |
| CVE-2019-1010315 | WavPack | Divide by zero | rejected, with a counterexample |
| CVE-2026-4891 | dnsmasq DNSSEC | Negative length underflow | rejected |
| CVE-2017-7529 | nginx | Integer overflow defeats a bounds check | **compiles** |
| CVE-2021-3156 (Baron Samedit) | sudo | Off-by-one in a copy loop | **compiles** |

Four of six.

## Why the other two get through, and it is one reason

Both ports move bytes with the raw address builtins (`peek8`, `poke8`) rather than by indexing.
Bounds checking applies to `a[i]` and `s[i]`; code doing its own pointer arithmetic is outside the
checker entirely, so nothing is proven about it.

That accounts for every failure here. It is also the argument for a real pointer type: until
addresses carry a provenance and a length the compiler can reason about, any code that reaches for
`peek8` leaves the verified part of the language behind. The overflow half of CVE-2017-7529 needs
the same thing from the other direction, since checked arithmetic cannot be turned on by default
while a pointer offset is indistinguishable from an integer.

Adding a case is welcome, and a case the compiler misses is worth more than one it catches.

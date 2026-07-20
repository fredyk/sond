# CWE-195/CWE-839 signed-length port: CVE-2026-4891 (dnsmasq DNSSEC)

## CVE chosen

**CVE-2026-4891**, heap-based out-of-bounds read in dnsmasq's DNSSEC
validation. Reported by Royce M. (xchglabs.com), fixed upstream 2026-03-25
(commit `2cacea42e4d45717bd0ce3ccfe8e78960245e5d`, "Verify rdlen field in
RRSIG packets. CVE-2026-4891"), disclosed 2026-05-11 alongside four other
dnsmasq CVEs (CVE-2026-2291, -4890, -4892, -4893, -5172; see
`kb.cert.org/vuls/id/471747`). Official patch:
`https://thekelleys.org.uk/dnsmasq/CVE/CVE-2026-4891.diff`.

## The real bug

`validate_rrset()` in `src/dnssec.c` re-parses an RRSIG record it already
found a pointer to. It reads the record's on-wire `rdlen` field (an `int`,
already validated by the *caller*, `explore_rrset()`, at dnssec.c:353 and
:368, `CHECK_LEN(header, p, plen, rdlen)` checks `rdlen` fits inside what's
left of the DNS packet, and `if (rdlen < 18) return 0;` checks it's at least
the RRSIG's fixed 18-byte header). Both of those are checks on `rdlen`
*alone*. `validate_rrset()` then re-walks the fixed header plus the RRSIG's
signer-name field, a compressed, variable-length DNS name decoded by
`extract_name()`, advancing a pointer `p` from a saved `psav`. At
dnssec.c:548 (pre-patch):

```c
sig = p;
sig_len = rdlen - (p - psav);
```

`(p - psav)` is however many bytes decoding the header + signer's name
actually took, a quantity neither of `rdlen`'s two checks accounts for. A
crafted packet whose signer's name is long enough makes `(p - psav) > rdlen`,
driving `sig_len` negative even though `rdlen` passed both its bounds
checks. `sig_len` (a signed `int`) then flows into `hash->update(ctx,
sig_len, sig)` / `verify(key, keylen, sig, sig_len, ...)`, both of which take
it as an unsigned byte count (`size_t`): the negative value wraps to
~2^64, and the resulting read walks far past the end of the packet buffer -
the heap out-of-bounds read this CVE is about.

The upstream fix (see full diff below) adds exactly one guard, immediately
before the `sig_len` assignment:

```c
   sig = p;
-  sig_len = rdlen - (p - psav);
+  if ((sig_len = rdlen - (p - psav)) <= 0)
+    return STAT_BOGUS;
```

with a comment confirming the diagnosis: *"Don't trust rdlen not to be too
small and give us a negative sig_len. It has already been checked that it
doesn't run us off the end of the packet."*, i.e. the packet-bound
(`CHECK_LEN`) upper-bound check already existed; the missing piece was
purely `sig_len > 0`.

## Mapping to the Sond port

| C (dnssec.c)                               | Sond port                          |
|---------------------------------------------|-------------------------------------|
| `explore_rrset()`'s `CHECK_LEN` + `rdlen<18`| `check_rdlen(rdlen, remaining_in_packet)` |
| `sig_len = rdlen - (p - psav)` (line 548)   | `compute_sig_len(rdlen, consumed)` |
| `hash->update(ctx, sig_len, sig)` / `verify(..., sig, sig_len, ...)` | `signature_trailer_byte(sig, sig_len)`, indexes `sig[sig_len - 1]`, standing in for a signature-verification routine reading a byte at an offset derived from the claimed length (a real RSASSA trailer/padding check does exactly this kind of relative access) |
| `p - psav` (bytes actually consumed by the 18-byte fixed header + signer's name) | `consumed` parameter |

`sig` is modeled as an opaque `Slice` parameter, deliberately **not**
constructed as a sub-slice of some larger "packet" buffer. This mirrors the
real C signature: `verify()`/`hash->update()` receive `unsigned char *sig`
with no independently known size, like the real functions, they trust
`sig_len` completely, and there is nothing tying `sig.len` to `rdlen` or
`consumed` for the compiler to exploit. This matters: an earlier draft of
this port constructed `sig` as `packet[consumed:remaining_in_packet]` (a
literal sub-slice, closer to `sig = p` on a single packet buffer); with that
construction the index `sig[sig_len - 1]` algebraically reduces to
`packet[rdlen - 1]` **regardless of `consumed`**, which is always in bounds
given `check_rdlen()`'s guarantee that `0 < rdlen <= remaining_in_packet` -
i.e. that framing accidentally cancels the bug out through arithmetic
identity and would misrepresent the CVE as unexploitable. Keeping `sig`
independent avoids that trap and is also the more faithful representation of
the real function boundary.

## An important honest caveat: the loop-vs-index shape of this specific bug

The real exploit is a **bulk read of `sig_len` bytes** (`hash->update`,
`verify`) whose *count* explodes from a small negative `int` to ~2^64 via C's
implicit signed→unsigned conversion at the `size_t` parameter boundary. Sond
has no unsigned/`size_t` type, every integer is `i64`, signed, uniformly.
A direct, `for i < sig_len { ... sig[i] ... }` port of the *bulk-read* shape
of this bug does **not** reproduce as an out-of-bounds access in Sond: with
`sig_len` negative, `i < sig_len` is false at `i = 0`, so the loop simply
runs zero times, a silent logic difference (verification proceeds against
an empty signature instead of crashing/leaking) rather than a memory-safety
violation. That variant would tell us nothing about the compiler's bounds
prover, so this port instead uses the single relative-offset access
(`sig[sig_len - 1]`) described above, which **is** a genuine out-of-bounds
access for negative `sig_len` regardless of Sond's lack of `size_t`, a
negative *index* is unsafe under signed arithmetic too, it just doesn't
explode to the same catastrophic magnitude C's `size_t` reinterpretation
causes. This is a real, structural difference between the two languages
worth flagging on its own: whole shapes of CWE-195 (bulk copy/hash with an
attacker length) may be defused by Sond's design merely by *not having*
`size_t`, while others (a single access at an attacker-controlled offset)
remain exactly as dangerous as in C.

## Annotation added in fixed.snd

Two changes relative to vuln.snd:

1. **The literal upstream fix**: `if sig_len <= 0 { return -1 }` right after
   computing `sig_len`, before it is used, a direct port of the added
   `if ((sig_len = rdlen - (p - psav)) <= 0) return STAT_BOGUS;` guard.
2. **One thing beyond the literal upstream diff**, needed only because this
   port isolates `validate_rrset()` from the rest of dnsmasq: a `where`
   refinement on `signature_trailer_byte`'s `sig_len` parameter
   (`sig_len: i64 where 0 < sig_len && sig_len <= sig.len`), plus a matching
   runtime check `if sig_len > sig.len { return -1 }` in `validate_rrset()`.
   In the real C code this upper bound is never checked explicitly at this
   point, it holds *implicitly*, as a consequence of `CHECK_LEN` having
   already verified (several stack frames away, in `explore_rrset()`) that
   `psav + rdlen` sits inside the received packet, combined with `sig`
   always pointing into that same packet buffer. This port's simplified,
   single-function signature (`sig` as a bare `Slice` with no relation to
   `rdlen`) doesn't carry that cross-function invariant forward, so it has
   to be re-established locally for the prover. This is called out
   explicitly rather than silently added, per the instructions not to invent
   contracts the original code didn't have, it is a genuine, minimal,
   defensible strengthening (checking both extremes of the length, as the
   task's general guidance for this pattern asks), not a rewrite of the bug.

## What I observed compiling locally (for context; not the final measurement)

```
$ sondc vuln.snd --shared -o /tmp/probe_vuln.so
sondc: function `signature_trailer_byte`: cannot prove this index is in
bounds for slice `sig`, bound it with a `where`/`requires`/loop condition
to 0..sig.len

$ sondc fixed.snd --shared -o /tmp/probe_fixed.so
sondc: wrote native x86-64 binary `/tmp/probe_fixed.so`

$ sond test fixed.snd
5 test(s), 0 failed | coverage 11/11 (100%)
```

`vuln.snd` deliberately carries **no** `where`/`requires`/`ensures`
anywhere, it is the same unguarded shape as the pre-patch C. `fixed.snd`
adds only the two checks described above (the upstream one plus the one
locality-forced addition) and the `where` refinement they let the prover
use.

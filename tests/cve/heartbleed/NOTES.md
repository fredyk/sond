# CVE-2014-0160: "Heartbleed" (OpenSSL)

## The real bug

Heartbleed lives in `tls1_process_heartbeat()`, `ssl/t1_lib.c` in OpenSSL 1.0.1
through 1.0.1f (also mirrored in `ssl/d1_both.c` for the DTLS heartbeat path).
A TLS heartbeat request record is `[1 byte type][2 bytes payload_length][payload
bytes][padding]`. The vulnerable code read the attacker-controlled
`payload_length` field out of the record and then echoed that many bytes back
to the attacker, without ever checking that the record it actually received
was that long:

```c
/* Read type and payload length first */
hbtype = *p++;
n2s(p, payload);      /* attacker-controlled 16-bit length, read out of the record */
pl = p;
...
buffer = OPENSSL_malloc(1 + 2 + payload + padding);
bp = buffer;
...
memcpy(bp, pl, payload);   /* <-- THE BUG */
```

**Culprit line:** `memcpy(bp, pl, payload);`, copies `payload` bytes
(attacker-supplied, up to 65535) starting at `pl` (the payload field of the
received record) into the reply buffer, with no check that the record
actually contained `payload` bytes past its 3-byte header. An attacker could
send a 3-byte record claiming a 65535-byte payload and receive up to ~64KB of
adjacent heap memory back, private keys, session tickets, other connections'
plaintext.

The official fix (OpenSSL 1.0.1g, commit `4817504d`) added two checks: the
record must be at least big enough to hold the header before `hbtype`/`payload`
are even read, and the claimed `payload` length must fit inside the record's
actual length before the `memcpy`:

```c
if (1 + 2 + 16 > s->s3->rrec.length)
        return 0; /* silently discard */
hbtype = *p++;
n2s(p, payload);
if (1 + 2 + payload + 16 > s->s3->rrec.length)
        return 0; /* silently discard per RFC 6520 sec. 4 */
pl = p;
```

## Mapping to the Sond port

- `record: Slice` = the received TLS record (`s->s3->rrec.{data,length}`).
- `out: Slice` = the reply buffer (`bp`/`buffer`).
- `record[0]` = `hbtype`; `record[1]`, `record[2]` = the two bytes `n2s()`
  reads into `payload`.
- `header = 3` = the 1-byte type + 2-byte length fixed prefix.
- The `for i < payload_len { out[i] = record[header + i]; ... }` loop is the
  port of `memcpy(bp, pl, payload)`.

`vuln.snd` reproduces the bug exactly: `payload_len` is read straight out of
`record` and used to index `record[header + i]` with no check against
`record.len`. No `requires`/`where` was added, the C original had none.

## Result of compiling `vuln.snd`

```
sondc: function `process_heartbeat`: cannot prove this index is in bounds for
slice `record`, bound it with a `where`/`requires`/loop condition to
0..record.len
```

**Rejected at compile time**, but not because a contract caught a subtle
overflow. Per `docs/bootstrap-debt-registry.md` (entry #2, "SLICE INDICES ARE
NOW BOUNDS-CHECKED", shipped 2026-07-19), Sond's `Slice` indexing is
**checked-by-default**: `check.rs`'s `check_constant_bounds` /
`verify::index_proven_in_bounds` require *every* `s[i]` on a `Slice` parameter
- constant or variable, to be statically proven inside `[0, s.len)`, with or
without an explicit contract on the function. There is no opt-out; an
unprovable index is a hard compile error before codegen ever runs. So the
honest, non-adversarial, non-annotated port of the vulnerable C function is
categorically inexpressible as written: you cannot even get `vuln.snd` past
the front end, let alone to the point where a runtime OOB read could occur.
This is a *stronger* result than "the prover caught the postcondition
violation", Heartbleed's bug pattern (index derived from untrusted data,
never related back to the buffer's real length) is structurally excluded from
every program that type-checks, independent of whether the programmer thought
to write a contract at all.

(Note: `record[0]`/`record[1]`/`record[2]`, reading the header itself, are
*also* unprovable in `vuln.snd`, since nothing bounds `record.len >= 3`
either. This mirrors reality: the pre-1.0.1g code had no minimum-length check
before reading `hbtype`/`payload` either, only before the payload copy.)

## Annotation needed for `fixed.snd`

None beyond porting the real fix's own runtime checks. `fixed.snd` adds the
exact two guard clauses from the upstream patch, as ordinary `if { return 0 }`
statements, not contracts:

```sond
if record.len < header { return 0 }
...
if header + payload_len > record.len { return 0 }
```

plus one extra guard, `if payload_len > out.len { return 0 }`, for the
output/reply buffer. In the real C code the reply buffer is always big
enough because it's allocated with `OPENSSL_malloc(1 + 2 + payload + padding)`
- sized *from* `payload` itself, so it was never part of the vulnerability.
In the Sond port `out` is just a caller-supplied `Slice` (no allocator call
at this API boundary), so the equivalent guarantee has to be checked
explicitly rather than established by construction; this check was needed to
compile, not to fix the CVE.

With those three early-return guards in place, `fixed.snd` compiles clean
with **zero further annotation**, no `requires`, no `invariant`, no `where`.
The bootstrap-debt-registry's "guard-clause early-return refines the
continuation" flow analysis (2026-07-19) is exactly the mechanism doing the
work: a statement-position `if cond { return }` with no `else` makes the
compiler assume `!cond` for everything after it, so after the two length
guards the prover already knows `header + payload_len <= record.len` and
`payload_len <= out.len`; combined with the loop's monotonic counter
(`0 <= i < payload_len`), it derives `header + i < record.len` and
`i < out.len` on its own and accepts both slice writes in the loop body.

## Verification

```
$ bash tools/sandbox.sh --time 60 -- sondc vuln.snd  --shared -o /tmp/probe_vuln.so
sondc: function `process_heartbeat`: cannot prove this index is in bounds for slice `record`, bound it with a `where`/`requires`/loop condition to 0..record.len
(exit 1)

$ bash tools/sandbox.sh --time 60 -- sondc fixed.snd --shared -o /tmp/probe_fixed.so
sondc: wrote native x86-64 binary `/tmp/probe_fixed.so`
(exit 0)
```

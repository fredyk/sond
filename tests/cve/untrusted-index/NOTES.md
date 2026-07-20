# CWE-129 case study: CVE-2014-8612 (FreeBSD SCTP, `SCTP_SS_VALUE`)

## CVE chosen

**CVE-2014-8612**, "Multiple array index errors in the Stream Control
Transmission Protocol (SCTP) module in FreeBSD 10.1 before p5, 10.0 before
p17, 9.3 before p9, and 8.4 before p23 allow local users to gain privileges
via the stream id to the `setsockopt` function, when setting the
`SCTP_SS_VALUE` option, or read arbitrary kernel memory via the stream id to
the `getsockopt` function, when getting the `SCTP_SS_VALUE`/`SCTP_SS_PRIORITY`
option." Fixed by **FreeBSD-SA-15:01.sctp** (announced 2015-01-27), credited
to Clement Lecigne (Google Security Team) and Francisco Falcon (Core Security
Technologies). Also documented in Core Security's advisory "FreeBSD Kernel
Multiple Vulnerabilities" and on Exploit-DB (#35938).

### Why this one

- It is a **textbook, single-cause CWE-129**: one unchecked field from
  attacker-supplied data used directly as an array subscript, no pointer
  arithmetic tricks, no type confusion layered on top (contrast with e.g.
  CVE-2025-37752, which is really an unsigned-underflow-then-index bug, or
  CVE-2021-22555, which is a struct-size/heap-overflow bug that only
  incidentally touches an array). That makes it a faithful, uncomplicated
  port target.
- The "untrusted data" here is the `av->stream_id` field of a
  `struct sctp_stream_value` copied verbatim out of the buffer a process
  passes to `setsockopt()`. It is attacker/user-controlled input in exactly
  the sense the task asks for (a value taken directly from an external,
  untrusted source and used as an index), it just arrives via a syscall
  argument rather than a wire packet; SCTP's own on-wire "stream id" field in
  DATA/reset chunks is validated separately and was not the object of this
  particular CVE, so the syscall-facing variant is the one with the clean
  paper trail.
- It has a well-documented, minimal official fix: exactly one bounds check
  was added, nothing else changed. That makes "port the bug, port the fix"
  unambiguous, there's no judgment call about what "the fix" means.

## The real bug

`sctp_setopt()`, `sys/netinet/sctp_userreq.c`, case `SCTP_SS_VALUE`:

```c
case SCTP_SS_VALUE:
{
    struct sctp_stream_value *av;
    SCTP_CHECK_AND_CAST(av, optval, struct sctp_stream_value, optsize);
    if (stcb) {
        if (stcb->asoc.ss_functions.sctp_ss_set_value(stcb, &stcb->asoc,
            &stcb->asoc.strmout[av->stream_id], av->stream_value)) {
            error = EINVAL;
        }
        SCTP_TCB_UNLOCK(stcb);
    }
    break;
}
```

`av` is the raw `struct sctp_stream_value` copied out of the `setsockopt()`
buffer the calling process supplied; `av->stream_id` is a `uint16_t` under
full caller control. It is used directly to compute
`&stcb->asoc.strmout[av->stream_id]`, a pointer into `stcb->asoc.strmout[]`,
a fixed-size array sized to `stcb->asoc.streamoutcnt` (the number of
outbound streams negotiated for that SCTP association, typically a small
number like 10). **There is no check of `av->stream_id` against
`streamoutcnt` before the array is indexed.** `sctp_ss_prio_set_value()`
(reached through the `ss_functions` vtable) then does a plain field write
through that pointer:

```c
strq->ss_params.prio.priority = value;
```

giving an unprivileged local caller of `setsockopt(SCTP_SS_VALUE)` an
out-of-bounds 16-bit kernel-memory write at an offset it fully controls. The
`getsockopt(SCTP_SS_VALUE)` path has the mirror-image read bug (kernel memory
disclosure instead of corruption), driven by the same unchecked
`av->stream_id`.

**The culprit line**, faithfully preserved in `vuln.snd`:

```c
&stcb->asoc.strmout[av->stream_id]
```

## The real fix (FreeBSD-SA-15:01.sctp)

One check added before the array is ever touched:

```c
if ((av->stream_id >= stcb->asoc.streamoutcnt) ||
    (stcb->asoc.ss_functions.sctp_ss_set_value(stcb, &stcb->asoc,
     &stcb->asoc.strmout[av->stream_id], av->stream_value))) {
    error = EINVAL;
}
```

`av->stream_id >= streamoutcnt` is checked first; anything out of range takes
the `EINVAL` branch instead of reaching the array subscript.

## Mapping to the Sond port

| C (real bug)                                   | Sond port                                              |
|-------------------------------------------------|---------------------------------------------------------|
| `stcb->asoc.strmout[]` (fixed array of streams)  | `strmout: Slice` (backed by a `let table[8]` stack array standing in for `streamoutcnt` streams) |
| `av->stream_id` (untrusted `uint16_t` from `setsockopt()` payload) | `stream_id: i64` parameter |
| `strq->ss_params.prio.priority = value` (the OOB write itself) | `strmout[stream_id] = stream_value` |
| `sctp_ss_prio_set_value()` reached via `av->stream_id` with **no bounds check** | `handle_sctp_ss_value(strmout, stream_id, stream_value)`, same, no check, in `vuln.snd` |
| Added guard: `if (av->stream_id >= stcb->asoc.streamoutcnt) { error = EINVAL; }` | `sctp_setopt_ss_value()` guard in `fixed.snd`: `if stream_id < 0 || stream_id >= strmout.len { return 22 }`, plus the `where` refinement on `handle_sctp_ss_value`'s `stream_id` parameter |

`vuln.snd` is the direct, unguarded port, `handle_sctp_ss_value` indexes
`strmout` with the raw, uncontrolled `stream_id` parameter, exactly mirroring
`&stcb->asoc.strmout[av->stream_id]` with zero validation upstream. No
contract of any kind was added to `vuln.snd`, the C original had none, and
neither does the port.

## What annotation `fixed.snd` needed

Sond's array-bounds prover only handles **straight-line integer bodies** (no
loops, no cross-function propagation) per `Sond.md`. That means the
compiler cannot look inside `sctp_setopt_ss_value`'s `if` guard and
retroactively conclude that, past the `return`, `stream_id` is safe to hand
to `handle_sctp_ss_value`, the callee's own contract has to state the bound
it needs, and the caller has to satisfy it. The minimal annotation was a
`where` refinement on the indexing parameter itself:

```sond
fn handle_sctp_ss_value(strmout: Slice, stream_id: i64 where 0 <= stream_id && stream_id < strmout.len, stream_value: i64) {
    strmout[stream_id] = stream_value
}
```

This `where` is precisely a `requires` on `stream_id`, so any call site must
prove it holds. `sctp_setopt_ss_value()` supplies that proof the same way the
real FreeBSD patch does, with an explicit runtime range check
(`if stream_id < 0 || stream_id >= strmout.len { return 22 }`) before ever
calling `handle_sctp_ss_value`. No other change was needed; `fixed.snd`
compiles and all 5 tests pass (100% basic-block coverage) under
`sond test fixed.snd`.

## Compile-time result (measured, not claimed as a general verdict)

- `vuln.snd`, `$HOME/.local/bin/sondc vuln.snd --shared -o /tmp/probe.so`
  (also reproduced via `sond test vuln.snd`), **rejected at compile time**,
  before contracts/tests even run:

  ```
  sondc: function `handle_sctp_ss_value`: cannot prove this index is in bounds for slice `strmout`, bound it with a `where`/`requires`/loop condition to 0..strmout.len
  ```

  This happens purely because `handle_sctp_ss_value` indexes a `Slice` with a
  non-constant (`stream_id`) value and that parameter carries no bound -
  Sond refuses to compile ANY function shaped like that, independent of what
  its callers pass. `vuln.snd`'s `main()` only ever calls it with a literal,
  in-range index (`3`), and `vuln.snd`'s own test block calls it with `0` and
  `STREAM_COUNT - 1`, both in range, none of that matters, because the
  rejection is on the callee's body, not on any specific unsafe call site.

- `fixed.snd`, same compile command, **compiles successfully**
  (`sondc: wrote native x86-64 binary /tmp/probe_fixed.so`), and
  `sond test fixed.snd` reports `5 test(s), 0 failed | coverage 3/3 (100%)`,
  including tests that pass the exact out-of-range values from the real bug
  class (`STREAM_COUNT`, `65535`, the max of the original `uint16_t`
  `stream_id` field, and `-1`), all correctly routed to the `EINVAL` path
  instead of reaching the array.

I am not asserting a general verdict here beyond what was measured on this
one file pair; the parent agent/harness owns the final compile-and-record
step across the whole corpus.

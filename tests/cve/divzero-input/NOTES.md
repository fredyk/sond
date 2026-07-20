# CVE-2019-1010315: WavPack DSDIFF divide-by-zero (CWE-369)

## The CVE

**CVE-2019-1010315.** WavPack (an open-source lossless audio codec/CLI) versions
up to and including 5.1 contain a division-by-zero in the DSDIFF (`.dff`) file
parser. Parsing a crafted `.dff` file raises SIGFPE and crashes any process
that links WavPack's parsing code (the `wavpack` CLI, and anything embedding
`cli/dsdiff.c`), giving a trivial denial of service from a malicious audio
file. Originally reported as
[dbry/WavPack#65 "Divide by zero in ParseDsdiffHeaderConfig()"](https://github.com/dbry/WavPack/issues/65),
filed with a fuzzed `divzero.wav` reproducer. Fixed upstream in commit
[`4c0faba32fddbd0745cbfaf1e1aeb3da5d35b9fc`](https://github.com/dbry/WavPack/commit/4c0faba32fddbd0745cbfaf1e1aeb3da5d35b9fc).

## The real bug

`ParseDsdiffHeaderConfig()` in `cli/dsdiff.c` walks the DSDIFF chunk stream.
When it meets a `"PROP"` chunk containing an embedded `"SND "` chunk, it looks
for a `"CHNL"` sub-chunk and, if present, sets `config->num_channels` from it
(with its own internal `numChannels < 1` sanity check at that point, this
inner check is unrelated to the CVE). But a crafted file can present the
`"DSD "` data chunk *before, or without ever having*, a valid `"SND "/"CHNL"`
property chunk. `config` was never populated in that path, so
`config->num_channels` is left at its zero-initialized default. The line that
computes the sample count then divides by it unconditionally:

```c
// cli/dsdiff.c:282 (pre-fix)
else if (!strncmp (dff_chunk_header.ckID, "DSD ", 4)) {
    total_samples = dff_chunk_header.ckDataSize / config->num_channels;
    break;
}
```

`config->num_channels` is a header-derived field (present only conditionally,
depending on chunk order/presence in the input file) used as a divisor with
no check that it is nonzero, a textbook CWE-369 fed by untrusted file input.

The upstream fix inserts a guard immediately before this line:

```c
if (!config->num_channels) {
    error_line ("%s is not a valid .DFF file!", infilename);
    return WAVPACK_SOFT_ERROR;
}
```

## Mapping to the Sond port

| C (dsdiff.c)                              | Sond                                        |
|--------------------------------------------|----------------------------------------------|
| `WavpackConfig` (`num_channels` field)     | `struct DsdiffConfig { num_channels: i64 }`  |
| `config->num_channels` left at zero when no `SND `/`CHNL` chunk was parsed | `let cfg: DsdiffConfig` (zero-initialized struct, per Sond.md), never assigned |
| `dff_chunk_header.ckDataSize / config->num_channels` (dsdiff.c:282) | `data_size / cfg.num_channels` in `compute_total_samples` |
| the added `if (!config->num_channels) { ...; return ...; }` guard | `if cfg.num_channels == 0 { return -1 }` guard, same position, same shape |

`vuln.snd` is the direct, unguarded port of dsdiff.c:282, the division exactly
as upstream had it before the fix, called on a struct that mirrors the
crafted-file scenario (channel count never populated). No contract was added:
the C original carried none, and `vuln.snd` doesn't either, it is a plain
function and a plain division.

`fixed.snd` ports the *exact* upstream fix: a runtime guard that rejects the
zero-channel case before dividing, in the same position as the real patch.

## What annotation was needed

**None beyond the guard itself.** No `requires`/`ensures`/`where` clause was
added anywhere. `sondc` raises a built-in, unconditional proof obligation on
every integer `/` and `%` in the program, "the divisor is nonzero", for
every function, contract or no contract (see `compiler/src/verify.rs`, the
`BinOp::Div | BinOp::Mod` arm of the expression checker). This is not opt-in
per-function contract syntax; it is a standing safety obligation the compiler
always checks, the same category as its bounds checks.

- `vuln.snd`: `sondc` cannot prove `cfg.num_channels != 0` (nothing in the
  program says it is), so the build is **rejected**:
  ```
  sondc: contract violation, function `compute_total_samples`: cannot prove
  the divisor `__call1` is nonzero (counterexample: __call1 = 0)
  ```
- `fixed.snd`: the `if cfg.num_channels == 0 { return -1 }` guard gives the
  prover flow-sensitive evidence that `cfg.num_channels != 0` on every path
  that reaches the division, so the build **succeeds**.

Both were compiled with the full pipeline (not `--emit-header`):
`sondc <file>.snd --shared -o /tmp/probe.so`, under
`tools/sandbox.sh --time 60`.

## Honesty check

- `vuln.snd` was not artificially mangled to force rejection, it is the
  same shape as the real vulnerable C line, ported to Sond's struct/function
  syntax, called with an input that reproduces the actual real-world trigger
  (a config whose channel count was never populated).
- `vuln.snd` carries no contract the C original lacked.
- `fixed.snd` compiles and its fix is structurally identical to the real
  upstream patch (guard-then-divide), not a Sond-only trick.

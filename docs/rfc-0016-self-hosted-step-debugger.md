# RFC-0016: The self-hosted step debugger: a program that serves its own debug socket

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0016                                                                  |
| Status      | Partially implemented (first socket slice landed; see §3.2a)          |
| Date        | 2026-07-23                                                           |
| Authors     | Jhon Magdalena Vila                                                         |
| Motivation  | Step debugging without external tooling (no ptrace, no DWARF, no gdb): the program carries its own debugger and serves it over a socket |
| Related     | RFC-0000 (self-hosted tooling), coverage instrumentation, RFC-0008 (par/graph, for multi-thread), RFC-0003 (profiles) |

## 1. The thesis: the program debugs itself

Every other language debugs from the OUTSIDE: a separate process (gdb, lldb) attaches with `ptrace`,
reads DWARF debug info the compiler emitted, sets `int3` breakpoints, single-steps with the trap
flag. It is powerful and it is heavy: two toolchains (the compiler's debug-info emitter, the
debugger), an OS-specific control mechanism, and a format (DWARF) larger than most compilers.

Sond already does tests, coverage, contracts and the arena IN the language, self-contained. The
debugger should be no different: **a `--debug` build embeds a small debug agent that serves a Unix
socket; a client connects and drives execution** (interrupt, step-in, step-over, continue, inspect).
No ptrace, no DWARF, no external debugger. The program carries its own introspection.

The enabling observation: **Sond already has the exact instrumentation seam a step debugger needs.**
Coverage (`cov_mark`) emits, before every statement, a callout that records "this line ran". The
debugger is coverage's richer sibling: the same per-statement injection point, but instead of
recording the line it may PAUSE at it and serve the socket. That coverage ships and works is the
proof the seam is real and correctly placed (at statement boundaries, after the prologue).

## 2. Prior art (and where this sits)

- **`ptrace` + DWARF (gdb, lldb):** external control, rich, and the industry standard. The cost is
  the DWARF emitter and a separate ptrace debugger, both large, both OS-specific. This RFC does not
  compete on power; it trades some of it for zero external tooling.
- **The Debug Adapter Protocol (DAP):** the editor-facing protocol (VS Code et al.) that abstracts
  "set breakpoint / step / evaluate / stack trace". This RFC's wire protocol is deliberately
  DAP-shaped, so an editor can speak to a Sond program through a thin bridge, or directly.
- **In-process / interpreted debuggers** (Python's `pdb`, browser devtools): the runtime pauses
  itself and offers a REPL. That is exactly the model here, but for AOT-compiled native code and
  over a socket, so the client is decoupled from the process.
- **`--shared` C-ABI and the socket libraries Sond already has** (UDP/TCP, line-based stores) make
  the transport a solved problem: a Unix socket server in Sond is within reach today.

## 3. Design

### 3.1 The `--debug` build: a checkpoint per statement

`sondc --debug prog.snd` compiles as normal, but before each statement the codegen emits a callout,
the debug analogue of `cov_mark`:

```
    ; before statement at line L, in a frame based at %rbp
    movq $L, %rdi
    movq %rbp, %rsi
    call __dbg_step
```

A non-`--debug` build emits none of this and is byte-identical to today (as coverage is). The
callout passes the current source line and the frame pointer, which is everything the agent needs to
locate the paused point and read its locals (§3.4).

### 3.2 The debug agent (a Sond runtime)

`__dbg_step(line, frame)` is the whole debugger, written in Sond over the socket primitives. It owns
a small amount of module state:

- `mode`: `Running` | `Stepping` | `Paused`.
- `breakpoints`: a set of source lines.
- `step_depth`: the call depth captured when a step began (for step-over / step-out).
- `depth`: the current call depth, maintained by a matching callout at call entry/exit (or derived
  from `%rbp` chaining).
- `client_fd`: the connected client, or none.

On the FIRST checkpoint it binds the `.sock` (path from `SOND_DEBUG_SOCK`, default `./<prog>.dbg.sock`)
and, if `SOND_DEBUG_WAIT` is set, blocks until a client connects, so a debugger can catch `main` from
the first line. Thereafter, at each checkpoint:

```
fn __dbg_step(line, frame):
    if not should_pause(line, mode, step_depth, depth): return   // the common, fast path
    send Stopped { line, depth, reason }          // tell the client where we are
    loop:
        cmd = recv()                               // BLOCK: the program is paused here
        match cmd:
            StepIn:    mode = Stepping; step_depth = MAX; return   // stop at the very next checkpoint
            StepOver:  mode = Stepping; step_depth = depth; return // stop at the next checkpoint at THIS depth or shallower
            StepOut:   mode = Stepping; step_depth = depth - 1; return
            Continue:  mode = Running; return                      // run to the next breakpoint
            SetBreak L / ClearBreak L: update breakpoints          // stay paused, keep reading
            Inspect name:  send Value { name, read_local(frame, name, line) }   // stay paused
            Backtrace:     send Frames { walk_rbp_chain(frame) }               // stay paused
            Locals:        send Locals { all_locals_at(frame, line) }          // stay paused
```

`should_pause` is `mode == Stepping && depth <= step_depth`, or `line in breakpoints`. Pausing means
BLOCKING in `recv()`: the program's own thread stops there, which IS stepping. Commands that inspect
(Inspect/Backtrace/Locals) reply and keep looping; commands that resume (Step*/Continue) set the
mode and return, letting execution continue to the next checkpoint.

step-over and step-out fall out of the depth compare: step-over stops at the next checkpoint whose
depth is `<= step_depth` (so it skips INTO calls, which are deeper, and stops when control returns);
step-in uses `step_depth = MAX` (stop anywhere); step-out uses `depth - 1` (stop only after the
current frame returns).

### 3.2a Implementation status: the first socket slice

The design above is the target. What is implemented today (the agent source lives in the compiler as
`debug_agent::SOURCE`, appended to a `--debug` program after include resolution, and is itself Sond
that the same pipeline parses, PROVES, and lowers) is the core interactive loop over a real Unix
socket, deliberately narrower than the framed protocol so it could land end to end:

- **State.** Module globals: `__dbg_mode` (`0` run-free, `1` single-step, `2` run-to a line),
  `__dbg_target` (the run-to line), `__dbg_fd`/`__dbg_started` (the connection, bound once), and
  `__dbg_bp[64]` (the breakpoint set, a writable `.data` array indexed with raw peek64/poke64). No
  call-depth yet, so step-over / step-out are not in this slice.
- **Breakpoints.** A breakpoint is a source line that always stops, whatever the mode, so `continue`
  runs to the next one. Membership (`__dbg_bp_has`) is checked in `should_stop`; the set is edited
  live over the socket (see the protocol) and holds up to 64 lines.
- **Attach.** On the first checkpoint the agent binds `/tmp/sond.dbg.sock`, announces the path on
  stderr, and BLOCKS on `accept` until a client connects, so `main` is caught from its first line.
  The listener is bound exactly once; after a detach or a client disconnect the program runs free and
  is never rebound.
- **Wire protocol (line-oriented, not yet length-prefixed).** Each stop is reported to the client as
  `L<line>\n`. The client replies with one-byte commands: `s` (single-step, stop at the next
  checkpoint), `c` (continue, run to the next breakpoint or to the end), `g<line>\n` (run until the
  given source line), `b<line>\n` / `d<line>\n` (add / delete a breakpoint, acknowledged as
  `B<line>\n` / `D<line>\n`, without resuming), `q` (detach and run free). A client disconnect is
  treated as `c`.
- **Reentrancy.** Codegen excludes the whole `__dbg_` namespace from the checkpoint, so the agent (and
  every helper it calls to serve the socket) never re-enters itself.

The framed protocol of §3.4, call-depth (step-over/out), and `Inspect`/`Backtrace` (which need §3.3's
symbol table) are the following slices; this one proves the pause/step/breakpoint/resume loop over the
socket works against a native binary.

### 3.3 The line -> locals symbol table (the compiler already knows this)

The codegen already maps every local to a frame offset (`self.locals: name -> rbp-offset`) and knows
its kind (`i64`, `u8`, `f64`, a struct's word count). A `--debug` build emits, in a data section, a
compact table:

```
line 12: [ ("total", -8, i64), ("i", -16, i64) ]
line 13: [ ("total", -8, i64), ("i", -16, i64), ("row", -24, Slice) ]
...
```

`read_local(frame, name, line)` looks the name up in the table for `line` and reads
`frame + offset`. Scalars format directly; a struct/slice reads its words. This is the ONLY new
compiler output the debugger needs, and it is a direct dump of state the compiler already holds.
(It is the same information DWARF encodes, minus the format's generality that a self-hosted debugger
does not need.)

### 3.4 The transport: a framed protocol, WebSocket optional

The `.sock` carries a bidirectional, length-prefixed message stream. The base protocol is a small,
DAP-shaped set of messages (client -> agent commands, agent -> client events), framed as
`u32 length | bytes payload`. The payload is a compact struct (or JSON in the first cut, for
debuggability of the debugger):

```
Client -> agent:  SetBreak{line} | ClearBreak{line} | Continue | StepIn | StepOver | StepOut
                  | Inspect{name} | Locals | Backtrace | Evaluate{expr}
Agent -> client:  Stopped{line, depth, reason} | Value{name, repr} | Locals{[..]}
                  | Frames{[{fn, line}]} | Exited{code}
```

A browser client wants real WebSocket framing (an HTTP upgrade handshake plus the WS frame format).
That is a thin layer over the same stream (about a hundred lines: the handshake's SHA-1 accept key,
then masked/unmasked frames), added as an option, not the base. A CLI or editor speaks the raw
framed protocol directly. Either way the model is identical: the program listens for commands and
emits state.

## 4. Worked session

```
$ SOND_DEBUG_WAIT=1 ./settle --debug-sock ./settle.sock &
$ sond-dbg connect ./settle.sock
> break 42
ok
> continue
Stopped at line 42 (settle), depth 1
> locals
  batch: Slice{ptr: 0x..., len: 128}
  vat:   0
> step-over
Stopped at line 43 (settle), depth 1
> inspect vat
  vat: 4180
> step-in
Stopped at line 7 (sum_vat), depth 2
> step-out
Stopped at line 44 (settle), depth 1
> continue
Exited 0
```

The program ran itself, paused itself, and answered the client, with no external process attached.

## 5. Fit with the self-hosted philosophy

- **Reuses the coverage seam.** No new placement logic; the statement-boundary callout is exactly
  where coverage already proves it belongs.
- **Reuses the compiler's own symbol map.** The line->locals table is a dump of `self.locals`, not a
  new analysis.
- **Reuses Sond's sockets.** The agent is a Sond program using the same socket primitives the stdlib
  already ships.
- **Zero production cost.** The instrumentation exists only under `--debug`; the normal build is
  byte-identical, as coverage is.

The result is a debugger with no ptrace, no DWARF, no second toolchain: the language's introspection
travels inside the binary, the same way its tests, coverage and contracts do.

## 6. The honest limits

- **Multiple threads (`par` / `spawn`, RFC-0008/0010).** Several threads hitting checkpoints at once
  need the agent to serialize socket access and tell the client WHICH thread stopped, and to support
  per-thread run/pause. The first slice is single-threaded (one debuggee thread); multi-thread stop
  modes ("stop all" vs "stop one") are the hard follow-on, as in every debugger.
- **Complex values.** Scalars are trivial; structs/slices need the type in the table to format, and
  following a pointer into the arena needs care. Scalars and slices first.
- **Pausing changes timing.** Blocking in a checkpoint stops the world for that thread; for a program
  that talks to the outside (a socket peer with a timeout) the pause is observable. This is inherent
  to step debugging, not specific to Sond.
- **`Evaluate{expr}` is bounded.** Reading a local is easy; evaluating an arbitrary expression in the
  paused frame means a small interpreter over the frame (or JIT-ing a snippet). The first cut
  supports name lookup and simple field/index access, not arbitrary calls.
- **Optimization.** Line<->instruction mapping is clean because Sond emits direct assembly that `cc`
  assembles without aggressive reordering; a future optimizing backend would need location lists
  (the one place DWARF's generality earns its keep).
- **Socket security.** The `.sock` grants full introspection (and, via `Evaluate`, potentially more)
  of a running process; it must be created with restrictive permissions (0700 dir, owner-only) and
  never exposed beyond the local machine. A `--debug` build is a development artifact, never shipped.

## 7. Open questions

1. **Statement vs line granularity.** Pause per statement, or per source line (coalescing multiple
   statements on a line)? Coverage is per statement; a debugger usually wants per line.
2. **Reverse debugging.** Recording each checkpoint's frame would allow stepping BACKWARD (a
   record/replay debugger). How much state is affordable to log, and is it a separate mode?
3. **Multi-thread stop modes.** "Stop all threads on a breakpoint" vs "stop only the hitting thread":
   which is the default, and how does the client select a thread to step? This is the crux of
   debugging `par`/`graph`.
4. **Contracts as breakpoints.** A `requires`/`ensures` that would fail could DROP into the debugger
   at the violating point instead of trapping (`ud2`), turning a contract failure into a debug stop
   with the counterexample already in scope. Is that a `--debug` behavior of the contract guard?
5. **The `graph` view.** For a `graph { node .. }`, a debugger could show the DAG and let the user
   step node by node, or set a breakpoint on a node. Does the graph get a first-class debug view?
6. **DAP bridge.** Is a thin `sond-dbg <-> DAP` bridge worth shipping so editors work out of the box,
   or should the agent speak DAP directly?

---

*This RFC is exploratory. It describes a debugger that fits Sond the way its tests and coverage do:
inside the language, self-contained, reusing the seams that already exist. The program does not get
attached to; it opens a socket and answers.*

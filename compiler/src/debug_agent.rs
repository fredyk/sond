//! The RFC-0016 debug agent, written in Sond.
//!
//! In a `--debug` build the driver appends [`SOURCE`] to the program after include resolution, and
//! codegen stamps a `call __dbg_step(line)` before every statement. The agent is a self-hosted step
//! debugger served over a Unix socket: the instrumented program binds a listener at [the socket
//! path](SOURCE), announces it on stderr, pauses at entry until a client attaches, and then serves
//! one-byte commands over the connection:
//!
//! - `s` : single-step, stop at the next statement
//! - `c` : continue, run free
//! - `g<line>\n` : run until the given source line
//! - `q` : detach and run free
//!
//! Each stop is reported to the client as `L<line>\n`. The agent uses only compiler builtins
//! (`syscall` / `poke*` / `peek*`), because it is appended after include resolution and so cannot
//! pull in the standard library. Codegen excludes the whole `__dbg_` namespace from instrumentation,
//! so the agent never re-enters its own checkpoint.

/// The debug agent's Sond source. Appended verbatim to a `--debug` program by the driver.
pub const SOURCE: &str = r#"
// ---- RFC-0016 self-hosted step debugger (Sond) ----
let __dbg_path = "/tmp/sond.dbg.sock"
let __dbg_hello = "sond-dbg: waiting for a client on "

// Client connection; -1 when there is no live client.
let mut __dbg_fd = 0 - 1
// 0 until the listener has been set up once. The socket is bound exactly once, at the first
// checkpoint; after a detach or a client disconnect it is NOT rebound (the program just runs free),
// so this is what gates attachment, not `__dbg_fd < 0` (which also holds once a client leaves).
let mut __dbg_started = 0
// Stop policy: 0 = run free, 1 = single-step (stop every statement),
// 2 = run-to (stop only when the line equals __dbg_target).
let mut __dbg_mode = 1
let mut __dbg_target = 0

// Length of a NUL-terminated string at `s` (bounded, so a missing NUL cannot loop forever).
fn __dbg_strlen(s: i64) -> (r: i64) {
    let mut i = 0
    for i < 65536 {
        if peek8(s + i) == 0 { return i }
        i = i + 1
    }
    65536
}

// Write a NUL-terminated string to a file descriptor.
fn __dbg_puts(fd: i64, s: i64) {
    syscall(1, fd, s, __dbg_strlen(s))
}

// Write the decimal form of a non-negative integer to `fd`, followed by a newline.
fn __dbg_emit_int(fd: i64, n: i64) {
    let buf[4]
    let mut v = n
    if v < 0 { v = 0 }
    let mut pos = 31
    poke8(buf + pos, 10)
    pos = pos - 1
    let mut more = 1
    for more == 1 {
        poke8(buf + pos, 48 + v % 10)
        pos = pos - 1
        v = v / 10
        if v == 0 { more = 0 }
    }
    let start = pos + 1
    syscall(1, fd, buf + start, 32 - start)
}

// Parse a decimal integer from buf[off..n], stopping at the first non-digit.
fn __dbg_parse_int(buf: i64, off: i64, n: i64) -> (r: i64) {
    let mut i = off
    let mut acc = 0
    for i < n {
        let c = peek8(buf + i)
        if c < 48 { return acc }
        if c > 57 { return acc }
        acc = acc * 10 + c - 48
        i = i + 1
    }
    acc
}

// Fill a sockaddr_un at `addr` from the NUL-terminated path at `src`:
// { u16 sun_family = AF_UNIX(1); char sun_path[108] }. Returns the addrlen.
fn __dbg_fill_addr(addr: i64, src: i64) -> (r: i64) {
    poke16(addr, 1)
    let mut i = 0
    for i < 108 {
        let c = peek8(src + i)
        poke8(addr + 2 + i, c)
        if c == 0 { return 2 + i + 1 }
        i = i + 1
    }
    2 + 108
}

// On the first checkpoint, bind a listening Unix socket, announce it, and block until a client
// attaches. One client is enough, so the listener is closed once accepted.
fn __dbg_ensure_attached() {
    if __dbg_started == 0 {
        __dbg_started = 1
        let s = syscall(41, 1, 1, 0)
        let addr[16]
        let alen = __dbg_fill_addr(addr, __dbg_path)
        syscall(87, __dbg_path, 0, 0)
        syscall(49, s, addr, alen)
        syscall(50, s, 1, 0)
        __dbg_puts(2, __dbg_hello)
        __dbg_puts(2, __dbg_path)
        let nl[1]
        poke8(nl, 10)
        syscall(1, 2, nl, 1)
        __dbg_fd = syscall(43, s, 0, 0)
        syscall(3, s, 0, 0)
    }
}

fn __dbg_should_stop(line: i64) -> (r: i64) {
    if __dbg_mode == 1 { return 1 }
    if __dbg_mode == 2 {
        if line == __dbg_target { return 1 }
    }
    0
}

// Report the stop as `L<line>\n`, then read commands until one resumes execution.
fn __dbg_interact(line: i64) {
    let ev[1]
    poke8(ev, 76)
    syscall(1, __dbg_fd, ev, 1)
    __dbg_emit_int(__dbg_fd, line)
    let buf[8]
    let mut waiting = 1
    for waiting == 1 {
        let n = syscall(0, __dbg_fd, buf, 64)
        if n < 1 {
            __dbg_mode = 0
            __dbg_fd = 0 - 1
            return
        }
        let c = peek8(buf)
        if c == 115 { __dbg_mode = 1  waiting = 0 }
        if c == 99  { __dbg_mode = 0  waiting = 0 }
        if c == 103 {
            __dbg_target = __dbg_parse_int(buf, 1, n)
            __dbg_mode = 2
            waiting = 0
        }
        if c == 113 {
            syscall(3, __dbg_fd, 0, 0)
            __dbg_fd = 0 - 1
            __dbg_mode = 0
            waiting = 0
        }
    }
}

pub fn __dbg_step(line: i64) {
    __dbg_ensure_attached()
    if __dbg_should_stop(line) == 1 {
        __dbg_interact(line)
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::SOURCE;
    use crate::codegen::emit_program_debug_g;
    use crate::lexer::tokenize_located;
    use crate::parser::Parser;
    use crate::verify::verify_with_globals;

    // The agent is Sond, so it must survive the same pipeline as any program: parse, PROVE its
    // contracts, and lower to code. This is what actually breaks if a prover or codegen change stops
    // accepting the agent (its raw poke/peek buffers, its global mutation, its socket syscalls).
    #[test]
    fn the_debug_agent_parses_verifies_and_lowers() {
        // A trivial host program plus the agent, exactly as the --debug driver assembles them.
        let program = format!("fn main() -> i64 {{ let x = 1\nx }}\n{SOURCE}");

        let (toks, lines) = tokenize_located(&program);
        let mut p = Parser::with_lines(toks, lines);
        let funcs = p.parse_program();
        let globals = p.take_globals();
        let structs = p.take_structs();

        // The agent declares its state as module globals and its logic as `__dbg_*` functions.
        assert!(
            globals.iter().any(|g| g.name == "__dbg_mode"),
            "the agent's stop-policy state is a global"
        );
        assert!(
            funcs.iter().any(|f| f.name == "__dbg_step"),
            "the checkpoint entry point is present"
        );

        // The prover must accept the agent: its buffers use raw poke/peek (no bounds obligation) and
        // it mutates only its own `let mut` globals.
        verify_with_globals(&funcs, &globals).expect("the debug agent must verify");

        // And it must lower without panicking, emitting the checkpoint the codegen calls into.
        let asm = emit_program_debug_g(&globals, &structs, &funcs);
        assert!(asm.contains("__dbg_step"), "the agent is lowered into the program");
    }
}

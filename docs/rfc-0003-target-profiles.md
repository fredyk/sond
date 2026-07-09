# RFC-0003: Target profiles --- Sond universality

| Field         | Value                                  |
|---------------|----------------------------------------|
| RFC           | 0003                                   |
| Status        | Draft                                  |
| Date          | 2026-07-09                             |
| Authors       | JMV + agents                          |
| Related       | RFC-0000 (vision), RFC-0001 (grammar)  |

> **Syntax note**: the examples in this RFC use the original function form
> from RFC-0000/0001 (`= impl:` + indentation) for simplicity. The **canonical
> surface form** (direct body, contracts as clauses, `effects [...] { }`, no
> `= impl`) is defined in the **RFC-0002 Sec.3.1** and replaces the one in these examples.
> The examples will be updated to the RFC-0002 idiom in a consistency pass across
> the series. What matters here is the **profile mechanism**, not the surface syntax.

---

## 1. Motivation and thesis

Sond aspires to be a **universal** language: from the Linux kernel to a loose script, from the browser/WASM to the GPU. But "universal" cannot mean *write the kernel and the script with identical code*. That's physically impossible: a kernel hot-path needs zero-GC, zero-dynamic allocation, predictable latency, and hardware access; a script wants GC, convenience, and minimal ceremony. They pull in opposite directions.

What is achievable and valuable is: **one language, one verifier, one toolchain, one mental model --- and the type system tells you which target profile you're in.** The mechanism is not new: it's capabilities + effects + the sub-language tower (RFC-0000 Sec.8), with a proper name: **target profiles**. Kernel code *will still look different* (no alloc, explicit references, no GC); the triumph is not changing language, toolchain, or head when crossing from one world to another.

Sond's universality is measured in **continuity**, not identity: the same tool verifies, compiles, and deploys on each target, and the programmer's mental model (contracts, capabilities, effects, verification) never changes.

---

## 2. Target profile definition

A **target profile** is a set of constraints that the verifier **enforces statically** on a module or function, combined with the compilation backend the code is oriented toward. The profile defines three things:

1. **Forbidden effects and capabilities**: the verifier rejects code that uses effects or capabilities not allowed by the profile.
2. **Syntax restrictions**: language constructs that the verifier forbids in the profile (unlimited recursion, assignments, dynamic types, etc.).
3. **Compilation backend**: the code generator that translates the verified module into an executable artifact (native binary, kernel object, WASM, PTX/GPU).

### 2.1 Declaration syntax

A profile is declared at the start of a module or function. The syntax is consistent with the RFC-0001 grammar:

```ebnf
<decl_module> ::= <profile_annotations>? "module" <ident> "{" <declarations> "}"

<profile_annotations> ::= <profile_annotation>+

<profile_annotation> ::= "@profile" <profile_ident>
                       | "@target" <target_ident>

<profile_ident> ::= "kernel" | "script" | "wasm" | "server" | "gpu"
<target_ident> ::= <ident>   -- free name for custom targets
```

For individual functions, the profile is inherited from the module or explicitly annotated:

```sond
-- Module-level declaration: entire module is kernel
@profile kernel
module network_driver {
    fn init_hw() -> Result<(), ErrorHW>
        ...
}

-- Function-level declaration: this function is gpu
module shaders {
    @profile gpu
    fn compute_brightness(pixels: List<Pixel>) -> List<f32>
        ...
}
```

For scripts, the profile is declared on the shebang line:

```sond
#!/usr/bin/env snd --profile=script
-- This file runs as a script: GC allowed, minimal ceremony
let message = "Hello, world"
print(message)
```

The default profile (no annotation) is `server`: native binary, GC, all capabilities available. This preserves ergonomics for the most common case.

---

## 3. Canonical profile catalog

### 3.1 `kernel` --- kernel modules, firmware, embedded

**Verifier prohibitions:**
- No GC (the garbage collector does not exist in this profile).
- No dynamic allocation in hot-paths: `List`, `Map`, `String`, and other types requiring dynamic allocation are restricted to initialization areas, not functions marked as `#[hot]`.
- No `panic` / `unwrap` / unhandled error: all errors must propagate or be resolved.
- No network, filesystem, UI, or temporal effects (only `CapMMIO` and `CapDMA` for hardware access).
- No unlimited recursion: only bounded loops (for) or recursion with a demonstrable decreasing invariant.
- Bounded stack: the verifier estimates stack usage and rejects functions that exceed a configurable threshold (default 4 KiB).

**Allowed capabilities:**
- `CapMMIO` --- access to memory-mapped hardware registers.
- `CapDMA` --- direct memory access for DMA transfers.
- `CapIRQ` --- enable/disable interrupts.

**Allowed effects:**
- Only explicit hardware effects (`MMIO`, `DMA`, `IRQ`).

**Compilation backend:**
- Generates relocatable objects (relocatable ELF) or kernel modules (`.ko` for Linux, `.ko` for Zephyr/RTOS).
- Target: ARM Cortex-M, RISC-V, x86_64 kernel mode.

**Example:**

```sond
@profile kernel
module ethernet_driver {
    -- This function runs in hot-path: no alloc, no panic
    fn recv_packet(buf: &mut [u8; 1512], len: u16) -> Result<(), ErrorHW>
        requires:
            len <= 1512
            buf != null
        ensures:
            -- the buffer contains the received packet
            ...
        = impl:
            let mut i: u32 = 0
            while i < len as u32 {
                buf[i as usize] = read_register(REG_RX_DATA)?
                i = i + 1
            }
            Ok(())
}
```

**What the compiler rejects:**

```sond
@profile kernel
fn bad_example() -> Result<(), ErrorHW>
    = impl:
        let mut list = List::new()   -- error: dynamic allocation in kernel
        list.push(42)
        panic!("something failed")   -- error: panic in kernel
        let file = fs::open("x")     -- error: FS effect not allowed in kernel
```

### 3.2 `script` --- loose scripts, interactive scripting

**Verifier relaxations (ceremony only, soundness is NEVER relaxed):**
- No capability is prohibited by default: the script can use whatever the system grants via the shebang line.
- Contracts are optional: `requires`/`ensures` are not mandatory. But any contract you *do* write is verified with full soundness, an unprovable one is still a compile error, never a runtime surprise.
- Effect annotations are optional: functions may leave effects implicit (the verifier infers them), but the effect system still holds, no undeclared effect escapes inference.
- Top-level statements allowed: the script can have executable code directly (not just definitions).
- **Soundness is not weakened.** Types, memory safety, exhaustiveness and every present contract are checked with the same soundness as in every other profile. The `script` profile lowers *required ceremony*, never the *strength of the guarantee*. Per the **Prime Directive** (RFC-0000: correctness always outranks ease of authorship), this supersedes this RFC's earlier draft line "the verifier will not require total soundness", that clause is void.

**Allowed capabilities:**
- All: `CapRed`, `CapFS`, `CapDB`, `CapEnv`, `CapCrypto`, `CapTemporal`, `CapRandom`, `CapUI`.
- Granted automatically according to the shebang line or execution context.

**Allowed effects:**
- All.

**Compilation backend:**
- Runs via a JIT interpreter or compiles to a native binary with included runtime (GC, error handling, full standard library).

**Shebang syntax:**

```sond
#!/usr/bin/env snd --profile=script

-- Direct top-level statements
let config = read_yaml("config.yaml")
let data = fetch_api("https://api.example.com/data")

-- Functions with optional contracts
fn process(data: List<Record>) -> List<Processed>
    -- requires/ensures optional
    = impl:
        data.map(fn(r) => transform(r))

-- Direct execution
let result = process(data)
save(result, "output.json")
print("Done: " + result.len().to_string() + " records processed")
```

**Complete example:**

```sond
#!/usr/bin/env snd --profile=script
import yaml
import json

let args = system_args()
require(args.len() >= 2, "Usage: snd-script file.yaml")

let content = read_file(args[1])?
let data = yaml::deserialize<DataSet>(content)?

let statistics = data.rows.map(fn(row) => {
    Statistic {
        mean: row.values.sum() / row.values.len() as f64,
        max: row.values.max(),
        min: row.values.min()
    }
})

json::serialize_to_file(statistics, "output.json")?
print("Statistics computed for " + data.rows.len().to_string() + " rows")
```

### 3.3 `wasm` --- browser, edge, WebAssembly sandbox

**Verifier prohibitions:**
- No direct filesystem: no `CapFS`, no `fs::open`, no `fs::write`.
- No direct network access: no `CapRed`. Network is accessed exclusively via WASI or host JavaScript bindings.
- No hardware access: no `CapMMIO`, no `CapDMA`.
- No `CapEnv`: no environment variables in the browser.
- GC allowed (WASM GC proposal) or no GC (linear WASM), depending on the compilation flag.
- No direct `CapTemporal`: time is accessed via host bindings.

**Allowed capabilities:**
- `CapDOM` --- browser DOM access (exclusive to this profile).
- `CapUI` --- interface rendering.
- `CapCrypto` --- cryptography via WebCrypto (subset).
- `CapTemporal` --- clock via host `performance.now()`.

**Allowed effects:**
- `UI`, `Crypto`, `Temporal` (limited).
- `IO` --- only for async I/O via promises/host bindings.

**Compilation backend:**
- Generates `.wasm` (WebAssembly) with or without GC, depending on configuration.
- For `@client` components with UI: generates WASM components that integrate with the browser runtime.

**Example:**

```sond
@profile wasm
module calc_app {
    fn calculate_total(items: List<Item>) -> Currency
        requires items.len() > 0
        ensures result >= 0.00
        = impl:
            items.sum(fn(item) => item.price * item.quantity as f64)

    -- Component UI
    @profile wasm
    fn render_app() -> UI<List<Item>>
        effects: [UI]
        reactive:
            let mutable items: List<Item> = []
            let mutable total: Currency = 0.00
        = impl:
            div(
                h1("Total Calculator"),
                item_list(items),
                text("Total: " + total.to_string()),
                button("Calculate", fn() => {
                    total = calculate_total(items)
                })
            )
}
```

**What the compiler rejects:**

```sond
@profile wasm
fn bad_example() -> Result<(), Error>
    = impl:
        let file = fs::open("data.csv")    -- error: CapFS not available in wasm
        let response = red::fetch("https://...") -- error: CapRed not available in wasm
        let data = mmap::map("/dev/sda1")    -- error: CapMMIO not available in wasm
```

### 3.4 `server` --- backend, services, native binary

**Verifier prohibitions:**
- No additional capability restrictions: all system capabilities are available.
- No effect restrictions: all effects are available.
- No dynamic type restrictions: GC, dynamic allocation, growing lists, everything allowed.
- Mandatory contracts: `requires`/`ensures` are verified but do not restrict what can be done.
- The verifier checks total soundness: every contract must be provable.

**Allowed capabilities:**
- All: `CapRed`, `CapFS`, `CapDB`, `CapEnv`, `CapCrypto`, `CapTemporal`, `CapRandom`, `CapUI` (for CLI tools).

**Allowed effects:**
- All.

**Compilation backend:**
- Generates native binary (ELF, Mach-O, PE) with full runtime (GC, standard library, memory manager).
- Target: x86_64, ARM64, RISC-V (user mode).

**Example:**

```sond
@profile server
module payment_api {
    fn process_payment(
        payment: Payment,
        _cap_red: CapRed,
        _cap_db: CapDB,
        _cap_crypto: CapCrypto
    ) -> Result<PaymentConfirmation, PaymentError>
        requires:
            payment.amount > 0.00
            payment.currency in ["EUR", "USD"]
            payment.status == PaymentStatus::Pending
        effects: [Red, DB, Crypto]
        = impl:
            let hash = sha256(payment.data)?          -- OK: CapCrypto
            let record = save_to_db(payment)?         -- OK: CapDB
            let confirmation = send_to_gateway(payment)? -- OK: CapRed
            Ok(PaymentConfirmation {
                hash: hash,
                record_id: record.id,
                confirmation: confirmation
            })
}
```

### 3.5 `gpu` --- parallel compute, shaders, ML

**Verifier prohibitions:**
- No unlimited recursion: only bounded loops with a known static limit.
- No side effects: pure functions, no I/O, no network, no filesystem, no DB, no global state.
- No dynamic heap: all data lives in pre-allocated GPU buffers or in registers.
- No arbitrary pointers: buffer access via bounded indices and verifiable ranges.
- No `panic` / unhandled errors: all errors are resolved at compile-time or propagated to the host.
- No closures capturing mutable state: only first-class pure functions.

**Allowed capabilities:**
- `CapGPU` --- access to GPU buffers, textures, samplers.
- `CapSharedMemory` --- CPU-GPU shared memory (for transfers).

**Allowed effects:**
- `GPU` --- GPU execution kernel.

**Compilation backend:**
- Generates PTK (NVIDIA), SPIR-V (Vulkan), Metal Shading Language, or WGSL (WebGPU) kernels.
- The generator automatically maps Sond data structures to GPU buffers.

**Example:**

```sond
@profile gpu
module brightness_shader {
    -- Pure total function, mappable to a GPU kernel
    fn compute_brightness(
        pixel_r: f32,
        pixel_g: f32,
        pixel_b: f32
    ) -> f32
        ensures result >= 0.0 and result <= 1.0
        = impl:
            0.2126 * pixel_r + 0.7152 * pixel_g + 0.0722 * pixel_b

    -- Parallel kernel: runs once per element
    @profile gpu
    fn apply_brightness(
        pixels: Buffer<Pixel>,
        brightnesses: Buffer<f32>
    ) -> ()
        requires:
            pixels.len() == brightnesses.len()
        effects: [GPU]
        = impl:
            let idx = gpu::thread_idx_x() + gpu::block_idx_x() * gpu::block_dim_x()
            if idx < pixels.len() {
                let p = pixels[idx]
                brightnesses[idx] = compute_brightness(p.r, p.g, p.b)
            }
}
```

**What the compiler rejects:**

```sond
@profile gpu
fn bad_example(x: i32) -> i32
    = impl:
        if x == 0 {
            return bad_example(x - 1)  -- error: unlimited recursion in gpu
        }
        let file = fs::open("x")       -- error: FS effect not allowed in gpu
        let mut global = 0              -- error: mutable global state in gpu
        global = global + x
        return global
```

---

## 4. Enforcement by the verifier

The profile is a **set of forbidden effects and capabilities**, plus syntax restrictions. The verifier operates in two phases:

### 4.1 Profile restriction phase

Before verifying contracts, the verifier applies the profile's restrictions:

1. **Blacklist of capabilities**: if the profile forbids `CapFS`, any function that requires `CapFS` as a parameter produces a compilation error *before* verifying its contract.
2. **Blacklist of effects**: if the profile forbids the `FS` effect, any function annotated with `effects: [FS]` produces an error.
3. **Syntax restrictions**: the verifier rejects recursion without a decreasing invariant in `kernel`/`gpu`, top-level statements in modules that are not `script`, etc.

### 4.2 Contract verification phase

After applying the profile restrictions, the verifier checks contracts normally (RFC-0000 Sec.4). The profile does not affect the semantics of verification: it remains total and sound.

### 4.3 Enforcement examples

**Example 1: Growing `List` in `kernel` -> error**

```sond
@profile kernel
fn process(data: List<u8>) -> List<u8>
    = impl:
        let mut result = List::new()  -- error: dynamic allocation forbidden in kernel
        for item in data {
            result.push(transform(item))
        }
        result
```

The verifier rejects `List::new()` because `List` requires dynamic allocation, and the `kernel` profile forbids dynamic allocation in hot-paths.

**Example 2: `fs.open` in `wasm` -> error**

```sond
@profile wasm
fn load_config() -> Result<Config, Error>
    = impl:
        let file = fs::open("config.json")?  -- error: CapFS not available in wasm
        deserialize(file.read())
```

The verifier rejects `fs::open` because the `wasm` profile forbids `CapFS`.

**Example 3: Recursion in `gpu` -> error**

```sond
@profile gpu
fn factorial(n: u32) -> u32
    = impl:
        if n <= 1 { 1 } else { n * factorial(n - 1) }  -- error: recursion in gpu
```

The verifier rejects recursion because the `gpu` profile forbids unlimited recursion (no decreasing invariant demonstrable statically).

**Example 4: `panic` in `kernel` -> error**

```sond
@profile kernel
fn divide(a: i32, b: i32) -> i32
    = impl:
        if b == 0 {
            panic!("division by zero")  -- error: panic in kernel
        }
        a / b
```

The verifier rejects `panic!` because the `kernel` profile forbids panic (must use `Result` or error propagation).

---

## 5. The escape hatch

The extreme 0.1% of code needs to get out of the verification sandbox. Sond provides an **explicit, auditable, and rare** escape hatch.

### 5.1 Mechanism: `unsafe` / `extern`

```sond
-- Unsafe block: the programmer assumes responsibility
unsafe fn read_port(address: u64) -> u8
    -- Inside unsafe, profile restrictions are relaxed
    -- The verifier emits a warning, not an error
    = impl:
        asm("in al, dx", address, result)

-- External declaration: calls unverified code
extern "C" fn libc_malloc(size: usize) -> *mut u8
extern "C" fn libc_free(pointer: *mut u8)
```

### 5.2 Isolation rules

1. **Lexical containment**: `unsafe` can only appear inside an `unsafe { ... }` block or in a function declared `unsafe fn`. It does not propagate: a function that calls `unsafe` is not automatically `unsafe`.
2. **Audit trail**: the compiler emits an explicit warning (`warning: unsafe block in kernel module`) that can be treated as an error with `--Werror`.
3. **Size limit**: an `unsafe` block cannot contain more than 20 lines of code (configurable). This forces the escape hatch to be small and reviewable.
4. **Capability isolation**: `unsafe` does not grant additional capabilities. It only relaxes the profile's syntax restrictions. If an `unsafe` function needs `CapFS` in a `kernel` profile, it must receive it explicitly as a parameter (the compiler rejects it because `kernel` does not allow it on its whitelist).
5. **Mandatory comment**: every `unsafe` block must include a comment explaining why it is necessary:

```sond
unsafe {
    -- REASON: direct access to hardware register for PHY reset
    -- No kernel abstraction for this edge case
    let value = read_register(0xFF00)
    write_register(0xFF01, value | 0x01)
}
```

### 5.3 Audit and traceability

The compiler generates an `unsafe` report per module:

```
warning: 2 unsafe blocks in module network_driver (profile: kernel)
  -> network_driver.snd:45  -- unsafe: PHY register access
  -> network_driver.snd:78  -- unsafe: DMA buffer without bounds check
  Use --Werror to treat as error
```

This enables focused code reviews: the reviewer only needs to inspect the `unsafe` blocks.

---

## 6. Profiles and the three frontiers

Target profiles intersect with the three frontiers from RFC-0000 Sec.7 in predictable and verifiable ways.

### 6.1 Network frontier (`@server` / `@client`)

The network frontier is, at its core, a particular case of `server`/`wasm` profiles in the same file. A `.sond` with functions of multiple profiles compiles to multiple artifacts:

```sond
-- A file containing code from two profiles
@profile server
module backend_api {
    @server
    fn list_users() -> Result<List<User>, Error>
        effects: [DB]
        = impl: query_all()

    @profile server
    fn persist_user(user: User) -> Result<(), Error>
        effects: [DB]
        = impl: save(user)
}

@profile wasm
module frontend_app {
    @client
    fn render_users() -> UI<List<User>>
        effects: [Red]
        reactive:
            let mutable users: List<User> = []
        = impl:
            div(
                list(users, fn(u) => user_item(u)),
                button("Refresh", fn() => {
                    users = fetch_api("/users")
                })
            )
}
```

**Multi-artifact compilation:**

```
sond build file.sond
  -> build/server/backend_api      (native binary, server profile)
  -> build/wasm/frontend_app.wasm  (WebAssembly, wasm profile)
  -> build/types/schema.sql        (schema derived from types)
```

The compiler generates the communication infrastructure (RPC/serialization) between the two artifacts, verifying that:
- The `@client` only calls `@server` via the defined interface.
- Shared types (`User`) are serializable on both sides.
- The `@server`'s capabilities (DB) do not propagate to the `@client`.

### 6.2 State and UI frontier (reactivity)

Reactivity works within a `wasm` or `server` profile with UI. It does not interfere with `kernel` or `gpu` profiles because those have no UI.

### 6.3 Time frontier (sync and async)

The `kernel` and `gpu` profiles are inherently synchronous (no async runtime). The `server` and `wasm` profiles can use async. The verifier checks that async functions are only called from profiles that support them.

### 6.4 Multi-artifact deployment from a single file

A `.sond` file can contain modules of multiple profiles. The compiler generates one artifact per profile:

| Profile | Generated artifact | Target |
|---------|-------------------|--------|
| `kernel` | Relocatable object (`.o` / `.ko`) | Linux kernel module, firmware |
| `script` | Binary with runtime or JIT script | CLI, scripts |
| `wasm` | `.wasm` | Browser, edge, sandbox |
| `server` | Native binary | Backend, services |
| `gpu` | Kernel (PTX / SPIR-V / Metal) | GPU compute |

---

## 7. Honest comparison with Rust

Rust already spans kernel -> WASM -> CLI -> embedded TODAY. It is the language that comes closest to universality. But it lacks something concrete to be *the* universal language:

### 7.1 What Rust does well

- **Ownership and borrow checker**: memory safety without GC. Works in kernel, WASM, embedded.
- **`no_std`**: allows code without runtime (kernel, embedded, minimal WASM).
- **`wasm32-unknown-unknown`**: first-class WASM target.
- **Ecosystem**: crates.io, cargo, mature tooling.

### 7.2 What Rust lacks (and Sond has)

1. **Script/GC ergonomics**: Rust charges a tax on the easy 90%. A simple 20-line script requires `fn main()`, `Result`, `?`, explicit error handling, and the borrow checker verifies things a script doesn't need. Sond with `script` profile eliminates that ceremony.
2. **Contracts and formal verification**: Rust has traits, which are partial contracts (type interfaces), but no formal preconditions/postconditions/invariants. The borrow checker is a partial verifier (memory only), not a behavior verifier.
3. **Explicit capabilities**: Rust has ownership (good memory safety), but no capabilities for I/O, network, or arbitrary effects. You cannot statically prove a function makes no network calls.
4. **One single verifier**: in Rust, `cargo check`, `cargo test`, `clippy`, `miri` are different tools. In Sond, the verifier is one and verifies everything (types, contracts, effects, capabilities, profile).

### 7.3 Real limits of universality

Universality is a **spectrum**, not a yes/no:

| Capability | Sond | Rust | C | Haskell |
|------------|------|------|---|---------|
| Linux kernel | Yes | Yes (`no_std`) | Yes | No |
| Interactive script | Yes (GC, minimal ceremony) | Partial (clumsy) | No | Yes (GHCi) |
| Browser WASM | Yes | Yes | Partial | Partial |
| Server backend | Yes | Yes | Yes | Yes |
| GPU compute | Yes (dedicated profile) | Partial (cust CUDA) | Partial | No |
| Formal verification | Yes (contracts + solver) | No (borrow only) | No | Partial (types) |
| Capability-based security | Yes | Partial (ownership) | No | No |

**Where the real limit is:**
- **GPU**: the `gpu` profile is a strongly restricted sub-language (Futhark/Bend style). It is not the full language: it's a data-parallel subset. This is an explicit tradeoff, not a limitation.
- **Kernel**: the `kernel` profile is C-like by necessity (no GC, no alloc, bounded stack). The code *looks* different, but uses the same language, verifier, and toolchain.
- **Script**: the `script` profile relaxes verification for ergonomics. It does not have total soundness (contracts are optional). This is a deliberate tradeoff: a script does not need the same guarantee as a kernel.

Sond's universality is achieved not because everything looks the same, but because **everything is verified with the same tool and deployed with the same toolchain**. The programmer learns one mental model (contracts, capabilities, effects) and reuses it on all targets.

---

## 8. Comparison with other languages and existing mechanisms

| Mechanism | Where it exists | What Sond does differently |
|-----------|----------------|---------------------------|
| `#[no_std]` (Rust) | A flag that disables the runtime | Full profile with statically verified restrictions |
| `unsafe` (Rust) | Unverified code block | Same idea, but with size limit and mandatory audit |
| `@target` (Zig) | Cross-compilation target | Profile = semantic restrictions, not just compilation target |
| Sub-languages (Futhark, CUDA) | Separate languages for GPU | GPU is a profile of the same language, not a different language |
| Effect systems (Koka, Eff) | Algebraic effects as types | Effects intersect with capabilities and profiles for static enforcement |

---

## 9. Open questions

1. **Can a module mix profiles?** The current proposal allows a `.sond` file to contain modules of different profiles, but not that an *individual module* can have functions of different profiles (except by inheriting from the module). **Pending**: decide whether a function can be annotated with a different profile than its parent module, and what implications that has for verification.

2. **How is the stack limit verified in `kernel`?** The verifier must estimate stack usage for each function. For recursion with a decreasing invariant, the estimate is decidable (max depth = initial value of the invariant). But for indirect calls (function pointers) or mutual recursion, the estimate is more complex. **Pending**: design the stack estimation algorithm and define when it fails with a clear error rather than a false positive.

3. **Is the `gpu` profile a separate sub-language or a subset?** The current proposal treats it as a subset of the full language, but the restrictions are so strong it might deserve its own parser and grammar (like Futhark). **Pending**: decide whether `gpu` is implemented as a verifier restriction phase or as a separate language with translation to Sond.

4. **Interoperability between profiles**: when a `server` module calls a `gpu` function, how is data passed? GPU buffers are a runtime resource that does not exist in the `server` profile. **Pending**: design the CPU-GPU transfer mechanism (shared buffers, copy-in/copy-out) and how the verifier checks that the transfer is correct.

5. **Custom profiles**: the grammar admits `@target <ident>` with a free name. What restrictions can a user define? If a user can define their own capabilities and effects, the profile system becomes extensible but potentially insecure. **Pending**: decide whether custom profiles are internal-use (restricted) or open (anyone can define a profile).

6. **Verifier performance with multiple profiles**: verifying a file with modules from 3 different profiles requires 3 verifier runs with different restrictions. Can it be parallelized? Does the cost accumulate or share phases? **Pending**: design the verifier architecture to support multiple profiles without quadratic degradation.

7. **Profile migration**: if a module changes from `script` to `server` (because it grew and needs more verification), does the verifier reject existing code? Is there a gradual migration path? **Pending**: design a migration mechanism where the verifier reports which contracts are missing for the new profile, rather than rejecting everything at once.

8. **Profiles and testing**: the `script` profile relaxes contracts. How do you test `script` code that has no contracts? Are tests automatically generated from types? Is manual testing required? **Pending**: define the relationship between testing and verification for relaxed profiles.

9. **Conditional deployment**: a `.sond` file can compile to WASM for the browser OR to a native binary for the server. But sometimes you want the *same* code deployed to two targets with *different* capabilities. **Pending**: design a "conditional compilation" mechanism where the same code compiles with different restrictions depending on the deployment target.

10. **Profiles and the sub-language tower**: the tower's sub-languages (RFC-0000 Sec.8, RFC-0001 Sec.6) --- templates, styles, queries --- do they have their own profile? Templates and styles are total-terminating (level 1 of the tower); queries are level 2. **Pending**: decide whether each level of the tower is an implicit profile or whether sub-languages operate *within* any profile.

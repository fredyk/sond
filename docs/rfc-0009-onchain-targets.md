# RFC-0009: On-chain targets --- provably-correct smart contracts (Solana SBF + Ethereum EVM)

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0009                                                                  |
| Status      | Draft (exploratory)                                                   |
| Date        | 2026-07-18                                                            |
| Authors     | JMV + agents                                                         |
| Motivation  | On-chain code is where a proven contract is worth the most: bugs are irreversible and paid in money |
| Related     | RFC-0000 (contracts, capabilities, effects), RFC-0003 (target profiles), RFC-0006 (packages) |

## 1. Why on-chain is Sond's killer application

A smart contract is the purest case of Sond's thesis. On-chain code:

- **Cannot be patched** once deployed (immutable), you get exactly one chance to be correct.
- **Fails in money**: a wrong postcondition is not a crash to retry, it is a drained treasury
  (The DAO, Wormhole, countless re-entrancy and overflow exploits, all *contract violations*
  that a sound oracle would have refused to compile).
- **Runs in a restricted, deterministic VM**, no threads, no clock, no filesystem, which is
  a *smaller* verification surface than a general OS, so the prover has less to reason about.
- **Is small and high-value**, a few hundred lines guarding millions; the ideal ratio for
  "spend more implementation effort per stronger guarantee" (RFC-0000).

Every exploit class that has drained on-chain funds maps onto a Sond guarantee that already
exists or is on the debt registry: integer overflow (#3), unchecked arithmetic, re-entrancy
(an *effect*-ordering property, capabilities/effects, RFC-0000 §5.3/§6), access control (a
capability you were not given, §6), unchecked external calls (the `--shared`/FFI trust
frontier, RFC-0007). **Sond's differentiator on-chain is not performance or ergonomics, it
is that the compiler proves the invariant, so the contract cannot be exploited into a state
its `ensures` forbids.**

## 2. Two targets, one shape: a restricted verified VM

Both chains run a sandboxed bytecode VM with no ambient authority, exactly the environment
Sond's capability model assumes. Each becomes a new **target profile** (RFC-0003), alongside
`hosted`/`freestanding`/`uefi`:

| | Solana | Ethereum |
|---|---|---|
| VM | **SBF** (Solana Bytecode Format, an eBPF variant) | **EVM** (stack machine) |
| Word | 64-bit registers (11 GP regs) | **256-bit** stack words |
| Artifact | an SBF ELF (`.so`) | EVM bytecode (init + runtime) |
| Entry | `process_instruction(prog_id, accounts, data)` | 4-byte selector dispatch |
| State | passed-in **accounts** (borsh) | contract **storage** (SLOAD/SSTORE) |
| Resource | compute units | **gas** |
| Serialization | Borsh | ABI (keccak256 selectors) |

The codegen effort differs sharply (see §5): SBF is close to Sond's existing register model;
the EVM is a stack machine over 256-bit words and needs a new numeric type.

## 3. Solana target (`@profile solana`)

### 3.1 Backend (codegen)
SBF is register-based eBPF: 11 registers, a fixed instruction set, a stack, and an ELF
container with relocations for **syscalls**. It is the *closest* of the two to Sond's existing
x86-64 backend, a new instruction selection + register allocation pass over the same IR, and
an SBF-ELF emitter. Effort: a **new backend**, but a tractable one (eBPF is small and
regular; the existing `--freestanding`/`--uefi` emitters show the pattern of a
profile-specific ELF/PE writer).

### 3.2 The builtin library (`lib/solana/…`)
The runtime glue a program needs, written in Sond over the SBF syscalls:
- **Entrypoint wrapper**: deserialize the opaque input region into `(program_id, accounts[],
  instruction_data)` per the runtime's layout; call the user's `process` with a typed view.
- **Syscalls**: `sol_log`, `sol_log_64`, `sol_memcpy/memset`, `sol_invoke_signed` (CPI),
  `sol_get_clock_sysvar`, `sol_create_program_address` / `sol_try_find_program_address`
  (PDAs), `sol_sha256`, `sol_keccak256`, `sol_secp256k1_recover`.
- **Borsh** encode/decode over the byte buffer (reuse `lib/x86_64/linux/bytes.snd`'s model,
  moved to a target-neutral `lib/any/bytes.snd`, the arena/allocator becomes a bump over the
  program's heap region).
- **Account model**: lamports, owner, data slice, `is_signer`/`is_writable` as *capabilities*
 , a function that mutates an account it was not given as writable **does not compile**.

### 3.3 The verification win
Re-entrancy and CPI safety become effect-ordering obligations; account-write authority becomes
a capability; arithmetic on lamports/token amounts rides the overflow closure (#3). A Solana
program that compiles has *proven* it never writes an account it was not authorized to, never
overflows a balance, and satisfies its declared `ensures` on the resulting account state.

## 4. Ethereum target (`@profile evm`)

### 4.1 Backend (codegen)
The EVM is a **stack machine over 256-bit words**, structurally alien to Sond's register IR.
The realistic path is to lower Sond's IR to **Yul** (the EVM's structured intermediate
language) or directly to EVM bytecode via a stack-scheduling pass. This is the **harder**
backend: it needs (a) a `u256`/`i256` numeric type in the surface (money is 256-bit), (b) a
stack allocator instead of registers, (c) the two-part deploy artifact (constructor + runtime).

### 4.2 The builtin library (`lib/evm/…`)
- **Dispatcher**: compute the 4-byte selector = `keccak256("f(uint256,address)")[0:4]`, match
  against `pub fn` signatures, decode ABI args, encode the return.
- **ABI** encode/decode (static + dynamic types) over the byte buffer.
- **Storage**: `SLOAD`/`SSTORE` behind a typed `storage` capability; **memory** and
  **calldata** as distinct address spaces.
- **Environment**: `CALLER`, `CALLVALUE`, `ADDRESS`, `TIMESTAMP`, `keccak256`, `CALL`/
  `STATICCALL`/`DELEGATECALL` (the last gated hard behind a capability, delegatecall is the
  proxy-exploit vector), `REVERT` (a failed `requires` compiles to a `revert`, not a trap).
- **Gas as a resource**: an effect the prover accounts for; a `@profile evm` function can
  carry a `gas ≤ N` bound in its contract.

### 4.3 The verification win
Re-entrancy (checks-effects-interactions) is an effect-ordering property the prover enforces;
`msg.sender` access control is a capability; overflow is #3; the classic "unchecked call
return" is the FFI trust frontier (RFC-0007). A contract that compiles has proven its
`ensures` on storage hold across every entry point, the invariant an auditor pays for.

## 5. Difficulty, honestly

- **Solana/SBF**: a *new backend* but tractable (regular RISC-like ISA, small syscall set,
  ELF emitter pattern already exists). The builtin lib is the larger part (Borsh, accounts,
  CPI) and is plain Sond. **The recommended first target.**
- **Ethereum/EVM**: harder, stack machine + 256-bit + Yul lowering + a new numeric type.
  Highest exploit-prevention value, but a bigger lift; do it after SBF proves the on-chain
  profile machinery.
- **Shared prerequisites** (do these regardless, they also help self-hosting): move `bytes`/
  arena to `lib/any/` (target-neutral), a `u256` type, and target-neutral crypto (keccak256 -
  the PQ fleet's SHA-3 work is adjacent; Solana/EVM both need keccak).
- **What Sond does NOT need to win**: it does not need to beat Rust/Solidity on gas or tooling
  maturity. It needs one thing they cannot offer: *the compiler refused to build the exploit.*

## 6. Staged plan

1. **Prereqs**: target-neutral `bytes`/arena; `u256`; keccak256 (align with the PQ SHA-3 lib).
2. **`@profile solana`**: SBF backend (MVP: log + return) → account/borsh builtin lib →
   a proven token-transfer example (balance conservation as an `ensures`).
3. **`@profile evm`**: Yul lowering (MVP: a `uint256` getter/setter) → ABI + dispatcher +
   storage builtin lib → a proven ERC-20 with overflow-free, access-controlled invariants.
4. **The pitch**: "the first contract language where the exploit does not compile."

## 7. Open questions

1. **Yul vs. direct EVM bytecode** for the Ethereum backend, Yul buys an existing optimizer
   and the object format; direct bytecode buys control. Which fits Sond's IR better?
2. **`u256` in the core** vs. an on-chain-only type, money is 256-bit; does the general
   language gain a fixed-width big-int, or is it profile-scoped?
3. **Gas as a first-class effect**, can the prover carry a real `gas ≤ N` bound, or is gas
   left to the VM and only *soundness* (not cost) is proven?
4. **CPI / external calls** as the FFI trust frontier (RFC-0007), the boundary drops
   guarantees; how much of the callee contract can we assume from its on-chain interface?
5. **Determinism**, both VMs are deterministic; does that let us prove *more* than off-chain
   (no clock/threads/entropy to havoc)?

---

*Exploratory. This RFC does not change the implemented surface; it records why on-chain is the
application where Sond's core promise, "the compiler proves it, or it does not compile", is
worth the most, and sketches the two target profiles and their builtin libraries.*

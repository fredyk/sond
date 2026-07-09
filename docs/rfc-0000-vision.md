# RFC-0000: Sond -- A Programming Language Optimized for Verifiability in the AI Era

| Field       | Value                                  |
|-------------|----------------------------------------|
| RFC         | 0000                                   |
| State       | Draft                                  |
| Date        | 2026-07-09                             |
| Authors     | JMV + agents                          |
| Name        | Sond                     |

---

## ⭐ Prime Directive (permanent, JMV 2026-07-14)

**Sond is a language written by AI and used by AI.** It is no longer necessary to
avoid features that are hard to implement, even ones that look herculean. What is
needed is a language in which it is *almost impossible* to make a programming or logic
error: a language that is **extremely hard for a human to write, yet in which it is
also extraordinarily hard for an AI to be wrong**. Every design decision serves that
end. When a choice trades more implementation effort for a stronger correctness
guarantee, take the harder implementation. Ease of authorship is never a reason to
weaken a guarantee. This directive stands above all others and does not expire.

**The one bound on that difficulty:** the language must *always* remain writable by a
**qwen3.6-class model** (a modest AI) working in the compiler's red→green oracle loop.
That is the difficulty ceiling, push every guarantee as hard as possible *up to* the
point where such a model can still produce valid programs against the oracle, and no
harder. A guarantee that no in-scope AI can actually satisfy is not a guarantee, it is
a wall. It follows that the oracle must be fast and its rejections legible, because a
modest model reaches correctness by *iteration*, not on the first try.

**The acceptance test for difficulty (operational, falsifiable).** A Sond feature's
difficulty is *tolerable* if launching a qwen3.6-class model **up to three times** over
a given codebase gets it to ship a new user-facing feature *using* that Sond feature,
without collapsing (looping, diverging, or giving up). If qwen still collapses on the
third attempt, the feature is **too hard**, redesign it (raise its ergonomics or the
oracle's feedback) *without weakening its guarantee*. This makes the difficulty ceiling
a test you can run, not an opinion.

**Compiler feedback is a first-class feature, co-equal with the guarantees.** Because a
modest AI reaches correctness by *iteration*, the quality of the compiler's rejection
*is* the gradient it descends. Every rejection must be **precise** (which proposition
failed, and exactly where), **actionable** (a counterexample input, the missing
invariant/contract, a concrete suggested fix), **machine-legible** (structured output an
AI parses reliably), and **fast**. A guarantee whose violation is reported opaquely is
worthless to qwen3.6, it will collapse the three-attempt test. Corollary: **better
feedback raises the difficulty ceiling**, the harder a feature, the more the compiler
must guide. Feedback quality and the difficulty ceiling are coupled; invest in the
oracle's diagnostics as heavily as in its soundness.

> Put plainly: **a language so brutally hard to write that no human will use it, but
> when an AI writes in it, it never has to come back to the code to fix a bug later.**

Where today's compiler still contradicts this directive is tracked, explicitly and
temporarily, in the [Bootstrap Debt Registry](bootstrap-debt-registry.md).

---

## 1. Abstract

Sond is a programming language whose primary unit is not code, but the **verified specification**: human intent codified as a contract (preconditions, postconditions, invariants) plus an implementation that the compiler proves satisfies said contract. The design responds to a paradigm shift in the era of AI-generated code: when AI types at scale, the bottleneck stops being *writing* and becomes *verifying*. Sond makes verification the central axis of the workflow: the human writes and approves contracts; the AI fills in and proves bodies; the compiler acts as an oracle in a tight loop that always terminates, is sound, and rejects what it cannot prove. The result is code that can be cheaply distrusted because every line has demonstrable properties, security is by capabilities (not trust), and reasoning is local (a diff is verified against its interface, not against the universe).

---

## 2. Motivation: the paradigm shift

### 2.1 The wrong question

The usual question about AI and programming is: *"Can AI write good code?"*. This question is ill-posed. In 2026, AI writes functional code with inconsistent quality: sometimes brilliant, sometimes subtly broken. The problem is not AI's ability to type -- it is the human's inability to **trust** what AI types.

The real bottleneck is **verifying**, not writing. An experienced human writes 50 lines in 30 minutes. An AI writes them in 3 seconds. But the human takes 2 hours to understand whether those 50 lines are correct, safe, and don't break anything. The cost-benefit ratio has inverted: writing is cheap, verifying is expensive.

### 2.2 The era of generated code

Existing languages were designed for an era where humans typed every line. Their ergonomics (brevity, DRY, syntactic sugar) are optimized to minimize keystrokes. When AI types for you, those optimizations lose value. What gains value is what makes **distrusting** cheap:

- Knowing what a function *should* do without reading its implementation.
- Knowing that a local change doesn't break anything globally.
- Knowing that a component cannot access resources it was not explicitly granted.

Sond reorients the language's ergonomics toward **verifiability** instead of **authorship**.

### 2.3 The paradox of formal languages

Dafny, F*, SPARK/Ada, and other languages with contracts and formal verification already exist. They have been marginalized for decades because writing contracts by hand is expensive: the annotation cost exceeds the verification benefit for most projects. But this calculus inverts when AI writes the bodies. If AI fills in implementations and the compiler verifies them against human contracts, the friction of formal languages becomes their advantage: the human only writes the small part (the contracts), and AI does the heavy lifting (the implementations), with the compiler as an inflexible arbiter.

Sond exists to exploit this inversion.

---

## 3. Design principles

### 3.1 Pillar 1: The spec is the primary artifact; code is derived

In Sond, the unit of program is not a function or a module: it is a **verified specification** that contains:

- **Intent** (name + semantic docstring, not just syntactic).
- **Contracts** (preconditions, postconditions, type/structure invariants).
- **Implementation** (body that the compiler proves satisfies the contracts).

The human owns the contracts. The AI fills in the body. The compiler rejects any body that doesn't satisfy the contract. The human reviews **intentions**, not lines.

#### 3.1.1 Contract syntax

```sond
-- A function with a complete contract
fn calcular_tax_base(
    items: List<LineaFactura>,
    descuento_global: Percent
) -> (base: Money)
    requires:
        -- Each line has positive quantity and non-negative price
        forall(i in 0..items.len()):
            items[i].amount > 0 and items[i].precio_unitario >= 0

        -- The global discount is within valid range
        0 <= descuento_global <= 100

        -- The list is not empty
        items.len() > 0

    ensures:
        -- The base is the sum of (quantity x price) for each line,
        -- minus the global discount, rounded to 2 decimal places
        base == round(
            sum(
                items,
                fn(item) => item.amount * item.precio_unitario
            ) * (1.0 - descuento_global / 100.0),
            decimals=2
        )

        -- The base is non-negative (logical consequence of requires,
        -- but explicit for clarity)
        base >= 0.00

    -- The body is filled in by the AI; the compiler verifies it
    = impl:
        -- The AI implements this; the compiler proves that it satisfies
        -- the postconditions given the contracts
        ...
```

#### 3.1.2 Type invariants

```sond
-- A type with an invariant: always represents a valid percentage
type Percent = f64
    where self >= 0.0 and self <= 100.0
    -- The compiler inserts the invariant check
    -- at each construction of the type

-- A type with a compound invariant
type RangoFecha = {
    inicio: Fecha,
    fin: Fecha
}
    where self.inicio <= self.fin
    -- Destructuring in the where has access to fields
```

#### 3.1.3 `old()` for postconditions referencing prior state

```sond
fn transfer(
    source: &mut Account,
    destination: &mut Account,
    amount: Money,
) -> Result<(), ErrorTransferencia>
    requires:
        amount > 0
        cuenta_source.balance >= amount
        cuenta_source.id != cuenta_dest.id

    ensures:
        -- Balance is preserved (global invariant)
        cuenta_source.balance + cuenta_dest.balance
            == old(cuenta_source.balance) + old(cuenta_dest.balance)

        -- The source lost exactly the amount
        cuenta_source.balance == old(cuenta_source.balance) - amount

        -- The destination gained exactly the amount
        cuenta_dest.balance == old(cuenta_dest.balance) + amount
```

### 3.2 Pillar 2: The compiler as oracle in a tight loop

Sond's type and effect system must be **total** (always terminates, always produces a verdict) and **sound** (if it says "correct", it is; zero false negatives). This turns code generation into a tight loop:

```
AI proposes body -> compiler verifies -> if it fails, AI fixes -> repeat
```

In each iteration, AI receives a verdict with precise information about which contract was not satisfied and why. The compiler is not a linter that suggests: it is an **oracle** that proves.

#### 3.2.1 Totality: always terminates

Sond's verifier operates over a bounded set of decidable properties. It doesn't try to prove arbitrary theorems: it verifies contracts against implementations using:

- **Dependent types** for arithmetic properties over bounded integers.
- **SMT solver** (Z3/CVC5) for properties over finite ranges and structures.
- **Bounded unrolling** for properties over lists and recursion (with configurable limit).
- **Invariant inference** for common cases (summations, ranges, ordering).

If a property is not automatically decidable, the compiler marks it as **pending human test** and generates a test case that the developer must approve. But this is the exception, not the rule: for well-written contracts (arithmetic properties, structure invariants, resource conservation), automatic verification covers >95% of practical cases.

#### 3.2.2 Soundness: zero false negatives

If the compiler says "this body satisfies this contract", it does. There are no hidden bugs that the verifier missed. This is possible because:

- Dependent types are sound by construction (the SMT solver only says "satisfied" when it actually is).
- Type invariants are verified at each construction and destruction.
- Contracts are verified against the full implementation, not against a partial abstraction.

The cost of soundness is that the compiler may reject correct code (**false positives**). Sond minimizes this with invariant inference and a configurable solver, but accepts it as a trade-off: a false positive is an inconvenience (the human annotates an auxiliary lemma); a false negative is a production bug.

#### 3.2.3 Guide for AI self-correction

The AI-compiler loop works because the verifier produces **high-quality signals**:

```
error: postcondition not satisfied
  -> `base >= 0.00` fails at line 47
  -> the execution path where items is empty is not covered by the requires
  -> suggestion: add requires(items.len() > 0)
```

The AI receives: (1) which contract failed, (2) at which line, (3) which path caused it, (4) a suggestion. With this, correction is mechanical: the AI adjusts the body or the contract and retries. The typical loop is 1-3 iterations.

### 3.3 Pillar 3: LOCAL reasoning, zero action at a distance

AI edits in chunks: a diff, a file, a function. If fixing a diff depends on the global state of the program, verification is impractical (AI would need to load the universe into context). Sond guarantees that a module's correctness can be proved **locally**: if a module type-checks against its interface, it is correct. Period.

#### 3.3.1 Immutability by default

```sond
-- By default, everything is immutable
let x = 42
x = 43  -- compilation error: `x` is immutable

-- Mutability is an explicit effect
let mut x = 42  -- `mut` is an effect marker
x = 43  -- correct: x was declared mutable
```

#### 3.3.2 No mutable globals

There are no mutable global variables in Sond. Shared state is passed explicitly:

```sond
-- THIS DOES NOT EXIST in Sond:
-- global mutable_counter = 0  -- PROHIBITED

-- Instead, state is an explicit parameter
type EstadoContador = { mutable valor: i64 }

fn incrementar(estado: &mut EstadoContador) -> ()
    ensures estado.valor == old(estado.valor) + 1
    = impl:
        estado.valor = estado.valor + 1
```

#### 3.3.3 Explicit effects in the signature

```sond
-- A function that performs I/O declares it in its type
fn leer_configuracion(ruta: Ruta) -> Result<Configuracion, ErrorIO>
    effects: [IO]
    -- The compiler knows this function has I/O effects
    -- and can verify it's not called in pure contexts

-- A pure function cannot call effects
fn calcular_total(items: List<Item>) -> Money
    -- Without `effects: [IO]`, the compiler rejects calls to functions with I/O
    = impl:
        sum(items, fn(i) => i.precio)  -- correct
        leer_archivo("config.txt")     -- error: undeclared IO effect
```

#### 3.3.4 Why this makes "AI touches 40 files" scalable

When AI modifies 40 files in a refactoring, each file is verified locally against its interface. There are no hidden dependencies: if a module's interface didn't change, its implementation can change freely. If the interface changed, the compiler says exactly which callers are broken and where. The AI doesn't need to "understand the whole system": it only needs to know the interface of the module it's touching.

### 3.4 Pillar 4: Security by capabilities, not by trust

In a world where code is generated by AI, you can't trust that a function won't do malicious things. Prompt injection, malicious input data, code that AI "snuck in" without the human reviewing it. The only defense is that **if you didn't give a capability to a function, it is physically incapable of using it**, guaranteed by the type.

#### 3.4.1 Capability model

```sond
-- A capability is a phantom type passed as a parameter
-- Phantom: no runtime representation, only exists in the type system

phantom type CapRed       -- network access
phantom type CapFS         -- filesystem access
phantom type CapDB         -- database access
phantom type CapEnv        -- environment variable access

-- formatInvoice can only do what its capabilities allow
fn format_invoice(
    factura: Factura,
    _red: CapRed  -- IF it received CapRed, it CAN do network I/O
) -> Result<Bytes, ErrorFormato>
    effects: [IO]
    = impl:
        -- OK: has CapRed, can send
        enviar_a_servidor(factura)

-- if it does NOT receive CapRed, it's physically incapable of networking
fn format_invoice_local(
    factura: Factura
    -- NO CapRed parameter
) -> Result<Bytes, ErrorFormato>
    -- Without CapRed, the compiler rejects any calls to functions requiring CapRed
    = impl:
        enviar_a_servidor(factura)  -- error: CapRed not available in scope
        generar_pdf(factura)        -- OK if CapFS is available
```

#### 3.4.2 Static sandbox

AI-generated code runs in a sandbox defined by its capabilities:

```sond
-- An AI component that processes user data
fn procesar_entrada_usuario(
    datos: DatosUsuario,
    _cap_db: CapDB,          -- can read/write DB
    _cap_env: CapEnv          -- can read config
    -- NO CapRed: cannot send data to the Internet
    -- NO CapFS: cannot write arbitrary files
) -> Result<Respuesta, ErrorProcesamiento>
    effects: [DB, Env]
    = impl:
        -- The AI implements this; the compiler guarantees that
        -- it cannot make network calls, cannot write files,
        -- can only access DB and Env
        let config = leer_env("API_KEY")    -- OK: has CapEnv
        let registro = buscar_en_db(datos.id)  -- OK: has CapDB
        Ok(procesar(registro, config))
```

This solves the fundamental problem of AI-generated code: you don't need to trust that AI didn't inject a hidden `curl` or a malicious `write`. If it doesn't have the capability, the compiler rejects it. It's not a convention: it's a theorem.

---

## 4. Compilation and verification model

### 4.1 When it's verified > how it runs

The real axis of Sond's design is not *how it runs* (compiled vs. interpreted) but *when it's verified*. An interpreter discovers the bug by running -- in production, with real data, at 3 AM. A compiler with verification turns it into "a program that doesn't exist because it didn't compile".

Sond is **AOT compiled** (Ahead-of-Time) for these reasons:

1. **Total verification at compile-time**: everything provable is resolved before execution. The binary that reaches production has already been proven correct against its contracts.
2. **Reproducibility**: a compiled binary is always identical. No runtime dependencies, no "works on my machine".
3. **Performance**: native binary, no interpretation or GC overhead.
4. **Single-artifact deployment**: a `.sond` compiles to WASM (browser) + native binary (server) + SQL schema (derived from types).

### 4.2 Compilation pipeline

```
                    +-------------+
                    |  Source .sond |
                    +------+------+
                           |
                    +------v------+
                    |  Parser +    |
                    |  Type Check  |
                    +------+------+
                           |
                    +------v------+
                    |  Contracts   |
                    |  Extract     |
                    +------+------+
                           |
              +------------+------------+
              |            |            |
       +------v------+ +--v---+ +-----v-----+
       |  Invariants  | |  SMT  | |  Unrolling |
       |  Checker     | |Solver | |  Checker   |
       +------+------ +--+---+ +-----+------+
              |            |            |
              +------------+------------+
                           |
                    +------v------+
                    |  Verdict    |
                    |  (OK / FAIL) |
                    +------+------+
                           | OK
                    +------v------+
                    |  Code Gen    |
                    |  (WASM +     |
                    |   Native)    |
                    +-------------+
```

### 4.3 What the compiler proves and what it doesn't

**Automatically provable** (>95% of practical contracts):
- Arithmetic properties on integers and floats with bounded ranges.
- Structure invariants (sorted lists, valid ranges).
- Resource conservation (sum of balances, element count).
- Absence of common errors (division by zero, index out of bounds, overflow).
- Effect contract compliance (a pure function has no side effects).

**Requires human annotation** (<5% of contracts):
- Properties that depend on non-trivially provable mathematical theorems.
- Algorithmic complexity invariants (O(n log n) vs O(n^2)).
- Properties involving recursive loops with non-standard invariants.

For these cases, Sond provides **lemmas** that the human writes once and the compiler uses in subsequent verifications:

```sond
-- A lemma that the human writes and proves manually (or with AI assistance)
lemma suma_no_negativa(items: List<Money>)
    requires forall(i in 0..items.len()): items[i] >= 0
    ensures sum(items) >= 0
    -- This lemma is used in future verifications automatically

-- Now the compiler can use this lemma in other contracts
fn total_pedido(items: List<Money>) -> (total: Money)
    requires forall(i in 0..items.len()): items[i] >= 0
    ensures total >= 0  -- proved via suma_no_negativa
```

---

## 5. Type system, effects, and contracts

### 5.1 Concrete grammar (BNF extension)

```bnf
<program>       ::= <module>+

<module>        ::= <module_directive> <decl>+

<module_directive> ::= "modulo" <ident> ";"

<decl>          ::= <decl_fn>
                   | <decl_type>
                   | <decl_const>
                   | <decl_lemma>

<decl_fn>       ::= <vis>? "fn" <ident> "(" <params>? ")"
                       "->" <return_type>
                       <contracts>?
                       ("effects" "[" <effects> "]")?
                       "=" "impl" ":" <block>

<contracts>     ::= <preconditions>? <postconditions>?

<preconditions> ::= "requires" ":" <prop>+

<postconditions> ::= "ensures" ":" <prop>+

<prop>          ::= <prop_forall>
                   | <prop_exists>
                   | <prop_expr>

<prop_forall>   ::= "forall" "(" <binder> "in" <range> ")" ":" <prop_expr>
<prop_exists>   ::= "exists" "(" <binder> "in" <range> ")" ":" <prop_expr>

<binder>        ::= <ident>
<range>         ::= <expr> ".." <expr>

<type>          ::= <type_base>
                   | <type_compound>
                   | <type_dependent>
                   | <type_effect>

<type_base>     ::= "i8" | "i16" | "i32" | "i64"
                   | "u8" | "u16" | "u32" | "u64"
                   | "f32" | "f64"
                   | "bool" | "char" | "String"
                   | <ident>

<type_compound> ::= "List" "<" <type> ">"
                   | "Map" "<" <type> "," <type> ">"
                   | "Result" "<" <type> "," <type> ">"
                   | "Option" "<" <type> ">"
                   | "{" <field>+ "}"

<type_dependent> ::= <type_base> "where" <prop_expr>

<field>         ::= <ident> ":" <type> ("," <ident> ":" <type>)*

<type_effect>   ::= <type> ("effects" "[" <effects> "]")?

<effects>       ::= <ident> ("," <ident>)*

<decl_type>     ::= <vis>? "type" <ident> (<type_params>)?
                       "=" <type> <invariant>?

<invariant>     ::= "where" <prop_expr>

<phantom_type>  ::= "phantom" "type" <ident>

<decl_lemma>    ::= "lemma" <ident> "(" <params>? ")"
                       <contracts>
                       "=" <block>

<let>           ::= "let" ("mut")? <ident> (":" <type>)? "=" <expr>
<mut>           ::= <ident> "=" <expr>

<expr>          ::= <lit>
                   | <ident>
                   | <expr> <op> <expr>
                   | <expr> "." <ident>
                   | <expr> "(" <args>? ")"
                   | "old" "(" <expr> ")"
                   | "if" <expr> "{" <block> "}" ("else" "{" <block> "}")?
                   | <expr> "=>" <expr>   -- lambda
                   | "sum" "(" <expr> "," <lambda> ")"
                   | "forall" "(" <binder> "in" <range> ")" <expr>

<lambda>        ::= "fn" "(" <ident> ")" "=>" <expr>

<op>            ::= "+" | "-" | "*" | "/" | "%"
                   | "==" | "!=" | "<" | ">" | "<=" | ">="
                   | "and" | "or" | "not"
                   | "++"  -- list concatenation

<vis>           ::= "pub" | "priv"
<ident>         ::= [a-zA-Z_][a-zA-Z0-9_]*
<lit>           ::= <num> | <string> | "true" | "false" | "()" | "[]"
```

### 5.2 Dependent type system

```sond
-- Dependent types for bounded ranges
type Indice = u32 where self < MAX_ITEMS

-- The compiler verifies that every array access uses a valid Indice
fn obtener_item(items: List<Item>, idx: Indice) -> Item
    requires idx < items.len()
    = impl:
        items[idx]  -- safe: the compiler proved idx < items.len()

-- Dependent types for percentages
type Percent = f64 where self >= 0.0 and self <= 100.0

-- Dependent types for dates
type FechaNoFutura = Fecha where self <= fecha_actual()
```

### 5.3 Effect system

Effects are part of a function's type. The compiler verifies that declared effects match used effects, and that pure functions have no effects:

```sond
-- Effects declared and verified
effects:
    IO          -- general I/O
    Red         -- network access
    FS          -- filesystem access
    DB          -- database access
    Env         -- environment variable access
    Temporal    -- system clock access
    Crypto      -- cryptographic operations
    Random      -- random number generation

-- A function with multiple effects
fn procesar_pago(
    pago: Pago,
    _cap_red: CapRed,
    _cap_db: CapDB,
    _cap_crypto: CapCrypto
) -> Result<ConfirmacionPago, ErrorPago>
    effects: [Red, DB, Crypto]
    = impl:
        let hash = sha256(pago.datos)    -- OK: has CapCrypto
        let registro = guardar_en_db(pago) -- OK: has CapDB
        let confirmacion = enviar_a_pasarela(pago) -- OK: has CapRed
        Ok(Confirmacion { hash, registro, confirmacion })
```

### 5.4 Quantified properties

```sond
-- Universal: for every i in a range, a property holds
fn ordenada(lista: List<i32>) -> (resultado: bool)
    ensures resultado == forall(i in 0..lista.len()-1): lista[i] <= lista[i+1]

-- Existential: at least one element satisfies
fn contiene(lista: List<i32>, valor: i32) -> (resultado: bool)
    ensures resultado == exists(i in 0..lista.len()): lista[i] == valor

-- Combination of quantifiers
fn todos_positivos_o_album_vacio(album: Album) -> (resultado: bool)
    ensures:
        resultado == (
            album.fotos.len() == 0
            or forall(i in 0..album.fotos.len()): album.fotos[i].brillo > 0
        )
```

---

## 6. Capability and security model

### 6.1 Capabilities as phantom types

Capabilities in Sond are **phantom types**: they don't exist at runtime, only in the type system. This means:

- No execution overhead.
- Capabilities are "passed" as phantom parameters.
- The compiler tracks them statically.

```sond
-- Capability definitions
phantom type CapRed
phantom type CapFS
phantom type CapDB
phantom type CapEnv
phantom type CapCrypto
phantom type CapTemporal
phantom type CapRandom
phantom type CapUI

-- Capabilities are passed as parameters starting with _
fn funcion_segura(
    datos: DatosEntrada,
    _cap_fs: CapFS,     -- can read files
    _cap_env: CapEnv    -- can read config
) -> Result<DatosSalida, Error>
    effects: [FS, Env]
    = impl:
        let config = leer_env("API_KEY")     -- OK: has CapEnv
        let reglas = leer_archivo("rules.txt") -- OK: has CapFS
        Ok(procesar(datos, config, reglas))
```

### 6.2 Capability composition

```sond
-- A component that composes capabilities from others
fn componente_completo(
    datos: DatosUsuario,
    _cap_db: CapDB,
    _cap_red: CapRed,
    _cap_crypto: CapCrypto
) -> Result<Respuesta, Error>
    effects: [DB, Red, Crypto]
    = impl:
        let hash = sha256(datos)             -- OK: has CapCrypto
        let registro = buscar_en_db(hash)    -- OK: has CapDB
        let verificacion = verificar_con_api(registro) -- OK: has CapRed
        Ok(construir_respuesta(registro, verificacion))

-- A restricted component that can only do one thing
fn solo_lectura(
    id: IdRegistro,
    _cap_db: CapDB  -- DB only, nothing else
) -> Result<Registro, Error>
    effects: [DB]
    = impl:
        let registro = buscar_en_db(id)  -- OK
        Ok(registro)
        -- Cannot do network, crypto, FS, anything else
```

### 6.3 Resolving the AI code trust problem

The capability model solves the fundamental problem:

```sond
-- AI-generated code: the compiler guarantees it's safe
fn procesar_datos_sensibles(
    datos: DatosPersonales,
    _cap_db: CapDB          -- can store in DB
    -- NO CapRed: CANNOT send data to the Internet
    -- NO CapFS: CANNOT write files
    -- NO CapUI: CANNOT display data on screen
) -> Result<(), Error>
    effects: [DB]
    = impl:
        -- The AI implements this; the compiler guarantees that
        -- data never leaves the DB, never gets written to disk,
        -- never shows up in UI
        let hash = hash_datos(datos)
        guardar_en_db(hash, datos)
        Ok(())
        -- If the AI tried something malicious, the compiler rejects it
```

---

## 7. The three full-stack boundaries and the deployment model

### 7.1 Network boundary (client <-> server)

The `@server`/`@client` location is a **type**, not a decorative annotation. The compiler generates communication infrastructure (RPC/serialization) with end-to-end security:

```sond
-- A module that lives on the server
modulo api_usuarios {
    @server
    fn obtener_usuario(id: IdUsuario) -> Result<Usuario, ErrorUsuario>
        effects: [DB]
        -- Only accessible from the server
        -- The compiler generates the HTTP/JSON endpoint automatically
        = impl:
            buscar_en_db(id)

    -- A module that lives on the client
    @client
    fn mostrar_usuario(id: IdUsuario) -> UI<Usuario>
        effects: [Red]
        -- Only accessible from the browser
        -- The compiler generates the fetch call automatically
        = impl:
            let usuario = fetch_api("/usuarios/" + id.to_string())
            render_usuario(usuario)
}

-- The compiler generates:
-- 1. The REST endpoint on the server
-- 2. The HTTP client in the browser
-- 3. JSON serialization/deserialization
-- 4. Type validation on both sides
-- 5. Authentication/authorization (if declared)
```

### 7.2 State <-> UI boundary (reactivity)

Reactivity is a **language effect** with a dependency graph resolved at compile time:

```sond
-- A reactive component
@client
fn contador() -> UI<i32>
    reactive:
        let mutable contador = 0  -- reactive state

        -- The compiler builds the dependency graph
        -- and automatically updates the UI when it changes
        = impl:
            div(
                texto("Contador: " + contador.to_string()),
                boton("Incrementar", fn() => contador += 1)
            )
            -- When contador changes, only the affected
            -- text is re-rendered, not the entire component
```

### 7.3 Time boundary (sync <-> async)

Async functions are **algebraic effects** with handlers, without function coloring:

```sond
-- A function that can be sync or async depending on context
fn cargar_datos(id: IdDato) -> Result<Datos, Error>
    effects: [Temporal]
    -- The compiler generates sync OR async depending on the caller
    = impl:
        let datos = buscar_en_db(id)
        Ok(datos)

-- Sync caller (server with local DB)
fn procesar_sync(datos: Datos) -> Result<Procesado, Error>
    effects: [DB]
    = impl:
        let datos = cargar_datos(datos.id)  -- sync: local DB
        Ok(procesar(datos))

-- Async caller (browser with fetch)
@client
fn procesar_async(datos: Datos) -> UI<Procesado>
    effects: [Red]
    = impl:
        let datos = cargar_datos(datos.id)  -- async: fetch to API
        render(procesar(datos))
```

### 7.4 Single-artifact deployment

A `.sond` file compiles to:

1. **WASM** for the browser (`@client` components).
2. **Native binary** for the server (`@server` components).
3. **SQL schema** derived from types (types with invariants -> tables with `CHECK` constraints).

```sond
-- Derived types
type Usuario = {
    id: u64,
    nombre: String,
    email: String
} where self.email.contiene("@")

-- The compiler generates:
-- CREATE TABLE usuarios (
--     id BIGINT PRIMARY KEY,
--     nombre TEXT NOT NULL,
--     email TEXT NOT NULL CHECK (email LIKE '%@%')
-- );
```

---

## 8. The tower of sub-languages

Sond is not a monolithic language: it is a **tower of sub-languages of increasing power**, where each level is safe by construction and only needs to understand the previous level.

### 8.1 Level 1: Templates and styles (total but terminating)

Declarative sub-language for layouts, styles, and static data structures. It is **total** (always terminates) and **terminating** (cannot diverge). It is not Turing-complete: no loops or recursion.

```sond
-- Declarative template (level 1)
plantilla "FacturaLayout" = """
    <div class="factura">
        <h1>Invoice {{number}}</h1>
        <p>Date: {{date}}</p>
        <table>
            <row>
                <cell>Description</cell>
                <cell>Amount</cell>
            </row>
            {{#each line in lines}}
            <row>
                <cell>{{line.description}}</cell>
                <cell>{{line.amount}}</cell>
            </row>
            {{/each}}
        </table>
        <p>Total: {{total}}</p>
    </div>
"""

-- Declarative style (level 1)
estilo FacturaEstilo = """
    .factura { font-family: sans-serif; padding: 2rem; }
    .factura h1 { color: #333; }
    .factura table { width: 100%; border-collapse: collapse; }
    .factura .fila { border-bottom: 1px solid #ddd; }
"""
```

### 8.2 Level 2: Relational queries (no injection by construction)

Sub-language for database queries. Queries are **first-class types** that are statically verified:

```sond
-- Typed query (level 2)
query buscar_usuarios_activos: List<Usuario> = """
    SELECT * FROM usuarios
    WHERE activo = true
    AND fecha_registro > ?
    ORDER BY nombre
"""
-- Parameters are types, not strings
-- The compiler verifies that columns exist
-- and that types match

-- Query with joins (level 2)
query pedidos_con_cliente: List<{pedido: Pedido, cliente: Cliente}> = """
    SELECT p.*, c.*
    FROM pedidos p
    JOIN clientes c ON p.cliente_id = c.id
    WHERE p.estado = ?
"""
-- The compiler generates the return type automatically
-- and verifies that selected columns exist
```

### 8.3 Level 3: Business logic (full Turing-complete)

The full language level, with loops, recursion, effects, and complete verification. This is where most of the code lives:

```sond
-- Full business logic (level 3)
fn calcular_iva(base: Money, tipo: Percent) -> (vat_amount: Money)
    requires tipo in [4.0, 10.0, 21.0]  -- standard Spanish VAT
    ensures:
        vat_amount == round(base * tipo / 100.0, decimals=2)
        vat_amount >= 0.00
    = impl:
        round(base * tipo / 100.0, decimals=2)

-- Verification covers rounding and edge cases automatically
```

### 8.4 Why the gradation?

The gradation makes the system **safe**, not just **possible**:

- A designer can create templates (level 1) without understanding business logic.
- An analyst can write queries (level 2) without understanding type safety.
- A developer can write business logic (level 3) with the guarantee that lower levels are safe by construction.

The compiler verifies each level independently. A bug in level 3 cannot break level 1. This is **local reasoning** applied to the language's architecture.

---

## 9. Worked end-to-end examples

### 9.1 Example A: Fiscal function (base x type = quota, rounding)

Contract that corresponds to a cell in a Spanish tax agency form (VAT):

```sond
-- Calculates the VAT quota for an invoice line
fn compute_vat(
    tax_base: Money,
    vat_rate: Percent,
    intra_community: bool
) -> (vat_amount: Money)
    requires:
        -- The base is non-negative
        tax_base >= 0.00

        -- The rate is one of the legal Spanish rates
        vat_rate in [0.0, 4.0, 10.0, 21.0]

        -- An intra-community operation (reverse charge)
        -- ALWAYS goes with rate 0: the requires forces it, closing the gap
        -- where an intra-community with rate != 0 would have no
        -- postcondition restricting it.
        intra_community implies vat_rate == 0.0

    ensures:
        -- For intra-community operations, quota is 0
        intra_community implies vat_amount == 0.00

        -- For domestic operations, quota = base x rate / 100,
        -- rounded to 2 decimal places (accounting rounding: to the nearest
        -- cent, half-cent rounds up)
        (not intra_community)
            implies vat_amount == round_accounting(
                tax_base * vat_rate / 100.0,
                decimals=2
            )

        -- The quota is non-negative
        vat_amount >= 0.00

    = impl:
        if intra_community {
            0.00
        } else {
            round_accounting(
                tax_base * vat_rate / 100.0,
                decimals=2
            )
        }

-- Test cases that the compiler verifies automatically:
-- compute_vat(1000.00, 21.0, false) == 210.00
-- compute_vat(1000.00, 10.0, false) == 100.00
-- compute_vat(1000.00, 21.0, true) == 0.00
-- compute_vat(33.33, 21.0, false) == 7.00  (33.33 x 0.21 = 6.9993 -> 7.00)
-- compute_vat(0.00, 21.0, false) == 0.00
```

**What the compiler rejects:**
- `compute_vat(-100.00, 21.0, false)` -> error: `tax_base >= 0.00` not satisfied.
- `compute_vat(1000.00, 15.0, false)` -> error: `vat_rate in [0.0, 4.0, 10.0, 21.0]` not satisfied.
- If AI implements `tax_base * vat_rate / 100.0` without `round_accounting`, the compiler rejects: the postcondition requires rounding to 2 decimal places.

### 9.2 Example B: Bank transfer with conservation invariant

```sond
-- A type with a non-negative balance invariant
type Account = {
    id: IdAccount,
    holder: String,
    mutable balance: Money
} where self.balance >= 0.00

-- Transfer with balance conservation
fn transfer(
    source: &mut Account,
    dest: &mut Account,
    amount: Money,
) -> Result<(), ErrorTransferencia>
    requires:
        -- The amount is positive
        amount > 0.00

        -- The source has sufficient balance
        source.balance >= amount

        -- Not a self-transfer
        source.id != dest.id

        -- Both accounts are active (not frozen)
        source.estado == EstadoAccount::Activa
        dest.estado == EstadoAccount::Activa

    ensures:
        -- Conservation invariant: the sum of balances is maintained
        source.balance + dest.balance
            == old(source.balance) + old(dest.balance)

        -- The source lost exactly the amount
        source.balance == old(source.balance) - amount

        -- The destination gained exactly the amount
        dest.balance == old(dest.balance) + amount

        -- Both balances remain non-negative (type invariant)
        source.balance >= 0.00
        dest.balance >= 0.00

    = impl:
        -- The AI implements this; the compiler proves that
        -- all postconditions are satisfied
        source.balance = source.balance - amount
        dest.balance = dest.balance + amount
        Ok(())
```

**What the compiler rejects:**
- If AI forgets to update `dest.balance` -> error: `dest.balance == old(dest.balance) + amount` not satisfied.
- If AI adds instead of subtracting from the source -> error: `source.balance == old(source.balance) - amount` not satisfied.
- If AI makes two partial transfers without maintaining conservation -> error in global invariant.
- If implementation allows temporary negative balance -> error: `Account` type invariant violated.

### 9.3 Example C: Full-stack `@server` + `@client` component with reactivity

```sond
-- Shared type definitions
type Task = {
    id: u64,
    title: String,
    completed: bool
}

-- Server API
modulo api_tareas {
    @server
    fn listar_tareas() -> Result<List<Task>, Error>
        effects: [DB]
        = impl:
            buscar_todas_las_tareas()

    @server
    fn crear_tarea(title: String) -> Result<Task, Error>
        requires title.len() > 0 and title.len() <= 200
        effects: [DB]
        = impl:
            let tarea = Task {
                id: generar_id(),
                title: title,
                completed: false
            }
            guardar_en_db(tarea)
            Ok(tarea)

    @server
    fn toggle_tarea(id: u64) -> Result<Task, Error>
        effects: [DB]
        = impl:
            let mut tarea = buscar_por_id(id)?
            tarea.completed = not tarea.completed
            actualizar_en_db(tarea)
            Ok(tarea)
}

-- Client UI
@client
fn app_tareas() -> UI<List<Task>>
    effects: [Red]  -- fetch_api does network I/O; the effect must be declared
    reactive:
        let mutable tareas: List<Task> = []
        let mutable nuevo_title: String = ""

        = impl:
            div(
                h1("My Tasks"),

                -- Input for new task
                input(
                    valor: nuevo_title,
                    on_cambio: fn(v) => nuevo_title = v
                ),
                boton(
                    "Add",
                    fn() => {
                        if nuevo_title.len() > 0 {
                            -- Call to the server API
                            let tarea = fetch_api("/tasks", POST, nuevo_title)
                            tareas = tareas ++ [tarea]
                            nuevo_title = ""
                        }
                    }
                ),

                -- Task list (reactive: updates when `tareas` changes)
                lista(
                    tareas,
                    fn(tarea) => item_tarea(tarea, &reactive tareas)
                )
            )

-- The parent's reactive state is passed EXPLICITLY as a handle
-- (`&reactive`), never captured by closure from external scope:
-- this respects the "zero action at a distance" principle (3.3). The child
-- can only touch the state it was given, and its Red effect is visible.
fn item_tarea(tarea: Task, lista: &reactive List<Task>) -> UI<Task>
    effects: [Red]
    = impl:
        div(
            texto(tarea.title),
            boton(
                if tarea.completed { "Uncheck" } else { "Complete" },
                fn() => {
                    let actualizada = fetch_api("/tareas/" + tarea.id, PUT)
                    -- Update the task in the received reactive list
                    lista = lista.map(
                        fn(t) => if t.id == actualizada.id { actualizada } else { t }
                    )
                }
            )
        )
```

**What the compiler verifies:**
- That `api_tareas` only runs on `@server` (DB effects verified).
- That `app_tareas` only runs on `@client` (Red effects verified).
- That JSON serialization of `Task` is correct on both sides.
- That the reactive component only re-renders when `tareas` or `nuevo_title` change (dependency graph at compile time).

**What the compiler rejects:**
- If `app_tareas` tries to access DB directly -> error: undeclared DB effect.
- If `api_tareas` tries to render UI -> error: UI effect not available on `@server`.
- If `toggle_tarea` doesn't return the updated task -> error: postcondition not satisfied.

---

## 10. Honest comparison with the state of the art

| Language  | Contracts | Type/Effects | Verification | Locality | Capabilities | AI Ergonomics | Main limitation |
|-----------|-----------|--------------|--------------|----------|--------------|---------------|---------------------|
| **Dafny** | Yes (pre/post) | Medium | Total + Sound | Yes | No | Low | No effects, no capabilities, loops hard to verify |
| **F***   | Yes (refinement) | High | Total + Sound | Yes | Partial | Medium | Brutal learning curve, no ecosystem |
| **SPARK/Ada** | Yes (annotations) | High | Total + Sound | Yes | Partial | Low | Verbose, closed ecosystem, no explicit capabilities |
| **Roc**   | Yes (effects) | High | Partial | Yes | No | High | No formal verification, effects but no contracts |
| **Austral** | Yes (linear) | High | Total + Sound | Yes | Yes | Medium | Experimental, no ecosystem, no AI loop |
| **Unison** | No | Medium | No | Partial | No | High | No verification, content-addressed but no contracts |
| **Elm**   | Partial (types) | Medium | Partial | Yes | No | High | No formal contracts, no capabilities |
| **Rust**  | No (traits) | High | Partial (borrow) | Yes | Yes (ownership) | Medium | No formal verification, no contracts |
| **Ur/Web** | Partial | High | Total (partial) | Yes | Partial | Low | Abandoned, no ecosystem |
| **Links** | Yes (types) | High | Total | Yes | No | Medium | Experimental, no ecosystem |
| **Sond**  | Yes (contracts) | High (dependent) | Total + Sound | Yes | Yes (phantom) | **High** (AI loop) | New, no ecosystem, bootstrap pending |

### 10.1 Key differentiators of Sond

1. **AI-compiler loop**: no other language is designed for a verification loop where AI proposes and the compiler proves. Other languages assume humans write everything.
2. **Phantom capabilities**: Rust has ownership (good memory safety), but no capabilities for I/O, networking, or arbitrary effects. Sond has a general capability model that covers everything.
3. **Algebraic effects as types**: effects are part of the function's type, not decorative annotations. This enables compile-time effect verification.
4. **Tower of sub-languages**: the gradation allows different roles (designer, analyst, developer) to work at different levels with independent safety guarantees.

### 10.2 What Sond does NOT do better

- **Learning curve**: Sond is more complex than Elm or Rust for a human writing everything. The advantage is for AI-driven workflows.
- **Ecosystem**: it doesn't exist yet. Rust has crates.io, Python has PyPI. Sond has nothing.
- **Bootstrapping**: the compiler doesn't exist. It needs to be written in another language first.
- **IDE/tooling**: no VS Code extension, no LSP, no formatter. All pending.

---

## 11. Open questions / unresolved

1. **Decidability of verification**: for contracts with universal quantifiers over infinite ranges (e.g., `forall(n in Naturales): ...`), the verifier cannot terminate. Sond bounds ranges to `usize`, but this excludes properties over arbitrarily large integers. **Pending**: define a decidable subset of properties over `BigInt` that the verifier can handle, or accept that `BigInt` requires manual lemmas.

2. **Annotation load vs. inference**: how many contracts must the human write? If AI can infer correct contracts (postconditions, invariants), the load drops but the principle that the human approves intentions is violated. **Pending**: design an "assisted inference" mode where AI proposes contracts and the human approves them, without the compiler accepting code without an approved contract.

3. **Interoperability with the unverified world**: 99% of existing software has no contracts. Sond needs to interoperate with C, Python, JavaScript, etc. libraries. But unverified code is a black box. **Pending**: define a "trust boundary" mechanism where external code is wrapped in manual contracts, and the compiler verifies the wrapper satisfies its contracts even if the internal code is not verified.

4. **Bootstrapping the compiler**: Sond needs a compiler, but the compiler must be written in some language. If written in Rust/C, it's not verified. If written in Sond itself, there's a chicken-and-egg problem. **Pending**: decide whether the compiler is written in Rust (unverified but functional) and progressively migrated to Sond, or written in an existing formal language (Coq/Lean) and the compiler is generated via extraction.

5. **Verifier performance**: the SMT solver (Z3/CVC5) scales well for simple properties, but may take minutes or hours for complex contracts with many quantifiers. **Pending**: design an "incremental verification" system where only parts affected by a change are re-verified, not the entire module. Also: define time limits for the solver and degrade gracefully (mark as "pending proof" instead of failing).

6. **Concurrency support**: Sond has effects (Temporal, Random), but no defined concurrency model. **Pending**: decide between actors (like Erlang), CSP (like Go), or algebraic effects with concurrency handlers. The capability model could extend to `CapConcurrency` with per-thread ownership rules.

7. **Mutability and performance**: default immutability is safe but may be slow for large structures (full copies). **Pending**: design a "verified internal mutability" system where the compiler allows in-place mutation when it can prove there are no live aliases (similar to Rust's borrow checker, but integrated with the contract verifier).

8. **Macros and meta-programming**: Sond has no defined macros. For templates and repetitive code generation, meta-programming mechanisms are needed. **Pending**: decide between hygienic macros (like Rust), typed templates (like C++), or AI code generation with verification of the generated result.

9. **Package ecosystem and dependencies**: without a package manager, Sond cannot compete. **Pending**: design a package protocol where each package includes its contracts and the compiler verifies that dependencies satisfy their interfaces. Problem: unverified dependencies break the trust chain.

10. **Testing vs. verification**: Sond prioritizes formal verification, but testing remains useful for properties the verifier cannot prove (e.g., "the UI looks good"). **Pending**: define the relationship between tests and contracts: do tests replace contracts? complement contracts? are they automatically generated from contracts?

---

## 12. Glossary

| Term | Definition |
|------|------------|
| **Capability** | Phantom type that grants a right to access a resource (network, disk, DB). Without a capability, code is physically incapable of accessing the resource. |
| **Contract** | Set of preconditions (requires), postconditions (ensures), and invariants that a function must satisfy. The compiler verifies the implementation satisfies the contract. |
| **Totality** | Property of a verifier that it always terminates and always produces a verdict (OK or FAIL). |
| **Soundness** | Property of a verifier that if it says "correct", it is. Zero false negatives. |
| **False positive** | The verifier rejects code that is actually correct. Inconvenient but not catastrophic. |
| **False negative** | The verifier accepts code that is actually incorrect. Catastrophic: a production bug. Sond prohibits them. |
| **Effect** | Side capability of a function (I/O, network, DB, etc.) declared in its type and statically verified. |
| **Invariant** | Property that remains true at all times during a function's execution or a type's lifetime. |
| **Lemma** | Auxiliary theorem that the human writes and proves manually, and that the verifier uses in future verifications. |
| **Phantom type** | Type that has no runtime representation; only exists in the type system to track static properties. |
| **Bounded unrolling** | Verification technique where recursions and loops are expanded a finite number of times to verify properties. |
| **SMT solver** | Tool (Z3, CVC5) that solves satisfiability problems for theories (arithmetic, arrays, strings). Used by Sond's verifier. |
| **Trust boundary** | Limit between verified code (Sond) and unverified code (external). The manual wrapper creates the boundary. |
| **AI-compiler loop** | Loop where AI proposes an implementation, the compiler verifies it, and if it fails, AI fixes it. Repeats until verified. |

---

## 13. Document state

This RFC is in **Draft** state. The open questions in 11 are unresolved and require community discussion. The syntax shown is provisional; the final grammar will be defined in RFC-0001.

The name **Sond** reflects the language's nature: a probe is an instrument for measurement and verification.

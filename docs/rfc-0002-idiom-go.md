# RFC-0002: Go-flavored Sond Syntax and Idiom

| Field       | Value                                       |
|-------------|---------------------------------------------|
| RFC         | 0002                                        |
| State       | Draft                                       |
| Date        | 2026-07-09                                  |
| Authors     | JMV + agents                               |
| Reviews     | Surface syntax of RFC-0000/0001             |
| Resolves    | Open Question #9 (RFC-0001), Open Question #6 (RFC-0000) |

---

## 1. Metadata and scope

This RFC defines the **surface syntax revision** of the Sond language so that its *look & feel* approximates **Go**, fulfilling a firm design preference of the author (JMV). The document **does not touch semantics**: the 4 pillars of RFC-0000 (spec as primary artifact, compiler-as-oracle, local reasoning, capabilities), contracts, effects, and the tower of sub-languages remain intact. Only *how it's written* changes.

Where Go flavor clashes with verifiability, **verifiability wins**. But this almost never happens: what's good about Go (simplicity, explicitness, minimalism) is exactly what verification needs.

This RFC resolves **Open Question #9** of RFC-0001 (statement termination: implicit `;` insertion rule) and **Open Question #6** of RFC-0000 (concurrency model).

---

## 2. Motivation: why Go as idiom reference

### 2.1 Ultra-fast compilation = the oracle loop

Pillar 2 of RFC-0000 (compiler as tight-loop oracle) **lives** on compilation speed. The loop `AI proposes -> compiler verifies -> AI fixes -> repeat` only works if the verdict arrives in milliseconds. Go proved that sub-second compilation is not a luxury: it is a workflow enabler. Sond adopts the same philosophy: `sondc` must be fast enough that the human never waits.

### 2.2 `gofmt` = semantic diffs in the AI era

If there is **one way** to format code, every AI-generated diff is **semantic** (logic changes, not style noise). Without `gofmt`, an AI diff can be 80% reformatting and 20% real change, forcing the human to untangle what actually changed. With mandatory `sondfmt`, the diff is always pure: what changes is what matters.

### 2.3 Minimalism = less surface to verify

Go has ~25 keywords. Less syntactic surface means fewer cases the parser, verifier, and human have to understand. Go's minimalism is, for Sond, a **safety property**, not just a taste.

### 2.4 What from Go is NOT copied

Sond diverges from Go right where Go hurts and those points clash with verifiability:

| From Go that is NOT copied | Why | What replaces it |
|----------------------------|-----|------------------|
| `nil` | Causes null-deref, the most common bug in history | `Option<T>` -- the compiler requires handling the empty case |
| No *sum types* or exhaustiveness | `switch` without `default` can miss a case | `enum` + verified exhaustive `match` |
| Default mutability | Mutable shared state = data races | Immutability by default (Pillar 3) |
| `interface{}` / `any` | Untyped hatch, breaks local reasoning | Typed structural interfaces |
| No contracts or capabilities | Programmer can write any bug | Verified contracts + capability sandbox |

> Summary: Sond takes Go's **ergonomics, speed, and sobriety**, and layers
> contracts, capabilities, and effects on top. It's "Go, but the compiler proves
> your function fulfills what it promises and can't call home behind your back".

---

## 3. Surface changes

Each change is presented as: what it was in RFC-0000/0001 -> what it becomes, with before/after examples.

### 3.1 The canonical function form (key design decision)

In RFC-0000/0001 a function was written with `= impl:` + significant indentation, and in an intermediate draft with `= impl { ... }` nested inside outer braces. **Both are discarded.** The canonical Go-flavored form (JMV decision, 2026-07-09) is:

- The signature is followed by **clauses** `requires { }`, `ensures { }` (optional), and
- the **body is the final `{ }` block**, introduced by the `effects [...]` clause.
- **No `= impl`**: it was a vestigial marker that clashed with brace syntax.
- `effects [...]` replaces the old `effects: [...]` (no colon, consistent with `requires {}`).

```sond
fn name(
    parameter: Type,
) -> ReturnType
requires {
    // preconditions (optional)
}
ensures {
    // postconditions (optional)
}
effects [Effect1, Effect2] {
    // the body is this block; effects [] if the function is pure
}
```

A pure function without effects uses `effects [] { ... }`. A function without contracts or effects can write the body directly as `{ ... }`.

### 3.2 Comments

| Before (RFC-0000/0001) | Now (RFC-0002) |
|------------------------|----------------|
| `-- single-line comment` | `// single-line comment` |
| *(didn't exist)* | `/* block comment */` |

**Justification**: `--` is Haskell/Ada syntax. `//` and `/* */` are the universal convention (C, C++, Java, Go, Rust, JS). A Go programmer recognizes `//` instantly. Block comments `/* */` are added because they're useful for disabling code blocks during contract development.

**Before**:
```sond
-- Calculates the VAT quota
fn compute_vat(base: Money, rate: Percent) -> Money
    -- The base must be non-negative
    requires: base >= 0.00
    = impl:
        ...
```

**Now**:
```sond
// Calculates the VAT quota
fn compute_vat(base: Money, rate: Percent) -> Money
requires {
    // The base must be non-negative
    base >= 0.00
}
effects [] {
    ...
}
```

### 3.3 Blocks: `{ }` instead of indentation

| Before (RFC-0000/0001) | Now (RFC-0002) |
|------------------------|----------------|
| `requires:` + indentation | `requires { ... }` with braces |
| `ensures:` + indentation | `ensures { ... }` with braces |
| `effects: [...]` | `effects [...]` (no colon) |
| `reactive:` + indentation | `reactive { ... }` with braces |
| `if expr:` + indentation | `if expr { ... }` with braces |
| `else:` + indentation | `else { ... }` with braces |

**Justification**: Go uses braces `{ }` as block delimiters, not indentation. This eliminates *significant whitespace* ambiguity (Python, Haskell) and is more robust for automated editing tools and AI code generation: a block has an explicit start and end, without depending on how many spaces the generator inserted.

**Decision on return type**: `-> RetType` is **kept** instead of Go's position (`func f() RetType`). Reason: in Sond, functions frequently carry contracts and named returns `(quota: Money)`. The arrow `->` visually separates parameters from return and is more readable when contracts span multiple lines. Go can do without `->` because its functions are short and contractless; Sond cannot.

### 3.4 Unified `for`

Go has a single `for` keyword covering all iteration cases. Sond adopts the same and eliminates any other loop form.

```sond
// 1. for with condition (equivalent to while)
for condition {
    body
}

// 2. for with range (iterate over collections)
for value in collection {
    body
}

// 3. infinite for (no condition or range)
for {
    body
}

// 4. numeric range (inclusive-exclusive)
for i in 0..10 {
    process(i)
}
```

Idiomatic example:
```sond
for file in files {
    if file.ends_with(".log") {
        print(file.name)
    }
}

for {
    let event = wait_event()
    if event.is_exit() {
        break
    }
    process(event)
}
```

### 3.5 Statement termination: implicit `;` insertion (resolves Open Question #9 of RFC-0001)

The termination rule adopts Go's mechanism: `;` is **implicitly inserted** at end of line when the final token is of a certain type. Explicit `;` remains valid (and necessary in the three-clause `for`).

**Concrete rule**: the lexer inserts a `;` token (end of statement) automatically at end of line when the last token of the line is:

| End-of-line token | Insert `;`? |
|-------------------|-------------|
| Identifier (`IDENT`) | Yes |
| Numeric, string, boolean literal | Yes |
| `)` or `]` or `}` | Yes |
| Closing keywords: `true`, `false`, `self`, `break`, `continue`, `return` | Yes |
| Any other token (`+`, `==`, `fn`, `let`, `=`, `{`, `,`, `:`) | No |

**Implications**:

```sond
// Expression continuing on next line: group on one line or with parens
let total = items.map(fn(i) => i.price)

// Also valid with explicit parens
let total = (
    items
        .map(fn(i) => i.price)
)
```

**Explicit `;` is necessary in the three-clause `for`** (as in Go):
```sond
for i := 0; i < n; i++ {
    // explicit ; separate init; cond; post
}
```

**Note**: Sond does **not** adopt Go's convention that a line starting with `(`, `[`, or `{` doesn't receive an implicit `;`. In Sond, continuation is resolved with explicit parentheses or restructuring. This is more explicit and easier for tools to verify.

### 3.6 Naming and idiom

Sond adopts Go's naming conventions with one difference: **visibility stays `pub`/`priv`** (Go's "exported = Capitalized" is not adopted). Reason: explicit `pub`/`priv` is clearer and more verifiable, and less error-prone in AI generation than depending on capitalization. Go uses capitalization because it doesn't have `pub`/`priv`; Sond does.

| Convention | Go | Sond |
|------------|-----|------|
| Functions and variables | `camelCase` | `snake_case` |
| Types | `CamelCase` | `CamelCase` (same as Go) |
| Constants | `CamelCase`/`ALL_CAPS` | `CamelCase` or `UPPER_SNAKE` |
| Modules | `lowercase` | `snake_case` |
| Receiver | `(r *Receiver)` | `(self: Receiver)` explicit |
| Files | `snake_case.go` | `snake_case.snd` |

Sond uses `snake_case` for variables and functions (even though Go uses `camelCase`) because it's more readable in contracts with long names (`tax_base` vs `baseImponible`) and is the convention in contract languages (Dafny, F*, SPARK). Verification benefits from clarity, not brevity.

### 3.7 Errors as values

Go has no exceptions: errors are values returned explicitly. Sond shares this philosophy with `Result<T, E>` + the `?` propagation operator. The change is of **idiom**, not mechanism (it already existed in Sond).

```sond
// Go / Sond style: explicit errors, no hidden exceptions or panics
fn process(data: Data) -> Result<Money, ProcessingError> {
    let validated = validate(data)?      // propagates error on failure
    let calculated = calculate(validated)?
    Ok(round_accounting(calculated, decimals=2))
}
```

Error handling idiom with `match`:
```sond
fn handle_payment(payment: Payment) -> String {
    match process_payment(payment) {
        Ok(confirmation) => {
            "Payment processed: " + confirmation.id
        }
        Err(PaymentError::InsufficientBalance) => {
            "Error: insufficient balance"
        }
        Err(PaymentError::NetworkUnavailable) => {
            "Error: could not connect to gateway"
        }
        Err(other) => {
            "Unknown error: " + other.to_string()
        }
    }
}
```

The `where` clause in types is kept with `where` (no Go equivalent):
```sond
type Percent = f64
    where self >= 0.0 and self <= 100.0
```

---

## 4. CSP concurrency model with capabilities (resolves Open Question #6 of RFC-0000)

### 4.1 Design decisions

| Decision | Choice | Justification |
|----------|--------|---------------|
| Base model | **CSP** (goroutines + channels) | JMV's explicit preference; proven in Go; fits Pillar 3 because concurrency is modeled as communication, not shared state |
| Effects in signature | **Yes** (`effects [Concurrency]`) | The compiler knows which functions launch goroutines |
| Channels with capability | **Yes** (`chan<CapX> T`) | A channel can only be used if you have the appropriate capability (Pillar 4) |
| Data-race safety | **Immutability by default + channels** | No shared mutable state, no races; channels transfer ownership |

### 4.2 Goroutines

A goroutine is launched with `go`. The launched function declares its effects in the signature. The result is obtained through a channel.

```sond
// A function that can run as a goroutine
fn listen_events(ch: chan<- Event, cap_red: CapRed) -> Result<(), Error>
effects [Red, Concurrency] {
    for event in receive_from_socket(cap_red) {
        ch <- event
    }
    Ok(())
}

// Launch goroutines
fn start_server(cap_red: CapRed, cap_db: CapDB) -> Result<(), Error>
effects [Concurrency, Red, DB] {
    let ch = make(chan<Event>)
    go listen_events(ch, cap_red)
    go process_events(ch, cap_db)
    Ok(())
}
```

### 4.3 Channels

A channel transports messages between goroutines. Channels carry a **capability** so that only functions with the appropriate capability can send or receive.

```sond
chan T                    // bidirectional channel of type T
chan<- T                  // send-only channel (sender)
<-chan T                  // receive-only channel (receiver)

chan<CapRed> Message      // channel that can only be used with CapRed
chan<CapDB> Record        // channel that can only be used with CapDB
```

Construction:
```sond
let ch = make(chan<Event>(100))   // buffered (capacity 100)
let ch = make(chan<Event>)        // unbuffered (strict synchronization)
```

### 4.4 Send and receive

```sond
ch <- value            // sends value to channel
let value = <-ch       // receives from channel
let value, ok = <-ch   // receives with close indicator
```

### 4.5 `select` for multiplexing

```sond
select {
    case msg := <-ch_events:
        process_event(msg)
    case msg := <-ch_timer:
        process_timeout(msg)
    case <-ch_exit:
        return
    default:
        // no messages available
}
```

### 4.6 Channel closing

```sond
close(ch)  // closes the channel; receivers get the zero value
```

### 4.7 Data-race safety

Relies on three mechanisms:

1. **Immutability by default**: variables are immutable unless `let mut`. No shared mutable state.
2. **Channels for communication**: state is transferred through channels, not shared by reference.
3. **Capability on channels**: a `chan<CapX>` can only be used by functions that have `CapX`, guaranteeing that only authorized functions send/receive on a channel.

```sond
// Producer-consumer with capability
fn producer(ch: chan<- Order) -> Result<(), Error>
effects [Concurrency] {
    for order in orders {
        ch <- order
    }
    Ok(())
}

fn consumer(ch: <-chan Order, cap_db: CapDB) -> Result<(), Error>
effects [Concurrency, DB] {
    for order in ch {
        save_to_db(order)?
    }
    Ok(())
}

fn start_pipeline(cap_db: CapDB) -> Result<(), Error>
effects [Concurrency, DB] {
    let ch = make(chan<Order>)
    go producer(ch)
    go consumer(ch, cap_db)
    Ok(())
}
```

**What the compiler verifies:**
- That the `producer` goroutine has `effects [Concurrency]` declared.
- That the channel `ch` carries the required capability.
- That there is no shared mutable access without channels (no `&mut` crossing goroutine boundaries).
- That `close(ch)` is called from the sender, not from arbitrary receivers.

### 4.8 `WaitGroup` for synchronization

```sond
let wg = WaitGroup::new()

for i in 0..num_workers {
    wg.add(1)
    go fn() {
        defer wg.done()
        process(i)
    }()
}

wg.wait()  // blocks until all finish
```

> **Note**: `WaitGroup` is a standard library type that encapsulates an atomic counter; it is not a language primitive, but is included here because it is essential for the idiomatic CSP pattern.

---

## 5. The 3 examples from RFC-0000 section 9 rewritten in Go idiom

The `requires`/`ensures` contracts are kept **with braces** for consistency with Go-flavored block syntax, and the body is the block introduced by `effects [...]` (without `= impl`).

### 5.1 Example A: fiscal function (base x type = quota, round_accountinging)

Contract that corresponds to a cell in a Spanish tax agency form (VAT):

```sond
// Calculates the VAT quota for an invoice line
fn compute_vat(
    tax_base: Money,
    vat_rate: Percent,
    operacion_intracomunitaria: bool,
) -> (vat_amount: Money)
requires {
    // The base is non-negative
    tax_base >= 0.00

    // The rate is one of the legal Spanish rates
    vat_rate in [0.0, 4.0, 10.0, 21.0]

    // An intra-community operation (reverse charge)
    // ALWAYS goes with rate 0: the requires forces it, closing the gap
    // where an intra-community with rate != 0 would have no
    // postcondition restricting it.
    operacion_intracomunitaria implies vat_rate == 0.0
}
ensures {
    // For intra-community operations, the quota is 0
    operacion_intracomunitaria implies vat_amount == 0.00

    // For domestic operations, quota = base x rate / 100,
    // round_accountinged to 2 decimal places (accounting round_accountinging: to the nearest
    // cent, half-cent round_accountings up)
    (not operacion_intracomunitaria)
        implies vat_amount == round_accounting(
            tax_base * vat_rate / 100.0,
            decimals=2,
        )

    // The quota is non-negative
    vat_amount >= 0.00
}
effects [] {
    if operacion_intracomunitaria {
        0.00
    } else {
        round_accounting(
            tax_base * vat_rate / 100.0,
            decimals=2,
        )
    }
}

// Test cases that the compiler verifies automatically:
// compute_vat(1000.00, 21.0, false) == 210.00
// compute_vat(1000.00, 10.0, false) == 100.00
// compute_vat(1000.00, 21.0, true)  == 0.00
// compute_vat(33.33, 21.0, false)   == 7.00  (33.33 x 0.21 = 6.9993 -> 7.00)
// compute_vat(0.00, 21.0, false)    == 0.00
```

**What the compiler rejects:**
- `compute_vat(-100.00, 21.0, false)` -> error: `tax_base >= 0.00` not satisfied.
- `compute_vat(1000.00, 15.0, false)` -> error: `vat_rate in [0.0, 4.0, 10.0, 21.0]` not satisfied.
- If AI implements `tax_base * vat_rate / 100.0` without `round_accounting`, the compiler rejects: the postcondition requires round_accountinging to 2 decimal places.

### 5.2 Example B: bank transfer with conservation invariant

```sond
// A type with a non-negative balance invariant
type Account = {
    id: IdAccount,
    titular: String,
    mutable saldo: Money,
    state: EstadoAccount,
} where self.saldo >= 0.00

// Transfer with balance conservation
fn transferir(
    origen: &mut Account,
    destino: &mut Account,
    cantidad: Money,
) -> Result<(), ErrorTransferencia>
requires {
    // The amount is positive
    cantidad > 0.00

    // The source has sufficient balance
    origen.saldo >= cantidad

    // Not a self-transfer
    origen.id != destino.id

    // Both accounts are active (not frozen)
    origen.state == EstadoAccount::Activa
    destino.state == EstadoAccount::Activa
}
ensures {
    // Conservation invariant: the sum of balances is maintained
    origen.saldo + destino.saldo
        == old(origen.saldo) + old(destino.saldo)

    // The source lost exactly the amount
    origen.saldo == old(origen.saldo) - cantidad

    // The destination gained exactly the amount
    destino.saldo == old(destino.saldo) + cantidad

    // Both balances remain non-negative (type invariant)
    origen.saldo >= 0.00
    destino.saldo >= 0.00
}
effects [] {
    // The AI implements this; the compiler proves that
    // all postconditions are satisfied
    origen.saldo = origen.saldo - cantidad
    destino.saldo = destino.saldo + cantidad
    Ok(())
}
```

**What the compiler rejects:**
- If AI forgets to update `destino.saldo` -> error: `destino.saldo == old(destino.saldo) + cantidad` not satisfied.
- If AI adds instead of subtracting from the source -> error: `origen.saldo == old(origen.saldo) - cantidad` not satisfied.
- If AI makes two partial transfers without maintaining conservation -> error in global invariant.
- If implementation allows temporary negative balance -> error: `Account` type invariant violated.

### 5.3 Example C: full-stack `@server` + `@client` component with reactivity

```sond
// Shared type definitions
type Task = {
    id: u64,
    title: String,
    completed: bool,
}

// Server API
modulo api_tareas {
    @server
    fn listar_tareas() -> Result<List<Task>, Error>
    effects [DB] {
        buscar_todas_las_tareas()
    }

    @server
    fn crear_tarea(title: String) -> Result<Task, Error>
    requires {
        title.len() > 0 and title.len() <= 200
    }
    effects [DB] {
        let task = Task {
            id: generar_id(),
            title: title,
            completed: false,
        }
        guardar_en_db(task)?
        Ok(task)
    }

    @server
    fn toggle_tarea(id: u64) -> Result<Task, Error>
    effects [DB] {
        let mut task = buscar_por_id(id)?
        task.completed = not task.completed
        actualizar_en_db(task)?
        Ok(task)
    }
}

// Client UI
@client
fn app_tareas() -> UI<List<Task>>
reactive {
    let mutable tareas: List<Task> = []
    let mutable nuevo_title: String = ""
}
effects [Red] {
    div(
        h1("My Tasks"),

        // Input for new task
        input(
            value: nuevo_title,
            on_cambio: fn(v) => nuevo_title = v,
        ),
        button(
            "Add",
            fn() {
                if nuevo_title.len() > 0 {
                    // Call to the server API
                    let task = fetch_api("/tareas", POST, nuevo_title)
                    tareas = tareas ++ [task]
                    nuevo_title = ""
                }
            },
        ),

        // Task list (reactive: updates when `tareas` changes)
        list(
            tareas,
            fn(task) => item_tarea(task, &reactive tareas),
        ),
    )
}

// The parent's reactive state is passed EXPLICITLY as a handle
// (`&reactive`), never captured by closure from external scope:
// this respects the "zero action at a distance" principle (Pillar 3). The child
// can only touch the state it was given, and its Red effect is visible.
fn item_tarea(task: Task, list: &reactive List<Task>) -> UI<Task>
effects [Red] {
    div(
        text(task.title),
        button(
            if task.completed { "Uncheck" } else { "Complete" },
            fn() {
                let actualizada = fetch_api("/tareas/" + task.id, PUT)
                // Update the task in the received reactive list
                list = list.map(
                    fn(t) => if t.id == actualizada.id { actualizada } else { t },
                )
            },
        ),
    )
}
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

## 6. Traceability table: RFC-0000/0001 -> RFC-0002

| # | Syntax element | RFC-0000/0001 | RFC-0002 | Justification |
|---|----------------|---------------|----------|---------------|
| 1 | Line comment | `-- text` | `// text` | Universal convention (Go, C, Rust) |
| 2 | Block comment | *(didn't exist)* | `/* text */` | Useful for disabling code blocks |
| 3 | Function body | `= impl:` + indentation | brace block `{ }` after `effects [...]`, no `= impl` | Body as pure brace-block, Go style |
| 4 | `requires` block | `requires:` + indentation | `requires { ... }` | Braces, no significant whitespace |
| 5 | `ensures` block | `ensures:` + indentation | `ensures { ... }` | Braces |
| 6 | Effects | `effects: [...]` | `effects [...]` | No colon, consistent with `requires {}` |
| 7 | `reactive` block | `reactive:` + indentation | `reactive { ... }` | Braces |
| 8 | Conditional | `if expr:` + indentation | `if expr { ... }` | Go syntax |
| 9 | Loop | *(not defined)* | `for ... { ... }` | Single `for` keyword, Go style |
| 10 | Statement termination | Optional (`;` or newline) | Implicit `;` insertion | Open Question #9 resolved, Go rule |
| 11 | Formatter | *(didn't exist)* | `sondfmt` mandatory | Semantic diffs in the AI era |
| 12 | Visibility | `pub`/`priv` | `pub`/`priv` (kept) | More explicit than Go's capitalization |
| 13 | Variable/function names | `snake_case` | `snake_case` (kept) | More readable in long contracts |
| 14 | Type names | `CamelCase` | `CamelCase` (kept) | Same as Go |
| 15 | Return type | `-> RetType` | `-> RetType` (kept) | More readable with contracts and named return |
| 16 | Concurrency | *(Open Question #6)* | goroutines (`go`) + channels (`chan T`) | Go's CSP model |
| 17 | Channel with capability | *(didn't exist)* | `chan<CapX> T` | Integration with Pillar 4 |
| 18 | Concurrency effects | *(didn't exist)* | `effects [Concurrency]` | Consistent with explicit effects |
| 19 | `select` | *(didn't exist)* | `select { case ... }` | Go-style channel multiplexing |
| 20 | Close channel | *(didn't exist)* | `close(ch)` | Go style |

---

## 7. `sondfmt`: canonical formatter rules

`sondfmt` is Sond's canonical formatter. All source code must pass through `sondfmt` before compilation. It is **deterministic**: the same input always produces the same output.

### 7.1 Why it's critical in the AI era

When AI generates or modifies code, it can introduce formatting variations (indentation, spaces, line breaks). Without a canonical formatter, every AI diff contains style noise the human must filter. With mandatory `sondfmt`: (1) diffs are semantic; (2) code review is faster; (3) AI generation is more predictable; (4) there are fewer merge conflicts.

### 7.2 Formatting rules

| # | Rule | Detail |
|---|------|--------|
| 1 | **Indentation** | Tabs, not spaces. Visual width of 8 columns. |
| 2 | **Opening braces** | Same line as the construction (`if x {`, `for ... {`, `effects [] {`) |
| 3 | **Closing braces** | Own line, at the parent block's indentation level |
| 4 | **Space after `//`** | Always: `// comment`, never `//comment` |
| 5 | **Space around_accounting operators** | Always: `a + b`, `x == y`, `not cond` |
| 6 | **No space around_accounting `.`** | Always: `x.len()`, `account.balance` |
| 7 | **Trailing comma in multi-line lists** | Always (as Go): `(a, b, c,)` |
| 8 | **Parameter separators** | Comma + newline: one parameter per line in contracted functions |
| 9 | **Maximum line** | 100 columns; expressions exceeding it continue on the next line |
| 10 | **Blank lines** | Single blank line between top-level declarations |
| 11 | **Inside blocks** | No blank lines, except to separate logical sections |
| 12 | **Import grouping** | Single `import { ... }` block sorted alphabetically |

### 7.3 Formatting example

Unformatted input:
```sond
fn compute(base:Money,rate:Percent)->(vat_amount:Money)requires{base>=0.00 rate in [0.0,4.0,10.0,21.0]}ensures{vat_amount>=0.00}effects[]{round_accounting(base*rate/100.0,decimals=2)}
```

Formatted output:
```sond
fn compute(
    base: Money,
    rate: Percent,
) -> (vat_amount: Money)
requires {
    base >= 0.00
    rate in [0.0, 4.0, 10.0, 21.0]
}
ensures {
    vat_amount >= 0.00
}
effects [] {
    round_accounting(base * rate / 100.0, decimals=2)
}
```

### 7.4 Compiler integration

`sondfmt` runs automatically as a pre-compilation step. If code is not formatted, the compiler rejects it:

```
error: code not formatted with sondfmt
  -> line 5: expression exceeds 100 columns
  -> suggestion: run `sondfmt -w file.snd`
```

The `-w` flag overwrites the file in-place (like `gofmt -w`). Without `-w`, it prints output to stdout.

---

## 8. Open questions

1. **`effects []` in pure functions**: the canonical form introduces the body with `effects [...]`. A pure function writes `effects [] { ... }`, which is explicit but somewhat verbose. **Pending**: decide whether a pure function without contracts can omit `effects []` and write the body directly as `{ ... }`, or whether `effects [...]` is always mandatory as the body introductor (for parser uniformity).

2. **File extension**: the go-and-scripting design note proposes `.snd` (script) vs `.sond` (module) or a single one. **Pending**: decide whether the simple `.snd` extension suffices or whether scripts and modules must be distinguished.

3. **Imports / module system**: this RFC does not define how types and functions from other modules are referenced; examples assume a flat scope. **Pending**: design the import system (Go-style `import "path"` or something more expressive with namespaces).

4. **Constants**: with Go idiom the convention would be `const NAME = value`. **Pending**: decide whether `const` is a top-level declaration, whether it supports contracts (`const MAX: u32 where self > 0`), and how it interacts with the verifier.

5. **Keyword arguments**: examples use `round(expr, decimals=2)`. Go doesn't have them. **Pending**: decide whether Sond adopts them (improves readability with many parameters) or replaces them with positional (preserves Go's minimalism).

6. **`match` as expression**: examples use it but it's not in the formal grammar of RFC-0001. **Pending**: add `match` with verified exhaustive pattern matching to the grammar.

7. **Lambdas**: Sond has `fn(x) => expr` and `fn() { block }`. With Go flavor it could simplify to `x => expr`. **Pending**: decide whether the short form is adopted or `fn(x) => ...` is kept.

8. **`defer`**: Go has `defer` for guaranteed cleanup. **Pending**: decide whether Sond adopts `defer` or uses an RAII/type-with-destructor pattern (which fits better with capabilities).

9. **Scripting mode with `--caps`**: the go-and-scripting design note proposes `#!/usr/bin/env sond --caps=fs`. **Pending**: define how `sondfmt` interacts with scripts (which may lack contracts) and how the compiler handles content-addressed script caching.

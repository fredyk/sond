# RFC-0001: Complete Formal Grammar of Sond

| Field       | Value                                  |
|-------------|----------------------------------------|
| RFC         | 0001                                   |
| State       | Draft                                  |
| Date        | 2026-07-09                             |
| Authors     | JMV + agents                          |
| Supersedes  | 5 of RFC-0000                          |

---

## 1. Introduction

This document defines the complete and reconciled formal grammar of the Sond language. It formalizes **every** construction that appears in the code examples of RFC-0000, closing the gap between the partial grammar of section 5 of that document and the actual set of constructions used.

The grammar of RFC-0000 section 5 was an incomplete BNF that did not cover: block modules, `@server`/`@client`, `reactive:`, `let mutable`, lambdas with blocks, `implies`, the `?` error propagation operator, method calls, record literals, `enum`/variants, `phantom type`, `lemma`, template/style/query sub-languages, or operator precedence. RFC-0001 resolves all these gaps.

---

## 2. Notation conventions

**EBNF** (Extended Backus-Naur Form) is used with the following conventions:

| Symbol | Meaning |
|--------|---------|
| `A ::= B` | `A` is defined as `B` (production) |
| `A \| B` | Alternative: `A` or `B` |
| `A?` | Optional (0 or 1 time) |
| `A*` | Repetition (0 or more times) |
| `A+` | Repetition (1 or more times) |
| `(A)` | Grouping |
| `"xyz"` | Literal terminal (keyword or symbol) |
| `IDENT` | Non-terminal defined in the lexical grammar |
| `-- text` | Comment (not part of the grammar) |
| `<UPPERCASE>` | Lexical category (token) |
| `{ }` | Literal delimited block in the language (not EBNF) |

Non-terminals are written in `snake_case`. Reserved keywords appear in double quotes. Comments (`--`) are ignored in syntactic analysis and omitted from productions. Lexical categories defined in section 3 (like `IDENT`, `LIT_INTEGER`, etc.) are referenced in lowercase in syntactic productions: `<ident>`, `<lit_integer>`, etc.

---

## 3. Lexical grammar

### 3.1 Characters

Sond uses Unicode (UTF-8). Identifiers, comments, and string literals support Unicode characters.

### 3.2 Comments

```ebnf
COMMENT ::= "--" LINE_END
```

Comments begin with `--` and extend to the end of the line. There are no block comments.

### 3.3 Reserved keywords

The following keywords are reserved and cannot be used as identifiers. Sond is case-sensitive for keywords:

```
-- Logic and control
and, or, not, implies, in, if, else, forall, exists

-- Functions and types
fn, type, enum, phantom, lemma, where, pub, priv

-- Modules and declarations
modulo, query, plantilla, estilo

-- Effects and reactivity
effects, reactive, mutable, let, old, requires, ensures

-- Effects
IO, Red, FS, DB, Env, Temporal, Crypto, Random, UI

-- Base types
i8, i16, i32, i64, u8, u16, u32, u64, f32, f64, bool, char, String

-- Compound types
List, Map, Result, Option

-- Literals
true, false

-- Propagation
impl
```

### 3.4 Identifiers

```ebnf
IDENT ::= IDENT_START IDENT_BODY*

IDENT_START ::= [a-zA-Z_] | <Unicode letter>      -- includes accents: á é í ó ú ñ ü, etc.
IDENT_BODY  ::= [a-zA-Z0-9_] | <Unicode letter>
```

Identifiers are case-sensitive. An identifier cannot be a reserved keyword.

### 3.5 Literals

```ebnf
-- Numeric
LIT_INTEGER ::= DIGIT+                          -- 42, 0, 1000
LIT_DECIMAL ::= DIGIT+ "." DIGIT+               -- 3.14, 0.00, 100.0

-- Strings
LIT_STRING ::= '"' STRING_CHARACTER* '"'        -- "hello"
LIT_STRING_TRIPLE ::= '"""' CHARACTER* '"""'    -- """multiline"""

-- Booleans
LIT_BOOL ::= "true" | "false"

-- Unit and empty
LIT_UNIT ::= "()"                               -- unit value
LIT_EMPTY ::= "[]"                              -- empty list

DIGIT ::= [0-9]
STRING_CHARACTER ::= escaping | [^"\\\n]
CHARACTER ::= [^"]                              -- inside triple-string
escaping ::= "\\" [n"\\]
```

### 3.6 Operators and delimiters

```ebnf
OP_ARITHMETIC ::= "+" | "-" | "*" | "/" | "%"
OP_COMPARISON ::= "==" | "!=" | "<" | ">" | "<=" | ">="
OP_LOGICAL ::= "and" | "or" | "not" | "implies"
OP_LIST ::= "++"
OP_ERROR ::= "?"
OP_MEMBER ::= "."
OP_VARIANT_ACCESS ::= "::"
DELIMITER ::= "(" | ")" | "[" | "]" | "{" | "}" | "," | ":" | ";" | "->"
```

---

## 4. Complete syntactic grammar

### 4.1 Program and modules

```ebnf
<program> ::= <module>+

<module> ::= <module_annotations>? "modulo" <ident> "{" <declarations> "}"

<module_annotations> ::= <location_annotation>*

-- Reconciliation with RFC-0000 section 5: RFC-0000 defines modules as
-- statements `modulo Ident;`, but ALL examples use the block form
-- `modulo Ident { ... }`. This RFC adopts the block form
-- as canonical. The `modulo Ident;` form is declared NOT SUPPORTED.
```

### 4.2 Visibility and annotations

```ebnf
<visibility> ::= "pub" | "priv"

<location_annotation> ::= "@server" | "@client"
```

Location annotations (`@server`, `@client`) apply to function declarations. The compiler verifies that the function is only invoked from the correct side of the network boundary.

### 4.3 Declarations

```ebnf
<declarations> ::= <declaration>+

<declaration> ::= <decl_fn>
                | <decl_type>
                | <decl_type_enum>
                | <decl_phantom_type>
                | <decl_lemma>
                | <decl_template>
                | <decl_style>
                | <decl_query>
```

### 4.4 Function declaration

```ebnf
<decl_fn> ::= <location_annotation>?
              <visibility>?
              "fn" <ident>
              "<" <type_params> ">"?             -- optional, generics
              "(" <parameters>? ")"
              "->" <return_type>
              <contracts>?
              <effects_block>?
              <reactive_block>?
              "=" "impl" ":" <expr_block>

<type_params> ::= <ident> ("," <ident>)*

<parameters> ::= <parameter> ("," <parameter>)*

<parameter> ::= <param_cap>
             | <param_ref_reactive>
             | <param_ref_mut>
             | <param_normal>

<param_normal> ::= <ident> ":" <type>
<param_cap> ::= "_" <ident> ":" <ident>           -- _cap_red: CapRed
<param_ref_mut> ::= "&mut" <ident> ":" <type>     -- &mut cuenta: Account
<param_ref_reactive> ::= "&reactive" <ident> ":" <type>  -- &reactive tareas: List<Task>

<return_type> ::= <type>
                 | "(" <ident> ":" <type> ")"      -- named return: (base: Money)
```

**Example** (from RFC-0000 section 9.1):
```sond
fn compute_vat(
    tax_base: Money,
    vat_rate: Percent,
    intra_community: bool,
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
        intra_community implies vat_rate == 0.0
    }
    ensures {
        // For intra-community operations, the quota is 0
        intra_community implies vat_amount == 0.00

        // For domestic operations, quota = base x rate / 100,
        // rounded to 2 decimal places (accounting rounding: to the nearest
        // cent, half-cent rounds up)
        (not intra_community)
            implies vat_amount == round_accounting(
                tax_base * vat_rate / 100.0,
                decimals=2,
            )

        // The quota is non-negative
        vat_amount >= 0.00
    }
    = impl:
        ...
```

### 4.5 Contracts

```ebnf
<contracts> ::= <preconditions>? <postconditions>?

<preconditions> ::= "requires" ":" <propositions>+
<postconditions> ::= "ensures" ":" <propositions>+

<propositions> ::= <proposition> (<proposition>)*

<proposition> ::= <prop_forall>
                | <prop_exists>
                | <prop_expr>

<prop_forall> ::= "forall" "(" <binder> "in" <range> ")" ":" <prop_expr>
<prop_exists> ::= "exists" "(" <binder> "in" <range> ")" ":" <prop_expr>

<binder> ::= <ident>
<range> ::= <expr> ".." <expr>

<prop_expr> ::= <expr>
```

`<prop_expr>` is synonymous with `<expr>`: any boolean expression can be used as a proposition in contracts. The compiler verifies that the expression type is `bool`.

Propositions are boolean expressions. Multiple propositions in a `requires:` or `ensures:` block are interpreted as conjunction (all must hold).

### 4.6 Effects block

```ebnf
<effects_block> ::= "effects" ":" "[" <effects_list> "]"

<effects_list> ::= <effect> ("," <effect>)*

<effect> ::= "IO" | "Red" | "FS" | "DB" | "Env"
           | "Temporal" | "Crypto" | "Random" | "UI"
```

The canonical list of effects is: `IO`, `Red`, `FS`, `DB`, `Env`, `Temporal`, `Crypto`, `Random`, `UI`. A function without an effects block is **pure**: it cannot call functions with side effects.

### 4.7 Reactive block

```ebnf
<reactive_block> ::= "reactive" ":" <reactive_statements>

<reactive_statements> ::= <reactive_statement>+

<reactive_statement> ::= "let" "mutable" <ident> (":" <type>)? "=" <expr>
                       | <statement>
```

The `reactive:` block declares reactive state. Variables declared with `let mutable` inside this block are observed by the compiler to build the UI dependency graph.

### 4.8 Type declaration (alias with invariant)

```ebnf
<decl_type> ::= <visibility>?
                "type" <ident>
                "<" <type_params> ">"?
                "=" <type>
                <invariant>?

<invariant> ::= "where" <prop_expr>
```

**Example** (from RFC-0000 section 3.1.2):
```sond
type Percent = f64
    where self >= 0.0 and self <= 100.0
```

**Example** (from RFC-0000 section 9.2):
```sond
type Account = {
    id: IdAccount,
    holder: String,
    mutable balance: Money
} where self.balance >= 0.00
```

### 4.9 Enum declaration

```ebnf
<decl_type_enum> ::= <visibility>?
                     "enum" <ident>
                     "{" <variants> "}"

<variants> ::= <variant> ("," <variant>)*

<variant> ::= <ident>
            | <ident> "(" <type> ("," <type>)* ")"
```

**Example** (inferred from `EstadoAccount::Active` in RFC-0000 section 9.2):
```sond
enum EstadoAccount {
    Active,
    Blocked,
    Closed
}
```

Variant access is done with `Tipo::Variante`. This construct did not exist in the grammar of RFC-0000 section 5; it is formalized here as a closed gap.

### 4.10 Phantom type (capabilities)

```ebnf
<decl_phantom_type> ::= "phantom" "type" <ident>
```

A `phantom type` declares a type with no runtime representation that only exists in the type system to track static properties (capabilities).

**Example** (from RFC-0000 section 6.1):
```sond
phantom type CapRed
phantom type CapFS
phantom type CapDB
phantom type CapEnv
phantom type CapCrypto
phantom type CapTemporal
phantom type CapRandom
phantom type CapUI
```

### 4.11 Lemma

```ebnf
<decl_lemma> ::= "lemma" <ident>
                 "(" <parameters>? ")"
                 <contracts>
                 "=" <expr_block>
```

A lemma is an auxiliary theorem that the human writes and proves, and that the compiler uses in future verifications.

**Example** (from RFC-0000 section 4):
```sond
lemma suma_no_negativa(items: List<Money>)
    requires forall(i in 0..items.len()): items[i] >= 0
    ensures sum(items) >= 0
    = impl:
        ...
```

### 4.12 Types

```ebnf
<type> ::= <type_base>
         | <type_compound>
         | <type_dependent>
         | <type_ref_mut>
         | <type_ref_reactive>

<type_base> ::= "i8" | "i16" | "i32" | "i64"
              | "u8" | "u16" | "u32" | "u64"
              | "f32" | "f64"
              | "bool" | "char" | "String"
              | <ident>                              -- Money, Percent, etc.

<type_compound> ::= <type_record>
                   | "List" "<" <type> ">"
                   | "Map" "<" <type> "," <type> ">"
                   | "Result" "<" <type> "," <type> ">"
                   | "Option" "<" <type> ">"
                   | "UI" "<" <type> ">"

<type_record> ::= "{" <field_record> ("," <field_record>)* "}"

<field_record> ::= "mutable"? <ident> ":" <type>

<type_dependent> ::= <type> "where" <prop_expr>

<type_ref_mut> ::= "&mut" <type>
<type_ref_reactive> ::= "&reactive" <type>
```

**Note on `UI<T>`**: The `UI<T>` type is the return type of user interface components. It behaves as a generic compound type.

### 4.13 Tower sub-languages

The tower sub-languages (templates, styles, queries) are **nested but separate** grammars. They are defined as top-level declarations within a module.

#### 4.13.1 Templates

```ebnf
<decl_template> ::= "plantilla" <ident> "=" <lit_string_triple>

-- Interpolation within template strings
<interpolation> ::= "{{" <expr> "}}"
<iteration> ::= "{{#each" <ident> "in" <ident> "}}" <template_content> "{{/each}}"
<template_content> ::= (TEMPLATE_TEXT | <interpolation> | <iteration>)*
```

**Example** (from RFC-0000 section 8.1):
```sond
plantilla "InvoiceLayout" = """
    <div class="invoice">
        <h1>Invoice {{number}}</h1>
        <p>Date: {{date}}</p>
        {{#each line in lines}}
        <row>
            <cell>{{line.description}}</cell>
            <cell>{{line.amount}}</cell>
        </row>
        {{/each}}
        <p>Total: {{total}}</p>
    </div>
"""
```

The template sub-language is **total-terminating**: it admits no recursion or arbitrary loops. `{{#each}}` iterates over a list known at definition time, with no possibility of divergence.

#### 4.13.2 Styles

```ebnf
<decl_style> ::= "estilo" <ident> "=" <lit_string_triple>
```

**Example** (from RFC-0000 section 8.1):
```sond
estilo "InvoiceStyle" = """
    .invoice { font-family: sans-serif; padding: 2rem; }
    .invoice h1 { color: #333; }
"""
```

Style content is valid CSS. The compiler does not analyze its semantics; that is delegated to the rendering engine.

#### 4.13.3 Queries

```ebnf
<decl_query> ::= "query" <ident> ":" <type> "=" <lit_string_triple>

-- Positional parameters in SQL: "?"
-- The compiler verifies parameter and column types
```

**Example** (from RFC-0000 section 8.2):
```sond
query buscar_usuarios_activos: List<User> = """
SELECT * FROM users
    WHERE active = true
    ORDER BY name
"""
```

Positional parameters `?` are filled in according to the order of arguments at the call site. The compiler verifies that argument types match the table column types.

### 4.14 Statements

```ebnf
<expr_block> ::= <statement>* <final_expr>?

<statement> ::= <let_statement>
              | <assignment_statement>
              | <expr_statement>

<let_statement> ::= "let" "mutable"? <ident> (":" <type>)? "=" <expr> <statement_end>

<assignment_statement> ::= <expr> "=" <expr> <statement_end>

<expr_statement> ::= <expr> <statement_end>

<statement_end> ::= ";" | NEWLINE    -- terminator: explicit `;` OR newline (Go style)

<final_expr> ::= <expr>                -- last expression without terminator = block return value
```

A statement ends with an explicit `;` **or** a newline (like Go: `;` is optional and implicitly inserted at end of line). A block may end with a final expression without a terminator that becomes its return value; if the last construction is a terminated statement, the return value is `()`. The RFC-0000 examples don't use `;` anywhere: they are terminated by newline. The exact rule for implicit `;` insertion is fixed in RFC-0002 (Go-flavored idiom) -- see Open Question #9.

### 4.15 Expressions

```ebnf
<expr> ::= <lit>
         | <ident>
         | <expr> <binary_op> <expr>                -- binary
         | <unary_op> <expr>                        -- unary
         | <expr> "." <ident>                       -- field access
         | <expr> "." <ident> "(" <args>? ")"       -- method call
         | <expr> "(" <args>? ")"                   -- function call
         | <expr> "?"                                -- error propagation
         | "old" "(" <expr> ")"                      -- reference to prior state
         | "if" <expr> <expr_block> ("else" <expr_block>)?  -- conditional expression
         | "fn" "(" <lambda_params>? ")" ("->" <type>)? "=>" (<expr> | <expr_block>)  -- lambda
         | <record_literal>                          -- record literal
         | <variant_access>                          -- enum variant access
         | <builtin> "(" <args> ")"                  -- built-in function
         | "(" <expr> ")"                            -- grouping
         | "forall" "(" <binder> "in" <range> ")" <prop_expr>  -- quantifier in expression
         | "exists" "(" <binder> "in" <range> ")" <prop_expr>  -- quantifier in expression

<args> ::= <expr> ("," <expr>)*
<lambda_params> ::= <ident> ("," <ident>)*

<record_literal> ::= <ident> "{" <record_field_init> ("," <record_field_init>)* "}"
<record_field_init> ::= <ident> ":" <expr>

<variant_access> ::= <ident> "::" <ident>       -- EstadoAccount::Active

<builtin> ::= "sum" | "round" | "round_accounting"

-- Literal aggregator (referenced in <expr>)
<lit> ::= <lit_integer> | <lit_decimal> | <lit_string> | <lit_string_triple>
        | <lit_bool> | <lit_unit> | <lit_empty> | <lit_list>

-- Non-empty list literal: [0.0, 4.0, 10.0, 21.0]  (operand of `in`, args, etc.)
<lit_list> ::= "[" <expr> ("," <expr>)* "]"
```

### 4.16 Operators

```ebnf
<binary_op> ::= <op_or>
              | <op_and>
              | <op_implies>
              | <op_in>
              | <op_comparison>
              | <op_addition>
              | <op_multiplication>

<op_or> ::= "or"
<op_and> ::= "and"
<op_implies> ::= "implies"
<op_in> ::= "in"
<op_comparison> ::= "==" | "!=" | "<" | ">" | "<=" | ">="
<op_addition> ::= "+" | "-" | "++"
<op_multiplication> ::= "*" | "/" | "%"
<unary_op> ::= "-" | "not"
```

**Precedence table** (from lowest to highest priority):

| Precedence | Operator | Associativity | Description |
|---|---|---|---|
| 1 (lowest) | `implies` | Right | Logical implication |
| 2 | `or` | Left | Logical disjunction |
| 3 | `and` | Left | Logical conjunction |
| 4 | `not` | Prefix | Logical negation |
| 5 | `in` | Left | Set/range membership |
| 6 | `==`, `!=`, `<`, `>`, `<=`, `>=` | Left | Comparison |
| 7 | `+`, `-`, `++` | Left | Addition, subtraction, concatenation |
| 8 | `*`, `/`, `%` | Left | Multiplication, division, modulo |
| 9 | `-` (unary) | Prefix | Arithmetic negation |
| 10 | `.` | Left | Field/method access |
| 11 | `?` | Suffix | Error propagation |
| 12 (highest) | `()`, `[]` | Prefix/suffix | Call, indexing |

**Notes on precedence:**

- `implies` has the lowest precedence so that `A and B implies C` parses as `(A and B) implies C`.
- `not` as a prefix has higher precedence than `and`/`or` so that `not A and B` parses as `(not A) and B`.
- `in` has higher precedence than comparisons so that `x in a..b and x > 0` parses as `(x in a..b) and (x > 0)`.
- `++` (list concatenation) competes with `+`/`-` in precedence; it associates left.

### 4.17 Error propagation

The `?` operator applies to an expression returning `Result<T, E>`. If the value is `Ok(v)`, it extracts `v`; if it is `Err(e)`, it returns `Err(e)` from the current context. It is only valid in functions that return `Result`.

**Example** (from RFC-0000 section 9.3):
```sond
let mut task = buscar_por_id(id)?    -- if it fails, returns Err(error)
```

### 4.18 `old()` in postconditions

```ebnf
-- old(<expr>) in contracts ensures:
-- References the value of <expr> BEFORE function execution
```

`old(expr)` is only valid within `ensures:` blocks. The compiler captures the expression's value before execution and uses it in postcondition verification.

---

## 5. Operator precedence and associativity

See complete table in section 4.16.

**Parsing example** (from RFC-0000 section 9.2):

```sond
-- Expression: origen.saldo + destino.saldo == old(origen.saldo) + old(destino.saldo)
-- Parsing:
--   1. origen.saldo           (precedence 10: field access)
--   2. old(origen.saldo)      (precedence 12: call)
--   3. old(origen.saldo) + old(destino.saldo)  (precedence 7: addition)
--   4. origen.saldo + destino.saldo  (precedence 7: addition)
--   5. ... == ...             (precedence 6: comparison)
```

---

## 6. The three tower sub-languages

### 6.1 Level 1: Templates and styles

**Templates**: declarative, total-terminating. Interpolation with `{{expr}}` and iteration with `{{#each x in y}} ... {{/each}}`. No recursion, no conditionals, no arbitrary loops. The compiler can evaluate them completely at compile time.

**Styles**: literal CSS between triple quotes. No type checking; delegated to the rendering engine.

### 6.2 Level 2: Queries

Typed SQL queries with positional parameters `?`. The compiler verifies that referenced columns exist in the schema derived from types, and that argument types match column types.

### 6.3 Level 3: Business logic

The full Turing-complete language level, with loops, recursion, effects, contracts, and complete verification.

---

## 7. Changes relative to RFC-0000 section 5

| # | Aspect | RFC-0000 section 5 | RFC-0001 | Justification |
|---|--------|---------------------|----------|---------------|
| 1 | Modules | Statement `modulo Ident;` | Block `modulo Ident { ... }` | All examples use block form; the statement form is inconsistent with actual usage. |
| 2 | `@server`/`@client` | Not defined | Function annotations | Construction used in 7.1 and 9.3; formalized as location annotation. |
| 3 | `reactive:` | Not defined | Declarative block in function | Used in 7.2 and 9.3; formalizes reactive state with `let mutable`. |
| 4 | `let mutable` | Only `let` and assignment | `let mutable` for reactive state | Distinguishes immutable from mutable binding. |
| 5 | Lambdas | `fn(x) => expr` | `fn(x) => expr` and `fn() => { block }` | 9.3 uses lambdas with blocks; both forms formalized. |
| 6 | `implies` | Not defined | Binary logical operator | Used in 3.1.1, 9.1; formalizes implication as `implies`. |
| 7 | `?` propagation | Not defined | Suffix operator | Used in 9.3 (`buscar_por_id(id)?`); formalizes error propagation. |
| 8 | Method calls | Not defined | `expr.ident(args)` | `.len()`, `.to_string()`, `.map()`, `.contains()` used in examples. |
| 9 | `enum` and variants | Not defined | `enum Ident { ... }` and `Tipo::Variante` | `EstadoAccount::Active` used in 9.2; formalizes enumerations. |
| 10 | Record literals | Not defined | `Tipo { field: value }` | Used in 9.3 (`Task { id: ..., title: ..., completed: false }`). |
| 11 | `phantom type` | Defined as `<phantom_type>` | Top-level declaration | Consolidates existing definition. |
| 12 | `lemma` | Defined as `<decl_lemma>` | Declaration with `= impl:` | Adds missing implementation block. |
| 13 | Sub-languages | Not defined | Templates, styles, queries | Formalizes the tower from section 8 of RFC-0000. |
| 14 | Precedence | Not defined | Complete table | Essential for unambiguous parsing. |
| 15 | `effects:` in function | Syntax `[E1, E2]` | `effects: [E1, E2]` | Consolidated with colon before the list. |
| 16 | Named return | Not defined | `(name: Type)` | Used in 3.1.1 (`(base: Money)`); formalizes named return. |
| 17 | Capability parameters | Not defined | `_cap_x: CapX` | Used in 3.4, 6.1; formalizes phantom capability parameters. |
| 18 | `&mut` and `&reactive` | Not defined | Reference types | `&mut Account` in 9.2, `&reactive List<Task>` in 9.3. |
| 19 | `not` as prefix | `not` as infix | Prefix unary | `not intra_community` in 9.1; formalized as prefix. |
| 20 | `in` in contracts | Not defined | Membership operator | `vat_rate in [0.0, 4.0, 10.0, 21.0]` in 9.1. |

---

## 8. Traceability table

This table lists each construction from the RFC-0000 examples and the RFC-0001 production that covers it. It demonstrates there are no gaps.

| # | RFC-0000 Construction | Location in RFC-0000 | RFC-0001 Production |
|---|------------------------|----------------------|---------------------|
| 1 | `modulo api_tareas { ... }` | 7.1, 9.3 | 4.1 `<module>` |
| 2 | `@server` on function | 7.1, 9.3 | 4.2 `<location_annotation>` |
| 3 | `@client` on function | 7.1, 7.2, 9.3 | 4.2 `<location_annotation>` |
| 4 | `fn ident(params) -> Return` | 3.1.1, 9.1, 9.2, 9.3 | 4.4 `<decl_fn>` |
| 5 | `(base: Money)` named return | 3.1.1, 9.1 | 4.4 `<return_type>` |
| 6 | `_cap_red: CapRed` capability param | 3.4, 6.1, 6.2 | 4.4 `<param_cap>` |
| 7 | `&mut Account` mutable ref param | 9.2 | 4.4 `<param_ref_mut>` |
| 8 | `&reactive List<Task>` reactive param | 9.3 | 4.4 `<param_ref_reactive>` |
| 9 | `requires:` with propositions | 3.1.1, 9.1, 9.2, 9.3 | 4.5 `<preconditions>` |
| 10 | `ensures:` with propositions | 3.1.1, 9.1, 9.2 | 4.5 `<postconditions>` |
| 11 | `forall(i in 0..n): prop` | 3.1.1, 5.4, 9.1 | 4.5 `<prop_forall>` |
| 12 | `exists(i in 0..n): prop` | 5.4 | 4.5 `<prop_exists>` |
| 13 | `A implies B` | 9.1 | 4.5, 4.16 `<op_implies>` |
| 14 | `A in [v1, v2, ...]` | 9.1 | 4.16 `<op_in>` |
| 15 | `old(expr)` | 3.1.3, 9.2 | 4.15, 4.18 |
| 16 | `effects: [IO]` | 3.3.3, 6.1 | 4.6 `<effects_block>` |
| 17 | `effects: [Red, DB, Crypto]` multiple | 5.3 | 4.6 `<effects_list>` |
| 18 | `= impl: { ... }` | 3.1.1, 9.1, 9.2, 9.3 | 4.4 |
| 19 | `reactive: let mutable x = ...` | 7.2, 9.3 | 4.7 `<reactive_block>` |
| 20 | `type Percent = f64 where ...` | 3.1.2, 5.2 | 4.8 `<decl_type>` |
| 21 | `type Account = { ... } where ...` | 9.2 | 4.8 `<decl_type>` |
| 22 | `phantom type CapRed` | 3.4, 6.1 | 4.10 `<decl_phantom_type>` |
| 23 | `lemma suma_no_negativa(...)` | 4 | 4.11 `<decl_lemma>` |
| 24 | `EstadoAccount::Active` | 9.2 | 4.9 `<decl_type_enum>`, 4.15 `<variant_access>` |
| 25 | `Task { id: ..., title: ... }` record literal | 9.3 | 4.15 `<record_literal>` |
| 26 | `item.cantidad` field access | 3.1.1 | 4.15 |
| 27 | `items.len()` method call | 3.1.1, 5.4 | 4.15 `<expr> "." <ident> "(" ")"` |
| 28 | `.to_string()` method | 9.3 | 4.15 |
| 29 | `.map(fn(t) => ...)` method with lambda | 9.3 | 4.15, 4.15 lambda |
| 30 | `.contains("@")` method | 7.4 | 4.15 |
| 31 | `buscar_por_id(id)?` error propagation | 9.3 | 4.15, 4.17 |
| 32 | `fn(x) => expr` expression lambda | 3.1.1, 9.3 | 4.15 |
| 33 | `fn() => { block }` block lambda | 9.3 | 4.15 |
| 34 | `if cond { expr } else { expr }` | 9.1, 9.3 | 4.15 |
| 35 | `sum(items, fn(i) => ...)` builtin | 3.1.1, 3.3.3 | 4.15 `<builtin>` |
| 36 | `round(expr, decimals=2)` | 3.1.1, 8.3 | 4.15 `<builtin>` |
| 37 | `round_accounting(expr, decimals=2)` | 9.1 | 4.15 `<builtin>` |
| 38 | `let x = expr` immutable binding | 3.3.1 | 4.14 `<let_statement>` |
| 39 | `let mut x = expr` mutable binding | 3.3.1 | 4.14 `<let_statement>` |
| 40 | `x = expr` assignment | 3.3.1 | 4.14 `<assignment_statement>` |
| 41 | `42`, `0.00`, `3.14` numeric literals | 3.1.1, 9.1 | 3.5 `<lit_integer>`, `<lit_decimal>` |
| 42 | `"text"` string literal | 9.3 | 3.5 `<lit_string>` |
| 43 | `"""multiline"""` triple string | 8.1 | 3.5 `<lit_string_triple>` |
| 44 | `true`, `false` | 9.2, 9.3 | 3.5 `<lit_bool>` |
| 45 | `()` unit | 3.1.3, 9.2 | 3.5 `<lit_unit>` |
| 46 | `[]` empty list | 9.3 | 3.5 `<lit_empty>` |
| 47 | `-- comment` | 3.1.1, 9.1 | 3.2 `<comment>` |
| 48 | `pub`, `priv` visibility | 3.4 | 4.2 `<visibility>` |
| 49 | `plantilla Ident = """..."""` | 8.1 | 4.13.1 `<decl_template>` |
| 50 | `{{expr}}` template interpolation | 8.1 | 4.13.1 `<interpolation>` |
| 51 | `{{#each x in y}} ... {{/each}}` | 8.1 | 4.13.1 `<iteration>` |
| 52 | `estilo Ident = """..."""` | 8.1 | 4.13.2 `<decl_style>` |
| 53 | `query Ident: Type = """SQL"""` | 8.2 | 4.13.3 `<decl_query>` |
| 54 | `?` positional parameter in query | 8.2 | 4.13.3 |
| 55 | `Result<T, E>` return type | 3.1.3, 9.2 | 4.12 `<type_compound>` |
| 56 | `Option<T>` | 4.12 | 4.12 `<type_compound>` |
| 57 | `List<T>` | 3.1.1, 5.4 | 4.12 `<type_compound>` |
| 58 | `Map<K, V>` | 4.12 | 4.12 `<type_compound>` |
| 59 | `UI<T>` | 7.2, 9.3 | 4.12 `<type_compound>` |
| 60 | `i32`, `u64`, `f64`, `bool`, `String` | 5.1, 9.3 | 4.12 `<type_base>` |
| 61 | `Money`, `Percent` ident type | 3.1.1, 9.1 | 4.12 `<type_base>` |
| 62 | `where self.x >= 0` invariant | 3.1.2, 5.2, 9.2 | 4.8 `<invariant>` |
| 63 | `T where prop` dependent type | 5.2 | 4.12 `<type_dependent>` |
| 64 | `&mut T` mutable reference | 9.2 | 4.12 `<type_ref_mut>` |
| 65 | `&reactive T` reactive reference | 9.3 | 4.12 `<type_ref_reactive>` |
| 66 | `{ field: T, mutable field: T }` record | 5.1, 9.2 | 4.12 `<type_record>` |
| 67 | `IO`, `Red`, `FS`, `DB`, `Env`, `Temporal`, `Crypto`, `Random`, `UI` effects | 5.3 | 4.6 `<effect>` |
| 68 | `+`, `-`, `*`, `/`, `%` arithmetic | 5.1 | 4.16 `<op_addition>`, `<op_multiplication>` |
| 69 | `==`, `!=`, `<`, `>`, `<=`, `>=` comparison | 5.1 | 4.16 `<op_comparison>` |
| 70 | `and`, `or`, `not` logical | 5.1 | 4.16 `<op_and>`, `<op_or>`, `<unary_op>` |
| 71 | `++` list concatenation | 5.1, 9.3 | 4.16 `<op_addition>` |
| 72 | `i8`--`i64`, `u8`--`u64`, `f32`, `f64` | 5.1 | 4.12 `<type_base>` |

---

## 9. Open questions

1. **`<` ambiguity in generics vs. comparison**: The expression `a < b > c` is ambiguous: is it `(a < b) > c` (chained comparison) or `a < b>c` (malformed generic)? **Proposal**: the parser uses left-side context -- if `<` appears after a type identifier or after `,` inside `<>`, it parses as generic; otherwise, as comparison. This is imperfect and requires limited backtracking. **Pending**: decide whether Sond requires explicit parentheses for generics in expressions (e.g., `f::<T>(args)` Rust style) or if the greedy parser with context is sufficient.

2. **Provisional `reactive` syntax**: The `reactive:` block with `let mutable` is a first approximation. It is unclear how deep reactivity (nested objects), conditional reactivity, or subscription cleanup are handled. **Pending**: design the complete static reactivity model before implementing the parser.

3. **`Money` as native or library type**: Examples use `Money` as if it were a base type, but it is not in the primitive type list. **Proposal**: `Money` is a standard library type with fixed-precision decimal semantics (e.g., 28 digits), not a native language type. The compiler treats it as a nominal type with overloaded operators. **Pending**: confirm whether `Money` should be native (with rounding verification in the compiler) or library-based (with verification via contracts).

4. **`sum()`, `round()`, `round_accounting()` -- builtins or library**: RFC-0000 uses them as language builtins. But they could be standard library functions with contracts. **Proposal**: they are compiler builtins because the verifier needs to know their internal semantics to prove postconditions that reference them (e.g., `sum(items) >= 0`). Library functions would not have this deep integration with the solver. **Pending**: define the complete set of builtins and their relationship to the standard library.

5. **Named parameters in function calls**: `round(expr, decimals=2)` uses a named parameter, but the `<arguments>` grammar only defines positional arguments. **Proposal**: extend `<arguments>` to support `<ident> "=" <expr>` (keyword arguments). **Pending**: decide whether Sond supports keyword arguments or whether `decimals=2` is syntactic sugar that desugars to positional.

6. **`not` as infix vs. prefix**: RFC-0000 section 5 defines `not` as a binary operator (infix). But examples use it as a prefix (`not intra_community`). **Decision in this RFC**: `not` is a **prefix unary**. If it were needed as infix (e.g., `a not b`), there is no use case in current examples. **Pending**: confirm that infix is not needed.

7. **Module inheritance / imports**: Examples do not show `import` or `use`. How types and functions from other modules are referenced is not defined. **Pending**: design the module and dependency system.

8. **Constants**: The RFC-0000 section 5 grammar included `<decl_const>` but no example uses it. **Pending**: decide whether `const` is a top-level declaration or replaced by `let` at top level.

9. **Statement termination (`;` vs. newline)**: The RFC-0000 examples don't use `;`; this RFC makes the terminator optional (`;` explicit or newline, Go style). But the **exact rule for implicit `;` insertion** (which tokens at end of line trigger the terminator, how an expression continuing on the next line is handled) is not defined and is whitespace-sensitive. **Pending**: fix the semicolon insertion rule in **RFC-0002 (Go-flavored idiom)**, using Go's rule as reference (terminator after identifier, literal, `)`, `]`, `}`, or closing keyword).

10. **Module parameters with `@server`/`@client`**: the `<module>` production accepts `<module_annotations>?` (location annotations at the entire module level), but examples only annotate individual functions. **Pending**: decide whether an entire module can be marked `@server`/`@client` (propagating to all its functions) or whether the annotation is always per-function.

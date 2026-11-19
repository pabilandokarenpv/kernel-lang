# RFC-002: Grammar & language specification (v1 subset)

**Status:** Implemented (v1 core); extended syntax spec exists but is not
yet implemented in checker/interpreter/codegen (see ARD-001)

## Summary

Kernel reads like Python (indentation-based, no braces/semicolons) but
compiles like a systems language (ownership model, `unsafe` escape hatch,
freestanding binary output). This RFC specifies exactly what v1 parses,
checks, and executes.

## Example program

```kn
fn add(a: int, b: int) -> int:
    return a + b

fn main():
    let x = 5
    let y = 10
    print(add(x, y))
```

## Grammar surface (v1 implemented)

```mermaid
flowchart LR
    PROG["Program"] --> FN["Function\nfn name(params) -> type:"]
    PROG --> STRUCT["Struct"]
    FN --> STMT["Statement"]
    STMT --> LET["let (move-by-default binding)"]
    STMT --> REASSIGN["reassignment"]
    STMT --> IF["if / elif / else"]
    STMT --> WHILE["while"]
    STMT --> FORRANGE["for v in lo..hi"]
    STMT --> UNSAFE["unsafe block\n(raw memory: MMIO, ports, pointers)"]
    STMT --> PRINT["print(...)"]
    STMT --> RETURN["return"]
    STMT --> EXPR["expression: + - * % comparisons"]

    style UNSAFE fill:#4a1a1a,stroke:#822,color:#fff
```

## Ownership semantics

- Values **move by default** on assignment/pass — matches Rust's default,
  but with no lifetime annotations to write.
- `&` borrows a value without moving it.
- The checker (`checker.rs`) rejects use-after-move at compile time — see
  `examples/bad_use_after_move.kn` for the verified-rejected case.
- `unsafe { ... }` is the only way to perform raw memory access (MMIO,
  ports, raw pointers) — explicit and visually marked, never silently
  available outside the block. See `examples/bad_unsafe_outside_block.kn`
  for the verified-rejected case.

## Types (v1)

`int` is the only primitive type implemented in v1. Strings, floats, and
compound generic types are **not** implemented in the interpreter or
codegen yet, even though the broader syntax spec already anticipates
generics/traits/enums/pattern-matching/closures for corpus-generator
variety (see ARD-001). Attempting to use them is a parse/check error, not
a silent miscompile.

## Two run modes

| Command | Path | Purpose |
|---|---|---|
| `kernel run file.kn` | interpreter.rs | Dev-mode, Python-speed iteration |
| `kernel build file.kn out.bin` | codegen_c.rs → gcc/as/ld | Optimized, freestanding compiled binary |

## Why this grammar shape

The indentation-based syntax with an explicit ownership model and a
separate `unsafe` escape hatch is the core differentiator claim: "Rust's
safety story without Rust's borrow-checker tax." The grammar is
deliberately narrow in v1 (see ARD-001) specifically so this claim can be
fully verified (lex→parse→check→interpret→codegen→boot, cross-checked)
rather than partially true across a larger surface area.

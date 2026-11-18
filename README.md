# Kernel (.kn)

A systems programming language that reads like Python, but is memory-safe
enough to write a kernel in — Rust's safety story without Rust's borrow-checker
tax.

```kn
fn add(a: int, b: int) -> int:
    return a + b

fn main():
    let x = 5
    let y = 10
    print(add(x, y))
```

## Design pillars

- **Indentation-based syntax** — no braces, no semicolons. Kernel is the only
  systems-level language that reads clean.
- **Ownership model, safe by default** — values move by default, `&` borrows.
  Compile-time memory safety without lifetime annotations to fight.
- **`unsafe` escape hatch** — raw memory access (MMIO, ports, pointers) is
  explicit and clearly marked, never silently available.
- **Dev-mode interpreter + compiled release builds** — `kernel run` for
  Python-speed iteration while writing, `kernel build` for an optimized,
  freestanding compiled binary.

## Status: v1, early build

Current: hand-written lexer with full indentation tracking (Indent/Dedent/
Newline tokens), see `src/lexer.rs`. Next: parser → AST → typecheck →
ownership analysis → Kernel IR → C codegen (v1's backend).

## Roadmap

| Version | Target | Backend | Unlocks |
|---|---|---|---|
| **v1** | systems/kernel | transpile to C | boots a real toy kernel in QEMU; doubles as the training corpus for a from-scratch LLM interpretability research project |
| **v2** | native performance + backend/server | real LLVM backend (`inkwell`) | goroutine-style concurrency (spawn + channels, no async/await function-coloring), `net.http`/`net.tcp`/`json`/`db` stdlib, package manager, dev-mode interpreter — Node/Python-grade backend ergonomics |
| **v3** | web | WASM backend | browser + PWA |
| **v4** | mobile | native FFI (iOS/Android) or WASM+WebView shell | mobile apps |
| **v5** | full-stack apps | — | **Kernel UI**: a React-style component/state/diffing library, written *in* Kernel, running on the v3/v4 backends and talking to v2's server layer. One language: systems + backend + web + mobile. |

Each stage is a new backend plugged into the same frontend/IR — not a rewrite.
v1 is the only stage currently in active development; v2–v5 are the dated
build plan, not parallel work.

## Why Rust for the compiler itself

Matches the genre (Servo, Ladybird, Gosub, Rust-for-Linux are all Rust/C++),
and a memory-safe compiler implementation is its own credibility signal for
a language whose entire pitch is memory safety.

## License

MIT

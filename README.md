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

## Status: v1 — COMPLETE, boots in QEMU

Full pipeline verified end to end: lexer → parser → AST → ownership/safety
checker → tree-walking interpreter (dev mode) → C codegen → `gcc`/`as`/`ld`
→ a real freestanding x86 binary → boots in QEMU → produces pixel-exact
correct output matching the interpreter, including a `raw_write` MMIO write
landing on the precise byte address specified.

```
$ kernel run examples/boot_demo.kn      # dev-mode interpreter
$ kernel build examples/boot_demo.kn out.bin   # compiles to a bootable binary
$ qemu-system-i386 -kernel out.bin       # boots it for real
```

What's real and tested:
- `src/lexer.rs` — indentation tracking (Indent/Dedent/Newline)
- `src/parser.rs` + `src/ast.rs` — full recursive-descent parser for the core
  subset (functions, structs, if/elif/else, while, for-in-range, `unsafe`)
- `src/checker.rs` — move/ownership checking across straight-line code,
  branches, and loop re-iteration (verified: rejects use-after-move, including
  control-flow-merged cases), borrow-conflict detection (verified: rejects
  simultaneous mutable/immutable borrows, lexically scoped), and unsafe-block
  enforcement (verified: rejects unsafe ops outside `unsafe`)
- `src/interpreter.rs` — tree-walking dev-mode execution
- `src/codegen_c.rs` + `kernel_runtime/` — C backend, VGA text-mode runtime,
  multiboot1 boot glue, boots a real kernel binary in QEMU

Documented v1 scope limitations (not silent gaps):
- borrow-conflict detection is lexically scoped (a borrow is "alive" until
  the end of its enclosing block), not full Non-Lexical-Lifetime analysis
  (alive only until its actual last use) — see ARD-003 Revision 4
- generics/pattern-matching/closures ARE implemented (v1.x additions, not
  silent extras -- see RFC-002's "v1.x additions" section for each one's
  honest scope): monomorphization for generics, parse-time desugaring for
  `match`, lambda-lifting for closures. Traits and Rust-style tagged-union
  enums remain unimplemented.
- AST lowers straight to C (no separate Kernel IR yet) — deliberate, a formal
  pluggable-backend IR only earns its keep once there's a second backend
  (LLVM, v2) to validate it against
- a real LSP server (`kernel lsp`) exists for diagnostics (see RFC-002) —
  no hover/completion/go-to-definition yet

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

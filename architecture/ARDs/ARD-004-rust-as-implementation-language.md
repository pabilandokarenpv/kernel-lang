# ARD-004: Compiler implementation language is Rust

**Status:** Accepted
**Date:** 2026 (kernel-lang v1)

## Context

Kernel's entire pitch is memory safety. The language implementing a
memory-safety-focused compiler is itself a credibility signal — a compiler
written in a memory-unsafe language undermines the pitch before a user
even runs it.

## Decision

The Kernel compiler (`kernel-lang`) is implemented in Rust: `lexer.rs`,
`parser.rs`, `ast.rs`, `checker.rs`, `interpreter.rs`, `codegen_c.rs`,
`main.rs`, built via Cargo.

## Alternatives considered

- **C/C++** — rejected: matches the "systems language" genre but
  reintroduces exactly the memory-safety risk class Kernel exists to avoid;
  a C++ compiler for a memory-safety-pitched language is a credibility
  mismatch.
- **OCaml/Haskell** — rejected: a legitimate, common choice for compiler
  implementation (functional languages are a natural fit for AST
  manipulation), but doesn't carry the same "we eat our own dog food"
  signal, and Rust's tooling (Cargo, crates.io, the broader ecosystem) is
  more directly useful for a solo-maintained project.

## Consequences

- Implementation choice doubles as a credibility argument, matching other
  memory-safety-focused systems projects in the same genre (Servo,
  Ladybird, Gosub, Rust-for-Linux).
- The compiler itself benefits from Rust's real borrow checker during
  development — any accidental use-after-move or data race in the
  compiler's own internals is caught at `cargo build` time, not at runtime.

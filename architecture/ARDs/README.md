# Architecture Decision Records — Kernel Language

Each ARD captures one load-bearing technical decision: the context that forced
it, the alternatives considered, and the consequences we accepted.

| ID | Title | Status |
|---|---|---|
| [ARD-001](./ARD-001-language-scope-v1.md) | v1 scope: int/float/str/C-style-enums, no generics/traits/pattern-matching/closures yet | Accepted |
| [ARD-002](./ARD-002-compilation-pipeline-shape.md) | Pipeline shape: AST → C, no separate Kernel IR | Accepted |
| [ARD-003](./ARD-003-ownership-checker-scope.md) | Ownership checker: move-checking with control-flow merge, no borrow-conflict detection yet | Accepted |
| [ARD-004](./ARD-004-rust-as-implementation-language.md) | Compiler implementation language: Rust | Accepted |
| [ARD-005](./ARD-005-universal-backend-philosophy.md) | Universal backend philosophy: one checked AST, many pluggable targets, no per-domain forks | Accepted |

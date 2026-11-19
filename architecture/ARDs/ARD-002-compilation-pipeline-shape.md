# ARD-002: AST lowers directly to C — no separate Kernel IR in v1

**Status:** Accepted
**Date:** 2026 (kernel-lang v1)

## Context

Compilers with multiple backends (LLVM frontends, rustc, etc.) typically
insert an intermediate representation (IR) between the AST and each
backend, so new backends don't need to understand the full surface syntax.
Kernel's roadmap explicitly plans multiple backends over time: C (v1),
LLVM via `inkwell` (v2), WASM (v3), native FFI (v4).

## Decision

v1's `codegen_c.rs` lowers the checked AST straight to C source, with no
intermediate IR layer. The long-term plan (each roadmap stage plugs a new
backend into "the same frontend/IR") is the *stated direction*, but the IR
itself is not built until there is a second backend to validate it against.

## Alternatives considered

- **Build a formal Kernel IR now, before a second backend exists** —
  rejected: an IR designed against a single consumer (C codegen) has no
  real pressure-testing; its abstractions would be guesses about what v2's
  LLVM backend needs, likely wrong, and reworked anyway once `inkwell`
  lowering is actually attempted.
- **Go straight AST → LLVM for v1** — rejected: C codegen plus `gcc`/`as`/
  `ld` plus a hand-written `kernel_runtime/` (boot.s, link.ld, runtime.c)
  is a much shorter path to "boots in QEMU," which is v1's actual proof
  target. LLVM integration is deliberately deferred to v2 where native
  performance is the stated goal.

## Consequences

- v1's codegen is simple and fully auditable: one file (`codegen_c.rs`)
  mapping AST nodes to C constructs, no IR abstraction to get wrong.
- v2 (LLVM backend) will need to either extract an IR from the existing
  AST→C lowering logic, or go AST→LLVM directly and skip the IR
  question entirely — that decision is explicitly deferred, not
  pre-committed.
- This is a documented, intentional scope limitation (see README "v1 scope
  limitations"), not an oversight discovered later.

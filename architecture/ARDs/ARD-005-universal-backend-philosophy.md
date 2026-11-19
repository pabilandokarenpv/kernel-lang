# ARD-005: Universal backend philosophy — one checked AST, many pluggable targets, no per-domain forks

**Status:** Accepted (architectural principle guiding PRD-002's expanded
roadmap)

## Context

PRD-002 commits Kernel to an unusually wide target surface: systems,
servers, web, mobile, games, ML, quantum computing, embedded/Arduino, and
blockchain. The obvious failure mode for a project this ambitious is
ending up with N incompatible dialects of "Kernel" — one for systems, a
different one for web, another for quantum — which is exactly the
fragmentation problem this project is trying to avoid (it's the same
fragmentation en already lives with daily: Python for ML, Rust for
systems, Go for backend, JS for frontend).

## Decision

Every new target is a new **backend** consuming the *same* checked AST
produced by v1's frontend (lexer → parser → checker), never a fork of the
frontend itself. This was already the stated plan for v2 (LLVM) and v3
(WASM) in the original roadmap (PRD-001) — ARD-005 generalizes it
explicitly as the organizing principle for v6-v9 too:

| Target | Backend | What's new vs. existing backends |
|---|---|---|
| Systems (v1) | C → gcc/as/ld | baseline |
| Server/backend (v2) | LLVM (`inkwell`) | native performance, concurrency primitives |
| Web (v3) | WASM | browser sandboxing constraints |
| Mobile/games (v4) | native FFI / WASM+WebView | platform API surfaces, rendering/audio bindings |
| ML (v6) | still LLVM/native, + tensor ops as stdlib types | no new backend — a stdlib/type-system addition, not a new target |
| Quantum (v7) | quantum-circuit DSL → Qiskit/OpenQASM | genuinely new: compiles to an *external* SDK's representation, not to a binary Kernel runs itself |
| Embedded/Arduino (v8) | C backend, `no_std`-equivalent mode | reuses v1's freestanding-binary/QEMU-boot experience directly — closest relative to any existing backend |
| Blockchain (v9) | EVM bytecode / WASM contract format | genuinely new: a sandboxed, deterministic execution target with no filesystem/network access |

## Why this holds even for the genuinely novel targets (quantum, blockchain)

Quantum and blockchain don't fit the "compile to a binary that runs
somewhere" model the other six targets share — a quantum circuit is a
*description* handed to specialized hardware/simulators, and a smart
contract runs in a highly restricted deterministic sandbox, not a general
OS. The decision these still take the "new backend off the same AST"
shape rather than becoming separate languages is deliberate: a developer
who already knows Kernel's syntax, ownership model, and control flow
doesn't have to relearn a different language's syntax to target a
quantum circuit or a smart contract — only the backend-specific subset of
semantics (e.g., no heap allocation in a smart-contract context) changes,
enforced by the checker, not by switching languages.

## Alternatives considered

- **Separate DSLs per exotic domain** (a "Kernel-Quantum" / "Kernel-Chain"
  as distinct languages) — rejected: this is precisely the fragmentation
  problem Kernel exists to solve; a developer having to learn Kernel *and*
  Kernel-Quantum *and* Kernel-Chain defeats "one coherent language."
- **One universal backend that handles everything** — rejected: quantum
  hardware, EVM, and a freestanding kernel binary have fundamentally
  incompatible execution models; a single backend abstraction covering
  all of them would either be so generic it adds no value, or would leak
  implementation details that break the "one language" promise anyway.

## Consequences

- The checker (`checker.rs`) will need backend-aware rule sets over time
  — e.g., a smart-contract target needs to reject heap allocation and
  unbounded loops (gas-cost/determinism requirements) that are perfectly
  fine in v1's systems target. This means the checker's scope grows from
  "one fixed rule set" (v1) to "a rule set parameterized by target" by
  v9 — a real architectural change to plan for, not a free consequence.
- Each new backend is independently, incrementally buildable — v6 (ML)
  genuinely needs no new backend at all (just stdlib/type-system work on
  top of v2's existing LLVM target), which is a real, near-term-reachable
  win inside this otherwise very long roadmap.
- This principle is what makes PRD-002's nine-stage roadmap
  *architecturally* plausible rather than aspirational hand-waving — each
  stage is additive to the same core, matching how v1's own README
  already frames the v1→v5 plan ("each stage is a new backend plugged
  into the same frontend/IR — not a rewrite").

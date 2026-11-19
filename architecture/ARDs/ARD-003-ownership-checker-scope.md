# ARD-003: Ownership checker does move-checking per-branch, no control-flow merge yet

**Status:** Accepted
**Date:** 2026 (kernel-lang v1)

## Context

Kernel's pitch is Rust-grade memory safety without Rust's lifetime-annotation
tax. A *complete* borrow checker needs two hard things: (1) tracking whether
a value has been moved, merged correctly across every control-flow path
(`if`/`elif`/`else`, loops) so a value moved in one branch but not another
is still correctly flagged as "maybe moved" after the branch; and (2)
detecting simultaneous mutable/immutable borrow conflicts. Both are genuinely
hard static-analysis problems — (2) especially is "the hard 20%" of what
makes a real borrow checker a multi-year project (see Rust's own NLL work).

## Decision

v1's `checker.rs` implements move/use-after-move checking, verified to
reject real use-after-move cases, and `unsafe`-block enforcement, verified
to reject unsafe operations outside an `unsafe` block — but each
`if`/`elif`/`else`/loop branch is checked in its own scope snapshot, not
merged back into a single flow-sensitive state after the branch. Mutable/
immutable borrow-conflict detection is not implemented at all in v1.

## Alternatives considered

- **Full flow-sensitive merge + borrow-conflict detection in v1** —
  rejected: this is the single largest-scope item in the whole project;
  building it before the simpler 80% (move-checking, unsafe enforcement)
  is proven working and booting in QEMU inverts the risk order for no
  benefit to v1's actual goal (prove the pipeline, generate a training
  corpus).
- **Skip ownership checking in v1 entirely, add it in v2** — rejected:
  the ownership model is Kernel's core differentiator; a v1 with no
  memory-safety story at all undercuts the pitch more than a v1 with a
  partial but honestly-scoped one.

## Consequences

- v1 can correctly reject the two test cases that matter for the pitch
  (`examples/bad_use_after_move.kn`, `examples/bad_unsafe_outside_block.kn`)
  — both verified rejected, not just claimed.
- The gap is explicit and documented in the README under "v1 scope
  limitations," not discovered as a surprise bug later.
- Control-flow-merged analysis and borrow-conflict detection are named as
  v1.x refinements — scoped future work, not an open-ended TODO.

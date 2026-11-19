# ARD-003: Ownership checker — move-checking with control-flow merge, no borrow-conflict detection yet

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
to reject unsafe operations outside an `unsafe` block. Control-flow-merged
move checking across `if`/`elif`/`else` and into/out of `while`/`for`
bodies **is implemented** — a value moved inside just one `if` arm, or
inside a loop body, is correctly treated as moved for every statement
after that branch/loop (see `examples/bad_move_in_branch.kn` and
`examples/bad_move_in_loop.kn`, both verified rejected; see
`examples/branch_borrow_ok.kn`, verified NOT falsely rejected when
branches only borrow). The remaining gap is narrower than originally
scoped: re-move-on-next-iteration detection *within a single loop's own
body* (a value moved on pass 1 through a `while`/`for` body being reused
on pass 2 of that same loop) is not caught — this would need the loop
body checked twice (once to discover what gets moved, once more against
that already-moved state), not yet built. Mutable/immutable
borrow-conflict detection is not implemented at all in v1.

## Alternatives considered

- **Full flow-sensitive merge + borrow-conflict detection in v1** —
  rejected: this is the single largest-scope item in the whole project;
  building it before the simpler 80% (move-checking, unsafe enforcement)
  is proven working and booting in QEMU inverts the risk order for no
  benefit to v1's actual goal (prove the pipeline, generate a training
  corpus). The control-flow merge was revisited and implemented once v1's
  baseline was solid (see "Revision" below) — borrow-conflict detection
  remains deferred as genuinely the harder, lower-priority remaining
  piece.
- **Skip ownership checking in v1 entirely, add it in v2** — rejected:
  the ownership model is Kernel's core differentiator; a v1 with no
  memory-safety story at all undercuts the pitch more than a v1 with a
  partial but honestly-scoped one.

## Revision: control-flow merge implemented post-v1-ship

This ARD originally scoped control-flow-merged move checking entirely out
of v1, as a "v1.x refinement." It was implemented afterward, found by
constructing and confirming a real missed bug first (a struct moved
inside a single `if` arm, then used unconditionally afterward, ran
without any error and double-printed the moved value) rather than
assuming the gap mattered without evidence. The fix: each branch
(if/elif/else arm, while body, for body) is still checked against its own
scope snapshot as before, but afterward every variable that existed
before the branch is marked moved in the parent scope if it was moved on
*any* reachable branch output — a conservative "might have been moved,
so treat as moved" merge, which is the safe direction for a safety
checker to err in.

## Consequences

- v1 can correctly reject the two original test cases that matter for the
  pitch (`examples/bad_use_after_move.kn`,
  `examples/bad_unsafe_outside_block.kn`), **plus** the two
  control-flow-merge cases added in this revision
  (`examples/bad_move_in_branch.kn`, `examples/bad_move_in_loop.kn`) —
  all four verified rejected, and `examples/branch_borrow_ok.kn` verified
  NOT falsely rejected, not just claimed.
- The remaining gap (re-move-on-next-iteration inside one loop's own
  body) is explicit and documented here and in `checker.rs`'s own
  top-of-file comment, not discovered as a surprise bug later.
- Simultaneous mutable/immutable borrow-conflict detection remains named
  as future work — scoped, not an open-ended TODO.

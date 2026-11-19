# ARD-003: Ownership checker — move-checking with full control-flow merge (incl. loop re-iteration), no borrow-conflict detection yet

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
branches only borrow). Re-move-on-next-iteration detection within a
single loop's own body (a value moved on pass 1 through a `while`/`for`
body being reused on pass 2 of that same loop) **is also implemented**
(see "Revision 2" below) — the loop body is checked twice, once
normally and once more seeded with pass 1's end state, so a value
moved once and reused the next time around is now caught. Mutable/
immutable borrow-conflict detection is not implemented at all in v1.

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

## Revision 1: control-flow merge implemented post-v1-ship

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

## Revision 3: fixed a real reference-parameter codegen bug (found via the batch-compile sweep)

While verifying Revision 2's new examples through the full compile+QEMU
path (not just the interpreter, which both had already been passing),
`examples/loop_borrow_ok.kn` and the **pre-existing**
`examples/branch_borrow_ok.kn` both failed to even compile:
`peek(&origin)` call sites generated a correct `struct Point*` argument,
but `fn peek(p: &Point)`'s C signature generated `struct Point p` (by
value) — a real type mismatch, meaning `branch_borrow_ok.kn`'s compiled
path had been silently broken the entire time it existed, undetected
because nobody had re-run `build` on it since it was added (only `run`,
the interpreter path).

Root cause, found by tracing both representations of "is this
parameter a reference" back to the parser: `parse_type()` already folds
a leading `&` straight into the returned type *string* (e.g. `"&Point"`)
for the `p: &Point` syntax every example actually uses, but a second,
independent `Param.borrowed` boolean was set by checking for `&` in the
wrong grammatical position (before the parameter name — a syntax no
example ever used) and was therefore always `false`. Two different
representations of the same fact, only one of them ever actually true.
This also meant `checker.rs`'s `self.structs.contains(&p.ty)` lookup was
silently checking for the string `"&Point"` in a set that only ever
contained bare names like `"Point"` — always false, happening to still
produce the right `copy = true` result only because of the `||` with the
(itself always-false) `borrowed` check being OR'd against it, not
because the logic was actually right.

Fixed in the parser: strip a leading `&` from `parse_type()`'s result
and set `Param.borrowed = true` from that, once, at the source — `ty` is
now always the bare struct name everywhere downstream, and `borrowed` is
the real, single source of truth for "this parameter is a reference."
Verified: `branch_borrow_ok.kn` and `loop_borrow_ok.kn` now both compile
and boot in QEMU pixel-identical to their interpreter output. Full
example suite re-run through `build` (not just `run`) with zero other
regressions.

- v1 can correctly reject the two original test cases that matter for the
  pitch (`examples/bad_use_after_move.kn`,
  `examples/bad_unsafe_outside_block.kn`), **plus** the two
  control-flow-merge cases added in Revision 1
  (`examples/bad_move_in_branch.kn`, `examples/bad_move_in_loop.kn`),
  **plus** the re-iteration case added in Revision 2
  (`examples/bad_move_reiteration.kn`) — all five verified rejected, and
  `examples/branch_borrow_ok.kn` + `examples/loop_borrow_ok.kn` verified
  NOT falsely rejected, not just claimed.
- The ownership checker's move-checking side is now complete for every
  control-flow shape v1 has (straight-line, branching, looping, and
  re-iteration within a loop). The one deliberately-scoped-out piece
  left is simultaneous mutable/immutable borrow-conflict detection —
  named as future work, not an open-ended TODO.

## Revision 2: loop re-iteration move checking implemented

Found and fixed the same way as Revision 1 — by constructing and
confirming the real bug first. `take(origin)` inside a `while` body
moved `origin` on the conceptual "iteration 1," then the same loop body
calling `take(origin)` again represents what would happen on "iteration
2" if the loop runs more than once; this ran with zero error and
silently reused the already-moved value, three times over, before the
fix.

The fix: each loop body (`while`/`for`) is checked twice — pass 1
against the scope on entry (as before, this is what catches a normal
single-iteration bug), then pass 2 against pass 1's *end* state
(seeding the check with "what if this variable was moved on the
previous lap"). Any error that shows up only in pass 2 — not already
present in pass 1 — is specifically a re-move-on-next-iteration bug and
gets reported with a clarifying suffix explaining why; errors common to
both passes are the same root cause and are not duplicated.

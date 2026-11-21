# ARD-001: v1 scope — int/float/str/C-style-enums only, no generics/traits/pattern-matching/closures yet

**Status:** Accepted
**Date:** 2026 (kernel-lang v1)

## Context

Kernel's pitch is "memory-safe systems language that reads like Python."
Building the full surface area (generics, traits, enums, pattern matching,
closures, strings, floats) before anything compiled and booted would mean
months of parser/checker work with zero proof any of it actually works end
to end — lexer, parser, checker, interpreter, C codegen, and a real QEMU
boot all need to exist and agree with each other before the language means
anything.

Kernel also has a second consumer: it's the training corpus for a
from-scratch LLM interpretability project (Kernel-LLM). That project needs
*many* syntactically valid, checker-passing programs generated
programmatically — a combinatorially large grammar makes the corpus
generator itself a bigger project than the language.

## Decision

v1 ships with exactly: functions, structs, `if/elif/else`, `while`,
`for v in lo..hi`, `unsafe` blocks, `int`, `float`, and `str` as primitive
types (string concatenation via `+`/`+=`, and float arithmetic/comparison/
`abs`/unary negation, all verified working identically in both the
interpreter and the compiled-to-QEMU path — see `examples/string_concat.kn`
and `examples/floats.kn`), and C-style enums (`enum Color: Red Green Blue`
desugars at parse time into flat named int constants — `examples/enums.kn`,
also verified both paths). No generics, traits, pattern matching, or
closures in the interpreter or codegen for v1, and no Rust-style enums
with associated data/tagged unions (deliberately out of scope — the
desugared C-style version gives real ergonomics without that larger
lift) — even though richer syntax is already *specified* in the grammar
(for corpus-generator variety down the line), it is not implemented in
the checker/interpreter/codegen.

## Alternatives considered

- **Full language surface before any backend work** — rejected: no working
  proof of the pipeline until everything is built, high risk of discovering
  a fatal interpreter/codegen mismatch after investing in surface area that
  then needs rework anyway.
- **Strings + floats in v1, generics deferred** — rejected: strings alone
  drag in allocation/lifetime questions that interact with the ownership
  checker in ways int arithmetic never does; not worth the risk for v1's
  actual goal (prove the pipeline, generate a corpus).

## Consequences

- v1 can legitimately claim "boots in QEMU, pixel-exact output vs the
  interpreter" — a narrow but *fully verified* claim, not a partially-working
  broad one.
- The corpus generator (`corpus/generate_programs.py` in kernel-llm) only
  needs to emit int-arithmetic/control-flow programs, which is itself a much
  smaller generator to get right.
- v2+ roadmap (LLVM backend, concurrency, stdlib) explicitly treats this as
  additive — new backends plug into the same frontend/IR rather than
  rewriting v1, so the int-only constraint doesn't have to be "undone,"
  just extended.
- Known, documented gap (not a silent one): generics/traits/enums exist in
  the syntax spec already, so growing the grammar later is extension, not
  a breaking redesign.

## Revision 1: generics, pattern matching, and closures implemented as v1.x extensions

This ARD's "no generics, traits, pattern matching, or closures" scope was
accurate for v1's initial ship. All three of generics (functions and
structs, via monomorphization), pattern matching (`match`, via parse-time
desugaring to `if`/`elif`), and closures (via lambda-lifting to plain
functions with explicit captures) were implemented afterward as v1.x
additions -- each erased by an AST-to-AST pass before the checker ever
sees it, matching ARD-002's "no separate IR, keep each stage simple"
philosophy. Traits and Rust-style tagged-union enums with associated data
remain genuinely out of scope, unchanged from the original decision. A
real growable collection (`Vec<T>`) was also added, with real codegen on
the interpreter and the LLVM native backend (ARD-006) -- the freestanding
C backend explicitly rejects it (no heap allocator). Real Rust-style
TAGGED-UNION enums (variants carrying typed payload fields, e.g. `enum
Shape: Circle(int) / Rect(int, int)`) were added as well, closing the
"Rust-style enums... remain out of v1's scope" gap named two sentences
above -- a real runtime type on ALL THREE backends (not erased, same
treatment `Vec<T>` got), deliberately scoped to primitive-only payload
fields and globally-unique variant names (see RFC-002/RFC-003 for the
full design and each backend's layout). An enum where every variant has
zero fields is still handled by the OLD, unchanged C-style flat-int-const
desugar -- this was purely additive, not a breaking change to the
original mechanism. See RFC-002's "v1.x additions" section for the full,
current, honest scope of each.

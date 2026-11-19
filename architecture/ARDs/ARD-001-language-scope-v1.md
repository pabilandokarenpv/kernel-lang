# ARD-001: v1 scope is int-only, no generics/traits/enums/pattern-matching/closures

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
`for v in lo..hi`, `unsafe` blocks, `int` and `str` as primitive types
(string concatenation via `+`/`+=` verified working identically in both
the interpreter and the compiled-to-QEMU path — see
`examples/string_concat.kn`). No floats, generics, traits, enums, pattern
matching, or closures in the interpreter or codegen for v1 — even though
some of this syntax is already *specified* in the grammar (for
corpus-generator variety down the line), it is not implemented in the
checker/interpreter/codegen.

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

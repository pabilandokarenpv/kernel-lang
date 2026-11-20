# ARD-004: taking on `inkwell`/LLVM as the one external dependency

**Status:** Accepted (v2)

**Note on this file's history:** ARD-001 through ARD-004 have been
referenced from code comments across this project (`codegen_c.rs`,
`json.rs`, `lsp.rs`, `monomorphize.rs`, `parser.rs`, `README.md`) as
living in `engineering/architecture/ARDs/` since well before this
particular decision -- but that directory never actually existed on
disk; every one of those references has been a phantom pointer until
now. This file is written retroactively, honestly, for the ONE decision
it's actually needed to anchor (this session's `inkwell` dependency,
which is the thing that makes ARD-004's older "no external deps"
framing no longer simply true) -- it does not attempt to reconstruct
ARD-001/002/003's original content from inference, which would be
fabrication, not documentation. Those three remain a real, pre-existing
documentation gap, named here rather than silently papered over.

## Decision

Every stage of this project through v1.x was hand-rolled on purpose: no
parser-generator, no external JSON crate (see `json.rs`), no existing
compiler-backend crate -- codegen targeted raw C text directly
(`codegen_c.rs`). The reasoning, consistently: this project's whole point
is PROVING each stage of a real compiler pipeline end to end, which a
pulled-in library would short-circuit for the exact stage meant to be
proven.

v2's native backend (RFC-003) breaks that pattern on purpose: it depends
on `inkwell` (a safe wrapper over `llvm-sys`/LLVM 18).

## Why this one, and not the others

LLVM is categorically different from every dependency this project has
deliberately avoided so far. A parser-generator or a JSON crate would
replace a stage this project exists to demonstrate hand-building. LLVM's
actual reason to exist -- a mature, decades-deep instruction-selection,
register-allocation, and optimization pipeline for dozens of real
target architectures -- is not a stage anyone sets out to "prove" in a
learning/demonstration compiler; hand-rolling it would be reinventing a
genuinely separate, enormous piece of software, not practicing compiler
construction. The actual compiler-construction work in RFC-003 is
`codegen_llvm.rs` itself: translating this project's own AST into LLVM
IR correctly (including two real, subtle bugs it already caught -- see
RFC-003's "cross-backend verification" section) -- LLVM's job starts
only after that IR already exists.

## What this costs

- A real build-time dependency on LLVM 18 dev packages (`llvm-18-dev`,
  `libpolly-18-dev`, `libzstd-dev` on Debian/Ubuntu) -- `kernel build`
  (the original freestanding C backend) still needs only `gcc`/`as`/
  `ld`, completely unaffected; only `kernel build-native` needs LLVM.
- A real `Cargo.toml` dependency (`inkwell = { version = "0.11",
  features = ["llvm18-1"] }`) -- this project's first.
- `codegen_llvm.rs` carries real, LLVM-specific unsafety/complexity
  (opaque pointers, basic-block/phi construction for short-circuit
  booleans, two-pass struct-type registration) that `codegen_c.rs`
  never had to -- C text generation is comparatively trivial by
  comparison; this is the actual price of a REAL native backend instead
  of transpiling to another high-level language again.

## What this doesn't change

- `kernel run` (interpreter) and `kernel build` (freestanding C ->
  QEMU) are both completely untouched -- this is purely additive.
- The project's hand-rolled identity for everything else (lexer, parser,
  checker, interpreter, the LSP's own JSON-RPC framing) is unchanged.
  LLVM is the one deliberate, named exception, not a crack in the
  principle.

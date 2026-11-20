# Kernel (.kn)

A systems programming language that reads like Python, but is memory-safe
enough to write a kernel in — Rust's safety story without Rust's borrow-checker
tax.

```kn
fn add(a: int, b: int) -> int:
    return a + b

fn main():
    let x = 5
    let y = 10
    print(add(x, y))
```

## Design pillars

- **Indentation-based syntax** — no braces, no semicolons. Kernel is the only
  systems-level language that reads clean.
- **Ownership model, safe by default** — values move by default, `&` borrows.
  Compile-time memory safety without lifetime annotations to fight.
- **`unsafe` escape hatch** — raw memory access (MMIO, ports, pointers) is
  explicit and clearly marked, never silently available.
- **Three real backends, one language** — `kernel run` (dev-mode interpreter),
  `kernel build` (freestanding C, boots a real binary in QEMU/hardware), and
  `kernel build-native` (real LLVM-compiled native executable for your own
  OS). Every feature is verified against every backend it claims to support,
  not just one.

## Status: v1.x — core language + dev tooling, three working backends

```
$ kernel run examples/vec_test.kn              # dev-mode interpreter
$ kernel build examples/hello.kn out.bin       # freestanding kernel binary
$ qemu-system-i386 -kernel out.bin             # boots it for real
$ kernel build-native examples/hello.kn out    # real native executable
$ ./out                                        # runs directly, no QEMU
$ kernel lsp                                   # LSP server for editor diagnostics
$ kernel init myproject                        # scaffold a new project (kernel.toml)
```

### The language itself

- Functions, structs, `if`/`elif`/`else`, `while`, `for`-in-range, `unsafe`
  blocks for raw memory access.
- **Generics** (functions and structs) via monomorphization — inferred from
  call-site arguments, no explicit `<Type>` instantiation needed for those
  (`vec_new()` is the one builtin that DOES need one, since it takes no
  arguments to infer from — see below).
- **Pattern matching** (`match`) via parse-time desugaring to `if`/`elif`.
- **Closures** via lambda-lifting (erased to plain functions + explicit
  captured parameters before the checker ever sees them).
- **Fixed-size arrays** (`[T;N]`) with compile-time bounds checking, and
  **`Vec<T>`**, a real growable collection (`vec_new`/`vec_push`/`vec_get`/
  `vec_len`/`vec_set`) backed by real `malloc`/`realloc` on the backends
  that have a heap (interpreter + LLVM native; the freestanding C backend
  explicitly rejects it — no heap in a kernel with no allocator yet).
- **A real ownership/borrow checker**: move-by-default, explicit `&`/`&mut`
  borrows, use-after-move detection (including across branches and loop
  re-iteration), and borrow-conflict detection (simultaneous mutable +
  immutable borrows) — lexically scoped, not full Non-Lexical-Lifetime
  analysis (see the RFCs for the named gap).
- A **module/import system** (`import foo`, `import utils.math`) — files
  resolved relative to the importing file's own directory, cycle-safe,
  loud on name collisions.
- A **basic stdlib**: `len()` (arrays, Vecs, strings), `abs()`,
  `read_file`/`write_file` (dev-mode on the interpreter; real libc file I/O
  on LLVM native; rejected on the freestanding C backend — no filesystem).

### Developer experience

- **A real LSP server** (`kernel lsp`, hand-rolled JSON-RPC over stdio, zero
  external dependencies) — live diagnostics as you type, backed by the exact
  same compiler pipeline every other command uses.
- **Editor support for two real editors, not just a claim**: a packaged
  VS Code extension (`editors/vscode/`) and a Neovim config
  (`editors/nvim/`) — both actually tested against a real LSP client talking
  to the real server, not just written and assumed to work. Because
  `kernel lsp` is standard LSP, any other LSP-capable editor (Helix, Sublime,
  Emacs) needs only a few lines of client config pointing at the binary.
- **A package manifest skeleton** (`kernel.toml`, `kernel init <name>`) — no
  registry or dependency resolution yet, just a standard place to name a
  project and its entry point.
- **A 3-backend regression script** (`scripts/regression.sh`) — the
  mandatory pre-commit check for anything touching the checker, interpreter,
  either codegen backend, or the module/monomorphization/lambda-lifting
  passes. Documents, per backend, exactly what "pass" means (including named,
  intentional rejections — never a silent cross-backend disagreement).

### What's real and tested (source map)

- `src/lexer.rs` — indentation tracking (Indent/Dedent/Newline)
- `src/parser.rs` + `src/ast.rs` — recursive-descent parser: functions,
  structs, generics, `match`, closures, arrays, `Vec<T>`, imports, `unsafe`
- `src/checker.rs` — move/ownership + borrow-conflict checking
- `src/modules.rs` — module/import resolution (first pass, before
  monomorphization)
- `src/monomorphize.rs` — generics → concrete-type AST erasure
- `src/lambda_lift.rs` — closures → plain functions + explicit captures
- `src/interpreter.rs` — tree-walking dev-mode execution (`kernel run`)
- `src/codegen_c.rs` + `kernel_runtime/` — freestanding C backend, VGA
  text-mode runtime, multiboot1 boot glue (`kernel build`)
- `src/codegen_llvm.rs` — real native codegen via `inkwell`/LLVM 18
  (`kernel build-native`) — this project's first and only external
  dependency (see `architecture/ARDs/ARD-006-llvm-dependency.md`)
- `src/lsp.rs` + `src/json.rs` — hand-rolled LSP server, zero deps
- `src/manifest.rs` — `kernel.toml` parsing + `kernel init`
- `editors/vscode/`, `editors/nvim/` — real, tested editor integrations

### Documented scope limitations (not silent gaps)

- Borrow-conflict detection is lexically scoped (a borrow is "alive" until
  the end of its enclosing block), not full Non-Lexical-Lifetime analysis.
- Traits and Rust-style tagged-union enums remain unimplemented.
- `Vec<T>` has no codegen on the freestanding C backend (no heap allocator
  in a kernel with no allocator yet) — interpreter and LLVM native both
  support it fully, including struct/array/nested-`Vec` element types.
- `raw_write`/`unsafe` (MMIO) only makes sense on the freestanding C
  backend — the LLVM native backend targets a normal OS process with no
  memory-mapped I/O to write to, and rejects it with a named error.
- No cross-compilation in `kernel build-native` yet — host target triple
  only.
- See each RFC under `engineering/RFCs/` for the full, honest scope of the
  feature it documents.

## Roadmap

| Version | Target | Backend | Unlocks |
|---|---|---|---|
| **v1.x** | systems/kernel + dev tooling | C backend (transpile) + real LLVM native backend + LSP | **current** — boots a real toy kernel in QEMU, runs as a real native executable, has real editor support in 2+ editors |
| **v2** | native performance + backend/server | LLVM backend matures further | goroutine-style concurrency (spawn + channels, no async/await function-coloring), `net.http`/`net.tcp`/`json`/`db` stdlib, a real package manager/registry |
| **v3** | web | WASM backend | browser + PWA |
| **v4** | mobile | native FFI (iOS/Android) or WASM+WebView shell | mobile apps |
| **v5** | full-stack apps | — | **Kernel UI**: a React-style component/state/diffing library, written *in* Kernel, running on the v3/v4 backends and talking to v2's server layer. One language: systems + backend + web + mobile. |

Each stage is a new backend plugged into the same frontend — not a rewrite.
v1.x is the current, actively-developed stage; v2–v5 are the dated build
plan, not parallel work.

## Why Rust for the compiler itself

Matches the genre (Servo, Ladybird, Gosub, Rust-for-Linux are all Rust/C++),
and a memory-safe compiler implementation is its own credibility signal for
a language whose entire pitch is memory safety.

## License

MIT

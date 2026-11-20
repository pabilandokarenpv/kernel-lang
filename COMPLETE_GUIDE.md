# Kernel (.kn) — Complete Technical Guide

**A memory-safe systems programming language, built from scratch, that reads like Python and compiles to a real bootable binary.**

This document explains everything about Kernel: what it is, why it exists, every technical term involved, how the compiler actually works end to end, and what's real and verified versus what's planned. Written so both a non-technical reader and an engineer can follow it completely.

---

## Table of Contents

1. [The 30-Second Version](#1-the-30-second-version)
2. [Why Kernel Exists](#2-why-kernel-exists)
3. [Glossary — Every Term Used, Defined](#3-glossary--every-term-used-defined)
4. [What Kernel's Syntax Looks Like](#4-what-kernels-syntax-looks-like)
5. [The Compiler Pipeline, Stage by Stage](#5-the-compiler-pipeline-stage-by-stage)
6. [The Ownership Model — Memory Safety Without Rust's Tax](#6-the-ownership-model--memory-safety-without-rusts-tax)
7. [Compiling to a Real Bootable Binary](#7-compiling-to-a-real-bootable-binary)
8. [What's Implemented vs. What's Deliberately Deferred (v1 Scope)](#8-whats-implemented-vs-whats-deliberately-deferred-v1-scope)
9. [The Full Tech Stack](#9-the-full-tech-stack)
10. [Real Bugs Found and Fixed (Engineering Evidence, Not Claims)](#10-real-bugs-found-and-fixed-engineering-evidence-not-claims)
11. [How This Compares to Rust, Python, Go, and C](#11-how-this-compares-to-rust-python-go-and-c)
12. [The Long-Term Roadmap](#12-the-long-term-roadmap)
13. [Where Everything Lives (Repo Map)](#13-where-everything-lives-repo-map)
14. [Honest Limitations](#14-honest-limitations)
15. [Research References and Official Documentation](#15-research-references-and-official-documentation)

---

## 1. The 30-Second Version

Kernel is a programming language, built entirely from scratch (compiler and all), designed around one pitch: **write systems-level code — the kind you'd normally reach for C or Rust for — in a language that reads like Python.**

What's actually real and working today:
1. You write a `.kn` file in clean, indentation-based syntax (no braces, no semicolons)
2. A compiler — written in Rust, 83,833 characters of source across 7 files — turns it into either:
   - a fast interactive run via a built-in **interpreter** (for development), or
   - a real compiled **binary** that boots and runs on actual hardware-equivalent emulation (QEMU), with output that matches the interpreter exactly
3. Memory safety (the thing that normally requires Rust's famously difficult borrow checker) is enforced automatically at compile time, catching real bugs like "using a value after it's been given away" — without you ever writing a lifetime annotation

This isn't a toy syntax exercise — a real program written in Kernel has been compiled down to a freestanding x86 binary and verifiably booted in an emulator, producing pixel-exact correct output including a raw hardware memory write landing on the exact byte address it was told to.

---

## 2. Why Kernel Exists

Three real, separate problems motivate Kernel's design, each backed by actual research into how developers describe these languages' pain points (not assumption):

- **Python** is pleasant to write but isn't memory-safe and has no realistic story for compiling to a single freestanding binary — native extensions need per-platform builds, and packaging is widely cited as fragmented (pip/conda/poetry/venv all solving the same problem differently).
- **Rust** gives real memory safety but at a documented cost: the 2025 State of Rust Survey cites slow incremental compilation as the single biggest recurring developer complaint, and 22% of non-users cite the borrow checker's learning curve as the reason they haven't adopted it.
- **C/C++** are still the default for "actually write a kernel/bootloader," but offer no compile-time memory safety at all — exactly the class of bug (use-after-free, dangling pointers) that causes the majority of real-world security vulnerabilities in systems software.

Kernel's bet: Python's *readability* plus a *simplified* ownership model (move-by-default, explicit `&` borrows, no lifetime annotations to write) gets most of Rust's safety guarantee without most of its cognitive tax — accepting a real, named trade-off (see [Section 6](#6-the-ownership-model--memory-safety-without-rusts-tax)) rather than claiming to be a complete replacement.

There's also a second, concrete reason Kernel exists: it's the training-data generator for a separate from-scratch LLM project (Kernel-LLM/Whitekernel) — every `.kn` program, run through Kernel's own interpreter, produces a real (source code, actual output) pair used as training data teaching that model real programming semantics, not descriptions of them.

---

## 3. Glossary — Every Term Used, Defined

### Language/compiler fundamentals

| Term | Plain-language definition | Why it matters here |
|---|---|---|
| **Compiler** | A program that reads source code in one language and translates it into another form — here, either executable instructions a computer can run directly, or an intermediate language (C) that a further tool compiles. | What Kernel's `kernel` binary actually is. |
| **Interpreter** | A program that reads source code and executes it directly, statement by statement, without producing a separate compiled file first. | Kernel's `kernel run` command — fast to iterate with while developing. |
| **Lexer (tokenizer)** | The first compiler stage: reads raw text character by character and groups it into meaningful chunks ("tokens") — like recognizing `fn`, `add`, `(`, `a`, `:`, `int` as distinct pieces rather than one blob of characters. | `src/lexer.rs` — the very first thing that touches your `.kn` file. |
| **Parser** | The second stage: takes the lexer's token stream and organizes it into a tree structure (see AST) that reflects the code's actual grammatical structure — which statements are inside which function, which expression is the condition of which `if`. | `src/parser.rs`. |
| **Recursive descent parsing** | A specific, very common parsing technique where the parser is built as a set of functions that call each other recursively, each function responsible for recognizing one grammatical construct (e.g., a function for parsing "a statement," which calls a function for parsing "an expression," which may call itself for nested expressions). | The technique `src/parser.rs` uses. |
| **AST (Abstract Syntax Tree)** | A tree-shaped data structure representing a program's structure after parsing — e.g., a `+` node with two child nodes for the two things being added. | `src/ast.rs` defines Kernel's AST node types; everything downstream (checker, interpreter, codegen) operates on this tree, not on raw text. |
| **Semantic checker (type/safety checker)** | A compiler stage that walks the AST looking for *meaningful* errors the parser can't catch — not "this isn't valid grammar," but "this is grammatically valid and still wrong" (e.g., using a variable after it's been moved away). | `src/checker.rs` — this is where Kernel's memory-safety guarantee is actually enforced. |
| **Code generation (codegen)** | The compiler stage that converts a checked AST into the final output format — in Kernel's case, C source code. | `src/codegen_c.rs`. |
| **Grammar** | The formal set of rules describing what counts as validly structured code in a language (e.g., "a function is the keyword `fn`, then a name, then parentheses with parameters, then `:`, then indented statements"). | Specified in `engineering/RFCs/RFC-002-grammar-specification.md`. |
| **Indentation-based syntax** | A syntax style (Python's signature trait) where the *amount of whitespace* at the start of a line is itself meaningful — it determines which block of code a statement belongs to — instead of explicit braces `{ }`. | Kernel's lexer tracks indentation changes as real tokens (`Indent`/`Dedent`), exactly like Python's own lexer does. |

### Memory safety / ownership terms

| Term | Plain-language definition | Why it matters here |
|---|---|---|
| **Memory safety** | A program property guaranteeing it never accesses memory incorrectly — reading freed memory, writing past the end of an allocation, using a value that's already been given away, etc. The majority of real-world security vulnerabilities in C/C++ systems software trace back to violations of this. | Kernel's core differentiator and reason for existing. |
| **Ownership** | A model where every value has exactly one "owner" at a time; when you pass or assign a value, ownership transfers (moves) by default rather than being silently copied or shared. | Kernel's central safety mechanism — same core idea as Rust's. |
| **Move** | What happens by default when a value is assigned or passed to a function in Kernel: the *original* variable becomes invalid, and only the new location can use the value afterward. | Prevents two different parts of a program from both thinking they own (and can free/modify) the same piece of memory. |
| **Borrow** | Temporarily accessing a value (via `&`) *without* taking ownership of it — the original variable remains valid and usable after a borrow ends. | How Kernel lets you read/inspect data without forcing a move every time. |
| **Use-after-move** | A bug where code tries to use a value *after* it's already been moved away — in an unsafe language this can mean reading garbage or a dangling pointer. | The specific, concrete bug class Kernel's checker is built to catch and reject at compile time — see `examples/bad_use_after_move.kn`. |
| **Control-flow merge (in move-checking)** | Correctly tracking whether a value is "moved" not just in a straight line of code, but across branches (`if`/`elif`/`else`) and loops — e.g., if a value is moved inside just *one* branch of an `if`, it must be treated as moved for every statement after the branch, regardless of which branch actually ran. | A genuinely hard static-analysis problem that Kernel's checker implements (not just a naive single-pass check) — see [Section 10](#10-real-bugs-found-and-fixed-engineering-evidence-not-claims). |
| **Borrow-conflict detection** | Catching the case where a value is simultaneously borrowed mutably (allowing writes) and accessed another way at the same time — a different, harder problem than move-checking. | Explicitly **not implemented** in Kernel v1 — named directly in [Section 8](#8-whats-implemented-vs-whats-deliberately-deferred-v1-scope), not hidden. |
| **`unsafe` block** | An explicitly marked section of code where raw, unchecked memory operations (direct hardware addresses, raw pointers) are allowed — outside an `unsafe` block, the checker rejects these operations entirely. | Kernel's deliberate "escape hatch" for the rare cases (like writing directly to video memory) where safety checking must be bypassed — but only ever visibly, never by accident. |
| **MMIO (Memory-Mapped I/O)** | A hardware design where physical devices (like a screen) are controlled by reading/writing specific memory addresses, rather than through separate dedicated instructions. | What Kernel's `raw_write` example actually does — writing a byte to a specific address makes a character appear on a VGA text-mode display. |

### Systems/boot terms

| Term | Plain-language definition | Why it matters here |
|---|---|---|
| **Freestanding binary** | An executable program that runs with *no* underlying operating system — no file system, no standard library support assuming an OS is present, nothing to rely on except the raw hardware. | What `kernel build` actually produces — a tiny "operating system" of sorts, not a normal application. |
| **Bootloader** | The first code that runs when a computer powers on, responsible for loading and handing control to the actual operating system/kernel. | Kernel relies on the **Multiboot** standard (see below) so existing bootloaders (like GRUB) can load a Kernel-compiled binary without Kernel needing its own custom bootloader. |
| **Multiboot Specification** | An open standard (maintained by the GNU project) defining exactly how a bootloader hands control to an operating system image — any Multiboot-compliant bootloader can boot any Multiboot-compliant OS. | Kernel's compiled binaries include a Multiboot header (`kernel_runtime/boot.s`) specifically so QEMU/GRUB can boot them without custom tooling. |
| **QEMU** | A free, open-source machine emulator — it simulates real PC hardware in software, letting you "boot" an operating system image without needing physical hardware. | How Kernel's compiled binaries are actually verified to boot and run correctly — `qemu-system-i386 -kernel out.bin`. |
| **VGA text mode** | A classic PC display mode where the screen is controlled by writing character/color byte pairs directly into a fixed memory region, without any graphics driver. | What Kernel's `kernel_runtime/runtime.c` uses to actually display output from a booted binary. |
| **Linker script** | A configuration file telling the linker (the tool that combines compiled code into a final binary) exactly how to lay out the final binary in memory — which section goes at which address. | `kernel_runtime/link.ld` — controls exactly where in memory Kernel's compiled binary expects to be loaded. |

---

## 4. What Kernel's Syntax Looks Like

A real, complete program (`examples/hello.kn`):

```kn
fn add(a: int, b: int) -> int:
    return a + b

fn classify(n: int) -> str:
    if n < 0:
        return "negative"
    elif n == 0:
        return "zero"
    else:
        return "positive"

struct Point:
    x: int
    y: int

fn main():
    let x = 5
    let y = 10
    print(add(x, y))
```

A program demonstrating ownership and borrowing together (`examples/bad_use_after_move.kn` — this one is *supposed* to fail):

```kn
struct Point:
    x: int
    y: int

fn take(p: Point):
    print(p.x)

fn peek(p: &Point):
    print(p.x)

fn main():
    let origin = Point(x: 0, y: 0)
    peek(&origin)         # borrow, origin still usable after
    take(origin)          # move, origin is gone after this line
    take(origin)          # ERROR: use of moved value
```

Running this through the compiler produces a real, specific compile-time error rejecting the last line — not a runtime crash, not undefined behavior, a caught mistake before the program ever executes.

A program that actually boots and does real hardware I/O (`examples/boot_demo.kn`):

```kn
struct Point:
    x: int
    y: int

fn distance(p1: Point, p2: Point) -> int:
    return abs(p1.x - p2.x) + abs(p1.y - p2.y)

fn fizzbuzz(n: int):
    for i in 1..n + 1:
        if i % 15 == 0:
            print("fizzbuzz")
        elif i % 3 == 0:
            print("fizz")
        elif i % 5 == 0:
            print("buzz")
        else:
            print(i)

unsafe fn write_vga(addr: int, byte: int):
    raw_write(addr, byte)

fn main():
    print("Hello from Kernel! (.kn)")

    let p1 = Point(x: 0, y: 0)
    let p2 = Point(x: 3, y: 4)
    print("distance:", distance(p1, p2))

    fizzbuzz(15)

    unsafe:
        write_vga(757504, 35)
```

That `raw_write(757504, 35)` call writes directly to VGA text-mode memory — address 757504 is a specific byte in the screen's character buffer, and 35 is the ASCII code for `#`. Running the *compiled* version of this program in QEMU makes a `#` character genuinely appear on the emulated screen, at the exact position that address maps to.

---

## 5. The Compiler Pipeline, Stage by Stage

Every `.kn` file passes through the same five stages, implemented as five separate Rust source files that each do exactly one job:

**Stage 1 — Lexing** (`src/lexer.rs`, ~10KB of Rust): converts raw text into a stream of tokens, correctly tracking indentation level changes as real `Indent`/`Dedent` tokens — the same fundamental approach Python's own lexer uses (see the Python Language Reference, cited in [Section 15](#15-research-references-and-official-documentation)).

**Stage 2 — Parsing** (`src/parser.rs`, ~19KB): a recursive-descent parser that consumes the token stream and builds an AST (`src/ast.rs`), recognizing functions, structs, `if`/`elif`/`else`, `while`, `for v in lo..hi` range loops, `unsafe` blocks, and expressions with full operator precedence.

**Stage 3 — Checking** (`src/checker.rs`, ~13KB): walks the AST enforcing two independent rule sets:
- **Move/ownership checking** — is any value used after being moved away, across straight-line code, branches, *and* loop re-iteration (see [Section 10](#10-real-bugs-found-and-fixed-engineering-evidence-not-claims) for the real bugs this caught)
- **`unsafe` enforcement** — are any raw-memory operations (like `raw_write`) attempted outside an explicit `unsafe` block

If the checker finds any violation, compilation stops here with a clear error — nothing downstream ever runs on unsafe code.

**Stage 4a — Interpretation** (`src/interpreter.rs`, ~15KB): a tree-walking interpreter that executes the checked AST directly, used by `kernel run` for fast development iteration (no compile step, immediate output).

**Stage 4b — Code generation** (`src/codegen_c.rs`, ~19KB): lowers the same checked AST directly into C source code — no separate intermediate representation (IR) layer exists yet in v1 (a deliberate, documented choice — see ARD-002, cited in [Section 15](#15-research-references-and-official-documentation)).

**Stage 5 — Native compilation** (only for `kernel build`): the generated C code is compiled by `gcc`, assembled/linked via `as`/`ld` together with `kernel_runtime/` (hand-written boot glue, linker script, and a minimal VGA runtime), producing a freestanding binary that can actually boot.

The CLI exposes four commands, all backed by this same pipeline:
- `kernel tokens <file>` — run just the lexer, print the token stream (for debugging the lexer itself)
- `kernel run <file>` — lex → parse → check → interpret, print output
- `kernel trace <file>` — same as `run`, but output is structured JSON with a full execution trace (used by Kernel-LLM's training-data generator)
- `kernel build <file> <output>` — the full pipeline through native compilation, producing a real binary

---

## 6. The Ownership Model — Memory Safety Without Rust's Tax

This is Kernel's central technical bet, so it's worth explaining precisely what it does and doesn't do.

**What it does**: every value has one owner. Assigning or passing a value moves ownership by default — the original variable becomes unusable afterward. `&` lets you temporarily borrow a value (read/inspect it) without taking ownership, so the original stays usable. The checker enforces both rules at compile time, and — critically — it does this correctly *across control flow*: if a value might have been moved on just one branch of an `if`, or on a previous pass through a loop, the checker still catches a later use as an error, rather than only catching the simplest straight-line case.

**What it deliberately doesn't do (yet)**: Rust's *complete* borrow checker also detects simultaneous mutable/immutable borrow conflicts (e.g., holding a mutable reference and a regular reference to the same data at the same time) — this is a substantially harder static-analysis problem, formally the subject of real ongoing PL research (see Rust's own Non-Lexical Lifetimes work and the Oxide formalization paper, both cited in [Section 15](#15-research-references-and-official-documentation)). Kernel v1 does not implement this half of the problem — it's named explicitly as deferred work, not silently missing.

**The actual trade-off, stated plainly**: Kernel gets you real, compiler-enforced protection against an entire class of memory bugs (use-after-move/use-after-free-equivalent) with zero lifetime annotations to write, in exchange for *not yet* catching a narrower, harder class of bugs (simultaneous aliasing conflicts) that Rust's full borrow checker does catch. This isn't "as safe as Rust" — it's a real, honestly-scoped subset.

---

## 7. Compiling to a Real Bootable Binary

This is the part that proves the compiler pipeline actually works end to end, not just in theory.

1. `kernel build examples/boot_demo.kn out.bin` runs the full pipeline (lex → parse → check → codegen) and writes generated C code to a temp directory
2. That C code is compiled with `gcc` (freestanding mode, no host OS assumptions) alongside `kernel_runtime/runtime.c` (the VGA text-mode output driver)
3. `kernel_runtime/boot.s` (hand-written x86 assembly) provides the actual Multiboot header and the very first instructions that run when the binary is loaded — before any C code executes at all
4. `kernel_runtime/link.ld` (a linker script) tells the linker exactly where in memory each piece of the final binary must be placed, matching what a Multiboot-compliant bootloader expects
5. The result is a single freestanding binary, `out.bin`
6. `qemu-system-i386 -kernel out.bin` boots it in a real x86 emulator — QEMU recognizes the Multiboot header, loads the binary exactly as a real bootloader would, and execution begins at the entry point defined in `boot.s`

The verification isn't "it compiles without errors" — it's that the *actual rendered output on the emulated screen* matches, character for character, what the same program produces when run through the interpreter instead. Including the raw `write_vga` MMIO call landing on the exact byte address specified.

---

## 8. What's Implemented vs. What's Deliberately Deferred (v1 Scope)

Stated as plainly as the project's own internal architecture decision records (ARDs) state it — nothing here is a secret or an oversight discovered later.

### Implemented and verified

- Functions (with typed parameters and return types), structs, `if`/`elif`/`else`, `while`, `for v in lo..hi`
- `unsafe` blocks with enforcement (rejecting raw-memory operations outside them)
- Primitive types: `int`, `float` (arithmetic, comparison, `abs`, unary negation), `str` (concatenation via `+`/`+=`)
- C-style enums (`enum Color: Red Green Blue` — desugars at parse time into flat named integer constants; this is deliberately *not* Rust-style tagged-union enums with associated data, which is a much bigger feature left for later)
- Move/ownership checking, including correctly merging move-state across `if`/`elif`/`else` branches *and* detecting re-move-on-next-iteration within a single loop's own body (both added after v1's initial ship, each found and fixed by first constructing a real missed bug, not assumed as a gap)
- Full compile-to-bootable-binary pipeline, verified booting in QEMU with pixel-exact output matching the interpreter

### Deliberately deferred, named explicitly

- **Generics, traits, pattern matching, closures** — these already exist in the written grammar specification (for future corpus-generator variety) but are not implemented in the checker, interpreter, or codegen
- **Rust-style enums with associated data (tagged unions)** — only the simpler C-style flat-constant form is implemented
- **Simultaneous mutable/immutable borrow-conflict detection** — the genuinely hardest remaining piece of a complete borrow checker; move-checking is complete, this is not
- **A separate intermediate representation (IR)** between the AST and code generation — v1 lowers directly to C; an IR is planned once a second backend (LLVM, for v2) actually needs one to validate its design against

---

## 9. The Full Tech Stack

| Layer | Technology | Why |
|---|---|---|
| Compiler implementation language | **Rust** | The language implementing a memory-safety-focused compiler is itself a credibility signal; Rust's own compiler (`cargo build`) catches accidental memory bugs in Kernel's *own* source during development, for free |
| Build system | **Cargo** | Rust's standard build tool/package manager |
| Lexer/Parser | Hand-written recursive descent (no parser-generator library) | Full control over error messages and indentation-sensitive tokenization; no dependency on an external grammar-definition tool |
| Backend (v1) | Transpile to **C**, then **GCC** (compiler) + **GNU `as`** (assembler) + **GNU `ld`** (linker) | Shortest realistic path from "checked AST" to "boots in QEMU" — the actual v1 proof target |
| Boot standard | **Multiboot Specification** (GNU) | Lets any Multiboot-compliant bootloader (GRUB, QEMU's own loader) boot a Kernel-compiled binary with zero custom bootloader code |
| Verification environment | **QEMU** (`qemu-system-i386`) | Free, open-source machine emulation — verifies real boot behavior without needing physical hardware |
| Planned backend (v2) | **LLVM** via the `inkwell` Rust bindings | Native performance target once a second backend is actually being built (also when a formal IR gets introduced) |

---

## 10. Real Bugs Found and Fixed (Engineering Evidence, Not Claims)

Consistent with this whole project's standard of evidence over assumption, every non-trivial capability claim was verified by first constructing a program that exposes the gap, confirming it actually fails, then fixing it and re-verifying.

**Control-flow move-merge bug**: the checker originally checked each `if` branch in its own isolated scope snapshot, meaning a struct moved inside *just one* branch and then used unconditionally afterward ran with no error and silently double-printed the already-moved value. Fixed by: after checking each branch independently, any variable moved on *any* reachable branch gets conservatively marked moved in the parent scope — "might have been moved, so treat as moved," the safe direction for a safety checker to err toward.

**Loop re-iteration move bug**: a value moved once inside a `while`/`for` body, then "reused" by the same statement on a conceptual next pass through the loop, ran with zero error and silently reused the moved value multiple times. Fixed by checking each loop body *twice* — once normally, then again seeded with the first pass's end state — so an error that only appears on the second pass gets correctly flagged as a re-iteration bug.

**Reference-parameter codegen bug**: found while verifying the above fixes through the *full compile-to-QEMU path*, not just the interpreter (which had already been passing) — `peek(&origin)` call sites generated a correct pointer argument, but the function's own C signature generated the parameter *by value* instead, a real type mismatch that had been silently broken since the example was first added, undetected because nobody had re-run `build` on it, only `run`. Root cause: the parser had two independent representations of "is this parameter a reference" (one folded into the type string, one a separate boolean set by checking the wrong grammatical position), and only one of them was ever actually correct. Fixed by making the boolean the single source of truth, derived once, at the source.

Each of these is documented in full, including the exact reasoning and fix, in `architecture/ARDs/ARD-003-ownership-checker-scope.md`.

---

## 11. How This Compares to Rust, Python, Go, and C

| Aspect | Rust | Python | C | Kernel |
|---|---|---|---|---|
| Memory safety | Yes, compile-time (full borrow checker) | Yes, but via garbage collection (runtime, not compile-time) | No | Partial, compile-time (move-checking; no borrow-conflict detection yet) |
| Syntax style | Braces, semicolons, explicit lifetimes | Indentation-based, no braces | Braces, semicolons | Indentation-based, no braces — matches Python, not Rust/C |
| Compiles to a real binary | Yes | Not natively (interpreted, or needs embedding tools) | Yes | Yes (v1, via C/gcc) |
| Learning curve for safety | Steep — borrow checker + lifetime annotations widely cited as the top adoption barrier | N/A (no compile-time safety to learn) | N/A (no compile-time safety to learn) | Lower — move/borrow rules with zero lifetime syntax to write |
| Current real-world maturity | Extremely mature, production-grade, large ecosystem | Extremely mature | Extremely mature | v1 only — proves the pipeline works, not production-ready |

Kernel is not claiming to be "as capable as Rust" today — it's a from-scratch v1 proving a specific, narrower bet: that a real subset of Rust's safety guarantee is reachable with Python's syntax ergonomics, with the harder remaining 20% (borrow-conflict detection) explicitly scoped as future work rather than glossed over.

---

## 12. The Long-Term Roadmap

Every future stage adds a **new backend** consuming the same checked AST that v1's frontend already produces — never a fork of the frontend into a separate dialect. This is a deliberate architectural principle (see ARD-005, cited below), specifically to avoid the fragmentation problem of needing a different language per target (the exact problem Kernel is trying to solve, not recreate).

| Stage | Target | Backend | What it unlocks |
|---|---|---|---|
| **v1 (current, complete)** | Systems/kernel-level | C → gcc/as/ld | Boots a real toy kernel in QEMU; also the training-data generator for Kernel-LLM |
| v2 | Native performance + backend/server | LLVM (`inkwell`) | Goroutine-style concurrency, `net.http`/`net.tcp`/`json`/`db` standard library, package manager |
| v3 | Web | WASM | Browser + PWA support |
| v4 | Mobile | Native FFI (iOS/Android) or WASM+WebView | Mobile apps |
| v5 | Full-stack apps | — | "Kernel UI": a React-style component/state library, written *in* Kernel itself |
| v6 | ML | (No new backend — stdlib/type-system addition on top of v2's LLVM target) | Tensor types as first-class stdlib |
| v7 | Quantum computing | Quantum-circuit DSL → Qiskit/OpenQASM | Compiles to an external SDK's representation rather than a binary Kernel runs itself |
| v8 | Embedded/Arduino | C backend, `no_std`-equivalent mode | Reuses v1's exact freestanding-binary/QEMU-boot experience |
| v9 | Blockchain | EVM bytecode / WASM contract format | A sandboxed, deterministic execution target |

**v1 is the only stage in active development.** v2 through v9 are a dated, researched build plan (including real cited competitor pain-point research per target — see `product/PRDs/PRD-002-universal-language-vision.md`), not parallel or speculative work.

---

## 13. Where Everything Lives (Repo Map)

```
kernel-lang/
├── src/
│   ├── lexer.rs        — tokenizes raw .kn text, tracks indentation
│   ├── parser.rs       — recursive-descent parser, builds the AST
│   ├── ast.rs           — AST node type definitions
│   ├── checker.rs       — move/ownership checking + unsafe enforcement
│   ├── interpreter.rs   — tree-walking dev-mode execution
│   ├── codegen_c.rs     — lowers checked AST to C source
│   └── main.rs          — CLI (tokens/run/trace/build subcommands)
├── kernel_runtime/
│   ├── boot.s           — x86 assembly, Multiboot header, entry point
│   ├── link.ld          — linker script (memory layout)
│   └── runtime.c        — VGA text-mode output driver
├── examples/             — real .kn programs, including deliberately-
│                           rejected ones used as checker test cases
├── architecture/ARDs/    — Architecture Decision Records (why each
│                           design choice was made, alternatives
│                           considered, consequences)
├── engineering/RFCs/     — RFC-001 (system architecture), RFC-002
│                           (full grammar specification)
└── product/PRDs/         — PRD-001 (original roadmap), PRD-002
                            (expanded vision + researched competitor
                            pain points, cited per target)
```

---

## 14. Honest Limitations

Stated as directly as everywhere else in this document:

- **Only `int`, `float`, and `str` are real primitive types in v1** — no generics, no traits, no pattern matching, no closures, despite these already existing in the written grammar specification for future use.
- **No simultaneous mutable/immutable borrow-conflict detection.** Move-checking is complete and verified across all of v1's control-flow shapes; this separate, harder problem is not attempted yet.
- **Only C-style enums**, not Rust-style tagged unions with associated data.
- **No intermediate representation (IR)** between the checked AST and C code generation — a real, acknowledged simplification that will need addressing once a second backend (LLVM, v2) exists.
- **v1 targets a specific, narrow proof** (boots in QEMU, pixel-exact vs. the interpreter) — it is explicitly not a production-ready systems language yet, and the roadmap beyond v1 (backend/web/mobile/full-stack/ML/quantum/embedded/blockchain) is a researched plan, not built or tested.

None of this is hidden in a separate document only engineers would find — it's stated in this same guide, matching the honesty standard applied throughout this project's own architecture decision records.

---

## 15. Research References and Official Documentation

Research papers and official specifications only — no blog posts or secondary tutorials, except where explicitly marked as a textbook or working-group resource rather than a peer-reviewed paper.

### 15.1 Research papers (ownership/borrow-checking theory)

| Topic | Paper | Authors | Venue / Year | Link |
|---|---|---|---|---|
| Formal model of Rust's ownership/borrow checker | "Oxide: The Essence of Rust" | Weiss, Patterson, Matsakis, Ahmed | 2019 | [arXiv:1903.00982](https://arxiv.org/abs/1903.00982) |
| Non-Lexical Lifetimes (the "hard 20%" of borrow checking Kernel v1 defers) | RFC 2094: Non-Lexical Lifetimes | Niko Matsakis, Rust language team | Rust RFC process | [rust-lang.github.io/rfcs/2094-nll.html](https://rust-lang.github.io/rfcs/2094-nll.html) |

### 15.2 Official specifications and documentation

| Topic | Source | Link |
|---|---|---|
| Multiboot Specification (the boot standard Kernel's compiled binaries comply with) | GNU Project, official specification | [gnu.org/software/grub/manual/multiboot](https://www.gnu.org/software/grub/manual/multiboot/html_node/index.html) |
| QEMU (the emulator used to verify real boot behavior) | Official QEMU documentation | [qemu.org/docs](https://www.qemu.org/docs/master/) |
| Python's indentation-based lexical rules (the model Kernel's lexer follows) | The Python Language Reference, §2.1.8 Indentation | [docs.python.org/3/reference/lexical_analysis.html#indentation](https://docs.python.org/3/reference/lexical_analysis.html#indentation) |
| Rust's ownership and borrowing model (the system Kernel's own model is adapted from) | The Rust Programming Language, Ch. 4: "Understanding Ownership" | [doc.rust-lang.org/book/ch04-00-understanding-ownership.html](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html) |
| Rust itself (the implementation language) | Official Rust documentation | [doc.rust-lang.org](https://doc.rust-lang.org/) |
| Cargo (Rust's build system) | Official Cargo documentation | [doc.rust-lang.org/cargo](https://doc.rust-lang.org/cargo/) |
| LLVM (the planned v2 backend) | Official LLVM documentation | [llvm.org/docs](https://llvm.org/docs/) |
| `inkwell` (Rust bindings to LLVM, planned for v2) | Official `inkwell` documentation | [thedan64.github.io/inkwell](https://thedan64.github.io/inkwell/inkwell/index.html) |

### 15.3 Classic compiler-construction reference

| Topic | Source | Note |
|---|---|---|
| Lexical analysis, recursive-descent parsing, and general compiler theory | Aho, Lam, Sethi, Ullman, *Compilers: Principles, Techniques, and Tools* (2nd ed.), Pearson, 2006 (ISBN 978-0321486813) | The canonical academic textbook for the techniques `lexer.rs` and `parser.rs` implement — cited as a textbook, not a paper, for transparency. |

# Kernel Language -- VS Code extension

Real IDE support for Kernel (`.kn`): syntax highlighting + live diagnostics,
backed by the exact same compiler pipeline `kernel run`/`kernel build`/
`kernel build-native` use (see `../../src/lsp.rs`). A diagnostic shown here
means the real compiler would really reject the program -- never a separate,
drifted "IDE-only" analysis.

Because this is a real [LSP](https://microsoft.github.io/language-server-protocol/)
server (hand-rolled JSON-RPC over stdio, no external dependencies -- see
ARD-004), the SAME `kernel lsp` binary works with any LSP-capable editor, not
just VS Code: Neovim (`nvim-lspconfig`), Helix, Sublime (LSP package), Emacs
(`lsp-mode`/`eglot`) all just need to be told to run `kernel lsp` for `.kn`
files. This directory is specifically the VS Code packaging of that same
server, since VS Code is the most widely used editor and benefits from a real
one-click extension rather than hand-written client config.

## What's included (v1.x, honest scope)

- **Syntax highlighting** (`syntaxes/kernel.tmLanguage.json`): keywords,
  types (`int`/`float`/`str`/`bool`), struct/enum names (capitalized
  identifiers), strings, numbers, comments (`#`), operators. Matches the
  REAL grammar (see `src/lexer.rs`'s keyword table) -- deliberately does not
  invent syntax the language doesn't actually have.
- **Live diagnostics**: real-time lex/parse/module/generics/closure/
  move-checker errors as you type, with accurate line/column for lex/parse
  errors (checker errors are reported at (1,1) -- a named precision gap
  until the AST carries real spans, see `lsp.rs`'s doc comment).
- **NOT YET**: hover, go-to-definition, autocomplete, formatting,
  rename -- `lsp.rs` only implements diagnostics today; this extension
  can't offer what the server doesn't have.

## Running it

You need a built `kernel` binary with the `lsp` subcommand (that's every
build -- `cargo build` or `cargo build --release` from the repo root).

```
cd editors/vscode
npm install
npx @vscode/vsce package   # produces kernel-lang-0.1.0.vsix
code --install-extension kernel-lang-0.1.0.vsix
```

Then set `kernel.serverPath` in VS Code settings to the full path of your
built `kernel` binary (or add its directory to `PATH` and leave the default
`kernel`).

## Verified

- `npx @vscode/vsce package` produces a real, valid VSIX (a real zip archive,
  confirmed via `file`/`unzip -l`) with `vscode-languageclient` and its
  transitive dependencies correctly bundled (not just referenced) --
  321 files, ~470KB.
- The actual wire protocol was verified against a REAL npm LSP transport
  library (`vscode-jsonrpc`, the same one `vscode-languageclient` uses
  internally for its stdio transport) spawning the real `kernel lsp`
  binary, not just a hand-rolled test script: `initialize` returns the
  right capabilities, a valid program gets zero diagnostics, and an
  invalid program gets a real diagnostic with the real error message and
  position.

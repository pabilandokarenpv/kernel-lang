# Kernel Language -- Neovim LSP config

Wires Neovim's builtin LSP client directly to the real `kernel lsp`
server (`../../src/lsp.rs`) -- the exact same server the VS Code
extension (`../vscode/`) wraps. No plugin required (works on Neovim
>= 0.8's builtin `vim.lsp.start`; also fine registered through
`nvim-lspconfig` if you already use it for other languages).

## Setup

1. Build the compiler: `cargo build --release` from the repo root.
2. Put `target/release` (or `target/debug`) on your `PATH`, or edit
   `cmd = { "kernel", "lsp" }` in `kernel.lua` to the full binary path.
3. Source `kernel.lua` from your own `init.lua` (e.g.
   `dofile("/path/to/kernel-lang/editors/nvim/kernel.lua")`), or copy it
   into `~/.config/nvim/lua/` and `require` it.

Filetype detection needs Neovim's normal `:filetype on` (every real
interactive Neovim session has this already -- it only needs calling out
explicitly for a bare `-u NONE` headless session, which is how this was
verified).

## Verified

Headless Neovim session (`nvim --headless -u NONE`), loading this exact
config, confirms:
- The LSP client actually attaches to a `.kn` buffer (`kernel-lsp` shows
  up in `vim.lsp.get_clients()`).
- A program with a real syntax error gets a real diagnostic attached to
  the buffer (`vim.diagnostic.get(0)`), with the real parser error
  message -- not a stub, not a hardcoded example.
- A valid program gets ZERO diagnostics (no false positives).

`vim.fs.root` (used for workspace-root detection) is a Neovim 0.10+
API -- this config falls back to `vim.fn.getcwd()` on older versions
rather than hard-requiring the newest API for something this simple
(confirmed necessary: this environment's apt-installed Neovim is 0.9.5).

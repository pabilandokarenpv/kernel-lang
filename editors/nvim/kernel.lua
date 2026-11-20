-- Kernel (.kn) LSP config for Neovim (>= 0.8, using the builtin
-- vim.lsp.start API directly -- no nvim-lspconfig plugin needed, though
-- this also works fine registered THROUGH nvim-lspconfig if you prefer
-- it, or via the newer vim.lsp.config declarative API on Neovim 0.11+).
--
-- Proves the real claim made in editors/vscode/README.md and RFC-002:
-- `kernel lsp` is a real, standard LSP server (JSON-RPC over stdio), so
-- ANY LSP-capable editor can use it with a few lines of client config --
-- this is the SAME server the VS Code extension wraps, not a second
-- implementation.
--
-- Setup:
--   1. Build the compiler: `cargo build --release` from the repo root.
--   2. Put target/release (or target/debug) on your PATH, or set
--      `cmd` below to the full path.
--   3. Put this file at ~/.config/nvim/after/ftdetect/kernel.lua (for
--      the filetype association) or merge it into your own init.lua --
--      whatever your own Neovim config convention already is.

vim.filetype.add({
  extension = {
    kn = "kernel",
  },
})

vim.api.nvim_create_autocmd("FileType", {
  pattern = "kernel",
  callback = function(args)
    -- vim.fs.root is Neovim 0.10+; fall back to cwd on older versions
    -- rather than hard-requiring the newest API for a feature this
    -- simple.
    local root_dir = vim.fn.getcwd()
    if vim.fs.root then
      root_dir = vim.fs.root(args.buf, { "kernel.toml", ".git" }) or root_dir
    end
    vim.lsp.start({
      name = "kernel-lsp",
      cmd = { "kernel", "lsp" }, -- or the full path to your built binary
      root_dir = root_dir,
    })
  end,
})

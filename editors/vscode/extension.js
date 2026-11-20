// Kernel (.kn) VS Code extension -- wires the editor to the real `kernel
// lsp` server (src/lsp.rs) via the standard vscode-languageclient stdio
// transport. No separate "IDE-only" analysis here at all: every
// diagnostic shown in the editor comes from the EXACT SAME lex -> parse
// -> module-resolve -> monomorphize -> lambda-lift -> check pipeline
// `kernel run`/`kernel build`/`kernel build-native` all use (see
// lsp.rs's own doc comment) -- what you see in the editor is what the
// compiler would really do, not a drifted approximation of it.

const { LanguageClient, TransportKind } = require("vscode-languageclient/node");
const vscode = require("vscode");

let client;

function activate(context) {
  const config = vscode.workspace.getConfiguration("kernel");
  const serverPath = config.get("serverPath") || "kernel";

  const serverOptions = {
    command: serverPath,
    args: ["lsp"],
    transport: TransportKind.stdio,
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "kernel" }],
  };

  client = new LanguageClient(
    "kernelLanguageServer",
    "Kernel Language Server",
    serverOptions,
    clientOptions
  );

  client.start();
  context.subscriptions.push({ dispose: () => client && client.stop() });
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };

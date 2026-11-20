// Kernel (.kn) LSP server -- v1.x, "basic real IDE support" (Option C's
// first slice). Hand-rolled JSON-RPC over stdio (see json.rs), same
// no-external-dependencies identity as the rest of this project (ARD-004).
// Reuses the EXACT SAME pipeline main.rs's `kernel run`/`kernel build` use
// (lex -> parse -> monomorphize -> lambda_lift -> check) -- a diagnostic
// reported here means the exact same program would really fail to
// run/build, never a separate "IDE-only" analysis that could drift from
// what the compiler actually enforces.
//
// Honest v1.x scope (named gaps, not silent ones):
//   - Only `textDocument/publishDiagnostics` -- no hover, go-to-definition,
//     completion, or formatting yet. Diagnostics only, but REAL ones.
//   - Full-document sync only (`textDocumentSync: 1`) -- the whole file is
//     re-lexed/re-parsed/re-checked on every keystroke-triggered change,
//     not an incremental re-analysis. Fine for files of the size this
//     language is used at today; a real cost model problem for huge files,
//     a named future concern, not this pass's problem to solve.
//   - LEX/PARSE errors get a REAL (line, column) from the lexer's new
//     position tracking (see lexer.rs/parser.rs), extracted by parsing it
//     back out of the panic message text (every `expect()`/`ident()`/
//     lexer panic site was written to end with "at line L, column C" for
//     exactly this reason). CHECKER errors have no span at all yet (the
//     AST itself carries no position info, only tokens do) -- those are
//     reported at (1, 1) of the file, a real precision gap until the AST
//     gains spans too (a separate, larger future lift, named here rather
//     than silently pretending the position is accurate).
//   - The entire pipeline is panic-based for anything except checker
//     errors; `analyze()` below wraps it in `catch_unwind` so a malformed
//     program reports a diagnostic instead of ever crashing the server
//     process -- a real, necessary difference from `kernel run`, which is
//     fine panicking directly since it's a one-shot CLI invocation.

use crate::checker::Checker;
use crate::json::Json;
use crate::lambda_lift;
use crate::lexer::Lexer;
use crate::monomorphize;
use crate::modules;
use crate::parser::Parser;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

struct Diagnostic {
    line: usize, // 1-indexed; converted to LSP's 0-indexed on the wire
    col: usize,
    message: String,
}

// Best-effort LSP URI -> filesystem path: strips the `file://` scheme.
// Doesn't decode percent-escapes (e.g. `%20` for a space in a path) -- a
// real, named v1.x rough edge, not silently wrong for the common case of
// a plain ASCII path with no special characters, which is what every
// editor sends for a typical project layout.
fn uri_to_path(uri: &str) -> PathBuf {
    PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri))
}

// Runs the real compiler pipeline against `src` (the document open at
// `path`, needed so `import` resolves relative to the right directory --
// see modules.rs), catching any lex/parse/module/lambda-lift panic
// instead of letting it kill the server process -- see this module's doc
// comment for why that's necessary here specifically.
fn analyze(path: &Path, src: &str) -> Vec<Diagnostic> {
    let result = std::panic::catch_unwind(|| {
        let tokens = Lexer::new(src).tokenize();
        let program = Parser::new(tokens).parse_program();
        let program = modules::resolve_modules(path, program);
        let program = monomorphize::monomorphize(program);
        let program = lambda_lift::lambda_lift(program);
        Checker::new(&program).check(&program)
    });
    match result {
        Ok(errors) => errors
            .into_iter()
            .map(|message| Diagnostic { line: 1, col: 1, message })
            .collect(),
        Err(payload) => {
            let message = panic_message(&payload);
            let (line, col) = extract_position(&message).unwrap_or((1, 1));
            vec![Diagnostic { line, col, message }]
        }
    }
}

// Unwraps a caught panic's payload into its message string. Handles the
// normal cases (panic! with a format string produces `String`; a bare
// string literal produces `&str`) plus one real, empirically-found wrinkle
// in this environment: `catch_unwind` can hand back a DOUBLE-boxed
// `Box<dyn Any + Send>` payload (confirmed via instrumentation -- the
// outer Any didn't downcast to String/&str/Box<str> directly, only to
// another boxed Any wrapping the real String one level in) rather than
// the single-level payload the std docs describe as the common case.
// Recursing one level handles it; if some OTHER payload shape shows up
// that neither layer recognizes, this still fails safely with a labeled
// message instead of panicking again inside the LSP server itself.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    if let Some(s) = payload.downcast_ref::<&str>() {
        return s.to_string();
    }
    if let Some(s) = payload.downcast_ref::<Box<str>>() {
        return s.to_string();
    }
    if let Some(inner) = payload.downcast_ref::<Box<dyn std::any::Any + Send>>() {
        return panic_message(inner.as_ref());
    }
    "Kernel: internal error (a non-string panic payload -- this itself is a bug worth reporting)".to_string()
}

// Parses "...at line L, column C" back out of a panic message -- see this
// module's doc comment for why every relevant panic site was written to
// end with exactly that phrase.
fn extract_position(msg: &str) -> Option<(usize, usize)> {
    let idx = msg.rfind("at line ")?;
    let rest = &msg[idx + "at line ".len()..];
    let comma = rest.find(',')?;
    let line: usize = rest[..comma].trim().parse().ok()?;
    let col_start = rest.find("column ")? + "column ".len();
    let rest2 = &rest[col_start..];
    let end = rest2.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest2.len());
    let col: usize = rest2[..end].parse().ok()?;
    Some((line, col))
}

fn read_message(reader: &mut impl BufRead) -> Option<String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None; // stdin closed
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // blank line ends the header section
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn write_message(writer: &mut impl Write, json: &Json) {
    let body = json.to_json_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.as_bytes().len());
    let _ = writer.write_all(header.as_bytes());
    let _ = writer.write_all(body.as_bytes());
    let _ = writer.flush();
}

fn send_response(writer: &mut impl Write, id: Json, result: Json) {
    write_message(writer, &Json::obj(vec![("jsonrpc", Json::String("2.0".to_string())), ("id", id), ("result", result)]));
}

fn send_notification(writer: &mut impl Write, method: &str, params: Json) {
    write_message(
        writer,
        &Json::obj(vec![
            ("jsonrpc", Json::String("2.0".to_string())),
            ("method", Json::String(method.to_string())),
            ("params", params),
        ]),
    );
}

fn publish_diagnostics(writer: &mut impl Write, uri: &str, text: &str) {
    let path = uri_to_path(uri);
    let diags = analyze(&path, text);
    let json_diags: Vec<Json> = diags
        .into_iter()
        .map(|d| {
            // LSP positions are 0-indexed; ours are 1-indexed (matching
            // how humans read "line 3, column 14" in a panic message).
            let line0 = d.line.saturating_sub(1) as f64;
            let col0 = d.col.saturating_sub(1) as f64;
            Json::obj(vec![
                (
                    "range",
                    Json::obj(vec![
                        ("start", Json::obj(vec![("line", Json::Number(line0)), ("character", Json::Number(col0))])),
                        ("end", Json::obj(vec![("line", Json::Number(line0)), ("character", Json::Number(col0 + 1.0))])),
                    ]),
                ),
                ("severity", Json::Number(1.0)), // 1 = Error
                ("source", Json::String("kernel".to_string())),
                ("message", Json::String(d.message)),
            ])
        })
        .collect();
    send_notification(
        writer,
        "textDocument/publishDiagnostics",
        Json::obj(vec![("uri", Json::String(uri.to_string())), ("diagnostics", Json::Array(json_diags))]),
    );
}

fn text_document_uri(params: Option<&Json>) -> String {
    params
        .and_then(|p| p.get("textDocument"))
        .and_then(|d| d.get("uri"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn run_lsp_server() {
    // Silences the default panic backtrace-to-stderr -- analyze()'s
    // catch_unwind already turns every lex/parse panic into a proper
    // diagnostic, so the raw backtrace would just be noise (or, on some
    // clients, a confusing log entry) on every single syntax error typed
    // mid-edit.
    std::panic::set_hook(Box::new(|_| {}));

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let Some(raw) = read_message(&mut reader) else { break };
        let Some(msg) = Json::parse(&raw) else { continue };
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let id = msg.get("id").cloned();

        match method.as_str() {
            "initialize" => {
                let result = Json::obj(vec![(
                    "capabilities",
                    Json::obj(vec![("textDocumentSync", Json::Number(1.0))]), // 1 = Full sync
                )]);
                send_response(&mut writer, id.unwrap_or(Json::Null), result);
            }
            "textDocument/didOpen" => {
                let params = msg.get("params");
                let uri = text_document_uri(params);
                let text = params
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|d| d.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                publish_diagnostics(&mut writer, &uri, &text);
            }
            "textDocument/didChange" => {
                let params = msg.get("params");
                let uri = text_document_uri(params);
                // Full sync (textDocumentSync: 1): the one contentChanges
                // entry's `text` IS the whole new document, not a delta.
                let text = params
                    .and_then(|p| p.get("contentChanges"))
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|first| first.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                publish_diagnostics(&mut writer, &uri, &text);
            }
            "textDocument/didClose" => {
                let uri = text_document_uri(msg.get("params"));
                send_notification(
                    &mut writer,
                    "textDocument/publishDiagnostics",
                    Json::obj(vec![("uri", Json::String(uri)), ("diagnostics", Json::Array(Vec::new()))]),
                );
            }
            "shutdown" => {
                send_response(&mut writer, id.unwrap_or(Json::Null), Json::Null);
            }
            "exit" => break,
            // Every other request/notification: if it carried an `id` it's
            // a request a spec-compliant client expects SOME reply to (or
            // it'll hang waiting) -- reply with a harmless null result
            // rather than silently dropping it. Notifications (no `id`,
            // e.g. textDocument/didSave) are safely ignored.
            _ => {
                if let Some(id) = id {
                    send_response(&mut writer, id, Json::Null);
                }
            }
        }
    }
}

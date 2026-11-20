import subprocess, json, sys

def frame(obj):
    body = json.dumps(obj).encode()
    return b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body

def read_message(f):
    headers = {}
    while True:
        line = f.readline()
        if line in (b"\r\n", b"\n", b""):
            break
        if b":" in line:
            k, v = line.split(b":", 1)
            headers[k.strip()] = v.strip()
    length = int(headers.get(b"Content-Length", b"0"))
    body = f.read(length)
    return json.loads(body)

proc = subprocess.Popen(
    ["./target/debug/kernel", "lsp"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
)

def send(obj):
    proc.stdin.write(frame(obj))
    proc.stdin.flush()

# 1. initialize
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
resp = read_message(proc.stdout)
print("initialize response:", resp)
assert resp.get("id") == 1
assert "capabilities" in resp.get("result", {})

# 2. initialized notification (no response expected)
send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

# 3. didOpen with a GOOD program -- expect EMPTY diagnostics
good_src = "fn main():\n    let x = 5\n    print(x)\n"
send({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
    "textDocument": {"uri": "file:///tmp/good.kn", "languageId": "kernel", "version": 1, "text": good_src}
}})
diag_msg = read_message(proc.stdout)
print("didOpen (good) diagnostics:", diag_msg)
assert diag_msg["method"] == "textDocument/publishDiagnostics"
assert diag_msg["params"]["diagnostics"] == [], "expected zero diagnostics for a valid program"

# 4. didChange with a SYNTAX ERROR -- expect one diagnostic with a real position
bad_src = "fn main():\n    let x = 5\n    let y = @\n"
send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
    "textDocument": {"uri": "file:///tmp/good.kn", "version": 2},
    "contentChanges": [{"text": bad_src}],
}})
diag_msg2 = read_message(proc.stdout)
print("didChange (bad syntax) diagnostics:", diag_msg2)
diags = diag_msg2["params"]["diagnostics"]
assert len(diags) == 1, f"expected exactly one diagnostic, got {diags}"
d = diags[0]
assert "unexpected character" in d["message"], d["message"]
assert d["range"]["start"]["line"] == 2, f"expected 0-indexed line 2 (source line 3), got {d['range']['start']}"
print("position check OK: line", d["range"]["start"]["line"], "character", d["range"]["start"]["character"])

# 5. didChange with a CHECKER error (use-after-move) -- expect one diagnostic
move_bad_src = (
    "struct Point:\n    x: int\n    y: int\n\n"
    "fn take(p: Point):\n    print(p.x)\n\n"
    "fn main():\n    let p = Point(x: 1, y: 2)\n    take(p)\n    take(p)\n"
)
send({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
    "textDocument": {"uri": "file:///tmp/good.kn", "version": 3},
    "contentChanges": [{"text": move_bad_src}],
}})
diag_msg3 = read_message(proc.stdout)
print("didChange (checker error) diagnostics:", diag_msg3)
diags3 = diag_msg3["params"]["diagnostics"]
assert len(diags3) == 1, f"expected exactly one checker diagnostic, got {diags3}"
assert "moved" in diags3[0]["message"] or "move" in diags3[0]["message"], diags3[0]["message"]

# 6. shutdown + exit
send({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None})
resp2 = read_message(proc.stdout)
print("shutdown response:", resp2)
assert resp2.get("id") == 2

send({"jsonrpc": "2.0", "method": "exit", "params": None})
proc.stdin.close()
proc.wait(timeout=5)
print("exit code:", proc.returncode)
print("ALL LSP CHECKS PASSED")

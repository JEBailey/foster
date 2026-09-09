"""Measure actual LSP requests; edits are unsaved overlays, never file writes."""
import argparse
import json
import pathlib
import queue
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=pathlib.Path)
    parser.add_argument("document", type=pathlib.Path)
    parser.add_argument("--outline", action="store_true")
    parser.add_argument("--interrupt", action="store_true")
    args = parser.parse_args()
    messages = queue.Queue()
    process = subprocess.Popen(
        [str(args.executable.resolve()), "lsp"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )

    def read():
        while True:
            headers = {}
            while (line := process.stdout.readline()) not in (b"\r\n", b"\n", b""):
                key, value = line.decode().split(":", 1)
                headers[key.lower()] = value.strip()
            if not line:
                messages.put(None)
                return
            messages.put(json.loads(process.stdout.read(int(headers["content-length"]))))

    def send(method, params, ident=None):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if ident is not None:
            message["id"] = ident
        payload = json.dumps(message).encode()
        process.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
        process.stdin.flush()

    responses = {}

    def response(ident):
        while ident not in responses:
            message = messages.get(timeout=240)
            if message is None:
                raise RuntimeError("language server exited before responding")
            if "id" in message:
                responses[message["id"]] = message
        return responses.pop(ident)

    threading.Thread(target=read, daemon=True).start()
    try:
        document = args.document.resolve()
        uri = document.as_uri()
        source = document.read_text(encoding="utf-8")
        send("initialize", {"processId": None, "rootUri": document.parent.as_uri(), "capabilities": {}}, 1)
        response(1)
        send("initialized", {})
        send("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "foster", "version": 1, "text": source,
        }})
        params = {"textDocument": {"uri": uri}}
        hover = {**params, "position": {"line": 0, "character": 0}}

        def edit():
            send("textDocument/didChange", {"textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "// latency probe\n" + source}]})

        if args.interrupt:
            send("textDocument/hover", hover, 2)
            time.sleep(0.1)
            start = time.perf_counter()
            edit()
            send("textDocument/documentSymbol", params, 3)
            result = response(3)
            elapsed = time.perf_counter() - start
            obsolete = response(2)
            print(json.dumps({"case": "interrupt", "seconds": round(elapsed, 3),
                "symbols": len(result.get("result") or []), "error": result.get("error"),
                "obsolete_error": obsolete.get("error")}))
            return

        for ident, label in [(2, "cold"), (3, "cached"), (4, "edited")]:
            start = time.perf_counter()
            if label == "edited":
                edit()
            method = "textDocument/documentSymbol" if args.outline else "textDocument/hover"
            send(method, params if args.outline else hover, ident)
            result = response(ident)
            print(json.dumps({"case": label, "method": method,
                "seconds": round(time.perf_counter() - start, 3), "error": result.get("error")}), flush=True)
    finally:
        process.kill()
        process.wait()


if __name__ == "__main__":
    main()

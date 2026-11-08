#!/usr/bin/env python3
"""Mock OpenAI-compatible server for aisle e2e tests.
Streams SSE chat completions; logs what it saw to stderr."""
import json, sys, re
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 38411

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        req = json.loads(self.rfile.read(n))
        msgs = req.get("messages", [])
        n_imgs = sum(
            1 for m in msgs if isinstance(m.get("content"), list)
            for p in m["content"] if p.get("type") == "image_url"
        )
        print(f"[mock] {self.path} model={req.get('model')} msgs={len(msgs)} images={n_imgs}", flush=True)
        reply = f"MOCK-REPLY saw {len(msgs)} messages and {n_imgs} image(s)."
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for chunk in re.findall(".{1,7}", reply):
            self.wfile.write(b"data: " + json.dumps(
                {"choices": [{"delta": {"content": chunk}}]}).encode() + b"\n\n")
        self.wfile.write(b'data: {"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":9}}\n\n')
        self.wfile.write(b"data: [DONE]\n\n")
    def log_message(self, *a):
        pass

HTTPServer(("127.0.0.1", PORT), H).serve_forever()

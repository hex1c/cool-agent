#!/usr/bin/env python3
"""Minimal mock HTTP provider for the local sandbox (Task 40).

Serves seeded sanitized responses from a JSON route table so the sandbox can
exercise Telegram, Google, and AI provider paths without any real upstream
calls or credentials. The route table format is intentionally simple:

    {
      "routes": [
        {
          "method": "POST",            # or "*" to match any method
          "path": "/bot TOKEN/getChatMember",
          "status": 200,
          "headers": {"Content-Type": "application/json"},
          "body": {"ok": true, "result": {"status": "member"}}
        },
        ...
      ]
    }

The first route whose method matches and whose path is a prefix of the request
path wins. `body` may be a JSON object/array (serialized) or a string. A
`/health` endpoint always returns 200 OK.
"""

from __future__ import annotations

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def load_routes(path: str) -> list[dict]:
    try:
        with open(path, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot load mock routes from {path}: {error}") from error
    return data.get("routes", [])


def parse_int(value: object, default: int) -> int:
    try:
        return int(str(value))
    except (TypeError, ValueError):
        return default


class Handler(BaseHTTPRequestHandler):
    routes: list[dict] = []

    def _match(self) -> dict | None:
        for route in self.routes:
            method = route.get("method", "*")
            if method != "*" and method != self.command:
                continue
            route_path = route.get("path", "")
            if self.path.startswith(route_path):
                return route
        return None

    def _respond(self) -> None:
        if self.path == "/health":
            self._send(200, {"status": "ok"})
            return
        route = self._match()
        if route is None:
            self._send(404, {"error": "no mock route matched", "path": self.path})
            return
        status = parse_int(route.get("status", 200), 200)
        headers = route.get("headers", {"Content-Type": "application/json"})
        body = route.get("body", {})
        if isinstance(body, (dict, list)):
            payload = json.dumps(body).encode("utf-8")
        elif isinstance(body, str):
            payload = body.encode("utf-8")
        else:
            payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        for key, value in headers.items():
            self.send_header(key, str(value))
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _send(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:
        self._respond()

    def do_POST(self) -> None:
        # Drain the request body so the connection stays reusable.
        length = parse_int(self.headers.get("Content-Length", "0") or "0", 0)
        if length:
            self.rfile.read(length)
        self._respond()

    def do_PUT(self) -> None:
        length = parse_int(self.headers.get("Content-Length", "0") or "0", 0)
        if length:
            self.rfile.read(length)
        self._respond()

    def log_message(self, format: str, *args: object) -> None:
        # Quiet by default; uncomment for debugging.
        return


def main() -> None:
    routes_path = sys.argv[1] if len(sys.argv) > 1 else "/app/mock-providers.json"
    port = parse_int(sys.argv[2], 8081) if len(sys.argv) > 2 else 8081
    Handler.routes = load_routes(routes_path)
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    print(f"mock-providers listening on :{port} with {len(Handler.routes)} routes", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

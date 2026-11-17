"""A tiny backend for Porthole, using only the Python standard library.

It follows the backend contract from the Porthole design:
1. Listen on 127.0.0.1:$APP_PORT only.
2. On `?token=<APP_TOKEN>`, set the `app_session` cookie and redirect to
   the same path without the query string.
3. Require that cookie on every other request (401 otherwise).
4. Reject requests whose Host header isn't 127.0.0.1:<port> or
   localhost:<port> (DNS rebinding).
"""

import hmac
import html
import os
import signal
import sys
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

PORT = int(os.environ.get("APP_PORT", "8000"))
TOKEN = os.environ.get("APP_TOKEN")  # unset = standalone dev mode, no auth
DATA_DIR = os.environ.get("APP_DATA_DIR", ".")
ALLOWED_HOSTS = {f"127.0.0.1:{PORT}", f"localhost:{PORT}"}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.headers.get("Host") not in ALLOWED_HOSTS:
            return self.send_error(403, "bad Host header")

        url = urlsplit(self.path)
        if url.path == "/healthz":
            return self.reply(200, "ok", "text/plain")

        if TOKEN is not None:
            given = parse_qs(url.query).get("token", [None])[0]
            if given is not None and hmac.compare_digest(given, TOKEN):
                self.send_response(302)
                self.send_header(
                    "Set-Cookie",
                    f"app_session={TOKEN}; HttpOnly; SameSite=Strict; Path=/",
                )
                self.send_header("Location", url.path or "/")
                self.end_headers()
                return
            cookie = SimpleCookie(self.headers.get("Cookie", ""))
            session = cookie.get("app_session")
            if session is None or not hmac.compare_digest(session.value, TOKEN):
                return self.send_error(401)

        page = f"""<!doctype html>
<title>Hello from Python</title>
<style>body {{ font: 16px system-ui, sans-serif; margin: 3em; }}</style>
<h1>Hello from Python {html.escape(sys.version.split()[0])}</h1>
<p>Backend pid {os.getpid()}, listening on 127.0.0.1:{PORT}.</p>
<p>Data folder: <code>{html.escape(DATA_DIR)}</code></p>
"""
        self.reply(200, page, "text/html; charset=utf-8")

    def reply(self, status, body, content_type):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_request(self, code="-", size="-"):
        # Goes to stderr, which Porthole writes to sidecar.log. Log the path
        # only: the first request's query string carries the token.
        path = urlsplit(self.path).path
        sys.stderr.write(f"{self.command} {path} {code}\n")


def stop(*_):
    print("shutting down", flush=True)
    sys.exit(0)


signal.signal(signal.SIGTERM, stop)
if hasattr(signal, "SIGBREAK"):  # Windows: Porthole sends Ctrl+Break
    signal.signal(signal.SIGBREAK, stop)

server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
print(f"listening on 127.0.0.1:{PORT}", flush=True)
server.serve_forever()

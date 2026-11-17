"""A Porthole sidecar backend in FastAPI. The auth kit's middleware does the
whole handshake: it checks the Host header, swaps ?token= for the session
cookie, and answers 401 (or closes WebSockets) without it."""

import os
import platform
import signal
import sys

import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse

from porthole_auth import PortholeAuth

PORT = int(os.environ.get("APP_PORT", "8000"))

app = FastAPI()
# `python3 server.py --standalone` runs it without Porthole (no APP_TOKEN),
# with auth off. Without the flag, a missing APP_TOKEN is an error.
app.add_middleware(PortholeAuth, allow_standalone="--standalone" in sys.argv)


@app.middleware("http")
async def log_requests(request: Request, call_next):
    # Added after PortholeAuth, so it wraps it and sees rejected requests
    # too. Logs the path only (the first query string carries the token);
    # Porthole writes stdout to sidecar.log.
    response = await call_next(request)
    print(f"{request.method} {request.url.path} {response.status_code}", flush=True)
    return response


@app.get("/api/ping")
def ping():
    return {"ok": True, "python": platform.python_version(), "pid": os.getpid()}


@app.get("/", response_class=HTMLResponse)
def index():
    return f"""<!doctype html>
<title>Hello from FastAPI</title>
<style>body {{ font: 16px system-ui, sans-serif; margin: 3em; }}</style>
<h1>Hello from FastAPI on Python {platform.python_version()}</h1>
<p>Backend pid {os.getpid()}, listening on 127.0.0.1:{PORT}.</p>
<p>API call with the session cookie: <b id="api">…</b></p>
<script>
  fetch('/api/ping').then(r => r.json())
    .then(j => {{ document.getElementById('api').textContent = j.ok ? 'authenticated' : 'failed'; }})
    .catch(e => {{ document.getElementById('api').textContent = 'failed: ' + e; }});
</script>
"""


if hasattr(signal, "SIGBREAK"):  # Windows: Porthole sends Ctrl+Break
    signal.signal(signal.SIGBREAK, lambda *_: sys.exit(0))

uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")

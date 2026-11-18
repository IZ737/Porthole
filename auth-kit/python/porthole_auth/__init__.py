"""Porthole's auth handshake as ASGI middleware, for FastAPI, Starlette or
any other ASGI app. Standard library only.

The spec lives in Porthole's README ("Auth handshake"); this module is a
convenience and follows it.

FastAPI / Starlette::

    from porthole_auth import PortholeAuth
    app = FastAPI()
    app.add_middleware(PortholeAuth)

Any ASGI app::

    app = PortholeAuth(app)
"""

from __future__ import annotations

import hmac
import os
import sys
from typing import Any, Awaitable, Callable, MutableMapping, Optional
from urllib.parse import unquote_plus

__all__ = ["PortholeAuth", "COOKIE_NAME"]
__version__ = "1.0.0"

COOKIE_NAME = "app_session"

Scope = MutableMapping[str, Any]
Message = MutableMapping[str, Any]
Receive = Callable[[], Awaitable[Message]]
Send = Callable[[Message], Awaitable[None]]
ASGIApp = Callable[[Scope, Receive, Send], Awaitable[None]]


class PortholeAuth:
    """Enforce the handshake on every HTTP and WebSocket request.

    ``token``: use this instead of the ``APP_TOKEN`` environment variable.
    ``allow_standalone``: when there is no token, serve every request
    without auth instead of raising. The Host check still applies.
    """

    def __init__(
        self,
        app: ASGIApp,
        *,
        token: Optional[str] = None,
        allow_standalone: bool = False,
    ) -> None:
        self.app = app
        if token is None:
            token = os.environ.get("APP_TOKEN")
        if not token:
            if not allow_standalone:
                raise RuntimeError(
                    "porthole_auth: APP_TOKEN is not set. Porthole sets it when it "
                    "starts the backend. To run the backend on its own without "
                    "auth, pass allow_standalone=True."
                )
            print(
                "porthole_auth: APP_TOKEN is not set; running standalone with auth "
                "OFF. Any local process or web page can call this server.",
                file=sys.stderr,
                flush=True,
            )
            token = None
        self.token = token
        self._expected = token.encode() if token else None

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        kind = scope["type"]
        if kind not in ("http", "websocket"):
            return await self.app(scope, receive, send)

        verdict, location = self._check(scope)
        if verdict == "ok":
            return await self.app(scope, receive, send)
        if kind == "websocket":
            # Closing before accepting makes the server answer the upgrade
            # with 403. An upgrade can't follow the redirect, so a valid
            # ?token= alone isn't enough here.
            return await send({"type": "websocket.close", "code": 1008})
        if verdict == "redirect":
            assert self.token is not None
            headers = [
                (b"set-cookie",
                 f"{COOKIE_NAME}={self.token}; HttpOnly; SameSite=Strict; Path=/".encode()),
                (b"location", location.encode("latin-1")),
                (b"cache-control", b"no-store"),
                (b"content-length", b"0"),
            ]
            await send({"type": "http.response.start", "status": 302, "headers": headers})
            return await send({"type": "http.response.body", "body": b""})

        body = b"Forbidden: bad Host header\n" if verdict == 403 else b"Unauthorized\n"
        await send({
            "type": "http.response.start",
            "status": verdict,
            "headers": [
                (b"content-type", b"text/plain; charset=utf-8"),
                (b"content-length", str(len(body)).encode()),
                (b"cache-control", b"no-store"),
            ],
        })
        await send({
            "type": "http.response.body",
            "body": b"" if scope.get("method") == "HEAD" else body,
        })

    def _check(self, scope: Scope) -> tuple[Any, str]:
        headers = scope.get("headers") or []
        host = _header(headers, b"host")
        server = scope.get("server")
        port = server[1] if server else None
        if not _host_allowed(host, port):
            return 403, ""
        if self._expected is None:
            return "ok", ""

        query = scope.get("query_string", b"").decode("latin-1")
        if query:
            given, rest = _take_token(query)
            if given is not None and hmac.compare_digest(given.encode(), self._expected):
                raw = scope.get("raw_path")
                path = raw.decode("latin-1") if raw else _quote_path(scope.get("path", "/"))
                path = path.split("?", 1)[0]
                return "redirect", _safe_path(path) + (f"?{rest}" if rest else "")

        session = _cookie(_header(headers, b"cookie"), COOKIE_NAME)
        if session is not None and hmac.compare_digest(session.encode(), self._expected):
            return "ok", ""
        return 401, ""


def _header(headers: Any, name: bytes) -> Optional[str]:
    for key, value in headers:
        if key.lower() == name:
            return value.decode("latin-1")
    return None


def _host_allowed(host: Optional[str], port: Optional[int]) -> bool:
    if host is None or not port:
        return False
    host = host.lower()
    return host in (f"127.0.0.1:{port}", f"localhost:{port}")


def _take_token(query: str) -> tuple[Optional[str], str]:
    """Split ``token`` out of a raw query string, keeping the other
    parameters byte for byte."""
    given = None
    rest = []
    for part in query.split("&"):
        key, eq, value = part.partition("=")
        if unquote_plus(key) == "token":
            if given is None:
                given = unquote_plus(value) if eq else ""
        elif part:
            rest.append(part)
    return given, "&".join(rest)


def _quote_path(path: str) -> str:
    from urllib.parse import quote
    return quote(path, safe="/:@!$&'()*+,;=-._~%")


def _safe_path(path: str) -> str:
    # A Location of //evil.example/ would leave the app.
    if not path.startswith("/"):
        return "/"
    return "/" + path.lstrip("/\\")


def _cookie(header: Optional[str], name: str) -> Optional[str]:
    if header is None:
        return None
    for pair in header.split(";"):
        key, eq, value = pair.partition("=")
        if eq and key.strip() == name:
            return value.strip()
    return None

"""Behaviour beyond the shared conformance checks (../../conformance.py).

Drives the middleware as raw ASGI, so it needs nothing but pytest.
"""

import asyncio

import pytest

from porthole_auth import PortholeAuth

TOKEN = "a" * 64
PORT = 5000
HOST = f"127.0.0.1:{PORT}"


async def inner(scope, receive, send):
    if scope["type"] == "websocket":
        await send({"type": "websocket.accept"})
        return
    await send({"type": "http.response.start", "status": 200, "headers": []})
    await send({"type": "http.response.body", "body": b"app"})


def call(app, path="/", query=b"", host=HOST, cookie=None, kind="http", method="GET"):
    headers = [] if host is None else [(b"host", host.encode())]
    if cookie is not None:
        headers.append((b"cookie", cookie.encode()))
    scope = {
        "type": kind,
        "method": method,
        "path": path,
        "raw_path": path.encode(),
        "query_string": query,
        "headers": headers,
        "server": ("127.0.0.1", PORT),
    }
    sent = []

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(message):
        sent.append(message)

    asyncio.run(app(scope, receive, send))
    return sent


def status(sent):
    return sent[0].get("status", sent[0]["type"])


def header(sent, name):
    return dict(sent[0]["headers"]).get(name, b"").decode()


def test_raises_without_token_unless_standalone(monkeypatch):
    monkeypatch.delenv("APP_TOKEN", raising=False)
    with pytest.raises(RuntimeError, match="APP_TOKEN is not set"):
        PortholeAuth(inner)
    PortholeAuth(inner, allow_standalone=True)


def test_reads_app_token(monkeypatch):
    monkeypatch.setenv("APP_TOKEN", TOKEN)
    app = PortholeAuth(inner)
    assert status(call(app, cookie=f"app_session={TOKEN}")) == 200
    assert status(call(app)) == 401


def test_standalone_still_checks_host(monkeypatch, capsys):
    monkeypatch.delenv("APP_TOKEN", raising=False)
    app = PortholeAuth(inner, allow_standalone=True)
    assert "auth OFF" in capsys.readouterr().err
    assert status(call(app)) == 200
    assert status(call(app, host="evil.example")) == 403


def test_handshake_sets_cookie_and_redirects():
    sent = call(PortholeAuth(inner, token=TOKEN), query=f"token={TOKEN}".encode())
    assert status(sent) == 302
    assert header(sent, b"location") == "/"
    assert header(sent, b"set-cookie") == f"app_session={TOKEN}; HttpOnly; SameSite=Strict; Path=/"
    assert header(sent, b"cache-control") == "no-store"


def test_redirect_keeps_other_parameters_byte_for_byte():
    sent = call(
        PortholeAuth(inner, token=TOKEN),
        path="/a%20b/c",
        query=f"x=1&token={TOKEN}&y=%2F&z".encode(),
    )
    assert header(sent, b"location") == "/a%20b/c?x=1&y=%2F&z"


def test_redirect_never_leaves_the_origin():
    sent = call(PortholeAuth(inner, token=TOKEN), path="//evil.example/x", query=f"token={TOKEN}".encode())
    assert header(sent, b"location") == "/evil.example/x"


def test_cookie_lets_app_use_its_own_token_parameter():
    app = PortholeAuth(inner, token=TOKEN)
    assert status(call(app, query=b"token=reset-link", cookie=f"app_session={TOKEN}")) == 200


def test_head_401_has_no_body():
    sent = call(PortholeAuth(inner, token=TOKEN), method="HEAD")
    assert status(sent) == 401
    assert sent[1]["body"] == b""


def test_host_is_case_insensitive():
    app = PortholeAuth(inner, token=TOKEN)
    assert status(call(app, host=f"LOCALHOST:{PORT}", cookie=f"app_session={TOKEN}")) == 200


def test_missing_host_is_forbidden():
    assert status(call(PortholeAuth(inner, token=TOKEN), host=None)) == 403


def test_websocket_needs_the_cookie():
    app = PortholeAuth(inner, token=TOKEN)
    assert status(call(app, kind="websocket", cookie=f"app_session={TOKEN}")) == "websocket.accept"
    assert status(call(app, kind="websocket")) == "websocket.close"
    assert status(call(app, kind="websocket", query=f"token={TOKEN}".encode())) == "websocket.close"
    assert status(call(app, kind="websocket", host="evil.example", cookie=f"app_session={TOKEN}")) == "websocket.close"


def test_lifespan_passes_through():
    seen = []

    async def app(scope, receive, send):
        seen.append(scope["type"])

    asyncio.run(PortholeAuth(app, token=TOKEN)({"type": "lifespan"}, None, None))
    assert seen == ["lifespan"]

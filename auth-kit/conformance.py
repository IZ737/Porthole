#!/usr/bin/env python3
"""Check a backend against Porthole's auth handshake spec.

Starts the backend the way Porthole does (a free port in APP_PORT, a random
token in APP_TOKEN, a temporary APP_DATA_DIR), sends it the requests a
webview, a hostile web page and another local process would, and checks
the answers. Standard library only.

    python3 auth-kit/conformance.py [--path /] -- node server.js
    python3 auth-kit/conformance.py -- python3 examples/hello-python/server.py

`--path` is a page the backend serves with 2xx once authenticated
(default `/`). Exit status 0 means every check passed.
"""

import argparse
import http.client
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time

failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(name)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(port, proc, timeout=20.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            sys.exit(f"the backend exited with status {proc.returncode} before listening")
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.25).close()
            return
        except OSError:
            time.sleep(0.05)
    sys.exit(f"nothing listened on 127.0.0.1:{port} within {timeout:.0f}s")


def request(port, path, host=None, cookie=None, method="GET"):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    headers = {"Host": host or f"127.0.0.1:{port}"}
    if cookie is not None:
        headers["Cookie"] = cookie
    conn.request(method, path, headers=headers)
    resp = conn.getresponse()
    resp.read()
    conn.close()
    return resp


def raw_status(port, data):
    with socket.create_connection(("127.0.0.1", port), timeout=5) as s:
        s.sendall(data)
        buf = b""
        while b"\r\n" not in buf:
            chunk = s.recv(4096)
            if not chunk:
                break
            buf += chunk
    try:
        return int(buf.split(b" ", 2)[1])
    except (IndexError, ValueError):
        return None


def cookie_attrs(resp):
    """The attributes of the app_session cookie, or None if it isn't set."""
    for value in resp.headers.get_all("Set-Cookie") or []:
        parts = [p.strip() for p in value.split(";")]
        name, _, val = parts[0].partition("=")
        if name == "app_session":
            return val, {p.split("=", 1)[0].lower(): (p.split("=", 1) + [""])[1] for p in parts[1:]}
    return None


def run_checks(port, token, page):
    sep = "&" if "?" in page else "?"
    good = f"app_session={token}"

    r = request(port, f"{page}{sep}token={token}")
    check("valid ?token= answers 302", r.status == 302, f"got {r.status}")
    loc = r.headers.get("Location", "")
    check("redirect goes to the same path", loc.split("?")[0] in (page.split("?")[0], f"http://127.0.0.1:{port}{page.split('?')[0]}"), loc)
    check("redirect drops the token", token not in loc and "token=" not in loc, loc)
    c = cookie_attrs(r)
    check("redirect sets app_session=<token>", c is not None and c[0] == token)
    if c:
        attrs = c[1]
        check("cookie is HttpOnly", "httponly" in attrs)
        check("cookie is SameSite=Strict", attrs.get("samesite", "").lower() == "strict")
        check("cookie has Path=/", attrs.get("path") == "/")

    r = request(port, f"/deep/path?a=1&token={token}&b=2")
    loc = r.headers.get("Location", "")
    check("deep link: 302 to the same path without the token",
          r.status == 302 and loc.split("?")[0].endswith("/deep/path") and "token" not in loc,
          f"{r.status} {loc}")

    r = request(port, page)
    check("no cookie answers 401", r.status == 401, f"got {r.status}")
    check("401 sets no cookie", cookie_attrs(r) is None)
    r = request(port, page, method="POST")
    check("POST without cookie answers 401", r.status == 401, f"got {r.status}")
    r = request(port, f"{page}{sep}token={'0' * len(token)}")
    check("wrong token answers 401", r.status == 401, f"got {r.status}")
    check("wrong token sets no cookie", cookie_attrs(r) is None)
    r = request(port, f"{page}{sep}token={token[:-1]}")
    check("truncated token answers 401", r.status == 401, f"got {r.status}")
    r = request(port, f"{page}{sep}token=")
    check("empty token answers 401", r.status == 401, f"got {r.status}")

    r = request(port, page, cookie=good)
    check("valid cookie is served", 200 <= r.status < 300, f"got {r.status}")
    r = request(port, page, cookie=f"theme=dark; {good}; other=1")
    check("valid cookie among others is served", 200 <= r.status < 300, f"got {r.status}")
    r = request(port, page, cookie="app_session=" + "f" * len(token))
    check("wrong cookie answers 401", r.status == 401, f"got {r.status}")
    r = request(port, page, cookie=f"app_session={token.upper() if token != token.upper() else token.lower()}")
    check("cookie compare is case-sensitive", r.status == 401, f"got {r.status}")

    r = request(port, page, cookie=good, host=f"localhost:{port}")
    check("Host localhost:<port> is served", 200 <= r.status < 300, f"got {r.status}")
    r = request(port, page, cookie=good, host=f"evil.example:{port}")
    check("Host evil.example:<port> answers 403", r.status == 403, f"got {r.status}")
    r = request(port, page, cookie=good, host="evil.example")
    check("Host evil.example answers 403", r.status == 403, f"got {r.status}")
    r = request(port, page, cookie=good, host=f"127.0.0.1:{port + 1}")
    check("Host with another port answers 403", r.status == 403, f"got {r.status}")
    r = request(port, page, cookie=good, host="127.0.0.1")
    check("Host without a port answers 403", r.status == 403, f"got {r.status}")
    r = request(port, f"{page}{sep}token={token}", host=f"evil.example:{port}")
    check("valid token with a bad Host answers 403", r.status == 403, f"got {r.status}")
    check("bad Host sets no cookie", cookie_attrs(r) is None)
    s = raw_status(port, f"GET {page} HTTP/1.0\r\nCookie: {good}\r\n\r\n".encode())
    check("no Host header is rejected", s is not None and 400 <= s < 500, f"got {s}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--path", default="/", help="a page served with 2xx once authenticated")
    ap.add_argument("command", nargs=argparse.REMAINDER, help="-- <command to start the backend>")
    args = ap.parse_args()
    cmd = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not cmd:
        ap.error("give the backend's command after --")

    port = free_port()
    token = secrets.token_hex(32)  # what Porthole sends: 64 hex characters
    with tempfile.TemporaryDirectory() as data:
        env = dict(os.environ, APP_PORT=str(port), APP_TOKEN=token, APP_DATA_DIR=data)
        proc = subprocess.Popen(cmd, env=env)
        try:
            wait_for(port, proc)
            run_checks(port, token, args.path)
        finally:
            proc.terminate()
            try:
                proc.wait(5)
            except subprocess.TimeoutExpired:
                proc.kill()
    if failures:
        sys.exit(f"\n{len(failures)} check(s) failed")
    print("\nall checks passed")


if __name__ == "__main__":
    main()

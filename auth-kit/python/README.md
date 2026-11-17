# porthole-auth (Python)

Porthole's [auth handshake](../../README.md#auth-handshake) as ASGI
middleware for FastAPI, Starlette and any other ASGI framework. One
module, standard library only, Python 3.9 or newer.

## Install

Copy [`porthole_auth/__init__.py`](porthole_auth/__init__.py) into your
project as `porthole_auth.py`, or install it from a checkout of this
repository:

```sh
pip install path/to/porthole/auth-kit/python
```

## Use

FastAPI or Starlette:

```python
import os
import uvicorn
from fastapi import FastAPI
from porthole_auth import PortholeAuth

app = FastAPI()
app.add_middleware(PortholeAuth)

uvicorn.run(app, host="127.0.0.1", port=int(os.environ["APP_PORT"]))
```

Any ASGI app:

```python
app = PortholeAuth(app)
```

It checks the `Host` header (403), swaps a valid `?token=` for the
`app_session` cookie with a 302, serves requests that carry the cookie,
and answers 401 to everything else. WebSocket connections without the
cookie are closed before they are accepted, which the server sends as a
403. Lifespan events pass straight through.

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `token` | `os.environ["APP_TOKEN"]` | The token to expect |
| `allow_standalone` | `False` | With no token, serve every request without auth (the `Host` check stays). Without this, a missing token raises `RuntimeError` |

Pass options as keyword arguments: `app.add_middleware(PortholeAuth,
allow_standalone=True)`. Starlette creates middleware when the app starts
serving, so with `add_middleware` a missing token shows up as a 500 on
every request (plus the error in `sidecar.log`) rather than a crash at
import. Either way, nothing is served without auth.

Standalone mode is for running the backend without Porthole while
developing. Make it an explicit choice, such as a flag:

```python
app.add_middleware(PortholeAuth, allow_standalone="--standalone" in sys.argv)
```

## Test

```sh
pip install -e '.[test]' && pytest
python3 ../conformance.py -- python3 tests/fixtures/fastapi_server.py
```

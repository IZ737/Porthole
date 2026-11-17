# Porthole auth kit

Middleware that does Porthole's [auth handshake](../README.md#auth-handshake)
for your sidecar backend, so the only thing that can use it is Porthole's
window. The README's spec is the source of truth; these follow it.

| Folder | For | Depends on |
| --- | --- | --- |
| [`node/`](node) | Express, Connect, plain `node:http` | Nothing (Node 18+) |
| [`python/`](python) | FastAPI, Starlette, any ASGI app | Nothing (Python 3.9+) |
| [`conformance.py`](conformance.py) | Checking any backend, in any language, against the spec | Python 3 |
| [`webview-e2e.sh`](webview-e2e.sh) | Checking a real webview completes the handshake with your app | A display (`xvfb-run` on Linux) |

Each middleware is one file and can be copied into your project, or
installed from this repository (they are not on npm or PyPI yet).

## Express

```js
const portholeAuth = require('porthole-auth');
app.use(portholeAuth());   // first, before your routes and static files
```

See [`node/`](node) and [`examples/hello-express`](../examples/hello-express).

## FastAPI and Starlette

```python
from porthole_auth import PortholeAuth
app.add_middleware(PortholeAuth)
```

See [`python/`](python) and [`examples/hello-fastapi`](../examples/hello-fastapi).

## Other languages

Go and ASP.NET helpers are planned. Until then, follow the
[spec](../README.md#auth-handshake) (it is about 30 lines in any
language; [`examples/hello-python`](../examples/hello-python) does it
with the standard library) and check your server with:

```sh
python3 auth-kit/conformance.py -- ./my-server --some-flag
```

## Running the tests

```sh
cd auth-kit/node && npm install && npm test
cd auth-kit/python && pip install -e '.[test]' && pytest

# The shared checks, against each middleware and example:
python3 auth-kit/conformance.py -- node auth-kit/node/test/fixtures/express-server.js
python3 auth-kit/conformance.py -- python3 auth-kit/python/tests/fixtures/fastapi_server.py
python3 auth-kit/conformance.py -- python3 examples/hello-python/server.py

# In a real webview, launched by Porthole:
xvfb-run -a auth-kit/webview-e2e.sh target/release/porthole \
  examples/hello-express/app.toml dev.porthole.hello-express
```

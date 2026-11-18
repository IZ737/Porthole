# porthole-auth (Node)

Porthole's [auth handshake](../../README.md#auth-handshake) as
middleware for Express, Connect and plain `node:http`. One file, no
dependencies, Node 18 or newer.

## Install

Copy [`index.js`](index.js) (and [`index.d.ts`](index.d.ts) for
TypeScript) into your project, or install it from a checkout of this
repository:

```sh
npm install --install-links path/to/porthole/auth-kit/node
```

`--install-links` copies the files instead of symlinking them, so
`porthole pack` bundles a working `node_modules`.

## Use

```js
const express = require('express');
const portholeAuth = require('porthole-auth');

const app = express();
app.use(portholeAuth());          // before routes and express.static
app.get('/api/things', ...);
app.listen(Number(process.env.APP_PORT), '127.0.0.1');
```

Without Express:

```js
const auth = portholeAuth();
http.createServer((req, res) => auth(req, res, () => handler(req, res)))
  .listen(Number(process.env.APP_PORT), '127.0.0.1');
```

It checks the `Host` header (403), swaps a valid `?token=` for the
`app_session` cookie with a 302, serves requests that carry the cookie,
and answers 401 to everything else.

### WebSockets

Upgrade requests skip Express middleware, so check them yourself in the
server's `upgrade` handler. `verifyUpgrade` answers and closes the socket
when the request is not allowed:

```js
const server = app.listen(port, '127.0.0.1');
server.on('upgrade', (req, socket, head) => {
  if (!auth.verifyUpgrade(req, socket)) return;
  wss.handleUpgrade(req, socket, head, (ws) => wss.emit('connection', ws, req));
});
```

(Keep the middleware in a variable, `const auth = portholeAuth()`, to use
it in both places.)

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `token` | `process.env.APP_TOKEN` | The token to expect |
| `allowStandalone` | `false` | With no token, serve every request without auth (the `Host` check stays). Without this, a missing token throws when the middleware is created |
| `onStandalone` | a warning on stderr | Called once when standalone mode is on |

Standalone mode is for running the backend without Porthole while
developing. Make it an explicit choice, such as a flag:

```js
app.use(portholeAuth({ allowStandalone: process.argv.includes('--standalone') }));
```

## Test

```sh
npm install && npm test
python3 ../conformance.py -- node test/fixtures/express-server.js
```

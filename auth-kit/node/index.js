'use strict';
// Porthole auth handshake for Node: Express, Connect, or plain `node:http`.
// No dependencies. The spec lives in Porthole's README ("Auth handshake");
// this file is a convenience and follows it.

const crypto = require('node:crypto');

const COOKIE = 'app_session';

/**
 * Build the middleware. Reads APP_TOKEN from the environment.
 *
 * Options:
 *   token            Use this token instead of process.env.APP_TOKEN.
 *   allowStandalone  When there is no token, serve every request without
 *                    auth instead of throwing. The Host check still applies.
 *   onStandalone     Called once, at setup, when standalone mode is active.
 *                    Default: a warning on stderr.
 *
 * Usage (Express):  app.use(portholeAuth());
 * Usage (node:http): const auth = portholeAuth();
 *                    http.createServer((req, res) => auth(req, res, () => handler(req, res)));
 */
function portholeAuth(options = {}) {
  const token = options.token !== undefined ? options.token : process.env.APP_TOKEN;
  const standalone = token === undefined || token === '';
  if (standalone) {
    if (!options.allowStandalone) {
      throw new Error(
        'porthole-auth: APP_TOKEN is not set. Porthole sets it when it starts the ' +
          'backend. To run the backend on its own without auth, pass ' +
          '{ allowStandalone: true }.'
      );
    }
    (options.onStandalone || warnStandalone)();
  }
  const expected = standalone ? null : Buffer.from(token, 'utf8');

  function middleware(req, res, next) {
    const verdict = check(req, expected);
    if (verdict.status === 'ok') return next();
    if (verdict.status === 'redirect') {
      res.statusCode = 302;
      res.setHeader('Set-Cookie', `${COOKIE}=${token}; HttpOnly; SameSite=Strict; Path=/`);
      res.setHeader('Location', verdict.location);
      res.setHeader('Cache-Control', 'no-store');
      res.setHeader('Content-Length', '0');
      return res.end();
    }
    const body = verdict.status === 403 ? 'Forbidden: bad Host header\n' : 'Unauthorized\n';
    res.statusCode = verdict.status;
    res.setHeader('Content-Type', 'text/plain; charset=utf-8');
    res.setHeader('Content-Length', Buffer.byteLength(body));
    res.setHeader('Cache-Control', 'no-store');
    return res.end(req.method === 'HEAD' ? undefined : body);
  }

  /**
   * Check a WebSocket upgrade request (`server.on('upgrade', ...)`), which
   * Express middleware never sees. Returns true to accept. On false, it has
   * already written an HTTP error response and closed the socket.
   * The handshake redirect doesn't apply: an upgrade must carry the cookie.
   */
  middleware.verifyUpgrade = function verifyUpgrade(req, socket) {
    let verdict = check(req, expected);
    if (verdict.status === 'redirect') verdict = { status: 401 };
    if (verdict.status === 'ok') return true;
    const reason = verdict.status === 403 ? 'Forbidden' : 'Unauthorized';
    socket.end(`HTTP/1.1 ${verdict.status} ${reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    return false;
  };

  return middleware;
}

// Returns { status: 'ok' | 'redirect' | 401 | 403, location? }.
function check(req, expected) {
  const port = req.socket && req.socket.localPort;
  if (!hostAllowed(req.headers.host, port)) return { status: 403 };
  if (expected === null) return { status: 'ok' };

  const url = req.originalUrl || req.url || '/';
  const q = url.indexOf('?');
  const path = q === -1 ? url : url.slice(0, q);
  const query = q === -1 ? '' : url.slice(q + 1);

  if (query) {
    const { given, rest } = takeToken(query);
    if (given !== null && equal(Buffer.from(given, 'utf8'), expected)) {
      return { status: 'redirect', location: safePath(path) + (rest ? '?' + rest : '') };
    }
  }
  const session = readCookie(req.headers.cookie, COOKIE);
  if (session !== null && equal(Buffer.from(session, 'utf8'), expected)) return { status: 'ok' };
  return { status: 401 };
}

function hostAllowed(host, port) {
  if (typeof host !== 'string' || !port) return false;
  const h = host.toLowerCase();
  return h === `127.0.0.1:${port}` || h === `localhost:${port}`;
}

// Splits `token` out of a raw query string, keeping the other parameters
// byte for byte.
function takeToken(query) {
  let given = null;
  const rest = [];
  for (const part of query.split('&')) {
    const eq = part.indexOf('=');
    const key = eq === -1 ? part : part.slice(0, eq);
    if (decode(key) === 'token') {
      if (given === null) given = eq === -1 ? '' : decode(part.slice(eq + 1));
    } else if (part !== '') {
      rest.push(part);
    }
  }
  return { given, rest: rest.join('&') };
}

function decode(s) {
  try {
    return decodeURIComponent(s.replace(/\+/g, ' '));
  } catch {
    return s;
  }
}

// A Location of `//evil.example/` would leave the app; keep it on this origin.
function safePath(path) {
  if (!path.startsWith('/')) return '/';
  return '/' + path.replace(/^[/\\]+/, '');
}

function readCookie(header, name) {
  if (typeof header !== 'string') return null;
  for (const pair of header.split(';')) {
    const eq = pair.indexOf('=');
    if (eq === -1) continue;
    if (pair.slice(0, eq).trim() === name) return pair.slice(eq + 1).trim();
  }
  return null;
}

function equal(a, b) {
  // timingSafeEqual needs equal lengths. The length of the token is not a
  // secret (it is always 64 hex characters).
  return a.length === b.length && crypto.timingSafeEqual(a, b);
}

function warnStandalone() {
  process.stderr.write(
    'porthole-auth: APP_TOKEN is not set; running standalone with auth OFF. ' +
      'Any local process or web page can call this server.\n'
  );
}

module.exports = portholeAuth;
module.exports.portholeAuth = portholeAuth;
module.exports.default = portholeAuth;

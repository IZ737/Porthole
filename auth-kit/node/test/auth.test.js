'use strict';
// Behaviour beyond the shared conformance checks (../../conformance.py).
const test = require('node:test');
const assert = require('node:assert');
const http = require('node:http');
const net = require('node:net');
const express = require('express');
const portholeAuth = require('..');

const TOKEN = 'a'.repeat(64);

async function serve(handler, onUpgrade) {
  const server = http.createServer(handler);
  if (onUpgrade) server.on('upgrade', onUpgrade);
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const port = server.address().port;
  return { port, close: () => new Promise((r) => server.close(r)) };
}

function get(port, path, { cookie, host, method = 'GET' } = {}) {
  return new Promise((resolve, reject) => {
    const headers = { Host: host || `127.0.0.1:${port}` };
    if (cookie) headers.Cookie = cookie;
    const req = http.request({ host: '127.0.0.1', port, path, method, headers }, (res) => {
      let body = '';
      res.on('data', (c) => (body += c));
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body }));
    });
    req.on('error', reject);
    req.end();
  });
}

function plain(auth) {
  return (req, res) => auth(req, res, () => res.end('app'));
}

test('throws without a token unless standalone is opted into', () => {
  const saved = process.env.APP_TOKEN;
  delete process.env.APP_TOKEN;
  try {
    assert.throws(() => portholeAuth(), /APP_TOKEN is not set/);
    let warned = false;
    portholeAuth({ allowStandalone: true, onStandalone: () => (warned = true) });
    assert.ok(warned);
  } finally {
    if (saved !== undefined) process.env.APP_TOKEN = saved;
  }
});

test('reads APP_TOKEN from the environment', async () => {
  process.env.APP_TOKEN = TOKEN;
  try {
    const s = await serve(plain(portholeAuth()));
    assert.strictEqual((await get(s.port, '/', { cookie: `app_session=${TOKEN}` })).status, 200);
    await s.close();
  } finally {
    delete process.env.APP_TOKEN;
  }
});

test('standalone mode serves without auth but still checks Host', async () => {
  const s = await serve(plain(portholeAuth({ token: '', allowStandalone: true, onStandalone() {} })));
  assert.strictEqual((await get(s.port, '/')).status, 200);
  assert.strictEqual((await get(s.port, '/', { host: 'evil.example' })).status, 403);
  await s.close();
});

test('redirect keeps other query parameters byte for byte', async () => {
  const s = await serve(plain(portholeAuth({ token: TOKEN })));
  const r = await get(s.port, `/a%20b/c?x=1&token=${TOKEN}&y=%2F&z`);
  assert.strictEqual(r.status, 302);
  assert.strictEqual(r.headers.location, '/a%20b/c?x=1&y=%2F&z');
  assert.strictEqual(r.headers['cache-control'], 'no-store');
  await s.close();
});

test('redirect never leaves the origin', async () => {
  const s = await serve(plain(portholeAuth({ token: TOKEN })));
  const r = await get(s.port, `//evil.example/x?token=${TOKEN}`);
  assert.strictEqual(r.status, 302);
  assert.strictEqual(r.headers.location, '/evil.example/x');
  await s.close();
});

test('a valid cookie lets an app use its own wrong ?token= parameter', async () => {
  const s = await serve(plain(portholeAuth({ token: TOKEN })));
  const r = await get(s.port, '/?token=reset-link', { cookie: `app_session=${TOKEN}` });
  assert.strictEqual(r.status, 200);
  await s.close();
});

test('HEAD gets a 401 without a body', async () => {
  const s = await serve(plain(portholeAuth({ token: TOKEN })));
  const r = await get(s.port, '/', { method: 'HEAD' });
  assert.strictEqual(r.status, 401);
  assert.strictEqual(r.body, '');
  await s.close();
});

test('works mounted on an Express router path', async () => {
  const app = express();
  app.use('/app', portholeAuth({ token: TOKEN }), (req, res) => res.send('app'));
  const s = await serve(app);
  const r = await get(s.port, `/app/page?token=${TOKEN}`);
  assert.strictEqual(r.status, 302);
  assert.strictEqual(r.headers.location, '/app/page');
  await s.close();
});

test('verifyUpgrade accepts the cookie and rejects everything else', async () => {
  const auth = portholeAuth({ token: TOKEN });
  const s = await serve(plain(auth), (req, socket) => {
    if (auth.verifyUpgrade(req, socket)) {
      socket.end('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n');
    }
  });
  const upgrade = (headers) =>
    new Promise((resolve, reject) => {
      const sock = net.connect(s.port, '127.0.0.1', () => {
        sock.write(`GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n${headers}\r\n`);
      });
      let buf = '';
      sock.on('data', (c) => (buf += c));
      sock.on('end', () => resolve(Number(buf.split(' ')[1])));
      sock.on('error', reject);
    });
  const host = `Host: 127.0.0.1:${s.port}\r\n`;
  assert.strictEqual(await upgrade(`${host}Cookie: app_session=${TOKEN}\r\n`), 101);
  assert.strictEqual(await upgrade(host), 401);
  assert.strictEqual(await upgrade(`Host: evil.example\r\nCookie: app_session=${TOKEN}\r\n`), 403);
  await s.close();
});

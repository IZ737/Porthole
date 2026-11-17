// A Porthole sidecar backend in Express. The auth kit's middleware does the
// whole handshake: it checks the Host header, swaps ?token= for the session
// cookie, and answers 401 to anything without it.
const express = require('express');
const portholeAuth = require('porthole-auth');

const PORT = Number(process.env.APP_PORT || 8000);
const app = express();

// Log the path only: the first request's query string carries the token.
// Porthole writes stdout to sidecar.log.
app.use((req, res, next) => {
  res.on('finish', () => console.log(`${req.method} ${req.path} ${res.statusCode}`));
  next();
});

// `node server.js --standalone` runs it without Porthole (no APP_TOKEN), with
// auth off. Without the flag, a missing APP_TOKEN is an error.
app.use(portholeAuth({ allowStandalone: process.argv.includes('--standalone') }));

app.get('/api/ping', (req, res) => res.json({ ok: true, node: process.version, pid: process.pid }));

app.get('/', (req, res) => {
  res.type('html').send(`<!doctype html>
<title>Hello from Express</title>
<style>body { font: 16px system-ui, sans-serif; margin: 3em; }</style>
<h1>Hello from Express on Node ${process.version}</h1>
<p>Backend pid ${process.pid}, listening on 127.0.0.1:${PORT}.</p>
<p>API call with the session cookie: <b id="api">…</b></p>
<script>
  fetch('/api/ping').then(r => r.json())
    .then(j => { document.getElementById('api').textContent = j.ok ? 'authenticated' : 'failed'; })
    .catch(e => { document.getElementById('api').textContent = 'failed: ' + e; });
</script>
`);
});

const server = app.listen(PORT, '127.0.0.1', () => console.log(`listening on 127.0.0.1:${PORT}`));

function stop() {
  console.log('shutting down');
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 1000).unref();
}
process.on('SIGTERM', stop);
process.on('SIGBREAK', stop); // Windows: Porthole sends Ctrl+Break

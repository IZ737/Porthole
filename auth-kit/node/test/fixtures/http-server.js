// A minimal node:http backend for conformance.py, no framework.
const http = require('node:http');
const portholeAuth = require('../..');

const auth = portholeAuth();
http
  .createServer((req, res) =>
    auth(req, res, () => {
      res.setHeader('Content-Type', 'text/plain');
      res.end(`hello ${req.method} ${req.url}\n`);
    })
  )
  .listen(Number(process.env.APP_PORT), '127.0.0.1');

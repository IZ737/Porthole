// A minimal Express backend for conformance.py: everything behind the auth.
const express = require('express');
const portholeAuth = require('../..');

const app = express();
app.use(portholeAuth());
app.all('*splat', (req, res) => res.type('text').send(`hello ${req.method} ${req.path}\n`));
app.listen(Number(process.env.APP_PORT), '127.0.0.1');

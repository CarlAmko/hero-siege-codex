import { createServer } from 'node:http';
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { items, runes, runewords, sets, searchItems, findItem } from './database.js';
import { evaluateItem } from './engine.js';
import { DEFAULT_CORPUS, loadSignals } from './social-source.js';
import { querySignals, summarizeCorpus } from './social.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const PORT = process.env.PORT || 3000;
const publicDir = join(__dirname, '..', 'public');

const server = createServer((req, res) => {
  const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`);
  const pathname = url.pathname;

  // CORS & headers
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');

  if (req.method === 'OPTIONS') {
    res.writeHead(204);
    res.end();
    return;
  }

  // API: Search items
  if (pathname === '/api/search' && req.method === 'GET') {
    const q = url.searchParams.get('q') || '';
    const rarity = url.searchParams.get('rarity') || '';
    const grade = url.searchParams.get('grade') || '';
    const slot = url.searchParams.get('slot') || '';

    const results = searchItems(q, { rarity, grade, slot });
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(results.slice(0, 100)));
    return;
  }

  // API: Evaluate item
  if (pathname === '/api/evaluate' && req.method === 'POST') {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      try {
        const payload = JSON.parse(body || '{}');
        const { item, name, quality = 100, sockets = null, seasonPhase = 'mid' } = payload;

        let targetItem = item;
        if (!targetItem && name) {
          const match = findItem(name);
          if (match) targetItem = match.data;
        }

        const evaluation = evaluateItem(targetItem, { quality, sockets, seasonPhase });
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(evaluation));
      } catch (err) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: err.message }));
      }
    });
    return;
  }

  // API: All items metadata
  if (pathname === '/api/items' && req.method === 'GET') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(items));
    return;
  }

  // API: Runes & Runewords
  if (pathname === '/api/runes' && req.method === 'GET') {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ runes, runewords, sets }));
    return;
  }

  // API: Social signals (Discord trade/price-check corpus)
  if ((pathname === '/api/social/query' || pathname === '/api/social/summary') && req.method === 'GET') {
    const corpus = process.env.HS_SOCIAL_CORPUS || DEFAULT_CORPUS;
    if (!existsSync(corpus)) {
      res.writeHead(503, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
          error: `no social corpus at ${corpus}`,
          hint: 'ingest with: node bin/hs-social.js ingest --channel <id>=<label>',
          corpus,
        }),
      );
      return;
    }

    const rows = loadSignals(corpus);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    if (pathname === '/api/social/summary') {
      res.end(JSON.stringify(summarizeCorpus(rows)));
      return;
    }
    const limit = Number(url.searchParams.get('limit'));
    res.end(
      JSON.stringify(querySignals(rows, url.searchParams.get('q') || '', limit > 0 ? { limit } : {})),
    );
    return;
  }

  // Static File Serving
  let filePath = join(publicDir, pathname === '/' ? 'index.html' : pathname);
  if (!existsSync(filePath)) {
    filePath = join(publicDir, 'index.html');
  }

  try {
    const content = readFileSync(filePath);
    const ext = filePath.split('.').pop() || 'html';
    const mimeTypes = {
      html: 'text/html; charset=utf-8',
      css: 'text/css; charset=utf-8',
      js: 'application/javascript; charset=utf-8',
      json: 'application/json; charset=utf-8',
      svg: 'image/svg+xml',
      png: 'image/png',
      ico: 'image/x-icon',
    };
    res.writeHead(200, { 'Content-Type': mimeTypes[ext] || 'text/plain' });
    res.end(content);
  } catch (err) {
    res.writeHead(404, { 'Content-Type': 'text/plain' });
    res.end('Not Found');
  }
});

server.listen(PORT, () => {
  console.log(`Hero Siege Economy server running at http://localhost:${PORT}`);
});

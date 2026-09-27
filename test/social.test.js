import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  extractEntity,
  extractPrice,
  normalizeMessage,
  parseQuery,
  priceStats,
  querySignals,
  summarizeCorpus,
} from '../src/social.js';
import {
  appendSignals,
  authHeader,
  detectAuthMode,
  fetchChannelMessages,
  fetchGuildChannels,
  fetchGuilds,
  importMessages,
  ingestChannels,
  ingestSearch,
  loadSignals,
  maskToken,
  parseChannelSpec,
  readClientTokens,
  resolveToken,
  searchGuildMessages,
  writeSnapshot,
} from '../src/social-source.js';

const CORPUS = 'trade';

const message = (overrides) => ({
  id: '1001',
  channel_id: '555',
  timestamp: '2026-09-01T12:00:00.000Z',
  author: { id: '987654321' },
  content: '',
  ...overrides,
});

const tempDir = () => mkdtempSync(join(tmpdir(), 'hs-social-'));

// --- extraction -----------------------------------------------------------

test('normalizeMessage extracts item, roll, sockets and rune price from a trade post', () => {
  const row = normalizeMessage(
    message({ content: "WTS Harlequinn's Crest 112% 2 sockets for 2 Jah" }),
    { channel: CORPUS },
  );

  assert.equal(row.entity.name, "Harlequinn's Crest");
  assert.equal(row.entity.grade, 'S');
  assert.equal(row.entity.rarity, 'satanic');
  assert.equal(row.quality, 112);
  assert.equal(row.sockets, 2);
  assert.equal(row.kind, 'sell');
  assert.equal(row.basis, 'asking');
  assert.equal(row.channel, CORPUS);
  assert.deepEqual(
    { currency: row.price.currency, rune: row.price.rune, amount: row.price.amount, tier: row.price.tier },
    { currency: 'rune', rune: 'Jah', amount: 2, tier: 3 },
  );

  // Identity is hashed, never stored raw.
  assert.equal(row.author.length, 8);
  assert.notEqual(row.author, '987654321');
  assert.equal(row.text.includes('987654321'), false);
});

test('garbage after an item name is not mistaken for a price', () => {
  const row = normalizeMessage(message({ content: 'WTS The Bucket 3 sockets lvl 100, mint' }));
  assert.equal(row.sockets, 3);
  assert.equal(row.price, null);
});

test('bare suffixes only count as gold in trade-intent messages', () => {
  assert.equal(extractPrice('2k dps with this build', { kind: 'build' }), null);
  assert.equal(extractPrice('wts 2k', { kind: 'sell' }).goldValue, 2000);
  assert.equal(extractPrice('500m gold', { kind: 'chat' }).goldValue, 500e6);
});

test('a piece count does not shadow the real price', () => {
  const row = normalizeMessage(message({ content: "WTS Anubi's Eternal Life 3 pieces, offer 2 Fawn" }));
  assert.equal(row.entity.name, "Anubi's Eternal Life");
  assert.equal(row.price.rune, 'Fawn');
  assert.equal(row.price.amount, 2);
});

test('extractEntity matches dictionary names exactly, not substrings', () => {
  assert.equal(extractEntity('anyone running old builds?'), null);
  assert.equal(extractEntity('pc the bucket')?.name, 'The Bucket');
  assert.equal(extractEntity('need Malice for leveling')?.name, 'Malice');
});

// --- query engine ---------------------------------------------------------

const corpus = [
  normalizeMessage(message({ id: '1', channel_id: 'a', content: "WTS The Bucket 115% 3 sockets 2000 gold" }), { channel: 'trade' }),
  normalizeMessage(message({ id: '2', channel_id: 'a', content: "WTS The Bucket 100% 500 gold" }), { channel: 'trade' }),
  normalizeMessage(message({ id: '3', channel_id: 'b', content: 'WTS The Bucket 3 Jah' }), { channel: 'price-check' }),
  normalizeMessage(message({ id: '4', channel_id: 'b', content: "PC Harlequinn's Crest 108%" }), { channel: 'price-check' }),
];

test('query separates currencies in the price bands instead of averaging them', () => {
  const result = querySignals(corpus, 'item:bucket');

  assert.equal(result.scoped.matched, 3);
  const gold = result.price.find((group) => group.key === 'gold');
  const rune = result.price.find((group) => group.key === 'rune:Jah');

  assert.deepEqual(
    { count: gold.count, min: gold.min, median: gold.median, max: gold.max },
    { count: 2, min: 500, median: 1250, max: 2000 },
  );
  // A Jah and 2000 gold never share a median.
  assert.deepEqual({ count: rune.count, median: rune.median }, { count: 1, median: 3 });
});

test('a quality filter excludes rows whose roll was never stated', () => {
  assert.deepEqual(querySignals(corpus, 'item:bucket quality:>=110').rows.map((row) => row.id), ['1']);
  // Snowflake ids ascend with time, so equal timestamps break newest-id-first.
  assert.deepEqual(querySignals(corpus, 'item:bucket quality:>=100').rows.map((row) => row.id), ['2', '1']);
});

test('free-text terms and kind filters narrow the corpus', () => {
  const result = querySignals(corpus, 'kind:sell currency:rune');
  assert.deepEqual(result.rows.map((row) => row.id), ['3']);
  assert.equal(querySignals(corpus, 'kind:price_check').scoped.matched, 1);
  assert.equal(querySignals(corpus, 'crest').scoped.matched, 1);
});

test('quoted queries keep multi-word item names as one token', () => {
  const parsed = parseQuery('item:"harlequinn\'s crest" since:2026-01-01');
  assert.equal(parsed.entity.name, "Harlequinn's Crest");
  assert.equal(parsed.since, Date.parse('2026-01-01'));
  assert.equal(querySignals(corpus, parsed).rows[0].id, '4');
});

test('priceStats groups rune denominations separately', () => {
  const stats = priceStats([
    { price: { currency: 'rune', rune: 'Jah', unit: '', amount: 2 }, basis: 'asking' },
    { price: { currency: 'rune', rune: 'Fawn', unit: '', amount: 1 }, basis: 'sold' },
    { price: { currency: 'rune', rune: 'Jah', unit: '', amount: 4 }, basis: 'asking' },
  ]);
  assert.deepEqual(stats.map((group) => [group.key, group.median]), [
    ['rune:Jah', 3],
    ['rune:Fawn', 1],
  ]);
});

test('summarizeCorpus reports resolution and intent mix', () => {
  const summary = summarizeCorpus(corpus);
  assert.equal(summary.total, 4);
  assert.equal(summary.withEntity, 4);
  assert.equal(summary.kinds.find((kind) => kind.kind === 'sell').count, 3);
});

// --- corpus I/O -----------------------------------------------------------

test('summarizeCorpus and priceStats fold a large corpus instead of spreading it', () => {
  const rows = Array.from({ length: 200_000 }, (_, index) => ({
    id: String(index),
    ts: 1_700_000_000_000 + index,
    entity: null,
    channel: 'general',
    kind: 'chat',
    price: index % 2 ? { currency: 'gold', goldValue: 1000 + index, unit: 'gold' } : null,
  }));

  const summary = summarizeCorpus(rows);
  assert.equal(summary.total, 200_000);
  assert.equal(summary.firstTs, 1_700_000_000_000);
  assert.equal(summary.lastTs, 1_700_000_199_999);
  assert.equal(summary.withPrice, 100_000);

  const [gold] = priceStats(rows);
  assert.deepEqual({ currency: gold.currency, count: gold.count, min: gold.min }, { currency: 'gold', count: 100_000, min: 1001 });
});

test('signals round-trip through the corpus file by id', () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    appendSignals(corpus.slice(0, 2), path);
    appendSignals([corpus[0], corpus[2]], path);
    const loaded = loadSignals(path);
    assert.equal(loaded.length, 3);
    assert.deepEqual(
      loaded.map((row) => row.id).sort(),
      ['1', '2', '3'],
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('importMessages normalizes a raw Discord export', () => {
  const dir = tempDir();
  const path = join(dir, 'dump.jsonl');
  try {
    writeFileSync(path, `${JSON.stringify(message({ id: '77', content: 'WTB Jah 1500 gold' }))}\n`);
    const rows = importMessages(path, { channel: 'trade' });
    assert.equal(rows.length, 1);
    assert.equal(rows[0].kind, 'buy');
    assert.equal(rows[0].price.goldValue, 1500);
    assert.equal(rows[0].channel, 'trade');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('loadSignals tolerates a missing corpus and a torn write', () => {
  const dir = tempDir();
  try {
    assert.deepEqual(loadSignals(join(dir, 'nope.jsonl')), []);
    const path = join(dir, 'signals.jsonl');
    writeFileSync(path, `${JSON.stringify(corpus[0])}\n{"id":"broken"`);
    assert.deepEqual(loadSignals(path).map((row) => row.id), ['1']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the committed snapshot round-trips and an append expands it', () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    appendSignals(corpus.slice(0, 2), path);
    const snap = writeSnapshot(path);
    assert.equal(snap.path, `${path}.gz`);
    assert.ok(snap.bytes > 0);

    // A fresh clone only has the snapshot, and must still read it.
    rmSync(path);
    assert.deepEqual(loadSignals(path).map((row) => row.id).sort(), ['1', '2']);

    // The first append expands the snapshot instead of replacing it.
    appendSignals([corpus[2]], path);
    assert.deepEqual(loadSignals(path).map((row) => row.id).sort(), ['1', '2', '3']);

    assert.throws(() => writeSnapshot(join(dir, 'absent.jsonl')), /no corpus/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('parseChannelSpec accepts ids with and without labels', () => {
  assert.deepEqual(parseChannelSpec('123=trade, 456=price check,789'), [
    { id: '123', label: 'trade' },
    { id: '456', label: 'price check' },
    { id: '789', label: '789' },
  ]);
});

// --- Discord puller -------------------------------------------------------

async function withDiscordStub(handler, run) {
  const stub = createServer(handler);
  await new Promise((resolve) => stub.listen(0, '127.0.0.1', resolve));
  const { port } = stub.address();
  try {
    return await run(`http://127.0.0.1:${port}`, stub);
  } finally {
    await new Promise((resolve) => stub.close(resolve));
  }
}

test('fetchChannelMessages pages with the before cursor and sends bot auth', async () => {
  const pages = [
    [message({ id: '5' }), message({ id: '4' })],
    [message({ id: '3' }), message({ id: '2' })],
    [message({ id: '1' })],
  ];
  const requests = [];

  await withDiscordStub((req, res) => {
    requests.push({ url: req.url, auth: req.headers.authorization });
    const before = new URL(req.url, 'http://localhost').searchParams.get('before');
    const index = before ? pages.findIndex((page) => page[page.length - 1].id === before) : -1;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(pages[index + 1] ?? []));
  }, async (apiBase) => {
    const messages = await fetchChannelMessages('555', { token: 'test-token', apiBase, max: 10, pageSize: 2 });
    assert.deepEqual(messages.map((m) => m.id), ['5', '4', '3', '2', '1']);
  });

  assert.equal(requests.length, 3);
  assert.equal(requests[0].auth, 'Bot test-token');
  assert.match(requests[1].url, /before=4/);
  assert.match(requests[2].url, /before=2/);
});

test('fetchChannelMessages retries after a 429', async () => {
  let calls = 0;
  await withDiscordStub((req, res) => {
    calls += 1;
    if (calls === 1) {
      res.writeHead(429, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ retry_after: 0.01 }));
      return;
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify([message({ id: '9', content: 'WTS The Bucket 1 Jah' })]));
  }, async (apiBase) => {
    const messages = await fetchChannelMessages('555', { token: 'test-token', apiBase, max: 5 });
    assert.equal(messages.length, 1);
  });
  assert.equal(calls, 2);
});

test('fetchChannelMessages stops at the since boundary instead of draining the channel', async () => {
  const pages = [
    [message({ id: '5', timestamp: '2026-09-05T12:00:00.000Z' }), message({ id: '4', timestamp: '2026-09-04T12:00:00.000Z' })],
    [message({ id: '3', timestamp: '2026-09-03T12:00:00.000Z' }), message({ id: '2', timestamp: '2026-09-02T12:00:00.000Z' })],
    [message({ id: '1', timestamp: '2026-09-01T12:00:00.000Z' })],
  ];
  let calls = 0;

  await withDiscordStub((req, res) => {
    calls += 1;
    const before = new URL(req.url, 'http://localhost').searchParams.get('before');
    const index = before ? pages.findIndex((page) => page[page.length - 1].id === before) : -1;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(pages[index + 1] ?? []));
  }, async (apiBase) => {
    const since = Date.parse('2026-09-02T18:00:00.000Z');
    const messages = await fetchChannelMessages('555', { token: 't', apiBase, max: 50, pageSize: 2, since });
    // '2' predates the window, and the page holding '1' is never requested.
    assert.deepEqual(messages.map((m) => m.id), ['5', '4', '3']);
  });

  assert.equal(calls, 2);
});

test('ingestChannels stores only rows inside the window and reports it', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    await withDiscordStub((req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify([
          message({ id: '21', timestamp: '2026-09-05T00:00:00.000Z', content: 'WTS The Bucket 900 gold' }),
          message({ id: '20', timestamp: '2026-08-01T00:00:00.000Z', content: 'WTS The Bucket 800 gold' }),
        ]),
      );
    }, async (apiBase) => {
      const since = Date.parse('2026-09-01T00:00:00.000Z');
      const summary = await ingestChannels([{ id: '555', label: 'trade' }], {
        token: 't',
        apiBase,
        corpusPath: path,
        max: 5,
        since,
      });
      assert.deepEqual({ added: summary.added, since: summary.since }, { added: 1, since });
    });
    assert.deepEqual(loadSignals(path).map((row) => row.id), ['21']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('searchGuildMessages honours the since window and stops paging past it', async () => {
  const group = (id, timestamp) => [
    { id, hit: true, content: 'WTS The Bucket 900 gold', channel_id: '901', timestamp, author: { id: '1' } },
  ];
  const pages = [
    [group('a', '2026-09-05T00:00:00.000Z'), group('b', '2026-09-04T00:00:00.000Z')],
    [group('c', '2026-08-20T00:00:00.000Z')],
  ];
  let calls = 0;

  await withDiscordStub((req, res) => {
    const offset = Number(new URL(req.url, 'http://localhost').searchParams.get('offset'));
    calls += 1;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ total_results: 50, messages: pages[offset / 2] ?? [] }));
  }, async (apiBase) => {
    const messages = await searchGuildMessages('215140390443941888', {
      token: 't',
      apiBase,
      query: 'bucket',
      max: 20,
      pageSize: 2,
      since: Date.parse('2026-09-01T00:00:00.000Z'),
    });
    assert.deepEqual(messages.map((message) => message.id), ['a', 'b']);
  });

  assert.equal(calls, 2);
});

test('a 5xx blip is retried instead of losing the channel', async () => {
  let calls = 0;
  await withDiscordStub((req, res) => {
    calls += 1;
    if (calls === 1) {
      res.writeHead(504, { 'Content-Type': 'text/html' });
      res.end('<html>gateway timeout');
      return;
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify([message({ id: '12', content: 'WTS The Bucket 900 gold' })]));
  }, async (apiBase) => {
    const messages = await fetchChannelMessages('555', {
      token: 't',
      apiBase,
      max: 5,
      retryDelayMs: 1,
    });
    assert.deepEqual(messages.map((m) => m.id), ['12']);
  });
  assert.equal(calls, 2);
});

test('ingestChannels appends once and skips messages already in the corpus', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    await withDiscordStub((req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify([message({ id: '11', content: 'WTS The Bucket 900 gold' })]));
    }, async (apiBase) => {
      const channels = [{ id: '555', label: 'trade' }];
      const first = await ingestChannels(channels, { token: 't', apiBase, corpusPath: path, max: 5 });
      const second = await ingestChannels(channels, { token: 't', apiBase, corpusPath: path, max: 5 });

      assert.deepEqual({ added: first.added, skipped: first.skipped }, { added: 1, skipped: 0 });
      assert.deepEqual({ added: second.added, skipped: second.skipped }, { added: 0, skipped: 1 });
    });
    assert.equal(loadSignals(path).length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- user-session credential ----------------------------------------------

// Credential resolution falls back to the environment and to the real client
// directory; tests must see neither.
delete process.env.HS_SOCIAL_TOKEN;
delete process.env.HS_SOCIAL_AUTH;
delete process.env.HS_SOCIAL_DISCORD_DIR;

// Synthetic fixture shaped like a client token (<24-28 chars>.<6>.<27-45>).
// Never a live credential: the real session token is read from
// HS_SOCIAL_TOKEN (see .env, gitignored) or the local client, and is never
// written into the repo or the corpus.
const USER_TOKEN = `MTIzNDU2Nzg5MDEyMzQ1Njc4.abcdef.${'x'.repeat(43)}`;
const MFA_TOKEN = `mfa.${'a'.repeat(84)}`;

test('auth scheme is chosen from the token shape, not the flag name', () => {
  assert.equal(detectAuthMode(USER_TOKEN), 'user');
  assert.equal(detectAuthMode(MFA_TOKEN), 'user');
  assert.equal(detectAuthMode('not-a-jwt'), 'bot');
  assert.equal(detectAuthMode('Bot abc.def.ghi'), 'bot');

  // A user token goes in bare; a bot token keeps the "Bot " prefix.
  assert.equal(authHeader(USER_TOKEN), USER_TOKEN);
  assert.equal(authHeader(USER_TOKEN, 'auto'), USER_TOKEN);
  assert.equal(authHeader('raw-bot-token'), 'Bot raw-bot-token');
  assert.equal(authHeader('Bot raw-bot-token', 'bot'), 'Bot raw-bot-token');
  // Forcing the mode wins over the shape.
  assert.equal(authHeader(USER_TOKEN, 'bot'), `Bot ${USER_TOKEN}`);
  assert.equal(authHeader('Bot x', 'user'), 'x');
});

test('maskToken keeps the credential out of logs', () => {
  assert.equal(maskToken(USER_TOKEN).includes(USER_TOKEN), false);
  assert.match(maskToken(USER_TOKEN), /^MTIzND…xxxx$/);
  assert.equal(maskToken(''), '(none)');
});

test('readClientTokens finds session tokens in the client LevelDB and ignores lookalikes', () => {
  const dir = tempDir();
  try {
    const leveldb = join(dir, 'Local Storage', 'leveldb');
    mkdirSync(leveldb, { recursive: true });
    // Both the real session token and an unrelated encrypted blob the client
    // stores under a "token" key — the latter 401s and must not win.
    writeFileSync(
      join(leveldb, '000005.ldb'),
      `\u0000\u0001token\u0002"token":"${'.eJxNzDEOgzAMBdCroKw'.repeat(10)}"\u0000${USER_TOKEN}\u0000`,
      'binary',
    );
    writeFileSync(join(leveldb, '006066.log'), `\u0000${USER_TOKEN}\u0000`, 'binary');

    const found = readClientTokens([dir]);
    assert.deepEqual(found.map((hit) => hit.token), [USER_TOKEN]);
    assert.match(found[0].source, /000005\.ldb$/);

    assert.deepEqual(readClientTokens([join(dir, 'nope')]), []);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('resolveToken falls through rejected candidates and validates the session token', async () => {
  const dir = tempDir();
  const seen = [];
  try {
    const leveldb = join(dir, 'Local Storage', 'leveldb');
    mkdirSync(leveldb, { recursive: true });
    const stale = 'MTY2MjE4OTgwMDI4MDU1NTU0.AAAAaa.staleStaleStaleStaleStaleStaleStale';
    writeFileSync(join(leveldb, '000005.ldb'), `\u0000${stale}\u0000${USER_TOKEN}\u0000`, 'binary');

    await withDiscordStub((req, res) => {
      seen.push({ url: req.url, auth: req.headers.authorization });
      if (req.headers.authorization === stale) {
        res.writeHead(401, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ message: '401: Unauthorized' }));
        return;
      }
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ id: '166218980028055554', username: 'okma' }));
    }, async (apiBase) => {
      const credential = await resolveToken({ dir, apiBase });
      assert.equal(credential.token, USER_TOKEN);
      assert.equal(credential.auth, 'user');
      assert.equal(credential.user.username, 'okma');
      assert.equal(credential.source, join(leveldb, '000005.ldb'));
    });

    assert.deepEqual(seen.map((request) => request.auth), [stale, USER_TOKEN]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('resolveToken reports what it tried when nothing works', async () => {
  const dir = tempDir();
  try {
    const leveldb = join(dir, 'Local Storage', 'leveldb');
    mkdirSync(leveldb, { recursive: true });
    writeFileSync(join(leveldb, '000005.ldb'), `\u0000${USER_TOKEN}\u0000`, 'binary');

    await withDiscordStub((req, res) => {
      res.writeHead(401, { 'Content-Type': 'application/json' });
      res.end('{}');
    }, async (apiBase) => {
      await assert.rejects(
        () => resolveToken({ dir, apiBase }),
        (error) => error.message.includes('no usable Discord credential') && !error.message.includes(USER_TOKEN),
      );
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('resolveToken requires a credential instead of silently going anonymous', async () => {
  const dir = tempDir();
  try {
    await assert.rejects(() => resolveToken({ dir, validate: false }), /no Discord credential found/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('an explicit bot token still authenticates as a bot', async () => {
  const seen = [];
  await withDiscordStub((req, res) => {
    seen.push(req.headers.authorization);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: '1', username: 'botty' }));
  }, async (apiBase) => {
    const credential = await resolveToken({ token: 'my-bot-token', apiBase });
    assert.equal(credential.auth, 'bot');
  });
  assert.deepEqual(seen, ['Bot my-bot-token']);
});

// --- guild discovery and keyword harvest ----------------------------------

test('fetchGuilds and fetchGuildChannels flatten the discovery payloads', async () => {
  await withDiscordStub((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    if (req.url.startsWith('/users/@me/guilds')) {
      res.end(JSON.stringify([{ id: '215140390443941888', name: 'Hero Siege', owner: false }]));
      return;
    }
    res.end(
      JSON.stringify([
        { id: '900', type: 4, name: 'TRADE' },
        { id: '901', type: 0, name: 'price-check', parent_id: '900' },
        { id: '902', type: 0, name: 'general', parent_id: '900' },
        { id: '903', type: 2, name: 'voice', parent_id: '900' },
      ]),
    );
  }, async (apiBase) => {
    const guilds = await fetchGuilds({ token: 't', apiBase });
    assert.deepEqual(guilds, [{ id: '215140390443941888', name: 'Hero Siege' }]);

    const channels = await fetchGuildChannels('215140390443941888', { token: 't', apiBase });
    assert.deepEqual(channels.map((channel) => channel.name), ['general', 'price-check', 'voice']);
    assert.equal(channels[0].parent, 'TRADE');
    assert.equal(channels[0].type, 'text');
    assert.equal(channels[2].type, 'voice');
  });
});

test('searchGuildMessages keeps the marked hit, pages by offset, and scopes to channels', async () => {
  const requests = [];
  const group = (id, hit) => [
    { id: `${id}-context`, content: 'context', channel_id: '901', timestamp: '2026-09-01T00:00:00.000Z', author: { id: '1' } },
    { id, hit, content: 'WTS The Bucket 1500 gold', channel_id: '901', timestamp: '2026-09-01T00:01:00.000Z', author: { id: '1' } },
  ];

  await withDiscordStub((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    requests.push({
      content: url.searchParams.get('content'),
      offset: url.searchParams.get('offset'),
      channels: url.searchParams.getAll('channel_id'),
      order: `${url.searchParams.get('sort_by')}/${url.searchParams.get('sort_order')}`,
    });
    const offset = Number(url.searchParams.get('offset'));
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(
      JSON.stringify({
        total_results: 2,
        messages: offset === 0 ? [group('a', true)] : [group('b', true)],
      }),
    );
  }, async (apiBase) => {
    const messages = await searchGuildMessages('215140390443941888', {
      token: 't',
      apiBase,
      query: 'bucket',
      channelIds: ['901', '902'],
      max: 10,
      pageSize: 1,
    });
    assert.deepEqual(messages.map((message) => message.id), ['a', 'b']);
  });

  assert.deepEqual(requests, [
    { content: 'bucket', offset: '0', channels: ['901', '902'], order: 'timestamp/desc' },
    { content: 'bucket', offset: '1', channels: ['901', '902'], order: 'timestamp/desc' },
  ]);
});

test('ingestSearch labels hits by channel and skips what is already stored', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    await withDiscordStub((req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(
        JSON.stringify({
          total_results: 1,
          messages: [
            [
              {
                id: '920001',
                hit: true,
                content: 'WTS The Bucket 2 Jah',
                channel_id: '901',
                timestamp: '2026-09-09T12:00:00.000Z',
                author: { id: '42' },
              },
            ],
          ],
        }),
      );
    }, async (apiBase) => {
      const channelNames = new Map([['901', 'price-check']]);
      const options = { token: 't', apiBase, corpusPath: path, query: 'bucket', max: 10, channelNames };

      const first = await ingestSearch('215140390443941888', options);
      const second = await ingestSearch('215140390443941888', options);

      assert.deepEqual({ added: first.added, skipped: first.skipped }, { added: 1, skipped: 0 });
      assert.deepEqual({ added: second.added, skipped: second.skipped }, { added: 0, skipped: 1 });
    });

    const rows = loadSignals(path);
    assert.equal(rows.length, 1);
    assert.equal(rows[0].channel, 'price-check');
    assert.equal(rows[0].price.rune, 'Jah');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a channel the credential cannot read is reported, not fatal', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    await withDiscordStub((req, res) => {
      if (req.url.includes('/channels/403/')) {
        res.writeHead(403, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ message: 'Missing Access', code: 50001 }));
        return;
      }
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify([message({ id: '31', content: 'WTS The Bucket 900 gold' })]));
    }, async (apiBase) => {
      const summary = await ingestChannels(
        [
          { id: '403', label: 'trade-listings' },
          { id: '200', label: 'general' },
        ],
        { token: 't', apiBase, corpusPath: path, max: 5 },
      );

      assert.deepEqual({ added: summary.added, skipped: summary.skipped }, { added: 1, skipped: 0 });
      assert.deepEqual(summary.channels.map((channel) => channel.label), ['general']);
      assert.equal(summary.errors.length, 1);
      assert.match(summary.errors[0].message, /403 Missing Access/);
    });

    // The readable channel's rows survive the unreadable one.
    assert.deepEqual(loadSignals(path).map((row) => row.id), ['31']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- process surfaces -----------------------------------------------------

const runProcess = (args, options = {}) =>
  new Promise((resolve, reject) => {
    const child = spawn(process.execPath, args, { cwd: options.cwd, env: { ...process.env, ...options.env } });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => (stdout += chunk));
    child.stderr.on('data', (chunk) => (stderr += chunk));
    child.on('error', reject);
    if (options.stdin !== undefined) child.stdin.end(options.stdin);
    if (options.timeout) {
      const timer = setTimeout(() => child.kill('SIGKILL'), options.timeout);
      child.on('close', () => clearTimeout(timer));
    }
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });

test('the MCP server answers initialize and tools/call over stdio', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  try {
    appendSignals([normalizeMessage(message({ content: 'WTS The Bucket 1500 gold' }))], path);
    const stdin = [
      JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {} }),
      JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list' }),
      JSON.stringify({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'hs_social_query', arguments: { query: 'item:bucket' } } }),
      JSON.stringify({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'hs_social_nope' } }),
    ].join('\n') + '\n';

    const { stdout } = await runProcess([join(process.cwd(), 'bin', 'hs-social-mcp.js')], {
      env: { HS_SOCIAL_CORPUS: path },
      stdin,
    });
    const replies = stdout.trim().split('\n').map((line) => JSON.parse(line));

    assert.equal(replies[0].result.serverInfo.name, 'hs-social');
    assert.ok(replies[1].result.tools.some((tool) => tool.name === 'hs_social_query'));
    const payload = JSON.parse(replies[2].result.content[0].text);
    assert.equal(payload.scoped.matched, 1);
    assert.equal(replies[3].result.isError, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the HTTP server exposes the social query and summary endpoints', async () => {
  const dir = tempDir();
  const path = join(dir, 'signals.jsonl');
  const port = 3400 + Math.floor(Math.random() * 200);
  let child;
  try {
    appendSignals([normalizeMessage(message({ content: 'WTS The Bucket 1500 gold' }))], path);
    child = spawn(process.execPath, [join(process.cwd(), 'src', 'server.js')], {
      env: { ...process.env, PORT: String(port), HS_SOCIAL_CORPUS: path },
    });

    const query = await poll(() => fetch(`http://127.0.0.1:${port}/api/social/query?q=item:bucket`));
    assert.equal(query.status, 200);
    const body = await query.json();
    assert.equal(body.scoped.matched, 1);
    assert.equal(body.rows[0].price.goldValue, 1500);

    const summary = await (await fetch(`http://127.0.0.1:${port}/api/social/summary`)).json();
    assert.equal(summary.total, 1);
  } finally {
    child?.kill('SIGKILL');
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the HTTP server reports a missing corpus instead of an empty answer', async () => {
  const dir = tempDir();
  const port = 3650 + Math.floor(Math.random() * 200);
  let child;
  try {
    child = spawn(process.execPath, [join(process.cwd(), 'src', 'server.js')], {
      env: { ...process.env, PORT: String(port), HS_SOCIAL_CORPUS: join(dir, 'missing.jsonl') },
    });

    const response = await poll(() => fetch(`http://127.0.0.1:${port}/api/social/query`));
    assert.equal(response.status, 503);
    assert.match((await response.json()).error, /no social corpus/);
  } finally {
    child?.kill('SIGKILL');
    rmSync(dir, { recursive: true, force: true });
  }
});

async function poll(attempt, tries = 40) {
  let lastError;
  for (let i = 0; i < tries; i++) {
    try {
      return await attempt();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  throw lastError;
}

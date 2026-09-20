/**
 * Hero Siege social-signal extraction and query engine.
 *
 * Discord trade/price-check chatter is the densest public record of the Hero
 * Siege economy: what is moving, at what price, in what condition, and which
 * zones players report dropping it from. This module turns raw messages into
 * structured signal rows — entity, quality, sockets, price — resolved against
 * the dictionaries in `database.js`, then answers questions over those rows.
 *
 * Pure and deterministic: no fs, no network, no clock except `parseWhen`'s
 * relative-date helper. `social-source.js` owns I/O; `bin/hs-social.js` owns
 * presentation.
 */

import { createHash } from 'node:crypto';
import { items, runes, runewords, sets, findItem, normalizeKey } from './database.js';

/** Message intent, first matching rule wins. */
export const SIGNAL_KINDS = ['sell', 'buy', 'price_check', 'build', 'bug', 'chat'];

/** Whether a quoted number is what the seller wants, what a buyer bid, or what it actually went for. */
export const PRICE_BASES = ['asking', 'bid', 'sold', 'unknown'];

const DEFAULT_MAX_TEXT = 400;

// --- entity index ---------------------------------------------------------

const ENTITY_SOURCES = [
  ['item', items],
  ['runeword', runewords],
  ['set', sets],
];

const entityByKey = new Map();
for (const [type, list] of ENTITY_SOURCES) {
  if (!Array.isArray(list)) continue;
  for (const data of list) {
    const key = normalizeKey(data?.name);
    if (key && !entityByKey.has(key)) entityByKey.set(key, { type, data });
  }
}

const MAX_ENTITY_TOKENS = [...entityByKey.keys()].reduce(
  (max, key) => Math.max(max, key.split(' ').length),
  1,
);

const runesByKey = new Map();
for (const rune of Array.isArray(runes) ? runes : []) {
  const key = normalizeKey(rune?.name);
  if (key && !runesByKey.has(key)) runesByKey.set(key, rune);
}

/**
 * Flatten a dictionary hit into the stable shape stored on a signal row, so a
 * later dataset change cannot change the shape of an existing corpus.
 */
export function summarizeEntity({ type, data }) {
  return {
    type,
    id: data?.id ?? null,
    name: data?.name ?? null,
    rarity: data?.rarity ?? null,
    grade: data?.grade ?? null,
    slot: data?.slot ?? null,
  };
}

// --- extraction -----------------------------------------------------------

const normalizeTokens = (text) => normalizeKey(text).split(' ').filter(Boolean);

const QUALITY_RE = /\b(\d{1,3})\s*(?:%|pct\b|percent\b)/i;
const SOCKETS_RE = /\b(\d)\s*(?:sockets?|socks?|socketed)\b/i;
const SOCKETS_OS_RE = /\b(\d)\s*os\b/i;

const GOLD_UNITS = { k: 1e3, kk: 1e6, m: 1e6, b: 1e9, t: 1e12, x: 1 };
/** Below this, an unsuffixed number in a trade post is a count, not a price. */
const MIN_BARE_GOLD = 100;
const PRICE_TOKEN_RE = /^(\d+(?:[.,]\d+)?)(k|kk|m|b|t|x)?$/;
const RUNE_WORDS = new Set(['hr', 'hrs', 'rune', 'runes']);
const GOLD_WORDS = new Set(['g', 'gold', 'gss', 'gp']);

const KIND_RULES = [
  ['sell', /\b(wts|selling|sell|sellout)\b/i],
  ['buy', /\b(wtb|buying|buy|iso|lf)\b/i],
  ['price_check', /\b(pc|p\/c|pricecheck|price check|whats it worth|what's it worth)\b/i],
  ['build', /\b(build|skill tree|dps|gear check|rotation)\b/i],
  ['bug', /\b(bug|crash|error|disconnect|hotfix|nerf)\b/i],
];

const cleanToken = (token) => token.replace(/^[^0-9a-z%]+|[^0-9a-z%]+$/gi, '');

/** Count words that follow their number ("3 sockets"); the number is never a price. */
const COUNT_SUFFIX = /^(socks?|sockets?|socketed|os)$/;
/** Labels that precede their number ("lvl 100", "tier 3"); same story. */
const COUNT_PREFIX = /^(lvls?|level|clvl|tier|act|characters?|players?)$/;

/**
 * Longest leftmost dictionary match against a message. Exact window matching
 * (rather than `findItem`'s substring fallback) keeps chat filler out of the
 * index — "old" must not resolve to the Old rune, "mask" must not resolve to
 * whichever mask sorts first.
 */
export function extractEntity(text) {
  const tokens = normalizeTokens(text);
  for (let i = 0; i < tokens.length; i++) {
    for (let n = Math.min(MAX_ENTITY_TOKENS, tokens.length - i); n >= 1; n--) {
      const hit = entityByKey.get(tokens.slice(i, i + n).join(' '));
      if (hit) return summarizeEntity(hit);
    }
  }
  return null;
}

/** Satanic Dice quality roll. Values outside 1-200 are treated as noise. */
export function extractQuality(text) {
  const match = QUALITY_RE.exec(text);
  if (!match) return null;
  const value = Number(match[1]);
  return value >= 1 && value <= 200 ? value : null;
}

export function extractSockets(text) {
  const match = SOCKETS_RE.exec(text) || SOCKETS_OS_RE.exec(text);
  if (!match) return null;
  const value = Number(match[1]);
  return value >= 1 && value <= 6 ? value : null;
}

/**
 * First plausible price in a message. Rune denominations are matched by name
 * against `data/runes.json`; everything with a k/m/b/t suffix, or an explicit
 * gold word, is gold. Bare suffixes only count as gold in trade-intent
 * messages, so "2k dps" in build chatter is not read as a price.
 *
 * @returns {{amount:number, currency:'gold'|'rune', rune:string|null, tier:number|null, unit:string, goldValue:number|null, raw:string}|null}
 */
export function extractPrice(text, { kind = 'chat' } = {}) {
  const tradeIntent = kind === 'sell' || kind === 'buy' || kind === 'price_check';
  const tokens = String(text ?? '')
    .toLowerCase()
    .split(/\s+/)
    .map(cleanToken)
    .filter(Boolean);

  for (let i = 0; i < tokens.length; i++) {
    const match = PRICE_TOKEN_RE.exec(tokens[i]);
    if (!match) continue;

    const amount = Number(match[1].replace(',', '.'));
    if (!Number.isFinite(amount) || amount <= 0) continue;

    const unit = (match[2] || '').toLowerCase();
    const next = tokens[i + 1] || '';
    const prev = tokens[i - 1] || '';
    const multiplier = unit in GOLD_UNITS ? GOLD_UNITS[unit] : 1;

    // "112% 2 sockets", "lvl 100" — counts that are never prices.
    if (COUNT_SUFFIX.test(next) || COUNT_PREFIX.test(prev)) continue;
    const raw = `${match[1]}${unit}`;

    // "3 Jah" is a Jah-denominated price; "Jah 1500 gold" is a gold one, so
    // only the token *after* the amount can name the denomination.
    const rune = runesByKey.get(normalizeKey(next));
    if (rune) {
      return { amount, currency: 'rune', rune: rune.name, tier: rune.tier ?? null, unit: '', goldValue: null, raw };
    }

    const runeWord = RUNE_WORDS.has(next) || RUNE_WORDS.has(prev);
    if (runeWord) {
      const word = RUNE_WORDS.has(next) ? next : prev;
      const high = word.startsWith('hr') || word === 'high rune';
      return { amount, currency: 'rune', rune: null, tier: high ? 3 : null, unit: high ? 'high' : 'unspecified', goldValue: null, raw };
    }

    const goldWord = GOLD_WORDS.has(next) || GOLD_WORDS.has(prev);
    const suffixed = unit !== '' && unit !== 'x';
    // A bare number is weak evidence: "3 pieces" is a count, not a 3g price.
    if (goldWord || (tradeIntent && (suffixed || amount >= MIN_BARE_GOLD))) {
      return {
        amount,
        currency: 'gold',
        rune: null,
        tier: null,
        unit: suffixed ? unit : '',
        goldValue: amount * multiplier,
        raw,
      };
    }
  }
  return null;
}

/** Asking vs. bid vs. actually-sold — the number means something different in each case. */
export function extractBasis(text) {
  if (/\b(sold|went for|sells? for|bought at)\b/i.test(text)) return 'sold';
  if (/\b(wtb|buying|bidding)\b/i.test(text)) return 'bid';
  if (/\b(wts|selling|asking|buyout|b\/o)\b/i.test(text)) return 'asking';
  return 'unknown';
}

export function classifyKind(text) {
  for (const [kind, re] of KIND_RULES) {
    if (re.test(text)) return kind;
  }
  return 'chat';
}

export function hashAuthor(authorId) {
  return createHash('sha1').update(String(authorId ?? 'unknown')).digest('hex').slice(0, 8);
}

export function parseTimestamp(value) {
  if (value === null || value === undefined) return null;
  if (typeof value === 'number') return value < 1e12 ? Math.round(value * 1000) : Math.round(value);
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

/**
 * Normalize one raw Discord message (REST shape or an already-flat object)
 * into a signal row. Author identity is stored as a short hash, never a name
 * or id; text is capped unless `keepText: false` drops it entirely.
 */
export function normalizeMessage(raw, options = {}) {
  const channelId = String(raw?.channel_id ?? raw?.channelId ?? options.channelId ?? 'unknown');
  const authorId = raw?.author?.id ?? raw?.author_id ?? raw?.authorId ?? null;
  const content = String(raw?.content ?? raw?.text ?? '')
    .replace(/\s+/g, ' ')
    .trim();
  const ts = parseTimestamp(raw?.timestamp ?? raw?.ts ?? raw?.createdAt ?? raw?.date);
  const kind = classifyKind(content);
  const keepText = options.keepText !== false;
  const maxText = options.maxTextLength ?? DEFAULT_MAX_TEXT;

  return {
    id: String(raw?.id ?? `${channelId}:${ts ?? 0}:${hashAuthor(authorId)}:${content.slice(0, 32)}`),
    channelId,
    channel: String(options.channel ?? raw?.channel ?? channelId),
    ts,
    author: hashAuthor(authorId),
    kind,
    basis: extractBasis(content),
    entity: extractEntity(content),
    quality: extractQuality(content),
    sockets: extractSockets(content),
    price: extractPrice(content, { kind }),
    text: keepText ? content.slice(0, maxText) : null,
  };
}

export function normalizeMessages(raws, options = {}) {
  if (!Array.isArray(raws)) return [];
  return raws.map((raw) => normalizeMessage(raw, options));
}

// --- query language -------------------------------------------------------

const splitOperator = (value) => {
  const match = /^(>=|<=|>|<|=)?\s*(.+)$/.exec(value);
  return { op: match?.[1] || null, value: (match?.[2] ?? value).trim() };
};

const NUMERIC_KEYS = new Set(['quality', 'sockets', 'price']);
const TEXT_KEYS = new Set(['rarity', 'grade', 'slot', 'channel', 'author', 'kind', 'basis', 'currency']);

/** Split on whitespace, but keep `item:"the bucket"` as one token. */
export function tokenizeQuery(input) {
  const out = [];
  let current = '';
  let quote = null;
  for (const ch of String(input ?? '')) {
    if (quote) {
      if (ch === quote) quote = null;
      else current += ch;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (/\s/.test(ch)) {
      if (current) out.push(current);
      current = '';
      continue;
    }
    current += ch;
  }
  if (current) out.push(current);
  return out;
}

/** `7d` / `24h` / `30m` / `2w` relative to now, or any `Date.parse`-able absolute date. */
export function parseWhen(value, now = Date.now()) {
  const relative = /^(\d+)([mhdw])$/.exec(String(value).trim());
  if (relative) {
    const multiplier = { m: 60e3, h: 3600e3, d: 86400e3, w: 604800e3 }[relative[2]];
    return now - Number(relative[1]) * multiplier;
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? null : parsed;
}

/**
 * Parse a query string into filters. Free-text tokens become AND terms;
 * `key:value` narrows by field; numeric fields accept `>=`, `<=`, `>`, `<`, `=`.
 *
 *   item:"harlequinn's crest" quality:>=110 sockets:2 currency:gold since:7d
 */
export function parseQuery(input) {
  const filters = {
    entity: null,
    rune: null,
    rarity: null,
    grade: null,
    slot: null,
    channel: null,
    author: null,
    kind: null,
    basis: null,
    currency: null,
    quality: null,
    sockets: null,
    price: null,
    since: null,
    until: null,
    terms: [],
    sort: 'recent',
    limit: 50,
  };

  for (const token of tokenizeQuery(input)) {
    const separator = token.indexOf(':');
    if (separator <= 0) {
      filters.terms.push(token.toLowerCase());
      continue;
    }

    const key = token.slice(0, separator).toLowerCase();
    const { op, value } = splitOperator(token.slice(separator + 1));

    if (key === 'item' || key === 'entity') {
      const hit = findItem(value);
      if (hit && hit.type === 'rune') {
        filters.rune = normalizeKey(hit.data.name);
      } else if (hit) {
        const entity = summarizeEntity({ type: hit.type, data: hit.data });
        filters.entity = { ...entity, key: normalizeKey(entity.name) };
      } else {
        filters.entity = { type: null, id: null, name: value, key: normalizeKey(value) };
      }
    } else if (key === 'rune') {
      const hit = findItem(value);
      filters.rune = normalizeKey(hit && hit.type === 'rune' ? hit.data.name : value);
    } else if (NUMERIC_KEYS.has(key)) {
      const numeric = Number(value);
      filters[key] = Number.isFinite(numeric) ? { op: op || '=', value: numeric } : null;
    } else if (key === 'since' || key === 'until') {
      filters[key] = parseWhen(value);
    } else if (key === 'limit') {
      const limit = Number(value);
      if (Number.isFinite(limit) && limit > 0) filters.limit = Math.min(1000, Math.trunc(limit));
    } else if (key === 'sort') {
      if (['recent', 'price', 'quality'].includes(value)) filters.sort = value;
    } else if (TEXT_KEYS.has(key)) {
      filters[key] = value.toLowerCase();
    } else {
      filters.terms.push(token.toLowerCase());
    }
  }

  return filters;
}

const compare = (actual, op, expected) => {
  switch (op) {
    case '>':
      return actual > expected;
    case '>=':
      return actual >= expected;
    case '<':
      return actual < expected;
    case '<=':
      return actual <= expected;
    default:
      return actual === expected;
  }
};

const within = (actual, constraint) =>
  actual !== null && actual !== undefined && compare(actual, constraint.op, constraint.value);

const priceValue = (price) => (price.currency === 'gold' ? price.goldValue : price.amount);

function matchesFilters(row, filters) {
  if (filters.entity) {
    const entity = row.entity;
    if (!entity) return false;
    const idMatch = filters.entity.id !== null && entity.id === filters.entity.id;
    if (!idMatch && normalizeKey(entity.name) !== filters.entity.key) return false;
  }
  if (filters.rune && normalizeKey(row.price?.rune) !== filters.rune) return false;
  if (filters.rarity && row.entity?.rarity !== filters.rarity) return false;
  if (filters.grade && row.entity?.grade !== filters.grade) return false;
  if (filters.slot && row.entity?.slot !== filters.slot) return false;
  if (filters.channel && row.channel.toLowerCase() !== filters.channel) return false;
  if (filters.author && row.author !== filters.author) return false;
  if (filters.kind && row.kind !== filters.kind) return false;
  if (filters.basis && row.basis !== filters.basis) return false;
  if (filters.currency && row.price?.currency !== filters.currency) return false;
  if (filters.quality && !within(row.quality, filters.quality)) return false;
  if (filters.sockets && !within(row.sockets, filters.sockets)) return false;
  if (filters.price && (!row.price || !within(priceValue(row.price), filters.price))) return false;
  if (filters.since !== null && !(row.ts !== null && row.ts >= filters.since)) return false;
  if (filters.until !== null && !(row.ts !== null && row.ts <= filters.until)) return false;

  for (const term of filters.terms) {
    const haystack = `${normalizeKey(row.text)} ${normalizeKey(row.entity?.name)}`;
    if (!haystack.includes(normalizeKey(term))) return false;
  }
  return true;
}

const SORTERS = {
  recent: (a, b) => (b.ts ?? 0) - (a.ts ?? 0) || String(b.id).localeCompare(String(a.id)),
  price: (a, b) => {
    const left = a.price ? priceValue(a.price) : -1;
    const right = b.price ? priceValue(b.price) : -1;
    return right - left || (b.ts ?? 0) - (a.ts ?? 0);
  },
  quality: (a, b) => (b.quality ?? -1) - (a.quality ?? -1) || (b.ts ?? 0) - (a.ts ?? 0),
};

export const median = (values) => {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = sorted.length >> 1;
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};

/**
 * Price aggregates, grouped so incomparable numbers never average together:
 * gold is normalized to a base value, runes stay per-denomination (a Zod and
 * an Old are not the same currency unit).
 */
export function priceStats(rows) {
  const groups = new Map();
  for (const row of rows) {
    const price = row.price;
    if (!price) continue;
    const key = price.currency === 'gold' ? 'gold' : `rune:${price.rune || price.unit || 'unspecified'}`;
    if (!groups.has(key)) {
      groups.set(key, { key, currency: price.currency, rune: price.rune, unit: price.unit, basis: {}, values: [] });
    }
    const group = groups.get(key);
    group.values.push(priceValue(price));
    group.basis[row.basis] = (group.basis[row.basis] || 0) + 1;
  }

  return [...groups.values()]
    .map((group) => {
      // Folded, not spread: a busy currency group can hold hundreds of
      // thousands of values, and `Math.min(...values)` overflows the stack.
      let min = Infinity;
      let max = -Infinity;
      for (const value of group.values) {
        if (value < min) min = value;
        if (value > max) max = value;
      }
      return {
        key: group.key,
        currency: group.currency,
        rune: group.rune ?? null,
        unit: group.currency === 'gold' ? 'gold' : group.unit,
        count: group.values.length,
        min,
        median: median(group.values),
        max,
        basis: group.basis,
      };
    })
    .sort((a, b) => b.count - a.count || a.key.localeCompare(b.key));
}

function facets(rows, key, limit = 10) {
  const counts = new Map();
  for (const row of rows) {
    const value = key === 'items' ? row.entity?.name : row[key];
    if (!value) continue;
    counts.set(value, (counts.get(value) || 0) + 1);
  }
  return [...counts.entries()]
    .map(([value, count]) => ({ value, count }))
    .sort((a, b) => b.count - a.count || String(a.value).localeCompare(String(b.value)))
    .slice(0, limit);
}

/**
 * Answer a question over signal rows.
 *
 * @param {object[]} rows signal corpus
 * @param {string|object} query raw query string, or an already-parsed filter set
 */
export function querySignals(rows, query, options = {}) {
  const parsed = typeof query === 'string' || query === undefined ? parseQuery(query ?? '') : query;
  const source = Array.isArray(rows) ? rows : [];
  const matched = source.filter((row) => matchesFilters(row, parsed));
  const sorted = [...matched].sort(SORTERS[parsed.sort] ?? SORTERS.recent);
  const limit = options.limit ?? parsed.limit;

  return {
    query: typeof query === 'string' ? query : '',
    filters: parsed,
    scoped: { corpus: source.length, matched: matched.length, returned: Math.min(sorted.length, limit) },
    rows: sorted.slice(0, limit),
    price: priceStats(matched),
    facets: {
      items: facets(matched, 'items'),
      channels: facets(matched, 'channel'),
      kinds: facets(matched, 'kind'),
    },
  };
}

/** Corpus-wide overview, for `hs-social stats` and the REST/MCP stats surface. */
export function summarizeCorpus(rows, { top = 10 } = {}) {
  const source = Array.isArray(rows) ? rows : [];
  const identified = source.filter((row) => row.entity);

  // Folded rather than spread: `Math.min(...ts)` over a real corpus (hundreds
  // of thousands of rows) overflows the call stack.
  let dated = 0;
  let firstTs = null;
  let lastTs = null;
  let withPrice = 0;
  for (const row of source) {
    if (row.ts !== null && row.ts !== undefined) {
      dated += 1;
      if (firstTs === null || row.ts < firstTs) firstTs = row.ts;
      if (lastTs === null || row.ts > lastTs) lastTs = row.ts;
    }
    if (row.price) withPrice += 1;
  }

  return {
    total: source.length,
    dated,
    firstTs,
    lastTs,
    withEntity: identified.length,
    withPrice,
    resolvedRate: source.length ? Number((identified.length / source.length).toFixed(4)) : 0,
    kinds: facets(source, 'kind', 12).map(({ value, count }) => ({ kind: value, count })),
    channels: facets(source, 'channel', top),
    items: facets(source, 'items', top),
    price: priceStats(source),
  };
}

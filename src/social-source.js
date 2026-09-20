/**
 * I/O for the social-signal pipeline: acquire a Discord credential, pull
 * messages, import exports, and persist the derived rows.
 *
 * Two credential modes share one code path:
 * - **user** — a session token lifted from the local Discord client
 *   (`Local Storage/leveldb`), sent as a bare `Authorization` header. This is
 *   what lets the tool read servers you are already in without inviting
 *   anything.
 * - **bot** — a bot token, sent as `Authorization: Bot <token>`.
 *
 * ⚠️ Automating a user account ("self-botting") is prohibited by Discord's
 * Terms of Service and puts that account at risk of termination. The user mode
 * exists because the operator asked for it and owns the account; run it on
 * your own session, read-only, and do not point it at other people's data.
 *
 * The corpus is an append-only JSONL file of normalized signal rows — derived
 * facts, not transcripts: no usernames, no author ids (only a short hash), and
 * message text capped unless told otherwise.
 */

import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { normalizeMessage, parseTimestamp } from './social.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');

export const DEFAULT_CORPUS = join(ROOT, 'data', 'social-signals.jsonl');
export const DEFAULT_API_BASE = 'https://discord.com/api/v10';
export const USER_AGENT = 'hs-social (hero-siege-codex, 1.0.0)';

/** Tokens are never stored or printed whole outside an explicit `--show`. */
export const maskToken = (token) =>
  !token ? '(none)' : token.length <= 12 ? '***' : `${token.slice(0, 6)}…${token.slice(-4)}`;

// --- credentials ----------------------------------------------------------

/** `mfa.…` or a three-part client token; anything else is treated as a bot token. */
const USER_TOKEN_RE = /^(?:mfa\.[\w-]{70,}|[\w-]{24,28}\.[\w-]{6}\.[\w-]{27,45})$/;
const TOKEN_SCAN_RE = /(?:mfa\.[\w-]{70,}|[\w-]{24,28}\.[\w-]{6}\.[\w-]{27,45})/g;

export function detectAuthMode(token) {
  const raw = String(token ?? '').trim();
  if (/^bot\s/i.test(raw)) return 'bot';
  return USER_TOKEN_RE.test(raw) ? 'user' : 'bot';
}

/** Build the `Authorization` header value for a token. */
export function authHeader(token, mode = 'auto') {
  const raw = String(token ?? '').trim();
  const resolved = mode && mode !== 'auto' ? mode : detectAuthMode(raw);
  if (resolved === 'bot') return /^bot\s/i.test(raw) ? raw : `Bot ${raw}`;
  return raw.replace(/^bot\s+/i, '');
}

/** Discord client data dirs we know how to look in, in preference order. */
export function defaultDiscordDirs(env = process.env) {
  const home = homedir();
  if (env.HS_SOCIAL_DISCORD_DIR) return [env.HS_SOCIAL_DISCORD_DIR];
  if (process.platform === 'win32') {
    const appData = env.APPDATA || join(home, 'AppData', 'Roaming');
    return ['discord', 'discordcanary', 'discordptb', 'discorddevelopment'].map((name) =>
      join(appData, name),
    );
  }
  if (process.platform === 'darwin') {
    return ['discord', 'discordcanary', 'discordptb'].map((name) =>
      join(home, 'Library', 'Application Support', name),
    );
  }
  return [
    ...['discord', 'discordcanary', 'discordptb', 'discorddevelopment'].map((name) =>
      join(env.XDG_CONFIG_HOME || join(home, '.config'), name),
    ),
    join(home, '.var', 'app', 'com.discordapp.Discord', 'config', 'discord'),
  ];
}

/**
 * Scan a Discord client's LevelDB for token-shaped strings. Matches are
 * returned unvalidated and un-ordered beyond first-seen; the caller validates
 * each against `/users/@me`, because the client's storage also holds expired
 * tokens and unrelated blobs of the same shape.
 */
export function readClientTokens(dirs = defaultDiscordDirs()) {
  const found = [];
  const seen = new Set();

  for (const dir of dirs) {
    const leveldb = join(dir, 'Local Storage', 'leveldb');
    if (!existsSync(leveldb)) continue;
    for (const entry of readdirSync(leveldb)) {
      const path = join(leveldb, entry);
      try {
        if (statSync(path).size > 32 * 1024 * 1024) continue;
        // latin1 maps bytes 1:1, so the ASCII token scan cannot be shifted by
        // binary content the way UTF-8 decoding would shift it.
        const data = readFileSync(path, 'latin1');
        for (const match of data.matchAll(TOKEN_SCAN_RE)) {
          if (seen.has(match[0])) continue;
          seen.add(match[0]);
          found.push({ token: match[0], source: path });
        }
      } catch {
        // A locked or unreadable shard is not worth failing the whole scan.
      }
    }
  }
  return found;
}

// --- transport ------------------------------------------------------------

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Query strings where a value may repeat (`channel_id` does, for scoped search). */
function buildQuery(searchParams) {
  if (!searchParams) return '';
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(searchParams)) {
    for (const entry of Array.isArray(value) ? value : [value]) {
      if (entry !== null && entry !== undefined) params.append(key, String(entry));
    }
  }
  const query = params.toString();
  return query ? `?${query}` : '';
}

/**
 * One Discord REST call: auth, rate-limit backoff, and errors in one place.
 * `options.auth` is `auto` | `user` | `bot`.
 */
export async function discordGet(path, options = {}) {
  const {
    token,
    auth = 'auto',
    apiBase = process.env.HS_SOCIAL_API_BASE || DEFAULT_API_BASE,
    fetchImpl = globalThis.fetch,
    retries = 3,
    retryDelayMs = 1000,
    userAgent = process.env.HS_SOCIAL_USER_AGENT || USER_AGENT,
    searchParams,
  } = options;

  if (!token) throw new Error('no Discord credential; run `hs-social token` to acquire one');
  if (typeof fetchImpl !== 'function') throw new Error('no fetch implementation available');

  const query = buildQuery(searchParams);
  let attempts = retries;
  const backoffMs = () => Math.min(60_000, retryDelayMs * 2 ** Math.max(0, retries - attempts - 1));

  for (;;) {
    let response;
    try {
      response = await fetchImpl(`${apiBase}${path}${query}`, {
        headers: { Authorization: authHeader(token, auth), 'User-Agent': userAgent },
      });
    } catch (error) {
      // A long sweep will see dropped sockets ("terminated", ECONNRESET) and
      // Cloudflare 5xx blips; one of them must not discard a channel's history.
      if (attempts <= 0) throw error;
      attempts -= 1;
      await sleep(backoffMs());
      continue;
    }

    if (response.status === 429) {
      if (attempts <= 0) throw new Error(`rate limited on ${path}`);
      attempts -= 1;
      const body = await response.json().catch(() => ({}));
      const header = Number(response.headers?.get?.('retry-after'));
      const retryAfter = Number(body.retry_after) || (Number.isFinite(header) ? header : 0) || 1;
      await sleep(Math.min(60, retryAfter) * 1000);
      continue;
    }
    if (response.status >= 500) {
      const body = await response.text().catch(() => '');
      if (attempts <= 0) {
        throw new Error(`discord responded ${response.status} for ${path}: ${body.slice(0, 200)}`);
      }
      attempts -= 1;
      await sleep(backoffMs());
      continue;
    }
    if (!response.ok) {
      const body = await response.text().catch(() => '');
      if (response.status === 403) {
        throw new Error(
          `discord denied access to ${path} (403 Missing Access) — the credential cannot read that channel or resource`,
        );
      }
      throw new Error(`discord responded ${response.status} for ${path}: ${body.slice(0, 200)}`);
    }
    return response.json();
  }
}

/**
 * Pick a working credential: an explicit token, `HS_SOCIAL_TOKEN`, or the
 * first token in the local client that `/users/@me` accepts.
 *
 * @returns {Promise<{token:string, auth:'user'|'bot', user:object|null, source:string}>}
 */
export async function resolveToken(options = {}) {
  const {
    token,
    dir,
    auth = process.env.HS_SOCIAL_AUTH || 'auto',
    validate = true,
    dirs = dir ? [dir] : defaultDiscordDirs(),
  } = options;

  const explicit = token || process.env.HS_SOCIAL_TOKEN;
  const candidates = explicit
    ? [{ token: String(explicit).trim(), source: 'explicit token' }]
    : readClientTokens(dirs);

  if (candidates.length === 0) {
    throw new Error(
      `no Discord credential found. Pass --token, set HS_SOCIAL_TOKEN, or set ` +
        `HS_SOCIAL_DISCORD_DIR if your client data is not under ${dirs[0]}`,
    );
  }

  const failures = [];
  for (const candidate of candidates) {
    const mode = auth !== 'auto' ? auth : detectAuthMode(candidate.token);
    if (!validate) return { ...candidate, auth: mode, user: null };
    try {
      const user = await discordGet('/users/@me', { ...options, token: candidate.token, auth: mode });
      return { ...candidate, auth: mode, user };
    } catch (error) {
      failures.push(`${maskToken(candidate.token)} (${candidate.source}): ${error.message}`);
    }
  }

  throw new Error(`no usable Discord credential. Tried:\n  ${failures.join('\n  ')}`);
}

// --- discovery ------------------------------------------------------------

/** Guilds the credential can see — the shortest path to a channel's snowflake. */
export async function fetchGuilds(options = {}) {
  const guilds = await discordGet('/users/@me/guilds', options);
  return (Array.isArray(guilds) ? guilds : []).map((guild) => ({ id: guild.id, name: guild.name }));
}

const CHANNEL_TYPES = {
  0: 'text',
  2: 'voice',
  4: 'category',
  5: 'announcement',
  10: 'thread',
  11: 'thread',
  12: 'thread',
  13: 'stage',
  15: 'forum',
};

/** Text-like channels of one guild, categories dropped. */
export async function fetchGuildChannels(guildId, options = {}) {
  const channels = await discordGet(`/guilds/${guildId}/channels`, options);
  const byId = new Map((Array.isArray(channels) ? channels : []).map((channel) => [channel.id, channel]));

  return (Array.isArray(channels) ? channels : [])
    .filter((channel) => channel.type !== 4)
    .map((channel) => ({
      id: channel.id,
      name: channel.name,
      type: CHANNEL_TYPES[channel.type] || String(channel.type),
      parent: byId.get(channel.parent_id)?.name ?? null,
    }))
    .sort((a, b) => String(a.parent).localeCompare(String(b.parent)) || a.name.localeCompare(b.name));
}

// --- ingestion ------------------------------------------------------------

/**
 * One channel's messages, newest first, following the `before` cursor.
 *
 * `since` / `until` are epoch milliseconds (see `parseWhen`): pages descend in
 * time, so a page whose oldest message predates `since` ends the walk instead
 * of draining the channel, and anything outside the window is dropped. That
 * makes a bounded backfill cost one request per page of *history*, not one per
 * page of channel.
 *
 * `delayMs` paces consecutive requests, for long sweeps that would otherwise
 * trip the rate limiter.
 */
export async function fetchChannelMessages(channelId, options = {}) {
  const { max = 500, pageSize = 100, since = null, until = null, delayMs = 0 } = options;
  const collected = [];
  let before = null;

  while (collected.length < max) {
    const requested = Math.min(pageSize, max - collected.length);
    const searchParams = { limit: String(requested) };
    if (before) searchParams.before = before;

    if (before && delayMs > 0) await sleep(delayMs);
    const page = await discordGet(`/channels/${channelId}/messages`, { ...options, searchParams });
    if (!Array.isArray(page) || page.length === 0) break;

    for (const message of page) {
      const ts = parseTimestamp(message.timestamp);
      if (since !== null && !(ts !== null && ts >= since)) continue;
      if (until !== null && !(ts !== null && ts <= until)) continue;
      collected.push(message);
    }

    const oldest = parseTimestamp(page[page.length - 1].timestamp);
    before = page[page.length - 1].id;
    if (page.length < requested) break;
    if (since !== null && oldest !== null && oldest < since) break;
  }

  return collected.slice(0, max);
}

/**
 * Keyword harvest across a whole guild via the search endpoint — the path that
 * does not require knowing any channel snowflakes. Returns messages with the
 * same shape as the channel endpoint (each already carries `channel_id`).
 *
 * `channelIds` scopes the search: unscoped, a term like "WTS" mostly matches
 * `#general` chatter, while the trade listings live in a handful of channels.
 */
export async function searchGuildMessages(guildId, options = {}) {
  const {
    query,
    channelIds = [],
    max = 200,
    pageSize = 25,
    since = null,
    until = null,
    delayMs = 0,
  } = options;
  if (!query) throw new Error('guild search needs a query');

  const collected = [];
  let offset = 0;

  while (collected.length < max && offset < 5000) {
    const requested = Math.min(pageSize, max - collected.length);
    if (offset > 0 && delayMs > 0) await sleep(delayMs);
    const body = await discordGet(`/guilds/${guildId}/messages/search`, {
      ...options,
      searchParams: {
        content: query,
        limit: String(requested),
        offset: String(offset),
        sort_by: 'timestamp',
        sort_order: 'desc',
        ...(channelIds.length ? { channel_id: channelIds } : {}),
      },
    });

    const groups = Array.isArray(body?.messages) ? body.messages : [];
    if (groups.length === 0) break;

    let oldest = null;
    for (const group of groups) {
      const hit = group.find?.((message) => message.hit) ?? group[0];
      if (!hit) continue;
      const ts = parseTimestamp(hit.timestamp);
      if (ts !== null && (oldest === null || ts < oldest)) oldest = ts;
      if (since !== null && !(ts !== null && ts >= since)) continue;
      if (until !== null && !(ts !== null && ts <= until)) continue;
      collected.push(hit);
    }

    offset += groups.length;
    if (offset >= (Number(body.total_results) || 0)) break;
    if (since !== null && oldest !== null && oldest < since) break;
  }

  return collected.slice(0, max);
}

/** `123=trade,456=price check` → `[{ id, label }]` */
export function parseChannelSpec(spec) {
  if (!spec) return [];
  return String(spec)
    .split(',')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      const separator = part.indexOf('=');
      if (separator < 0) return { id: part, label: part };
      return { id: part.slice(0, separator).trim(), label: part.slice(separator + 1).trim() || part };
    });
}

const isNormalizedRow = (row) =>
  row && typeof row === 'object' && 'kind' in row && 'author' in row && 'ts' in row;

/** Read the corpus, last write winning on duplicate ids. Missing file → []. */
export function loadSignals(path = DEFAULT_CORPUS) {
  if (!existsSync(path)) return [];
  const rows = new Map();
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const row = JSON.parse(trimmed);
      if (row?.id) rows.set(row.id, row);
    } catch {
      // A torn final line from an interrupted write is not worth failing over.
    }
  }
  return [...rows.values()];
}

export function appendSignals(rows, path = DEFAULT_CORPUS) {
  if (!Array.isArray(rows) || rows.length === 0) return 0;
  mkdirSync(dirname(path), { recursive: true });
  appendFileSync(path, rows.map((row) => JSON.stringify(row)).join('\n') + '\n');
  return rows.length;
}

/**
 * Import a channel export: JSONL of messages, a JSON array, or `{ messages:
 * [...] }`. Already-normalized rows pass through untouched; raw Discord
 * messages (or any object with `content`/`text`) are normalized.
 */
export function importMessages(filePath, options = {}) {
  const raw = readFileSync(filePath, 'utf8');
  let records;
  if (filePath.endsWith('.json')) {
    const parsed = JSON.parse(raw);
    records = Array.isArray(parsed) ? parsed : parsed.messages ?? [parsed];
  } else {
    records = raw
      .split('\n')
      .filter((line) => line.trim())
      .map((line) => JSON.parse(line));
  }

  const channel = options.channel ?? filePath.split('/').pop().replace(/\.[^.]+$/, '');
  return records.map((record) =>
    isNormalizedRow(record)
      ? record
      : normalizeMessage(record, { ...options, channel, channelId: record.channel_id ?? record.channelId ?? channel }),
  );
}

const corpusPath = (options) => options.corpusPath || process.env.HS_SOCIAL_CORPUS || DEFAULT_CORPUS;

/** Append only rows whose message id is not already in the corpus. */
function commit(rows, existing, path) {
  const fresh = rows.filter((row) => !existing.has(row.id));
  for (const row of fresh) existing.add(row.id);
  appendSignals(fresh, path);
  return fresh.length;
}

/**
 * Pull the configured channels and append what is new. Idempotent by message
 * id. A channel the credential cannot read is recorded and skipped — a guild's
 * channel list includes channels the account has no permission for, and one of
 * those must not discard the harvest that did work.
 */
export async function ingestChannels(channels, options = {}) {
  const { onProgress = () => {} } = options;
  const path = corpusPath(options);
  const existing = new Set(loadSignals(path).map((row) => row.id));
  const summary = {
    channels: [],
    added: 0,
    skipped: 0,
    errors: [],
    since: options.since ?? null,
    until: options.until ?? null,
  };

  for (const channel of channels) {
    onProgress(`fetching ${channel.label} (${channel.id})`);
    try {
      const messages = await fetchChannelMessages(channel.id, options);
      const rows = messages.map((message) =>
        normalizeMessage(message, { ...options, channel: channel.label, channelId: channel.id }),
      );
      const added = commit(rows, existing, path);

      summary.channels.push({ id: channel.id, label: channel.label, fetched: messages.length, added });
      summary.added += added;
      summary.skipped += messages.length - added;
    } catch (error) {
      summary.errors.push({ id: channel.id, label: channel.label, message: error.message });
    }
  }

  summary.corpus = path;
  return summary;
}

/** Keyword harvest across a guild; channel labels come from `channelNames` when known. */
export async function ingestSearch(guildId, options = {}) {
  const { query, onProgress = () => {}, channelNames = new Map() } = options;
  const path = corpusPath(options);
  const existing = new Set(loadSignals(path).map((row) => row.id));
  const summary = {
    guild: guildId,
    query,
    fetched: 0,
    added: 0,
    skipped: 0,
    errors: [],
    since: options.since ?? null,
    until: options.until ?? null,
  };

  onProgress(`searching guild ${guildId} for "${query}"`);
  let messages;
  try {
    messages = await searchGuildMessages(guildId, options);
  } catch (error) {
    summary.errors.push({ id: guildId, label: `search:${query}`, message: error.message });
    summary.corpus = path;
    return summary;
  }

  const rows = messages.map((message) =>
    normalizeMessage(message, {
      ...options,
      channel: channelNames.get(message.channel_id) || `search:${query}`,
      channelId: message.channel_id,
    }),
  );
  const added = commit(rows, existing, path);

  summary.fetched = messages.length;
  summary.added = added;
  summary.skipped = messages.length - added;
  summary.corpus = path;
  return summary;
}

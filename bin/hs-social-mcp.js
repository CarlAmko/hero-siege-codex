#!/usr/bin/env node

/**
 * hs-social MCP stdio server — JSON-RPC 2.0 by hand, no dependencies.
 *
 *   claude mcp add hs-social -- /full/path/bin/hs-social-mcp.js
 *
 * Tools:
 *   hs_social_query    answer a query over the social-signal corpus
 *   hs_social_summary  corpus overview (volume, intent, top items, price bands)
 *   hs_social_ingest   pull messages in, by guild keyword search or by channel
 *
 * The corpus is re-read on every call, so an ingest from the CLI or another
 * process is visible immediately. Diagnostics go to stderr; stdout carries
 * protocol traffic only.
 */

import readline from 'node:readline';

import { DEFAULT_CORPUS, ingestChannels, ingestSearch, loadSignals, maskToken, parseChannelSpec, resolveToken } from '../src/social-source.js';
import { parseWhen, querySignals, summarizeCorpus } from '../src/social.js';

const PROTOCOL_VERSION = '2024-11-05';
const SERVER_INFO = { name: 'hs-social', version: '1.0.0' };

const corpusPath = () => process.env.HS_SOCIAL_CORPUS || DEFAULT_CORPUS;

const TOOLS = [
  {
    name: 'hs_social_query',
    description:
      'Query Hero Siege Discord trade/price-check signals. Filters: item, rune, rarity, grade, slot, ' +
      'quality, sockets, price, currency, kind, basis, channel, author, since, until, sort, limit, plus ' +
      'free-text terms. Example: \'item:bucket quality:>=110 currency:gold since:7d\'.',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Query string, e.g. "item:bucket quality:>=110"' },
        limit: { type: 'integer', description: 'Max rows to return (default 50)' },
      },
      required: ['query'],
    },
  },
  {
    name: 'hs_social_summary',
    description: 'Corpus overview: message count, date range, intent mix, top items, price bands by currency.',
    inputSchema: {
      type: 'object',
      properties: { top: { type: 'integer', description: 'Rows per facet list (default 10)' } },
      required: [],
    },
  },
  {
    name: 'hs_social_ingest',
    description:
      'Pull messages into the corpus. Either harvest a whole server by keyword (guild + query) or walk ' +
      'specific channels (channels="id=label,id=label"). The credential is resolved from HS_SOCIAL_TOKEN ' +
      'or the local Discord client session — a user-session token is a self-bot and against Discord ToS.',
    inputSchema: {
      type: 'object',
      properties: {
        guild: { type: 'string', description: 'Guild id for a keyword harvest' },
        query: { type: 'string', description: 'Search term, used with guild' },
        channels: {
          type: 'string',
          description: 'Channel spec "id=label,id=label" — the channels to walk, or the scope of a guild search',
        },
        max: { type: 'integer', description: 'Messages to pull (default 200)' },
        since: { type: 'string', description: 'Only keep messages at or after this point (7d, 24h, 2w, or an ISO date)' },
        until: { type: 'string', description: 'Only keep messages at or before this point' },
        delay: { type: 'integer', description: 'Milliseconds to pause between paginated requests' },
      },
      required: [],
    },
  },
];

async function callTool(name, args = {}) {
  const rows = loadSignals(corpusPath());

  if (name === 'hs_social_query') {
    const result = querySignals(rows, String(args.query ?? ''), args.limit ? { limit: Number(args.limit) } : {});
    return { corpus: corpusPath(), ...result };
  }

  if (name === 'hs_social_summary') {
    return { corpus: corpusPath(), ...summarizeCorpus(rows, { top: Number(args.top) || 10 }) };
  }

  if (name === 'hs_social_ingest') {
    const credential = await resolveToken({
      token: process.env.HS_SOCIAL_TOKEN,
      dir: process.env.HS_SOCIAL_DISCORD_DIR,
      auth: process.env.HS_SOCIAL_AUTH,
      apiBase: process.env.HS_SOCIAL_API_BASE,
    });
    const options = {
      token: credential.token,
      auth: credential.auth,
      corpusPath: corpusPath(),
      max: Number(args.max) || 200,
      since: args.since ? parseWhen(String(args.since)) : null,
      until: args.until ? parseWhen(String(args.until)) : null,
      delayMs: Math.max(0, Number(args.delay) || 0),
      apiBase: process.env.HS_SOCIAL_API_BASE,
    };
    const identity = { auth: credential.auth, as: credential.user?.username ?? null, token: maskToken(credential.token) };

    if (args.guild && args.query) {
      const scope = parseChannelSpec(args.channels);
      return {
        identity,
        ...(await ingestSearch(String(args.guild), {
          ...options,
          query: String(args.query),
          channelIds: scope.map((channel) => channel.id),
        })),
      };
    }

    const channels = parseChannelSpec(args.channels);
    if (channels.length === 0) throw new Error('pass either guild+query or channels');
    return { identity, ...(await ingestChannels(channels, options)) };
  }

  throw new Error(`unknown tool: ${name}`);
}

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

function handleRequest(request) {
  const { id, method, params } = request ?? {};

  if (method === 'initialize') {
    return {
      jsonrpc: '2.0',
      id,
      result: { protocolVersion: PROTOCOL_VERSION, capabilities: { tools: {} }, serverInfo: SERVER_INFO },
    };
  }
  if (method === 'tools/list') {
    return { jsonrpc: '2.0', id, result: { tools: TOOLS } };
  }
  if (method === 'tools/call') {
    return callTool(params?.name, params?.arguments ?? {})
      .then((payload) => ({
        jsonrpc: '2.0',
        id,
        result: { content: [{ type: 'text', text: JSON.stringify(payload, null, 2) }] },
      }))
      .catch((error) => ({
        jsonrpc: '2.0',
        id,
        result: { content: [{ type: 'text', text: error.message }], isError: true },
      }));
  }
  if (method === 'ping') return { jsonrpc: '2.0', id, result: {} };
  if (typeof method === 'string' && method.startsWith('notifications/')) return null;

  return { jsonrpc: '2.0', id, error: { code: -32601, message: `method not found: ${method}` } };
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });

rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let request;
  try {
    request = JSON.parse(trimmed);
  } catch {
    send({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'parse error' } });
    return;
  }
  try {
    const response = handleRequest(request);
    if (response && typeof response.then === 'function') response.then((settled) => settled && send(settled));
    else if (response) send(response);
  } catch (error) {
    send({ jsonrpc: '2.0', id: request?.id ?? null, error: { code: -32603, message: error.message } });
  }
});

rl.on('close', () => process.exit(0));

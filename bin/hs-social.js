#!/usr/bin/env node

/**
 * hs-social — ingest Discord trade/price-check chatter and query it.
 *
 *   hs-social token                      # acquire/validate a credential
 *   hs-social guilds                     # list servers the credential can see
 *   hs-social channels --guild <id>      # pick channels to harvest
 *   hs-social ingest --guild <id> --search "Satanic"
 *   hs-social query 'item:"harlequinn\'s crest" quality:>=110' --limit 20
 *   hs-social stats
 */

import {
  DEFAULT_CORPUS,
  appendSignals,
  fetchGuildChannels,
  fetchGuilds,
  importMessages,
  ingestChannels,
  ingestSearch,
  loadSignals,
  maskToken,
  parseChannelSpec,
  resolveToken,
  writeSnapshot,
} from '../src/social-source.js';
import { parseWhen, querySignals, summarizeCorpus, SIGNAL_KINDS, PRICE_BASES } from '../src/social.js';

const ANSI = {
  reset: '\x1b[0m',
  bold: '\x1b[1m',
  dim: '\x1b[2m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  cyan: '\x1b[36m',
  white: '\x1b[37m',
};

const KIND_COLOR = {
  sell: ANSI.green,
  buy: ANSI.cyan,
  price_check: ANSI.yellow,
  build: ANSI.magenta,
  bug: ANSI.red,
  chat: ANSI.dim,
};

function parseArgs(args) {
  const flags = { _: [] };
  const set = (name, value) => {
    if (name === 'channel' || name === 'channels') {
      flags[name] = [...(flags[name] || []), value];
      return;
    }
    flags[name] = value;
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (!arg.startsWith('--')) {
      flags._.push(arg);
      continue;
    }
    const body = arg.slice(2);
    const equals = body.indexOf('=');
    if (equals >= 0) {
      set(body.slice(0, equals), body.slice(equals + 1));
      continue;
    }
    const next = args[i + 1];
    if (next !== undefined && !next.startsWith('--')) {
      set(body, next);
      i++;
    } else {
      set(body, true);
    }
  }
  return flags;
}

const corpusPath = (flags) => flags.corpus || process.env.HS_SOCIAL_CORPUS || DEFAULT_CORPUS;

const formatDate = (ts) =>
  ts === null || ts === undefined ? ANSI.dim + '   --        ' : new Date(ts).toISOString().slice(0, 16).replace('T', ' ');

const formatPrice = (price) => {
  if (!price) return `${ANSI.dim}-${ANSI.reset}`;
  return price.currency === 'gold'
    ? `${ANSI.yellow}${price.goldValue.toLocaleString('en-US')}g${ANSI.reset}`
    : `${ANSI.magenta}${price.amount} ${price.rune || price.unit}${ANSI.reset}`;
};

const formatRow = (row) => {
  const entity = row.entity
    ? `${ANSI.bold}${row.entity.name}${ANSI.reset}${row.entity.grade ? ` ${ANSI.dim}[${row.entity.grade}]${ANSI.reset}` : ''}`
    : `${ANSI.dim}(no item)${ANSI.reset}`;
  const roll = [row.quality ? `${row.quality}%` : null, row.sockets ? `${row.sockets}sock` : null]
    .filter(Boolean)
    .join(' ');
  const kindColor = KIND_COLOR[row.kind] || ANSI.reset;
  const snippet = row.text ? `${ANSI.dim}"${row.text.slice(0, 64)}"${ANSI.reset}` : '';

  console.log(
    `${formatDate(row.ts)}  ${kindColor}${row.kind.padEnd(11)}${ANSI.reset} ${entity.padEnd(38)} ` +
      `${roll.padEnd(9)} ${formatPrice(row.price).padEnd(30)} ${ANSI.blue}${row.channel}${ANSI.reset}`,
  );
  if (snippet) console.log(`                                          ${snippet}`);
};

function printPriceTable(price) {
  if (price.length === 0) return;
  console.log(`\n${ANSI.bold}Price by currency${ANSI.reset} ${ANSI.dim}(never mixed: gold normalizes, runes stay per-denomination)${ANSI.reset}`);
  for (const group of price) {
    console.log(
      `  ${group.key.padEnd(18)} n=${String(group.count).padStart(3)}  ` +
        `min ${String(group.min).padStart(9)}  median ${String(group.median).padStart(9)}  max ${String(group.max).padStart(9)}`,
    );
  }
}

function printFacets(result) {
  if (result.facets.items.length > 0) {
    console.log(`\n${ANSI.bold}Top items${ANSI.reset}`);
    for (const item of result.facets.items) console.log(`  ${String(item.count).padStart(3)}  ${item.value}`);
  }
  if (result.facets.channels.length > 0) {
    console.log(`\n${ANSI.bold}Channels${ANSI.reset}`);
    for (const channel of result.facets.channels) console.log(`  ${String(channel.count).padStart(3)}  ${channel.value}`);
  }
}

function handleQuery(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  const query = flags._.join(' ').trim();
  const rows = loadSignals(corpusPath(flags));

  if (rows.length === 0) {
    console.log(`${ANSI.yellow}Corpus is empty at ${corpusPath(flags)}${ANSI.reset}`);
    console.log(`Ingest first: ${ANSI.cyan}hs-social ingest --channel <id>=<label>${ANSI.reset}`);
    return;
  }

  const result = querySignals(rows, query, flags.limit ? { limit: Number(flags.limit) } : {});
  if (flags.json) {
    console.log(JSON.stringify(result, null, 2));
    return;
  }

  console.log('\n' + '─'.repeat(120));
  console.log(
    `${ANSI.bold}Social signals${ANSI.reset} ${ANSI.dim}·${ANSI.reset} ${result.scoped.matched} matched of ${result.scoped.corpus}` +
      (query ? ` ${ANSI.dim}(query: ${query})${ANSI.reset}` : ''),
  );
  console.log('─'.repeat(120));
  for (const row of result.rows) formatRow(row);
  if (result.rows.length === 0) console.log(`${ANSI.dim}No signal matched.${ANSI.reset}`);
  printPriceTable(result.price);
  printFacets(result);
  console.log('');
}

function handleStats(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  const rows = loadSignals(corpusPath(flags));
  const summary = summarizeCorpus(rows, { top: Number(flags.top) || 10 });

  if (flags.json) {
    console.log(JSON.stringify(summary, null, 2));
    return;
  }

  console.log('\n' + '─'.repeat(72));
  console.log(`${ANSI.bold}Social signal corpus${ANSI.reset} ${ANSI.dim}${corpusPath(flags)}${ANSI.reset}`);
  console.log('─'.repeat(72));
  console.log(`Messages:      ${ANSI.bold}${summary.total}${ANSI.reset}`);
  console.log(`Date range:    ${formatDate(summary.firstTs).trim()} → ${formatDate(summary.lastTs).trim()}`);
  console.log(`With item:     ${summary.withEntity} ${ANSI.dim}(${(summary.resolvedRate * 100).toFixed(1)}% resolved)${ANSI.reset}`);
  console.log(`With price:    ${summary.withPrice}`);

  if (summary.kinds.length > 0) {
    console.log(`\n${ANSI.bold}Intent${ANSI.reset}`);
    for (const kind of summary.kinds) console.log(`  ${String(kind.count).padStart(4)}  ${kind.kind}`);
  }
  if (summary.channels.length > 0) {
    console.log(`\n${ANSI.bold}Channels${ANSI.reset}`);
    for (const channel of summary.channels) console.log(`  ${String(channel.count).padStart(4)}  ${channel.value}`);
  }
  if (summary.items.length > 0) {
    console.log(`\n${ANSI.bold}Top items${ANSI.reset}`);
    for (const item of summary.items) console.log(`  ${String(item.count).padStart(4)}  ${item.value}`);
  }
  printPriceTable(summary.price);
  console.log('');
}

function handleSnapshot(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  try {
    const result = writeSnapshot(corpusPath(flags));
    if (flags.json) {
      console.log(JSON.stringify(result, null, 2));
      return;
    }
    const mb = (result.bytes / 1048576).toFixed(1);
    console.log(`${ANSI.green}snapshot written${ANSI.reset} ${ANSI.dim}${mb} MB → ${result.path}${ANSI.reset}`);
  } catch (error) {
    console.log(`${ANSI.red}${error.message}${ANSI.reset}`);
  }
}

/**
 * Resolve a credential the same way for every command: explicit flag, then
 * env, then the local Discord client's stored session token.
 */
async function resolveCliToken(flags) {
  return resolveToken({
    token: flags.token || flags['user-token'],
    dir: flags['discord-dir'],
    auth: flags.auth,
    apiBase: flags['api-base'] || process.env.HS_SOCIAL_API_BASE,
    validate: !flags['no-validate'],
  });
}

const connectionOptions = (flags, credential) => ({
  token: credential.token,
  auth: credential.auth,
  apiBase: flags['api-base'] || process.env.HS_SOCIAL_API_BASE,
});

const describeCredential = (credential) =>
  `${credential.auth} token ${maskToken(credential.token)}` +
  (credential.user?.username ? ` as ${ANSI.cyan}${credential.user.username}${ANSI.reset}` : '');

function reportAuthError(error) {
  console.log(`${ANSI.red}${error.message}${ANSI.reset}`);
  console.log(
    `${ANSI.dim}Acquire one with: hs-social token  ·  or point at your client: --discord-dir <path>${ANSI.reset}`,
  );
}

async function handleToken(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  let credential;
  try {
    credential = await resolveCliToken(flags);
  } catch (error) {
    reportAuthError(error);
    return;
  }

  const shown = flags.show ? credential.token : maskToken(credential.token);
  if (flags.json) {
    console.log(JSON.stringify({ ...credential, token: shown }, null, 2));
    return;
  }

  console.log(`\nAuth mode:  ${ANSI.bold}${credential.auth}${ANSI.reset}`);
  console.log(`Identity:   ${credential.user?.username ?? '(unverified)'} ${ANSI.dim}${credential.user?.id ?? ''}${ANSI.reset}`);
  console.log(`Source:     ${ANSI.dim}${credential.source}${ANSI.reset}`);
  console.log(`Token:      ${shown}`);
  if (!flags.show) console.log(`${ANSI.dim}Re-run with --show to print it in full.${ANSI.reset}`);
  console.log(
    `\n${ANSI.dim}Use it via env: export HS_SOCIAL_TOKEN='<token>' (or pass --token).${ANSI.reset}\n`,
  );
  if (credential.auth === 'user') {
    console.log(
      `${ANSI.yellow}User-session tokens are self-bots: Discord's ToS prohibits automating an account,${ANSI.reset}\n` +
        `${ANSI.yellow}and it can be terminated for it. Keep it read-only, on your own session.${ANSI.reset}\n`,
    );
  }
}

async function handleGuilds(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  let credential;
  try {
    credential = await resolveCliToken(flags);
  } catch (error) {
    reportAuthError(error);
    return;
  }

  const guilds = await fetchGuilds({ ...connectionOptions(flags, credential), validate: true });
  if (flags.json) {
    console.log(JSON.stringify(guilds, null, 2));
    return;
  }
  console.log(`\n${ANSI.bold}${guilds.length} guilds${ANSI.reset} ${ANSI.dim}(${describeCredential(credential)})${ANSI.reset}`);
  for (const guild of guilds) console.log(`  ${guild.id}  ${guild.name}`);
  console.log(`\n${ANSI.dim}Next: hs-social channels --guild <id>${ANSI.reset}\n`);
}

async function handleChannels(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  const guild = flags.guild || flags._[0] || process.env.HS_SOCIAL_GUILD;
  if (!guild) {
    console.log(`${ANSI.red}Error: pass a guild id.${ANSI.reset} ${ANSI.dim}hs-social channels --guild 215140390443941888${ANSI.reset}`);
    return;
  }

  let credential;
  try {
    credential = await resolveCliToken(flags);
  } catch (error) {
    reportAuthError(error);
    return;
  }

  const channels = await fetchGuildChannels(guild, connectionOptions(flags, credential));
  if (flags.json) {
    console.log(JSON.stringify(channels, null, 2));
    return;
  }
  console.log(
    `\n${ANSI.bold}${channels.length} channels${ANSI.reset} in ${guild} ${ANSI.dim}(${describeCredential(credential)})${ANSI.reset}`,
  );
  for (const channel of channels) {
    console.log(
      `  ${channel.id}  ${String(channel.parent || '-').padEnd(24)} ${channel.name} ${ANSI.dim}${channel.type}${ANSI.reset}`,
    );
  }
  console.log(`\n${ANSI.dim}Next: hs-social ingest --channel ${channels[0]?.id ?? '<id>'}=<label>${ANSI.reset}\n`);
}

async function handleIngest(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  const spec = [...(flags.channel || []), ...(flags.channels || [])].join(',');
  const channels = parseChannelSpec(spec || process.env.HS_SOCIAL_CHANNELS);
  const guild = flags.guild || process.env.HS_SOCIAL_GUILD;
  const query = flags.search;

  const since = flags.since ? parseWhen(flags.since) : null;
  const until = flags.until ? parseWhen(flags.until) : null;
  for (const [name, raw, parsed] of [['--since', flags.since, since], ['--until', flags.until, until]]) {
    if (raw && parsed === null) {
      console.log(`${ANSI.red}Error: ${name} "${raw}" is not a date.${ANSI.reset} ${ANSI.dim}Use 7d/24h/2w or an ISO date.${ANSI.reset}`);
      return;
    }
  }

  if (channels.length === 0 && !(guild && query)) {
    console.log(`${ANSI.red}Error: nothing to ingest.${ANSI.reset}`);
    console.log(`  ${ANSI.cyan}hs-social ingest --guild <id> --search "<term>"${ANSI.reset}   keyword harvest across a server`);
    console.log(`  ${ANSI.cyan}hs-social ingest --channel <id>[=<label>]${ANSI.reset}        one channel's recent history`);
    return;
  }

  let credential;
  try {
    credential = await resolveCliToken(flags);
  } catch (error) {
    reportAuthError(error);
    return;
  }

  const options = {
    ...connectionOptions(flags, credential),
    corpusPath: corpusPath(flags),
    max: Number(flags.max) || (query ? 200 : 500),
    keepText: !flags['no-text'],
    since,
    until,
    delayMs: Math.max(0, Number(flags.delay) || 0),
    onProgress: (message) => console.log(`${ANSI.dim}${message}${ANSI.reset}`),
  };
  console.log(`${ANSI.dim}using ${describeCredential(credential)}${ANSI.reset}`);
  if (since !== null || until !== null) {
    console.log(
      `${ANSI.dim}window: ${since === null ? 'start' : new Date(since).toISOString()} → ` +
        `${until === null ? 'now' : new Date(until).toISOString()}${options.delayMs ? ` · ${options.delayMs}ms between requests` : ''}${ANSI.reset}`,
    );
  }

  const summary =
    guild && query
      ? await ingestSearch(guild, {
          ...options,
          query,
          // `--channel` doubles as a scope filter for a guild search.
          channelIds: channels.map((channel) => channel.id),
          channelNames: await guildChannelNames(guild, options),
        })
      : await ingestChannels(channels, options);

  if (flags.json) {
    console.log(JSON.stringify(summary, null, 2));
    return;
  }
  const errors = summary.errors ?? [];
  for (const error of errors) {
    console.log(`${ANSI.red}✗ ${error.label ?? error.id}:${ANSI.reset} ${error.message}`);
  }
  if (errors.some((error) => error.message.includes('Missing Access'))) {
    console.log(
      `${ANSI.dim}A guild's channel list includes channels your account cannot read; those need an ` +
        `appropriate role, or a different account.${ANSI.reset}`,
    );
  }

  for (const channel of summary.channels ?? []) {
    console.log(`${ANSI.cyan}${channel.label}${ANSI.reset} ${ANSI.dim}(${channel.id})${ANSI.reset}: fetched ${channel.fetched}, added ${channel.added}`);
  }
  if (summary.query) console.log(`${ANSI.cyan}${summary.query}${ANSI.reset}: fetched ${summary.fetched}`);
  console.log(
    `\n${ANSI.green}${summary.added} rows added${ANSI.reset} ${ANSI.dim}(${summary.skipped} already present) → ${summary.corpus}${ANSI.reset}`,
  );
  if (summary.added === 0 && errors.length > 0) process.exitCode = 1;
}

/** Channel id → name, so search hits are labelled instead of `search:<term>`. */
async function guildChannelNames(guildId, options) {
  try {
    const channels = await fetchGuildChannels(guildId, options);
    return new Map(channels.map((channel) => [channel.id, channel.name]));
  } catch {
    return new Map();
  }
}

function handleImport(cmdArgs) {
  const flags = parseArgs(cmdArgs);
  const file = flags._[0];
  if (!file) {
    console.log(`${ANSI.red}Error: pass a message export to import.${ANSI.reset}`);
    console.log(`Example: hs-social import ./trade-dump.jsonl --channel trade`);
    return;
  }

  const path = corpusPath(flags);
  const existing = new Set(loadSignals(path).map((row) => row.id));
  const rows = importMessages(file, { channel: flags.channel?.[0], keepText: !flags['no-text'] }).filter(
    (row) => !existing.has(row.id),
  );
  appendSignals(rows, path);

  if (flags.json) {
    console.log(JSON.stringify({ file, added: rows.length, corpus: path }, null, 2));
    return;
  }
  console.log(`${ANSI.green}${rows.length} rows imported${ANSI.reset} ${ANSI.dim}from ${file} → ${path}${ANSI.reset}`);
}

function printHelp() {
  console.log(`
${ANSI.bold}hs-social${ANSI.reset} — Discord social signals for Hero Siege Codex

${ANSI.bold}Usage${ANSI.reset}
  hs-social <command> [options]

${ANSI.bold}Commands${ANSI.reset}
  ${ANSI.cyan}token${ANSI.reset}    Acquire and validate a Discord credential (flags the ToS risk)
  ${ANSI.cyan}guilds${ANSI.reset}   List the servers the credential can see
  ${ANSI.cyan}channels${ANSI.reset} List a server's channels, for picking ingest targets
  ${ANSI.cyan}ingest${ANSI.reset}   Pull messages into the corpus (by channel, or a guild keyword search)
  ${ANSI.cyan}import${ANSI.reset}   Import a channel export (JSONL / JSON) instead of pulling
  ${ANSI.cyan}query${ANSI.reset}    Answer a question over the corpus
  ${ANSI.cyan}stats${ANSI.reset}    Corpus overview: volume, intent mix, top items, price bands
  ${ANSI.cyan}snapshot${ANSI.reset} Write the gzipped corpus snapshot that ships with the repo

${ANSI.bold}Credential${ANSI.reset}
  Resolution order: ${ANSI.dim}--token · HS_SOCIAL_TOKEN · the local Discord client's stored session${ANSI.reset}
  --token <token>            Explicit credential; auto-detects user-session vs bot shape
  --auth auto|user|bot       Force the Authorization scheme (default: auto)
  --discord-dir <path>       Client data dir to read the session token from
                             (default: OS config path for discord/discordcanary/…)
  --show                     With ${ANSI.cyan}token${ANSI.reset}, print the token in full (otherwise masked)

${ANSI.bold}Options${ANSI.reset}
  --channel <id>[=<label>]   Channel to ingest (repeatable; or HS_SOCIAL_CHANNELS).
                             With --guild --search, restricts the search to these channels
  --guild <id> --search <q>  Keyword harvest across a whole server (or HS_SOCIAL_GUILD)
  --since <when>              Only keep messages at or after this point (7d, 24h, 2w, or an ISO date)
  --until <when>              Only keep messages at or before this point
  --delay <ms>                Pause between paginated requests, for long sweeps
  --api-base <url>           Override API base (default ${ANSI.dim}https://discord.com/api/v10${ANSI.reset})
  --max <n>                  Messages to pull (default 500 per channel, 200 per search)
  --corpus <path>            Corpus file (default ${ANSI.dim}data/social-signals.jsonl${ANSI.reset})
  --no-text                  Store extracted facts only, no message text
  --limit <n>                Rows returned by query (default 50)
  --json                     Machine-readable output

${ANSI.bold}Query language${ANSI.reset}
  ${ANSI.dim}item:"harlequinn's crest" quality:>=110 sockets:2 currency:gold since:7d sort:price${ANSI.reset}
  keys: ${ANSI.dim}item rune rarity grade slot channel author kind basis currency
        quality sockets price since until sort limit + free text${ANSI.reset}
  kinds: ${SIGNAL_KINDS.join(', ')}
  price basis: ${PRICE_BASES.join(', ')}

${ANSI.bold}Examples${ANSI.reset}
  hs-social token                      # validate the session token found on disk
  hs-social guilds                     # find the Hero Siege server id
  hs-social channels --guild 215140390443941888
  hs-social ingest --guild 215140390443941888 --search "Satanic" --max 400
  hs-social ingest --channel 123456=trade --max 1000
  hs-social ingest --channel 123456=trade --since 2w --delay 900
  hs-social import ./dump.jsonl --channel price-check
  hs-social snapshot                   # refresh data/social-signals.jsonl.gz
  hs-social query 'item:bucket quality:>=110'

${ANSI.yellow}Warning${ANSI.reset} A user-session token is a self-bot. Discord's ToS prohibits automating an
account and can terminate it for doing so. Use your own session, read-only.
`);
}

const args = process.argv.slice(2);
const command = args[0] ? args[0].toLowerCase() : 'help';
const rest = args.slice(1);

switch (command) {
  case 'token':
    await handleToken(rest);
    break;
  case 'guilds':
    await handleGuilds(rest);
    break;
  case 'channels':
    await handleChannels(rest);
    break;
  case 'ingest':
    await handleIngest(rest);
    break;
  case 'import':
    handleImport(rest);
    break;
  case 'query':
    handleQuery(rest);
    break;
  case 'stats':
    handleStats(rest);
    break;
  case 'snapshot':
    handleSnapshot(rest);
    break;
  case 'help':
  case '--help':
  case '-h':
    printHelp();
    break;
  default:
    console.log(`${ANSI.red}Unknown command: ${command}${ANSI.reset}`);
    printHelp();
}

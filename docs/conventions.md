# Conventions

Navigation pointer target from `AGENTS.md`. This is the reference the review
agent reads; keep judgement calls here and push anything mechanical into
`scripts/check.sh` or a linter instead of prose.

## Module System & Zero Dependencies

- Pure ES Modules (`"type": "module"` in `package.json`).
- The Node app relies exclusively on built-in modules (`node:fs`, `node:path`,
  `node:url`, `node:http`, `node:readline`, `node:test`, `node:assert/strict`).
  Do not introduce third-party Node runtime or build dependencies.
- `hs-sniff` is Rust and may carry Rust deps; the zero-dep rule is a Node rule.

## Key Normalization & Item Lookup

Always normalize string inputs using `normalizeKey(str)` in `src/database.js`
to strip casing, punctuation, and extraneous spaces before querying indexes:

```javascript
import { normalizeKey, findItem, searchItems } from './database.js';

const item = findItem("Lucifer's Crown");
const results = searchItems('helm', { rarity: 'satanic', grade: 'SS', slot: 'helmet' });
```

## Valuation Engine Pattern

`src/engine.js` functions are pure and deterministic:

```javascript
import { evaluateItem, VERDICTS } from './engine.js';

const result = evaluateItem(itemData, {
  quality: 115,       // Satanic Dice quality roll (50-115)
  sockets: 2,         // Socket count or null for base
  seasonPhase: 'mid', // 'early' | 'mid' | 'late'
});

if (result.verdict.code === VERDICTS.INSTANT_PRICE_CHECK.code) {
  // Flag for marketplace check
}
```

### Decision Hierarchy (`VERDICTS`)

1. `INSTANT_PRICE_CHECK` (Tier 1, Red): Angelic/Unholy/Heroic, Grade SS
   Satanics, High Runes (T3/T4), 115% God Rolls.
2. `CONDITIONAL_CHECK` (Tier 2, Yellow): Grade S Satanics, Mid Runes (T2),
   Leveling Runewords.
3. `RARE_EXCEPTION_DUST` (Tier 3, Blue): Grade A Satanics. Default to crystal
   dusting unless Quality $\ge 110\%$ or Max Sockets.
4. `DUST_PROSPECT` (Tier 4, Gray): Grade B/C/D Satanics, Low Runes (T1), vendor
   trash with zero market liquidity.

## Social Signal Pattern

`src/social.js` is pure like the engine — extraction and query logic take rows
and return rows; all I/O lives in `src/social-source.js`:

```javascript
import { querySignals, summarizeCorpus } from './social.js';
import { ingestSearch, loadSignals, resolveToken } from './social-source.js';

const credential = await resolveToken(); // explicit → env → local client session
await ingestSearch('215140390443941888', { ...credential, query: 'WTS', channelIds: ['907084529787895898'] });
const rows = loadSignals();
const answer = querySignals(rows, 'item:bucket quality:>=110 currency:gold since:7d');
```

Query language: `key:value` filters (`item`, `rune`, `rarity`, `grade`, `slot`,
`channel`, `author`, `kind`, `basis`, `currency`, `quality`, `sockets`, `price`,
`since`, `until`, `sort`, `limit`) with `>= <= > <` on numeric keys, plus
free-text AND terms. Prices are grouped by currency in every aggregate — a gold
value and a rune denomination are never averaged together.

`hs-sniff/src/market.rs` follows the same shape: `SessionContext`, request
building, and response decoding are pure and unit-tested; only `fetch_market`
touches the network.

## Error Handling & Safety

- Wrap JSON file parsing and HTTP request body extraction in `try/catch`.
- Provide safe fallback returns (empty arrays or `null`) for missing datasets or
  failed lookups.
- Return explicit HTTP status codes (400 for malformed JSON, 404 for unknown
  items/routes).
- Never log or persist credentials. In `hs-sniff`, the market session
  `identifier` and checksum are held for signing but masked in every JSON
  projection (`to_json` reports `*_present` booleans only).

## Naming Conventions

- Dictionaries & constants: `UPPER_SNAKE_CASE` (`VERDICTS`, `HIGH_VALUE_STATS`,
  `QUALITY_MULTIPLIERS`, `ANSI`).
- Functions & variables: `camelCase` (`evaluateItem`, `normalizeKey`,
  `findItem`).
- Dataset files: `kebab-case.json` (`unified-items.json`, `runes.json`).

## Testing & QA

- Framework: native `node:test` with `node:assert/strict`.
- Execute: `npm test`, or a single file (`node --test test/engine.test.js`).
- Suites run against live datasets loaded via `src/database.js`.
- Validate verdict thresholds for every grade tier (Angelic SS down to
  Satanic D).
- Validate quality multiplier upgrades (e.g. 115% turning Grade S/A items into
  instant price checks).
- Validate rune tiers (High T3/T4 vs Low T1).
- Validate fuzzy and exact matching via `findItem` and `searchItems`.
- Social suites build fixtures in a temp dir and point the puller at a local
  `node:http` stub via `HS_SOCIAL_API_BASE` / `apiBase` — never live Discord,
  never the real corpus.
- Credential tests synthesize a fake client `Local Storage/leveldb` and delete
  `HS_SOCIAL_TOKEN` / `HS_SOCIAL_AUTH` / `HS_SOCIAL_DISCORD_DIR` first, so the
  real session token is never read or asserted on.
- Rust: `cargo test --manifest-path hs-sniff/Cargo.toml`. Market HTTP is not
  exercised in tests; the parser and request builder are tested with synthetic
  bytes.

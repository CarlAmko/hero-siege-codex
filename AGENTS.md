# Repository Guidelines

## Project Overview
Hero Siege Codex is a zero-dependency Node.js application and CLI utility for Hero Siege ARPG item valuation, loot grading, and economy price-check triage. It provides instantaneous in-game loot decisions (Always Price Check, Conditional Check, Prospect/Dust, Vendor) based on item grade tiers (SS through D), Satanic Dice quality rolls (50% to 115%), socket counts, major affix weighting, and season lifecycle phases.

## Architecture & Data Flow
The project follows a modular, zero-dependency Node.js ES Modules architecture:

```
data/*.json ──> src/database.js ──> src/engine.js ──┬──> bin/hs-economy.js (CLI)
                                                    ├──> src/server.js (HTTP / API) ──> public/index.html
                                                    └──> src/index.js (Library Export)
```

- **Data Ingestion & Indexing (`src/database.js`)**: Synchronously loads JSON datasets (`data/*.json`) via `node:fs` on module initialization. Builds in-memory lookup `Map` structures (`itemsById`, `itemsByName`, `runesByName`, `runewordsByName`) for $O(1)$ exact queries and weighted substring/fuzzy matching.
- **Valuation & Scoring Engine (`src/engine.js`)**: Pure, deterministic evaluation logic scoring items (0–100) based on grade base values, weighted affixes (`HIGH_VALUE_STATS`), Satanic Dice quality multipliers (50% corrupted = 0.2x up to 115% god roll = 6.0x), socket thresholds, and season lifecycle adjustments (`early`, `mid`, `late`).
- **HTTP & REST Server (`src/server.js`)**: Native `node:http` server exposing REST endpoints (`/api/search`, `/api/evaluate`, `/api/items`, `/api/runes`) and serving static assets for the client-side SPA in `public/index.html`.
- **CLI Interface (`bin/hs-economy.js`)**: Terminal client supporting quick item checks, search filters, tier list browsing, and an interactive continuous triage REPL (`triage`) using `node:readline` and ANSI formatting.
- **Library Root (`src/index.js`)**: Barrel export re-exporting all database and engine APIs.

## Key Directories
- `src/`: Core application modules (database indexer, valuation engine, HTTP server, library entry point).
- `bin/`: CLI binary executable (`hs-economy.js`).
- `data/`: Reference JSON datasets for items, runes, runewords, sets, affixes, augments, and gear slots.
- `public/`: Frontend codex SPA (vanilla HTML5, CSS3, and JavaScript).
- `test/`: Native Node.js unit and integration test suites.

## Development Commands

### Run Web Server & API
```bash
npm start
# or: node src/server.js (Listens on process.env.PORT || 3000)
```

### Run Tests
```bash
npm test
# or: node test/engine.test.js
# or: node --test test/engine.test.js
```

### Run CLI Tools
```bash
# Interactive Triage REPL for fast in-game drop evaluation
npm run triage
# or: node bin/hs-economy.js triage

# Direct Item Valuation Check
npm run check -- "Harlequinn's Crest" -q 115 -s 2 --season mid
# or: node bin/hs-economy.js check "The Bucket" -q 100

# Item Search with Filters
npm run search -- "crown" --rarity satanic --grade SS
# or: node bin/hs-economy.js search "sword" --slot weapon

# View Grade Tier List
npm run tierlist
# or: node bin/hs-economy.js tierlist SS
```

## Code Conventions & Common Patterns

### Module System & Zero Dependencies
- Pure ES Modules (`"type": "module"` in `package.json`).
- Relies exclusively on Node.js built-in modules (`node:fs`, `node:path`, `node:url`, `node:http`, `node:readline`, `node:test`, `node:assert/strict`).
- Do not introduce third-party runtime or build dependencies.

### Key Normalization & Item Lookup
Always normalize string inputs using `normalizeKey(str)` in `src/database.js` to strip casing, punctuation, and extraneous spaces before querying indexes:

```javascript
import { normalizeKey, findItem, searchItems } from './database.js';

// Exact / normalized lookup
const item = findItem("Lucifer's Crown");

// Filtered multi-criteria search
const results = searchItems('helm', { rarity: 'satanic', grade: 'SS', slot: 'helmet' });
```

### Valuation Engine Pattern
Evaluation functions in `src/engine.js` are pure and deterministic:

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
1. `INSTANT_PRICE_CHECK` (Tier 1, Red): Angelic/Unholy/Heroic, Grade SS Satanics, High Runes (T3/T4), 115% God Rolls.
2. `CONDITIONAL_CHECK` (Tier 2, Yellow): Grade S Satanics, Mid Runes (T2), Leveling Runewords. Worth checking if rolls/sockets are high.
3. `RARE_EXCEPTION_DUST` (Tier 3, Blue): Grade A Satanics. Default to crystal dusting unless Quality $\ge 110\%$ or Max Sockets.
4. `DUST_PROSPECT` (Tier 4, Gray): Grade B/C/D Satanics, Low Runes (T1), vendor trash with zero market liquidity.

### Error Handling & Safety
- Wrap JSON file parsing and HTTP request body extraction in `try/catch` blocks.
- Provide safe fallback returns (e.g. empty arrays or `null`) for missing datasets or failed item lookups.
- Return explicit HTTP status codes (400 for malformed JSON, 404 for unknown items/routes).

### Naming Conventions
- Dictionaries & Constants: `UPPER_SNAKE_CASE` (e.g., `VERDICTS`, `HIGH_VALUE_STATS`, `QUALITY_MULTIPLIERS`, `ANSI`).
- Functions & Variables: `camelCase` (e.g., `evaluateItem`, `normalizeKey`, `findItem`).
- Dataset Files: `kebab-case.json` (e.g., `unified-items.json`, `data/runes.json`).

## Important Files
- `src/index.js`: Public barrel export re-exporting database and valuation engine interfaces.
- `src/database.js`: In-memory data store, JSON loader, key normalizer, and item lookup logic.
- `src/engine.js`: Valuation engine, verdict rules, affix weights, and score calculations.
- `src/server.js`: Native HTTP REST API (`/api/search`, `/api/evaluate`) and static file server.
- `bin/hs-economy.js`: CLI binary providing `check`, `search`, `tierlist`, and `triage` commands.
- `data/unified-items.json`: Master item database covering Satanics, Angelics, and Heroics with grades and stats.
- `data/runes.json` & `data/runewords.json`: Rune tiers, recipes, and market designations.
- `test/engine.test.js`: Native test suite asserting valuation scores, quality upgrades, and verdict accuracy.
- `public/index.html`: Web codex single-page application dashboard.
- `package.json`: Project manifest defining ESM module type, scripts, and `hs-economy` bin entry point.

## Runtime/Tooling Preferences
- **Runtime**: Node.js >= 18.0.0 (required for native `node:test` runner and ESM features). Compatible with Bun.
- **Package Manager**: npm. No lockfile required due to zero external runtime dependencies.
- **Build / Lint Constraints**: No bundler (Webpack/Vite/Rollup) or transpiler (Babel/TypeScript) is required. Code executes directly in vanilla Node.js.

## Testing & QA
- **Test Framework**: Native Node.js test runner (`node:test`) with strict assertions (`node:assert/strict`).
- **Execution**: Run `npm test` or `node test/engine.test.js`.
- **Testing Conventions**:
  - Test suites execute against live datasets loaded via `src/database.js`.
  - Validate verdict thresholds for every grade tier (Angelic SS down to Satanic D).
  - Validate quality multiplier upgrades (e.g. 115% Satanic Dice turning Grade S/A items into instant price checks).
  - Validate rune tiers (High Runes T3/T4 vs Low Runes T1).
  - Validate fuzzy and exact item matching via `findItem` and `searchItems`.

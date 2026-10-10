# Architecture

Navigation pointer target from `AGENTS.md`. Module layout, data flow, commands,
and where each owner lives.

## Project Overview

Hero Siege Codex is a zero-dependency Node.js application and CLI utility for
Hero Siege ARPG item valuation, loot grading, and economy price-check triage. It
provides instantaneous in-game loot decisions (Always Price Check, Conditional
Check, Prospect/Dust, Vendor) based on item grade tiers (SS through D), Satanic
Dice quality rolls (50% to 115%), socket counts, major affix weighting, and
season lifecycle phases.

The repo also vendors a Rust packet-sniffer (`hs-sniff/`) and carries reference
implementations under `refs/` (gitignored).

## Architecture & Data Flow

```
data/*.json ──> src/database.js ──> src/engine.js ──┬──> bin/hs-economy.js (CLI)
                                                    ├──> src/server.js (HTTP / API) ──> public/index.html
                                                    └──> src/index.js (Library Export)

Discord API / channel exports ──> src/social-source.js ──> src/social.js ──┬──> bin/hs-social.js (CLI)
                                       data/social-signals.jsonl           ├──> bin/hs-social-mcp.js (MCP stdio)
                                                                           └──> src/server.js (/api/social/*)

hs-sniff (Rust) ──> live TCP capture ──> src/events.rs ──┬──> bin (`watch`/`replay`)
                                                         └──> src/market.rs ──> game market API (live prices)
```

- **Data Ingestion & Indexing (`src/database.js`)**: Synchronously loads JSON
  datasets (`data/*.json`) via `node:fs` on module initialization. Builds
  in-memory lookup `Map` structures (`itemsById`, `itemsByName`, `runesByName`,
  `runewordsByName`) for $O(1)$ exact queries and weighted substring/fuzzy
  matching.
- **Valuation & Scoring Engine (`src/engine.js`)**: Pure, deterministic
  evaluation logic scoring items (0–100) based on grade base values, weighted
  affixes (`HIGH_VALUE_STATS`), Satanic Dice quality multipliers (50% corrupted =
  0.2x up to 115% god roll = 6.0x), socket thresholds, and season lifecycle
  adjustments (`early`, `mid`, `late`).
- **HTTP & REST Server (`src/server.js`)**: Native `node:http` server exposing
  REST endpoints (`/api/search`, `/api/evaluate`, `/api/items`, `/api/runes`)
  and serving static assets for the client-side SPA in `public/index.html`.
- **CLI Interface (`bin/hs-economy.js`)**: Terminal client supporting quick item
  checks, search filters, tier list browsing, and an interactive continuous
  triage REPL (`triage`) using `node:readline` and ANSI formatting.
- **Library Root (`src/index.js`)**: Barrel export re-exporting all database,
  engine, and social APIs.
- **Social Signal Pipeline (`src/social.js`, `src/social-source.js`)**: Turns
  Hero Siege Discord trade/price-check chatter into structured rows — entity
  (resolved against the dictionaries with exact name-window matching), Satanic
  Dice quality, sockets, price (gold normalized to a base value; runes kept per
  denomination) — and answers questions over them. `social.js` is pure
  (extraction, query language, price bands); `social-source.js` owns I/O:
  credential acquisition (explicit token → env → the local client's stored
  session), channel/guild discovery, keyword harvest via guild search, channel
  export import, and the append-only `data/social-signals.jsonl` corpus (with a
  committed gzipped snapshot it falls back to). Derived facts only: authors are
  stored as short hashes, never names or ids.
- **Packet Sniffer (`hs-sniff/`)**: Rust crate: passive TCP reassembly, protocol
  decode into normalized events, a hand-rolled MCP server, and a market client
  that re-issues the game's own HTTPS market query. See `hs-sniff/README.md`.

## Key Directories

- `src/`: Core application modules (database indexer, valuation engine, social
  pipeline, HTTP server, library entry point).
- `bin/`: Executables — `hs-economy.js`, `hs-social.js`, `hs-social-mcp.js`.
- `data/`: Reference JSON datasets, plus the generated live
  `social-signals.jsonl` corpus (gitignored) and its committed `.gz` snapshot.
- `public/`: Frontend codex SPA (vanilla HTML5, CSS3, JavaScript).
- `test/`: Native Node.js unit and integration test suites.
- `hs-sniff/`: Rust packet-sniffer (library + CLI + MCP + market client).
- `refs/`: Gitignored read-only reference projects (see `AGENTS.md`).
- `scripts/`: Repo tooling (`check.sh` guardrail).

## Development Commands

```bash
# Web server & API (process.env.PORT || 3000)
npm start

# Tests
npm test                 # Node suites only (engine, farming, social)
npm run test:rust        # hs-sniff crate
npm run test:all         # both
npm run guardrail        # what the pre-commit hook runs

# CLI tools
npm run triage                       # interactive triage REPL
npm run check -- "The Bucket" -q 100 # direct valuation
npm run search -- "crown" --rarity satanic --grade SS
npm run tierlist                      # grade tier list

# Social signals
npm run social -- query 'item:bucket quality:>=110 currency:gold since:7d'
npm run social -- stats
npm run social:mcp                    # MCP stdio server

# hs-sniff
cd hs-sniff && cargo test
cargo run --release -- watch          # live capture (needs cap_net_raw / Npcap)
cargo run --release -- market "2 socket Harlequinn's Crest" --live
```

`npm test` runs the local Node suites only; `refs/` and `hs-plan/` are excluded
deliberately.

## Important Files

- `src/index.js`: Public barrel export.
- `src/database.js`: In-memory data store, JSON loader, key normalizer, lookups.
- `src/engine.js`: Valuation engine, verdict rules, affix weights, scoring.
- `src/social.js`: Social-signal extraction, query parser, price bands. Pure.
- `src/social-source.js`: Discord puller, export import, signal corpus.
- `src/server.js`: Native HTTP REST API + static file server.
- `bin/hs-economy.js`: `check`, `search`, `tierlist`, `triage`.
- `bin/hs-social.js`: `token`, `guilds`, `channels`, `ingest`, `import`, `query`, `stats`.
- `bin/hs-social-mcp.js`: MCP tools `hs_social_query`, `hs_social_summary`, `hs_social_ingest`.
- `data/unified-items.json`: Master item database.
- `data/runes.json` & `data/runewords.json`: Rune tiers, recipes, market designations.
- `test/engine.test.js`, `test/social.test.js`, `test/farming.test.js`.
- `public/index.html`: Web codex SPA.
- `IDEAS.md`: Backlog; each entry states problem, rough shape, and open questions.
- `hs-sniff/README.md`: Capture, CLI, MCP, and the market client.

## Reading the data

`refs/captures/*.json` are **single-line** files (`statlist-helper.json` is one
~58 KB line). A plain `head`/`cat` dumps the whole thing into context — use
`jq` (installed) or bound the read:

```bash
jq '.[0:3]' refs/captures/statlist-helper.json
head -c 2000 refs/captures/lootfilter-mmphys-ids.json
```

## Runtime / Tooling

- **Runtime**: Node.js >= 18 (native `node:test`, ESM). Compatible with Bun.
- **Package manager**: npm. No lockfile required for the Node app (no external
  runtime deps). `hs-sniff` has its own `Cargo.lock`.
- **Build/lint**: no bundler or transpiler; code runs directly in Node.
- **Rust**: `hs-sniff` builds with cargo; `ureq`/`sha2`/`flate2` are its only
  non-serde runtime deps.

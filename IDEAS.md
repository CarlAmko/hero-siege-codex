# Ideas

Backlog of directions for Hero Siege Codex that are not yet built. Each entry
states the problem, why it matters, a rough shape, and the open questions that
block a spec. Promote an entry to a spec when it gets picked up.

## 1. Discord social-signal ingestion → lightweight query engine

**Captured:** 2026-09-10 · **Status:** built (2026-09-10)

### Problem

The codex grades items from static, datamined datasets (`data/*.json`,
`hs-plan/data/`). Those cover what an item *is*, not what the market *thinks*:
current prices, which rolls are actually moving, which zones and bosses players
report dropping a given item, whether an affix combination is even obtainable
this season. Hero Siege is large and thinly documented, so most of that lives
only in player conversation — and Discord trade/price-check channels are the
densest store of it.

Public sourcing is therefore one of the strongest signals available for
valuation: the game is too big for a small community to have written it down
anywhere canonical, so player discourse *is* the reference.

### As built

`src/social.js` (pure extraction + query engine), `src/social-source.js` (bot-API
puller, export import, JSONL corpus), `bin/hs-social.js` (CLI: `ingest`,
`import`, `query`, `stats`), `bin/hs-social-mcp.js` (MCP stdio), and
`/api/social/query` + `/api/social/summary` on the HTTP server.

1. **Ingest** — messages come from the official bot API (never a self-bot) or
   from an operator-exported channel dump, both through one normalizer.
2. **Query** — each message is resolved against the dictionaries
   (`normalizeKey`, `findItem`, item/rune/runeword/set name windows) into an
   entity, quality, sockets, and price; rows are indexed and queried with a
   `key:value` filter language plus price bands that never average across
   currencies.

### Decisions the open questions resolved to

- **Corpus source** — both. The puller is the primary path; `import` exists for
  exports and is what makes the puller testable without a credential.
- **Credential** — pivoted from bot-only to the user session (2026-09-10
  follow-up, reusing the 2026-08-29 harvest method): the token is read from the
  local Discord client's `Local Storage/leveldb` and validated against
  `/users/@me`, with `/users/@me/guilds` + `/guilds/{id}/channels` for
  discovery and `/guilds/{id}/messages/search` for keyword harvest. A bare
  `Authorization` header, not `Bot …`. Bot tokens still work. This is
  self-botting: against Discord's ToS with account-termination risk, so it is
  opt-in, read-only, and warned about in the CLI and module header.
- **Price normalization** — gold is normalized to a base value (`k/m/b/t`
  suffixes) and runes stay per-denomination; a row also records whether the
  number was `asking`, a `bid`, `sold`, or `unknown`. Bare small numbers are
  counts, not prices.
- **Ranking** — recency with a snowflake-id tie-break; `sort:price` and
  `sort:quality` are available but no reputation model was invented.
- **Storage** — derived rows, not transcripts: author is a short sha1 prefix,
  text is capped at 400 chars and droppable with `--no-text`.
- **Placement** — in-repo Node, not a separate Rust crate. The original lean was
  wrong: entity resolution is specified against Node APIs (`normalizeKey`,
  `findItem`), and Node's built-in `fetch` removes the dependency problem the
  entry cited.

### Still open

- Channel permissions: `/guilds/{id}/channels` lists channels the account may
  not read. The whole `Trading` category 403s (`sc-trade-listings`,
  `ns-sc-trade-listings`, `hc-trade-listings`, `price-check`, `sc-services`), as
  do several class channels gated behind a role reaction (`bard-discussion`,
  `illusionist-discussion`, `jötunn-discussion`, `prophet-discussion`,
  `redneck-discussion`, `stormweaver-discussion`, `viking-discussion`) plus
  `known-bugs-channel`. A harvest there yields nothing until the account claims
  the matching role. Trade coverage is gated on permissions, not on the code.
- Zone/boss attribution: nothing extracts drop-source claims from messages yet,
  so the "where does this drop" question is unanswered.
- Price reality: the live `#general` "WTS" results were RMT (`2 usd`, `90€`),
  which the gold/rune extractor deliberately ignores; whether to model cash
  offers is unresolved.
- Corpus growth and pruning: the 2026-09-11 S10 backfill (`--since`, 43
  readable channels) left ~568k rows / ~149 MB in the append-only JSONL, which
  every query reloads and dedupes — measured ≈1.2 s for `stats` and ≈0.9 s for
  a filtered query on this machine. Workable here, but there is no pruning,
  compaction, or index, and a second backfill of that size would not be.
- Reputation/corroboration ranking across authors, if raw recency proves noisy.

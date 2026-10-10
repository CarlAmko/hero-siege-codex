# hs-sniff

Passive Hero Siege traffic reader — a Rust port of the packet-sniffing
functionality from the reference implementations vendored in `refs/`
(notably [hs-tracker](../refs/hs-tracker) and
[hero-siege-stats](../refs/hero-siege-stats)), shaped as a library + CLI +
MCP server for the Hero Siege Codex tooling.

The game talks **plaintext JSON over TCP** (plus base64 blobs and
URL-encoded bodies). Nothing is injected, nothing is decrypted, nothing is
sent anywhere: the game's own conversation is read as it passes, the same
way `tcpdump` reads any traffic.

## Layout

```
src/reasm.rs      TCP flow reassembly (per flow+ack buffers, carry-over tail)
src/decode.rs     protocol decode: JSON scan, base64 blobs, query strings
src/events.rs     normalized GameEvents: gold, xp, kills, drops, zone, mail…
src/market.rs     market client: session context, checksum, query parse, prices
src/sweep.rs      order-book analysis: unit prices, depth weighting, craft margins
src/wire.rs       pcap file reader + jsonl flush log (replay/testing)
src/capture.rs    live capture (feature `live`): process discovery + BPF scoping
src/mcp.rs        MCP stdio server (hand-rolled JSON-RPC)
src/itemtable.rs  optional (type,id,weaponType) -> name/rarity table loader
src/main.rs       CLI: watch / replay / sniff-stdin / context / market / sweep / serve
data/             generated market catalogs (names -> masks, runewords, stats)
tools/            gen-market-catalog.mjs — regenerates data/ from the Companion
```

The game's gameplay traffic is plaintext, but its **market is HTTPS**: a
passive capture sees only the client's own outgoing request (its session
context, in the clear) and never the TLS answer. `market.rs` therefore
re-issues the game's own market query with the checksum/multipass the client
uses, exactly as the Hero Siege Companion does. Nothing is injected into the
game and no traffic is decrypted; the re-issued query is a normal authenticated
API call made under your own account, so treat it the way you treat the game's
own Market button.

## Build

```bash
cargo build --release            # full build (live capture on)
cargo build --release --no-default-features   # without libpcap dependency
cargo test                       # unit tests across all modules
```

Runtime dependencies: `serde`/`serde_json`/`base64`/`form_urlencoded`, plus
`sha2` (market checksum), `flate2` (the market's zlib `items` blob) and `ureq`
(the HTTPS market call). Only the live-capture feature adds the platform
capture stack (`pcap`, `etherparse`, `netstat2`, `sysinfo`), so
`--no-default-features` still builds a working `market` command.

Live capture needs the same rights as tcpdump: **Npcap** on Windows (install
with *WinPcap API-compatible Mode*), `cap_net_raw` on Linux
(`sudo setcap cap_net_raw+eip target/release/hs-sniff` or run as root).

## CLI

```bash
# Live session: stream events, print a rolling summary
sudo ./target/release/hs-sniff watch

# Replay a capture file (pcap) or a jsonl flush log
./target/release/hs-sniff replay session.pcap

# Decode a raw payload from stdin
echo -n '{"satanicZoneName":"Blood Moors","buffs":[1,2]}' | ./target/release/hs-sniff sniff-stdin
```

Event JSON lines look like:

```json
{"kind":"item_added","name":"","rarity":6,"tier":0,"item_type":6,"item_id":1,"ground":true,...}
{"kind":"satanic_zone","zone":"Blood Moors","buffs":[1,2],"debuffs":[]}
{"kind":"gold","gss":1234,"delta":0,...}
{"kind":"session_context","account_id":"2304","season":"11","ready":true,...}
```

## Market prices

The game's own Market button is the only way to see live listings, and it
speaks HTTPS. `market` follows the Companion: it collects the six session
fields from the client's outgoing requests, then posts the same
`market_fetch_items` query to the same endpoint under your account.

```bash
# What have we sniffed? (all six fields?)
./target/release/hs-sniff context --replay session.jsonl

# One live query, read out of a capture you took with `watch --log`
./target/release/hs-sniff market "2 socket Harlequinn's Crest" --replay session.jsonl

# Or capture live until the context is complete (open the in-game Market once)
sudo ./target/release/hs-sniff market "Grief" --live --timeout 60

# Raw result
./target/release/hs-sniff market "magic find >= 20 Harlequinn's Crest" --replay session.jsonl --json
```

Output is gold listings plus a currency-separated band:

```
gold market — 34 matching, showing 20 on this page
  lowest 120000  median 450000  highest 1900000  (gold)
  120000 gold
  180000 gold  (4500 / unit)
  ...
```

The plain-text parser is deterministic (no model call). It understands item
and runeword names from the bundled catalog, plus `N sockets`,
`min-sockets:N` / `max-sockets:N` / `sockets:N-M`, `min <stat> N` /
`<stat> >= N` for the 310 named market stats, and `mask:N` /
`runeword:<id|name>` for anything the catalog cannot name.

Live fetches are rate-limited: a **10-second cooldown** between different
queries, with a **30-second result cache** so an identical query inside the
window is served from memory. The long-lived MCP server shares one client, so
the cooldown is per-process; the one-shot CLI starts fresh each time.

### Priming the session context

A query needs the six fields the client signs its own requests with. They are
not on a timer — the game only emits them when it makes a signed request, so
**open the in-game Market once (or do a vote/instance reset) while capturing**.
`hs-sniff context --live` and `market --live` print which fields are still
missing. `crossregion_identifier` rotates every session, so a context read from
an old capture is rejected by the server as a checksum failure; capture fresh.

### Capturing under Steam / Proton (Linux)

- Rights: `cap_net_raw` on the binary (`sudo setcap cap_net_raw+eip hs-sniff`)
  or run as root, the same as tcpdump. Windows needs Npcap.
- The game is a Windows executable under Proton; its process shows up as
  `Hero_Siege.exe` and the crate's process matcher accounts for that.
- The sniffer scopes the BPF filter to the game's own TCP connections. When it
  cannot identify the process it falls back to all TCP and says so in `watch`'s
  status line (`bad filter:` / `note:`), which is the first thing to check when
  no events decode.
- **Port 443 is skipped by design** — it is TLS. Gameplay JSON and the market
  *requests* are plaintext on the game's other ports; the market *responses*
  come back over HTTPS and are never readable passively, which is why `market`
  re-issues the query instead of snooping the answer.

## Sweeping for mispricings

`market` answers one query. `sweep` walks a watchlist of fungible items and
reports what is structurally wrong with each book:

```bash
# Runes + runewords (the default watchlist), context read from a capture
hs-sniff sweep --replay session.jsonl

# A whole bucket, or explicit items
hs-sniff sweep --replay session.jsonl --materials
hs-sniff sweep --replay session.jsonl --name Mal --name Malice --mask 1073741824

# Machine-readable, with pacing
hs-sniff sweep --replay session.jsonl --delay 800 --json > sweep.json
```

**Everything is per unit.** `price` is the *stack total*: half the live rune rows
have `price != unit_price`, and a 13-unit stack of Mal at 24,500/unit reports
`price = 318,500`. Banding `price` — which the game's own `avgPrice` does —
averages stacks of different sizes and invents spreads that aren't there. Sorting
by it is worse: the true cheapest unit can sit *above* a smaller stack.

Each item is treated as an **order book**, and for a fungible one page is the
whole book (a tier-2 rune returns 62 listings and the next page is empty). The
ladder is grouped into price levels and weighted by units, so a 13-unit stack is
13 supply rather than one vote. The fair-value reference is the median across
*levels*, not across units: unit-weighting alone lets one huge expensive stack
drag the centre up and invent a discount for the ask.

Signals, in output order:

| Signal | Meaning |
|---|---|
| `CRAFT MARGIN` | The runes to build a runeword cost less than the runeword sells for. **The actionable direction** — runes are consumed by crafting and cannot be extracted again, so the reverse is not a trade. |
| `CLIFF` | The best ask sits well below the next level. Depth is shown: a 700% cliff on a single unit is a snipe, not a business. |
| `BELOW CENTRE` | The best ask sits far below the book's own middle. Suppressed when a cliff already explains it. |
| `DON'T CRAFT` | The runeword is cheaper than its runes — information, not an opportunity. |

Craft margins are ranked by **capturable profit** (`gap × crafts`), not by
percentage. `crafts` is bounded by the shallowest ingredient's depth *and* by the
runeword book's own supply, so a 34,000% margin you could only ever run eight
times ranks below a 74% margin worth five million. A runeword with a single
listing is skipped outright: one seller's ask is not a market price.

Two things the margin does **not** include: the socketed base item, and the
market's sale tax (the client prints a `Market Tax value:` readout). Both reduce
it, so treat the figures as an upper bound.

Pacing is `--delay` milliseconds between fetches (default 1000; a full
runes+runewords sweep is ~128 items and about three minutes). A rate-limit
rejection backs off and retries instead of aborting. `--data-dir` (default
`../data`) must hold `runes.json` and `runewords.json` for the ingredient lists;
names resolve through the bundled catalog. Runeword queries omit `filter_masks`
entirely — an explicit empty array is an HTTP 500.

### Realised prices, inferred from what disappears

The game exposes no market-wide transactions: `market_fetch_history` and
`market_fetch_sell` are your own basket only. So every sweep appends one row per
observed listing to a JSONL corpus and diffs it against the previous sweep:

```json
{"item":"Mal","market_id":10635905,"price":7333,"ts":1791651427482,"unit_price":7333,"units":1}
```

| | |
|---|---|
| `--observations FILE` | corpus path (default `../data/market-observations.jsonl`, gitignored) |
| `--no-observations` | skip recording and diffing entirely |

Keyed on the monotonic `market_id`, so each sweep reports three things:
**appeared** (new listings), **repriced**, and **gone** — the last observed price
of a listing that is no longer on sale, folded per item into an inferred-sale
view. A vanished listing may have sold, been cancelled, or simply expired at
midnight, so treat it as a proxy rather than a trade feed; it is nonetheless the
only market-wide evidence of a transaction the game gives you.

The diff is scoped to the items being swept this run, so dropping an item from
the watchlist does not read as all of its listings having sold. No seller
identity is stored — the corpus holds derived facts only.

## Item names

hs-tracker resolves `(type, id, weaponType)` to a name through 1,491
datamined entries generated from hero-siege-helper data. This crate does not
bundle that table for the *event* pipeline; names stay empty unless the
packet carries one. Supply your own:

```bash
./target/release/hs-sniff --table items.tsv replay session.pcap
```

TSV columns: `type  id  weaponType  name  rarity` (header optional).
Ambiguous names (two items sharing one) are flagged `Unknown` by the table
rather than guessed.

The *market* catalog is different and is bundled, because a name lookup there
needs the `repository` bit the event table drops: `data/market-items.tsv`
(1,911 normals/uniques), `data/market-runewords.tsv` (100) and
`data/market-stats.tsv` (310). Regenerate them from the vendored Companion
extract after bumping the submodule:

```bash
node hs-sniff/tools/gen-market-catalog.mjs
```

## MCP server

```bash
pi mcp add hs-sniff -- /full/path/to/target/release/hs-sniff serve
```

| Tool                 | What it gives the client                                       |
| -------------------- | -------------------------------------------------------------- |
| `hs_status`          | capture status + current session summary                       |
| `hs_summary`         | gold Δ, xp, drops by rarity, character, room, Satanic Zone     |
| `hs_events`          | drain recent normalized events (oldest first)                  |
| `hs_peek`            | look at recent events without draining them                    |
| `hs_replay`          | decode a pcap/jsonl file into the session                      |
| `hs_market_context`  | the six market session fields, and whether a query can be signed |
| `hs_market`          | live prices for a plain-text query (`{"query": "2 socket The Bucket"}`) |

Set `HS_SNIFF_LIVE=1` in the server env to also start live capture in the
background, if the process has capture rights. `hs_market` needs that context:
run `hs_replay` over a capture, or let the live capture see the in-game Market
opened once, before asking for prices.

## Attribution & license

Protocol work ported from MIT-licensed projects:
[hs-tracker](https://github.com/Parazeya/hs-tracker) (Rust; reassembler,
decoder, event rules, capture architecture) and
[hero-siege-stats](https://github.com/GuilhermeFaga/hero-siege-stats)
(Python/scapy; protocol signatures). Hero Siege is © Panic Art Studios;
neither this tool nor those projects are affiliated with or endorsed by
them. MIT.

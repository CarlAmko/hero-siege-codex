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
src/wire.rs       pcap file reader + jsonl flush log (replay/testing)
src/capture.rs    live capture (feature `live`): process discovery + BPF scoping
src/mcp.rs        MCP stdio server (hand-rolled JSON-RPC)
src/itemtable.rs  optional (type,id,weaponType) -> name/rarity table loader
src/main.rs       CLI: watch / replay / sniff-stdin / context / market / serve
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

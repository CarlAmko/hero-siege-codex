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
src/wire.rs       pcap file reader + jsonl flush log (replay/testing)
src/capture.rs    live capture (feature `live`): process discovery + BPF scoping
src/mcp.rs        MCP stdio server (hand-rolled JSON-RPC, no new deps)
src/itemtable.rs  optional (type,id,weaponType) -> name/rarity table loader
src/main.rs       CLI: watch / replay / sniff-stdin / serve
```

## Build

```bash
cargo build --release            # full build (live capture on)
cargo build --release --no-default-features   # without libpcap dependency
cargo test                       # unit tests across all modules
```

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
```

## Item names

hs-tracker resolves `(type, id, weaponType)` to a name through 1,491
datamined entries generated from hero-siege-helper data. This crate does not
bundle that table; the event pipeline leaves names empty unless the packet
carries one. Supply your own:

```bash
./target/release/hs-sniff --table items.tsv replay session.pcap
```

TSV columns: `type  id  weaponType  name  rarity` (header optional).
Ambiguous names (two items sharing one) are flagged `Unknown` by the table
rather than guessed.

## MCP server

```bash
claude mcp add hs-sniff -- /full/path/to/target/release/hs-sniff serve
```

| Tool         | What it gives the client                                   |
| ------------ | ---------------------------------------------------------- |
| `hs_status`  | capture status + current session summary                   |
| `hs_summary` | gold Δ, xp, drops by rarity, character, room, Satanic Zone |
| `hs_events`  | drain recent normalized events (oldest first)              |
| `hs_peek`    | look at recent events without draining them                |
| `hs_replay`  | decode a pcap/jsonl file into the session                  |

Set `HS_SNIFF_LIVE=1` in the server env to also start live capture in the
background, if the process has capture rights.

## Attribution & license

Protocol work ported from MIT-licensed projects:
[hs-tracker](https://github.com/Parazeya/hs-tracker) (Rust; reassembler,
decoder, event rules, capture architecture) and
[hero-siege-stats](https://github.com/GuilhermeFaga/hero-siege-stats)
(Python/scapy; protocol signatures). Hero Siege is © Panic Art Studios;
neither this tool nor those projects are affiliated with or endorsed by
them. MIT.

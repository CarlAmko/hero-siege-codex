//! hs-sniff — passive Hero Siege traffic reader.
//!
//! Ported from the protocol work in `refs/hs-tracker` (Rust) and
//! `refs/hero-siege-stats` (Python/scapy): the game talks plaintext JSON
//! (plus base64 blobs and URL-encoded bodies) over TCP, so a passive capture
//! is enough to follow drops, gold, XP and the Satanic Zone. Nothing is
//! injected and nothing is decrypted; the game's own conversation is read as
//! it passes.
//!
//! Pipeline:
//! bytes -> [`reasm::Reassembler`] (TCP flow/ack reassembly)
//! -> [`decode::extract_messages`] (JSON / base64 / query-string)
//! -> [`events::events_from_messages`] (normalized [`events::GameEvent`]s).
//!
//! Three front doors share that pipeline:
//! - [`capture`]: live capture off the wire (feature `live`).
//! - [`wire`]: replay from a pcap file or a jsonl flush log, for tests and
//!   offline analysis.
//! - [`mcp`]: MCP stdio server exposing status, a session summary and the
//!   recent event stream to an MCP client.
//!
//! Item-name tables are NOT bundled. hs-tracker resolves `(type, id,
//! weaponType)` to a name through 1,491 datamined entries; the event pipeline
//! leaves names empty unless the packet carries one, and offers
//! [`events::set_name_resolver`] to plug a table in without this crate
//! carrying one.

pub mod decode;
pub mod events;
pub mod market;
pub mod mcp;
pub mod reasm;
pub mod sweep;
pub mod wire;

#[cfg(feature = "live")]
pub mod capture;

pub use decode::extract_messages;
pub use events::{
    events_from_messages, Currency, GameEvent, Summary, RARITIES, SS_TIER,
};
pub use market::{
    build_market_body, fetch_market, item_mask, parse_market_query, parse_market_response,
    MarketCatalog, MarketCatalogTable, MarketClient, MarketError, MarketOutcome, MarketQuery,
    MarketResult, MarketTarget, PriceBand, SessionContext, StatFilter, MARKET_COOLDOWN_MS,
};
pub use reasm::{Flow, Reassembler};
pub use sweep::{
    analyze, recipes_from_json, ItemBook, PriceLevel, Recipe, Signal, Thresholds,
};
pub use wire::{parse_pcap_file, Frame, ReplaySource, WireError};

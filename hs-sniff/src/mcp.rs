//! MCP (Model Context Protocol) stdio server, implemented by hand on
//! JSON-RPC 2.0 — this crate keeps third-party dependencies to the four it
//! already carries.
//!
//! Tools exposed:
//! - `hs_status` — capture/pipeline status
//! - `hs_summary` — session summary (gold, xp, items by rarity, zone)
//! - `hs_events` — recent events, oldest first, drained from the stream
//! - `hs_replay` — run the pipeline over a pcap or jsonl file and return the
//!   summary
//!
//! With no live capture running, `hs_events`/`hs_summary` still work over a
//! replay fed by `hs_replay`: a replay's events feed the same buffers.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::events::{events_from_messages, GameEvent, Summary};
use crate::wire::ReplaySource;

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_BUFFERED_EVENTS: usize = 4096;

#[derive(Default)]
struct SessionState {
    summary: Summary,
    events: std::collections::VecDeque<Value>,
}

impl SessionState {
    fn push(&mut self, event: &GameEvent) {
        self.summary.observe(event);
        self.events.push_back(event.to_json());
        while self.events.len() > MAX_BUFFERED_EVENTS {
            self.events.pop_front();
        }
    }
}

/// Shared state used by both the MCP server and the CLI.
#[derive(Default)]
pub struct SharedState {
    inner: Mutex<SessionState>,
}

impl SharedState {
    pub fn ingest(&self, event: &GameEvent) {
        self.inner.lock().unwrap().push(event);
    }
    pub fn summary(&self) -> Summary {
        self.inner.lock().unwrap().summary.clone()
    }
    pub fn drain_events(&self, limit: usize) -> Vec<Value> {
        let mut g = self.inner.lock().unwrap();
        let n = limit.min(g.events.len());
        g.events.drain(..n).collect()
    }
    pub fn peek_events(&self, limit: usize) -> Vec<Value> {
        let g = self.inner.lock().unwrap();
        g.events.iter().rev().take(limit).rev().cloned().collect()
    }
    pub fn reset(&self) {
        *self.inner.lock().unwrap() = SessionState::default();
    }
}

/// Run the stdio server until the client closes its end. `status` is called
/// for `hs_status` so the caller can describe its own capture (an empty
/// string means "no live capture").
pub fn serve(shared: Arc<SharedState>, status: impl Fn() -> String + Send + Sync + 'static) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let status: Arc<dyn Fn() -> String + Send + Sync + 'static> = Arc::new(status);
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            let _ = writeln!(stdout, "{}", json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "Parse error"}
            }));
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = req.get("method").and_then(|m| m.as_str()) else { continue };

        // Notifications (no id) get no response.
        if id.is_null() && !matches!(method, "initialize") {
            if method == "notifications/initialized" {
                continue;
            }
            continue;
        }

        let result: Result<Value, (i32, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "hs-sniff", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools_list() })),
            "tools/call" => call_tool(&req, &shared, &status),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        let response = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }
}

fn call_tool(
    req: &Value,
    shared: &Arc<SharedState>,
    status: &Arc<dyn Fn() -> String + Send + Sync + 'static>,
) -> Result<Value, (i32, String)> {
    let name = req
        .pointer("/params/name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| (-32602, "missing params.name".to_string()))?;
    let args = req.pointer("/params/arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "hs_status" => {
            let live = status();
            json!({
                "live": if live.is_empty() { "not running".to_string() } else { live },
                "session": shared.summary_json(),
            })
        }
        "hs_summary" => shared.summary_json(),
        "hs_events" => {
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(50).min(1000) as usize;
            json!({"events": shared.drain_events(limit)})
        }
        "hs_peek" => {
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(50).min(1000) as usize;
            json!({"events": shared.peek_events(limit)})
        }
        "hs_replay" => {
            let path = args
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| (-32602, "missing params.arguments.path (pcap or jsonl file)".to_string()))?;
            let source = ReplaySource::from_path(path).map_err(|e| (-32602, e.to_string()))?;
            let mut events = Vec::new();
            match source {
                ReplaySource::Pcap(frames) => {
                    let mut asm = crate::reasm::Reassembler::default();
                    for frame in frames {
                        let Some(src) = frame.src else { continue };
                        // Reassemble per direction using a synthetic port
                        // pair from the TCP header when present.
                        let (sp, dp, ack) = tcp_parts(&frame.data).unwrap_or((0, 0, 0));
                        if let Some(flushed) = asm.push((src, sp, dp), ack, &frame.data) {
                            for m in crate::decode::extract_messages(&flushed) {
                                events.extend(events_from_messages(&[m]));
                            }
                        }
                    }
                    for (_src, flushed) in asm.drain_all() {
                        for m in crate::decode::extract_messages(&flushed) {
                            events.extend(events_from_messages(&[m]));
                        }
                    }
                }
                ReplaySource::Jsonl(rows) => {
                    for (src, data) in rows {
                        for m in crate::decode::extract_messages(&data) {
                            let _ = src;
                            events.extend(events_from_messages(&[m]));
                        }
                    }
                }
            }
            for e in &events {
                shared.ingest(e);
            }
            json!({
                "events_ingested": events.len(),
                "summary": shared.summary_json(),
            })
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&text).unwrap_or_default()}]
    }))
}

/// (src_port, dst_port, ack) out of a stripped IPv4/IPv6+TCP frame, best
/// effort.
fn tcp_parts(ip: &[u8]) -> Option<(u16, u16, u32)> {
    let ihl = match ip.first()? >> 4 {
        4 => ((*ip.first()? & 0x0f) as usize) * 4,
        6 => 40,
        _ => return None,
    };
    if ip.len() < ihl + 20 {
        return None;
    }
    let t = &ip[ihl..];
    let sp = u16::from_be_bytes([t[0], t[1]]);
    let dp = u16::from_be_bytes([t[2], t[3]]);
    let ack = u32::from_be_bytes([t[8], t[9], t[10], t[11]]);
    Some((sp, dp, ack))
}

impl SharedState {
    fn summary_json(&self) -> Value {
        let s = self.summary();
        json!({
            "gold_delta": s.gold_delta,
            "xp_gain": s.xp_gain,
            "items_seen": s.items_seen,
            "items_by_rarity": s.items_by_rarity,
            "satanic_zone": s.satanic_zone,
            "room": s.room,
            "account": s.account,
            "character": s.character,
            "level": s.level,
            "kills": s.kills,
        })
    }
}

/// The tool catalog, built at call time (json! allocates, so no `const`).
fn tools_list() -> Vec<Value> {
    vec![
    json!({
        "name": "hs_status",
        "description": "Hero Siege sniffer: live capture status and current session summary.",
        "inputSchema": {"type": "object", "properties": {}}
    }),
    json!({
        "name": "hs_summary",
        "description": "Session summary so far: gold, xp, drops by rarity, character, Satanic Zone.",
        "inputSchema": {"type": "object", "properties": {}}
    }),
    json!({
        "name": "hs_events",
        "description": "Drain recent normalized game events (gold, xp, item_added, satanic_zone, room, account...), oldest first. Drained events are not returned again.",
        "inputSchema": {
            "type": "object",
            "properties": {"limit": {"type": "integer", "description": "max events to return", "default": 50}},
            "required": []
        }
    }),
    json!({
        "name": "hs_peek",
        "description": "Look at recent events without draining them, newest last.",
        "inputSchema": {
            "type": "object",
            "properties": {"limit": {"type": "integer", "description": "max events to return", "default": 50}},
            "required": []
        }
    }),
    json!({
        "name": "hs_replay",
        "description": "Run the decode pipeline over a pcap capture file (or a hs-sniff jsonl flush log) and ingest the events into the session.",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string", "description": "path to a .pcap or .jsonl file"}},
            "required": ["path"]
        }
    }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_state_roundtrip() {
        let shared = SharedState::default();
        shared.ingest(&GameEvent::XpGain(15));
        shared.ingest(&GameEvent::Room("Act_01_02".into()));
        assert_eq!(shared.summary().xp_gain, 15);
        let drained = shared.drain_events(10);
        assert_eq!(drained.len(), 2);
        assert!(shared.drain_events(10).is_empty());
    }
}

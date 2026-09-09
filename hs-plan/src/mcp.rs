//! MCP (Model Context Protocol) stdio server for build planning — same
//! hand-rolled JSON-RPC 2.0 shape as hs-sniff's server, no new dependencies.
//!
//! Tools:
//! - `hs_plan_stats`  — full stats readout for a build
//! - `hs_plan_perf`   — DPS/performance readout for a build
//! - `hs_plan_suggest`— ranked tree-node upgrades under a point budget
//! - `hs_plan_decode` — planner share code → build JSON
//!
//! Everything is a pure function of the call arguments (no session state),
//! so the server works from any MCP client without setup.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use hs_plan::share;
use hs_plan::{PlanError, calc_performance, calc_stats, suggest};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the stdio server until the client closes its end.
pub fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
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
        if id.is_null() {
            continue;
        }

        let result: Result<Value, (i32, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "hs-plan", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools_list() })),
            "tools/call" => call_tool(&req),
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

fn call_tool(req: &Value) -> Result<Value, (i32, String)> {
    let name = req
        .pointer("/params/name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| (-32602, "missing params.name".to_string()))?;
    let args = req.pointer("/params/arguments").cloned().unwrap_or(json!({}));

    let text = match name {
        "hs_plan_stats" => {
            let build = build_arg(&args)?;
            let top = args.get("top").and_then(|t| t.as_u64()).map(|t| t as usize);
            json!({
                "stats": calc_stats(&build).map_err(tool_err)?,
                "top": top,
            })
        }
        "hs_plan_perf" => {
            let build = build_arg(&args)?;
            json!({ "performance": calc_performance(&build).map_err(tool_err)? })
        }
        "hs_plan_suggest" => {
            let build = build_arg(&args)?;
            let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(30) as u32;
            let skills: Option<Vec<String>> = args.get("activeSkillIds").and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
            });
            suggest(&build, budget, skills).map_err(tool_err)?
        }
        "hs_plan_decode" => {
            let input = args
                .get("code")
                .and_then(|c| c.as_str())
                .ok_or_else(|| (-32602, "missing params.arguments.code".to_string()))?;
            share::decode_share(input)
                .ok_or_else(|| (-32602, "not a valid planner share code (v2)".to_string()))?
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&text).unwrap_or_default()}]
    }))
}

fn tool_err(e: PlanError) -> (i32, String) {
    (-32602, e.to_string())
}

fn build_arg(args: &Value) -> Result<Value, (i32, String)> {
    args.get("build")
        .cloned()
        .ok_or_else(|| (-32602, "missing params.arguments.build (BuildPerformanceInput JSON)".to_string()))
}

fn tools_list() -> Vec<Value> {
    let build_prop = json!({
        "build": {
            "type": "object",
            "description": "HSPlanner BuildPerformanceInput (camelCase). Minimal example: {\"classId\":\"marksman\",\"level\":60,\"skillRanks\":{\"gunner_drone\":20},\"inventory\":{}} — or the `perf` object produced by hs_plan_decode."
        }
    });
    vec![
    json!({
        "name": "hs_plan_stats",
        "description": "Hero Siege build planner: full final-stats readout for a build (attributes, defensive stats, item bonuses).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "build": build_prop,
                "top": {"type": "integer", "description": "only include the N strongest stats (optional)"}
            },
            "required": ["build"]
        }
    }),
    json!({
        "name": "hs_plan_perf",
        "description": "Hero Siege build planner: DPS/performance readout for a build (hit DPS, proc DPS, combined, per-source contributions).",
        "inputSchema": {
            "type": "object",
            "properties": { "build": build_prop },
            "required": ["build"]
        }
    }),
    json!({
        "name": "hs_plan_suggest",
        "description": "Hero Siege build planner: ranked incarnation-tree node upgrades (greedy + 2-opt against the real DPS calc) under a point budget.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "build": build_prop,
                "budget": {"type": "integer", "description": "tree points to spend (default 30)"},
                "activeSkillIds": {"type": "array", "items": {"type": "string"}, "description": "skills to optimize DPS for (default: the build's main skill)"}
            },
            "required": ["build"]
        }
    }),
    json!({
        "name": "hs_plan_decode",
        "description": "Decode an HSPlanner share code or URL (?b=...) into the build JSON the other hs_plan_* tools accept.",
        "inputSchema": {
            "type": "object",
            "properties": {"code": {"type": "string", "description": "planner share code or full URL"}},
            "required": ["code"]
        }
    }),
    ]
}

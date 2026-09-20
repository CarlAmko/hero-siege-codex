//! hs-plan serve-ui — HTTP bridge that runs the stock HSPlanner web UI
//! against our vendored engine, with no Tauri.
//!
//! The upstream frontend calls `invoke('<cmd>', args)` for every computed
//! number. In a plain browser those calls fail (no Tauri backend). This
//! server exposes the exact same command surface over `POST /invoke`, and
//! `--dist <path>` injects a small browser shim (`ui-shim.js`) into the
//! built frontend so `invoke` reroutes here. Same UI, our implementation.
//!
//! ```text
//! hs-plan serve-ui [--port 8787] [--dist refs/HSPlanner/dist]
//! ```

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use hs_plan::calc::commands as cmd;
use hs_plan::calc::tooltip_parse::parse_tooltip_lines;

#[path = "../capture.rs"]
mod capture;

/// Browser shim, embedded at build time and written into the dist by
/// `--dist` (also served at `GET /ui-shim.js` as a fallback).
const SHIM: &str = include_str!("../ui-shim.js");

fn main() {
    let mut port: u16 = 8787;
    let mut dist: Option<String> = None;
    let mut argv = std::env::args().skip(1);
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--port" => port = argv.next().and_then(|v| v.parse().ok()).unwrap_or(8787),
            "--dist" => dist = argv.next(),
            other => {
                eprintln!("unknown arg: {other}");
                eprintln!("usage: serve-ui [--port 8787] [--dist <planner-dist-dir>]");
                std::process::exit(2);
            }
        }
    }

    if let Some(d) = &dist {
        match inject_shim(d) {
            Ok(()) => println!("[serve-ui] shim injected into {d}"),
            Err(e) => eprintln!("[serve-ui] shim injection into {d} failed: {e}"),
        }
    }

    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[serve-ui] bind {addr} failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[serve-ui] bridge on http://{addr} — expecting planner UI on :4173");

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                std::thread::spawn(move || handle(s));
            }
            Err(e) => eprintln!("[serve-ui] accept: {e}"),
        }
    }
}

/// Copy the shim into the dist and reference it from index.html so it runs
/// before the app bundle (plain <head> script = synchronous; module scripts
/// are deferred, so ordering is guaranteed).
fn inject_shim(dist: &str) -> std::io::Result<()> {
    let dir = std::path::Path::new(dist);
    std::fs::write(dir.join("ui-shim.js"), SHIM)?;
    let index = dir.join("index.html");
    let html = std::fs::read_to_string(&index)?;
    if !html.contains("ui-shim.js") {
        let tag = "<script src=\"./ui-shim.js\"></script>";
        let patched = if let Some(p) = html.find("<head>") {
            let mut out = String::with_capacity(html.len() + tag.len());
            out.push_str(&html[..p + 6]);
            out.push_str(tag);
            out.push_str(&html[p + 6..]);
            out
        } else {
            format!("{tag}{html}")
        };
        std::fs::write(&index, patched)?;
    }
    Ok(())
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let (method, path, body) = match read_request(&mut stream) {
        Some(r) => r,
        None => return,
    };

    if method == "OPTIONS" {
        respond(&mut stream, "204 No Content", b"", "text/plain");
        return;
    }

    if method == "GET" && (path == "/ui-shim.js" || path == "/ui-shim.js/") {
        respond(&mut stream, "200 OK", SHIM.as_bytes(), "text/javascript");
        return;
    }

    if method != "POST" || path != "/invoke" {
        let body = b"{\"ok\":false,\"error\":\"not found\"}";
        respond(&mut stream, "404 Not Found", body, "application/json");
        return;
    }

    let reply = match serde_json::from_slice::<Value>(&body) {
        Ok(req) => {
            let command = req.get("cmd").and_then(Value::as_str).unwrap_or("");
            let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
            match dispatch(command, &args) {
                Ok(data) => json!({ "ok": true, "data": data }),
                Err(e) => json!({ "ok": false, "error": e }),
            }
        }
        Err(e) => json!({ "ok": false, "error": format!("bad request body: {e}") }),
    };
    let bytes = serde_json::to_vec(&reply).unwrap_or_else(|_| b"{\"ok\":false}".to_vec());
    respond(&mut stream, "200 OK", &bytes, "application/json");
}

/// Read one HTTP request (headers + content-length body). Small-memory:
/// header scan buffered, body read straight to its exact size.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        match stream.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find(&buf, b"\r\n\r\n") {
                    break p + 4;
                }
                if buf.len() > 64 * 1024 {
                    return None;
                }
            }
            Err(_) => return None,
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let content_length = lines
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .next()
        .unwrap_or(0);

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
            Err(_) => return None,
        }
    }
    body.truncate(content_length);
    Some((method, path, body))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn respond(stream: &mut TcpStream, status: &str, body: &[u8], ctype: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\n\
         access-control-allow-origin: *\r\naccess-control-allow-methods: GET, POST, OPTIONS\r\n\
         access-control-allow-headers: content-type\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

// --- command dispatch ------------------------------------------------------

fn dispatch(command: &str, args: &Value) -> Result<Value, String> {
    let out = match command {
        "calc_build_performance" => serde_json::to_value(cmd::calc_build_performance(take(args, "input")?)),
        "calc_build_stats" => serde_json::to_value(cmd::calc_build_stats(take(args, "input")?)),
        "calc_stat_breakdown" => serde_json::to_value(cmd::calc_stat_breakdown(take(args, "input")?)),
        "rank_slot_items" => serde_json::to_value(cmd::rank_slot_items(take(args, "input")?)),
        "passive_stats_at_rank" => serde_json::to_value(cmd::passive_stats_at_rank(
            take(args, "skill")?,
            take(args, "rank")?,
        )),
        "mana_cost_at_rank" => {
            serde_json::to_value(cmd::mana_cost_at_rank(take(args, "skill")?, take(args, "rank")?))
        }
        "parse_custom_stats" => {
            serde_json::to_value(cmd::parse_custom_stats(take(args, "values")?))
        }
        "display_values" => serde_json::to_value(cmd::display_values(
            take(args, "input")?,
            opt(args, "season")?,
        )),
        "classify_tree_nodes" => serde_json::to_value(cmd::classify_tree_nodes(opt(args, "season")?)),
        "subskill_aggregation" => serde_json::to_value(cmd::subskill_aggregation(take(args, "input")?)),
        "calc_warmup" => serde_json::to_value(warmup()),
        // --- tooltip OCR + parsing (upstream Tauri commands, now bridged) ---
        "ocr_tooltip_lines" => {
            let b64: String = take(args, "imageBase64")?;
            serde_json::to_value(capture::ocr_from_base64(&b64)?)
        }
        "parse_tooltip_lines" => {
            let lines: Vec<String> = take(args, "lines")?;
            let season = opt(args, "season")?;
            serde_json::to_value(parse_tooltip_lines(lines, season))
        }
        // --- capture pipeline -------------------------------------------
        "hs_capture" => {
            let b64: String = take(args, "imageBase64")?;
            let source = args
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("api")
                .to_string();
            serde_json::to_value(capture::capture_from_base64(&b64, &source)?)
        }
        "hs_captures_take" => serde_json::to_value(capture::queue().take_all()),
        "hs_capture_dismiss" => {
            let id: u64 = take(args, "id")?;
            serde_json::to_value(capture::queue().remove(id))
        }
        "compute_skill_damage" => {
            serde_json::to_value(cmd::compute_skill_damage(take(args, "input")?))
        }
        "compute_attack_skill_damage" => {
            serde_json::to_value(cmd::compute_attack_skill_damage(take(args, "input")?))
        }
        "compute_weapon_damage" => {
            serde_json::to_value(cmd::compute_weapon_damage(take(args, "input")?))
        }
        other => return Err(format!("unknown command: {other}")),
    };
    out.map_err(|e| format!("serializing {command} output: {e}"))
}

/// Warmup is expensive and idempotent — run once per process.
fn warmup() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| cmd::run_warmup(|_, _| {}))
}

fn take<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let v = args
        .get(key)
        .ok_or_else(|| format!("missing arg: {key}"))?;
    serde_json::from_value(v.clone()).map_err(|e| format!("bad arg {key}: {e}"))
}

fn opt<T: DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("bad arg {key}: {e}")),
    }
}

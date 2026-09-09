//! hs-plan CLI — headless build planning.
//!
//! ```text
//! hs-plan stats   <build.json|-> [--top N]   final stats readout
//! hs-plan perf    <build.json|-> [--top N]   DPS/performance readout
//! hs-plan suggest <build.json|-> [--budget N] [--skills a,b] [--json]
//! hs-plan decode  <code-or-url>              planner share code → build JSON
//! hs-plan graph                              tree graph summary
//! hs-plan serve                              MCP stdio server
//! ```
//!
//! A build file is a HSPlanner `BuildPerformanceInput` JSON (camelCase). Use
//! `decode` to produce one from a planner share link, or hand-write a
//! minimal one: {"classId":"marksman","level":60,"skillRanks":{},"inventory":{}}

use std::io::Read;
use std::process::ExitCode;

use serde_json::Value;

use hs_plan::share;
use hs_plan::{calc_performance, calc_stats, graph_json, suggest};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    let result = match cmd.as_str() {
        "stats" => cmd_stats(rest, false),
        "perf" => cmd_stats(rest, true),
        "suggest" => cmd_suggest(rest),
        "decode" => cmd_decode(rest),
        "graph" => {
            println!("{}", pretty(&graph_json()));
            Ok(())
        }
        "serve" => {
            hs_plan_mcp::serve();
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            Err(other.to_string())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
hs-plan — Hero Siege build planning (Marksman drone ready)

USAGE:
  hs-plan stats   <build.json|-> [--top N]     final stats readout
  hs-plan perf    <build.json|-> [--top N]     DPS / performance readout
  hs-plan suggest <build.json|-> [--budget N] [--skills a,b] [--json]
  hs-plan decode  <share-code-or-url>          planner code → build JSON
  hs-plan graph                                tree graph summary
  hs-plan serve                                MCP stdio server

Build input is HSPlanner's BuildPerformanceInput (camelCase JSON); `decode`
turns a planner share link into exactly that.";

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn read_build(path: &str) -> Result<Value, String> {
    let text = match path {
        "-" => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            buf
        }
        p => std::fs::read_to_string(p).map_err(|e| format!("reading {p}: {e}"))?,
    };
    serde_json::from_str(&text).map_err(|e| format!("parsing build JSON: {e}"))
}

/// Pull `--top N` out of the args.
fn take_top(args: &mut Vec<String>) -> Result<Option<usize>, String> {
    let mut top = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--top" {
            let n = args.get(i + 1).ok_or("--top requires a number")?;
            top = Some(n.parse::<usize>().map_err(|_| "bad --top value")?);
            args.remove(i);
            args.remove(i);
        } else {
            i += 1;
        }
    }
    Ok(top)
}

fn cmd_stats(args: &[String], perf: bool) -> Result<(), String> {
    let mut args = args.to_vec();
    let json = take_flag(&mut args, "--json");
    let top = take_top(&mut args)?;
    let path = args.first().map(String::as_str).unwrap_or("-");
    let build = read_build(path)?;
    let out = if perf {
        calc_performance(&build)
    } else {
        calc_stats(&build)
    }
    .map_err(|e| e.to_string())?;
    if json {
        println!("{}", pretty(&out));
    } else {
        println!("{}", render_ranged(&out, top));
    }
    Ok(())
}

/// Remove `flag` from the args, returning whether it was present.
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let mut found = false;
    while let Some(i) = args.iter().position(|a| a == flag) {
        args.remove(i);
        found = true;
    }
    found
}

/// Render the big stats maps as a sorted `key  min..max` list, trimming the
/// JSON noise for terminal reading. `--top N` keeps only the strongest rows.
fn render_ranged(out: &Value, top: Option<usize>) -> String {
    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    if let Some(stats) = out.get("stats").and_then(Value::as_object) {
        for (k, v) in stats {
            let (min, max) = ranged(v);
            rows.push((k.clone(), min, max));
        }
    }
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    if let Some(n) = top {
        rows.truncate(n);
    }
    let mut s = String::new();
    if let Some(skill) = out.get("activeSkillName").and_then(Value::as_str) {
        s.push_str(&format!("skill: {skill}"));
        if let Some(entities) = out.get("entityCount").and_then(Value::as_array) {
            let n = entities.last().and_then(Value::as_f64).unwrap_or(1.0);
            if n > 1.0 {
                s.push_str(&format!(" x{n:.0} entities"));
            }
        }
        s.push('\n');
        for (label, key) in [
            ("combined DPS", "combinedDps"),
            ("hit DPS", "hitDps"),
            ("avg hit DPS", "avgHitDps"),
            ("proc DPS", "procDps"),
            ("ailment DPS", "ailmentDps"),
        ] {
            let min = out.get(format!("{key}Min")).and_then(Value::as_f64);
            let max = out.get(format!("{key}Max")).and_then(Value::as_f64);
            if let (Some(min), Some(max)) = (min, max) {
                s.push_str(&format!("  {label:16} {min:.1}..{max:.1}\n"));
            }
        }
        s.push('\n');
    }
    for (k, min, max) in &rows {
        if (min - max).abs() < f64::EPSILON {
            s.push_str(&format!("{k:38} {min:.2}\n"));
        } else {
            s.push_str(&format!("{k:38} {min:.2}..{max:.2}\n"));
        }
    }
    if s.is_empty() {
        s = pretty(out);
    }
    s
}

fn ranged(v: &Value) -> (f64, f64) {
    match v {
        Value::Number(n) => (n.as_f64().unwrap_or(0.0), n.as_f64().unwrap_or(0.0)),
        Value::Array(a) if a.len() == 2 => (
            a[0].as_f64().unwrap_or(0.0),
            a[1].as_f64().unwrap_or(0.0),
        ),
        _ => (0.0, 0.0),
    }
}

fn cmd_suggest(args: &[String]) -> Result<(), String> {
    let mut budget: u32 = 30;
    let mut skills: Option<Vec<String>> = None;
    let mut json = false;
    let mut positional: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--budget" => {
                i += 1;
                budget = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .ok_or("--budget requires a number")?;
            }
            "--skills" => {
                i += 1;
                let list = args.get(i).ok_or("--skills requires a,b,c")?;
                skills = Some(
                    list.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                );
            }
            "--json" => json = true,
            p if p.starts_with('-') => return Err(format!("unknown flag: {p}")),
            p => positional = Some(p.to_string()),
        }
        i += 1;
    }
    let build = read_build(positional.as_deref().unwrap_or("-"))?;
    let out = suggest(&build, budget, skills).map_err(|e| e.to_string())?;

    if json {
        println!("{}", pretty(&out));
        return Ok(());
    }
    println!(
        "base DPS {:.1} → final DPS {:.1} (+{:.1}%)  nodes {}/{} (budget {})",
        out["baseDps"].as_f64().unwrap_or(0.0),
        out["finalDps"].as_f64().unwrap_or(0.0),
        pct(out["baseDps"].as_f64(), out["finalDps"].as_f64()),
        out["budgetUsed"].as_u64().unwrap_or(0),
        out["budgetRequested"].as_u64().unwrap_or(0),
        out["budgetRequested"].as_u64().unwrap_or(0),
    );
    let Some(seq) = out["sequence"].as_array() else {
        return Ok(());
    };
    let width = seq.len().to_string().len().max(2);
    for (rank, step) in seq.iter().enumerate() {
        let title = step["title"].as_str().unwrap_or("?");
        let filler = if step["isFiller"].as_bool().unwrap_or(false) {
            " (path)"
        } else {
            ""
        };
        println!(
            " {:>width$}. +{:>7.1}  {}{}",
            rank + 1,
            step["gain"].as_f64().unwrap_or(0.0),
            title,
            filler,
            width = width
        );
        for line in step["lines"].as_array().cloned().unwrap_or_default() {
            if let Some(l) = line.as_str() {
                println!("        {l}");
            }
        }
    }
    if let Some(unsupported) = out["unsupportedLines"].as_array() {
        if !unsupported.is_empty() {
            println!("\nunsupported (ignored by the calc):");
            for l in unsupported {
                if let Some(l) = l.as_str() {
                    println!("  - {l}");
                }
            }
        }
    }
    Ok(())
}

fn pct(base: Option<f64>, after: Option<f64>) -> f64 {
    match (base, after) {
        (Some(b), Some(a)) if b > 0.0 => (a / b - 1.0) * 100.0,
        _ => 0.0,
    }
}

fn cmd_decode(args: &[String]) -> Result<(), String> {
    let input = args
        .first()
        .ok_or("usage: hs-plan decode <share-code-or-url>")?;
    if args.len() > 1 {
        return Err("decode takes exactly one argument (quote the URL)".to_string());
    }
    let decoded = share::decode_share(input).ok_or("not a valid planner share code (v2)")?;
    println!("{}", pretty(&decoded));
    Ok(())
}

/// MCP server module, shared shape with hs-sniff's.
#[path = "mcp.rs"]
mod hs_plan_mcp;

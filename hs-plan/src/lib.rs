//! hs-plan — Hero Siege build planning: stats, damage, and tree-node
//! upgrade suggestions for any class (built for the Marksman drone build).
//!
//! The valuation core is the MIT-licensed HSPlanner engine
//! (<https://github.com/zium1337/HSPlanner>), vendored under `src/calc` +
//! `src/suggest_engine` with the Tauri/OCR layers stripped; `data/` mirrors
//! upstream's `data/` (Season 10 base + patch layers).
//!
//! Pipeline:
//! planner share code (`?b=…`) → [`share::decode_share`] → build input JSON →
//! [`calc_stats`] / [`calc_performance`] for readouts, or [`suggest`] for
//! ranked tree-node upgrades against the real DPS calc as oracle.

pub mod calc;
pub mod graph;
pub mod lzstring;
pub mod share;
pub mod suggest_engine;

use serde_json::Value;
use std::collections::HashSet;

use crate::calc::commands::BuildPerformanceInput;
use crate::calc::season::SeasonScope;
use crate::suggest_engine::types::{SuggestInput, TreeGraph};

/// Errors surfaced to the CLI/MCP layer.
#[derive(Debug)]
pub enum PlanError {
    /// Input JSON did not deserialize into the engine's build-input shape.
    BadInput(String),
    /// Season id not present in the bundled data.
    UnknownSeason(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::BadInput(msg) => write!(f, "bad build input: {msg}"),
            PlanError::UnknownSeason(s) => write!(f, "unknown season: {s}"),
        }
    }
}
impl std::error::Error for PlanError {}

/// Warm the data LazyLocks (regex cache etc.) so the first real calc isn't
/// slow. Cheap and idempotent.
pub fn warmup() -> bool {
    crate::calc::commands::run_warmup(|_, _| {})
}

/// Deserialize a build-input JSON value, reporting the serde error path on
/// failure so users can fix their payload.
///
/// Mirrors upstream's frontend bridge (`utils/calc/bridge.ts`): the main
/// skill defaults to the first active skill id when the payload doesn't
/// carry one explicitly.
fn parse_perf(input: &Value) -> Result<BuildPerformanceInput, PlanError> {
    let mut input = input.clone();
    if input.get("mainSkillId").is_none() {
        if let Some(first) = input
            .get("activeSkillIds")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            input["mainSkillId"] = Value::String(first.to_string());
        }
    }
    serde_json::from_value(input).map_err(|e| PlanError::BadInput(e.to_string()))
}

/// Aggregated character readout: final stats + performance numbers.
pub fn calc_stats(input: &Value) -> Result<Value, PlanError> {
    let perf = parse_perf(input)?;
    let _scope = SeasonScope::enter(perf.season.clone());
    let stats = crate::calc::commands::calc_build_stats(perf);
    serde_json::to_value(stats).map_err(|e| PlanError::BadInput(e.to_string()))
}

/// Damage/performance readout (hit DPS, proc DPS, combined midpoint).
pub fn calc_performance(input: &Value) -> Result<Value, PlanError> {
    let perf = parse_perf(input)?;
    let _scope = SeasonScope::enter(perf.season.clone());
    let out = crate::calc::commands::calc_build_performance(perf);
    serde_json::to_value(out).map_err(|e| PlanError::BadInput(e.to_string()))
}

/// One suggested tree node with its DPS delta along the purchase order.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedNode {
    pub node_id: u32,
    pub title: String,
    pub lines: Vec<String>,
    pub dps_before: f64,
    pub dps_after: f64,
    pub gain: f64,
    pub is_filler: bool,
}

/// Run the upgrade suggester: given a build (perf payload) + tree budget,
/// returns the ranked node sequence that maximizes the build's DPS under the
/// real calc. `active_skill_ids` defaults to the perf's `mainSkillId`.
pub fn suggest(
    perf_json: &Value,
    budget: u32,
    active_skill_ids: Option<Vec<String>>,
) -> Result<Value, PlanError> {
    let perf = parse_perf(perf_json)?;
    let season = perf.season.clone();
    let _probe = SeasonScope::enter(season.clone());

    let active: Vec<String> = match active_skill_ids {
        Some(ids) if !ids.is_empty() => ids,
        _ => perf
            .main_skill_id
            .clone()
            .into_iter()
            .collect(),
    };
    let input = SuggestInput {
        perf,
        active_skill_ids: active,
        graph: graph::incarnation_graph(),
        budget,
    };
    let result = suggest_engine::command::run_suggest(&input, |_, _| {});
    let _scope = SeasonScope::enter(season);

    // Annotate each step with the node's title/lines from the data.
    let steps: Vec<SuggestedNode> = result
        .sequence
        .iter()
        .map(|s| {
            let (title, lines) = node_text(s.node_id);
            SuggestedNode {
                node_id: s.node_id,
                title,
                lines,
                dps_before: s.dps_before,
                dps_after: s.dps_after,
                gain: s.gain,
                is_filler: s.is_filler,
            }
        })
        .collect();

    Ok(serde_json::json!({
        "baseDps": result.base_dps,
        "finalDps": result.final_dps,
        "budgetUsed": result.budget_used,
        "budgetRequested": result.budget_requested,
        "addedNodes": result.added_nodes,
        "usedStarts": result.used_starts,
        "unsupportedLines": result.unsupported_lines,
        "sequence": steps,
    }))
}

fn node_text(id: u32) -> (String, Vec<String>) {
    match crate::calc::data::get_tree_node(id) {
        Some(n) => (n.title.clone(), n.lines.clone()),
        None => (format!("node {id}"), Vec::new()),
    }
}

/// Convenience: look up skills for a class (used by CLI/MCP to render
/// available drone/turret skill ids for MM).
pub fn class_skills(class_id: &str) -> Vec<(String, String, Vec<String>)> {
    crate::calc::data::get_skills_by_class(class_id)
        .iter()
        .map(|s| (s.id.clone(), s.name.clone(), s.tags.clone().unwrap_or_default()))
        .collect()
}

/// Expose the tree graph (adjacency/starts/valuables) as JSON for debugging.
pub fn graph_json() -> Value {
    let TreeGraph {
        adjacency,
        start_ids,
        warp_ids,
        valuable_ids,
        jewelry_ids,
    } = graph::incarnation_graph();
    serde_json::json!({
        "startIds": start_ids,
        "warpIds": warp_ids,
        "jewelryIds": jewelry_ids,
        "valuableCount": valuable_ids.len(),
        "nodeCount": adjacency.len(),
    })
}

/// Re-export for CLI/MCP: dedupe helper used when merging node lists.
pub fn dedupe_nodes(ids: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_has_starts_and_valuables() {
        let g = graph_json();
        assert_eq!(g["startIds"].as_array().unwrap().len(), 8);
        assert!(g["valuableCount"].as_u64().unwrap() > 300);
    }

    #[test]
    fn marksman_has_drone_skills() {
        let skills = class_skills("marksman");
        assert!(
            skills.iter().any(|(id, _, tags)| id == "gunner_drone" && tags.iter().any(|t| t == "Sentry")),
            "gunner_drone with Sentry tag expected"
        );
    }

    #[test]
    fn mm_drone_build_produces_stats() {
        let build = serde_json::json!({
            "classId": "marksman",
            "level": 60,
            "skillRanks": {"gunner_drone": 20, "turret_mastery": 10},
            "mainSkillId": "gunner_drone",
            "allocatedTreeNodes": [],
            "inventory": {}
        });
        let stats = calc_stats(&build).expect("stats");
        assert!(stats.get("stats").is_some());
        let perf = calc_performance(&build).expect("performance");
        assert!(perf.get("combinedDpsMin").is_some());
    }

    #[test]
    fn bad_input_reports_error() {
        let err = calc_stats(&serde_json::json!({"level": "sixty"})).unwrap_err();
        assert!(matches!(err, PlanError::BadInput(_)));
    }
}

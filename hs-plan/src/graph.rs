//! Derivation of the suggestion graph from the embedded tree data.
//!
//! Mirrors HSPlanner's frontend (`frontend/utils/tree/nativeSuggest.ts` +
//! `treeStats.ts`): adjacency from `incarnation-tree.json` edges, starts
//! from `n === "root"`, warps/jewelry from their node-info kinds, and
//! "valuable" candidates as every big + jewelry node except warps.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::calc::data::{INCARNATION_NODES_JSON, INCARNATION_TREE_JSON};
use crate::suggest_engine::types::TreeGraph;

#[derive(Deserialize)]
struct LayoutNode {
    id: u32,
    #[serde(default)]
    r: f64,
}

/// Build the graph lazily-used by the suggester. Panics are impossible on
/// well-formed data; malformed edges/nodes are skipped.
pub fn incarnation_graph() -> TreeGraph {
    let layout: Value = serde_json::from_str(INCARNATION_TREE_JSON).expect("incarnation-tree.json must parse");
    let info: HashMap<String, Value> =
        serde_json::from_str(INCARNATION_NODES_JSON).expect("incarnation-nodes.json must parse");

    let kind = |id: u32| -> String {
        info.get(&id.to_string())
            .and_then(|v| v.get("n"))
            .and_then(Value::as_str)
            .unwrap_or("normal")
            .to_string()
    };

    let mut adjacency: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut start_ids = Vec::new();
    let mut warp_ids = Vec::new();
    let mut jewelry_ids = Vec::new();
    let mut valuable_ids = Vec::new();

    if let Some(nodes) = layout.get("nodes").and_then(Value::as_array) {
        for raw in nodes {
            let Ok(n) = serde_json::from_value::<LayoutNode>(raw.clone()) else {
                continue;
            };
            match kind(n.id).as_str() {
                "root" => start_ids.push(n.id),
                "warp" => warp_ids.push(n.id),
                "jewelry" => {
                    jewelry_ids.push(n.id);
                    valuable_ids.push(n.id);
                }
                _ => {
                    // Layout radius >= 10 marks notables ("big" nodes).
                    if n.r >= 10.0 {
                        valuable_ids.push(n.id);
                    }
                }
            }
            adjacency.entry(n.id).or_default();
        }
    }

    if let Some(edges) = layout.get("edges").and_then(Value::as_array) {
        for raw in edges {
            let Some(pair) = raw.as_array() else { continue };
            if pair.len() != 2 {
                continue;
            }
            let (Some(a), Some(b)) = (pair[0].as_u64(), pair[1].as_u64()) else {
                continue;
            };
            let (a, b) = (a as u32, b as u32);
            adjacency.entry(a).or_default().push(b);
            adjacency.entry(b).or_default().push(a);
        }
    }

    start_ids.sort_unstable();
    warp_ids.sort_unstable();
    jewelry_ids.sort_unstable();
    valuable_ids.sort_unstable();
    for neighbors in adjacency.values_mut() {
        neighbors.sort_unstable();
        neighbors.dedup();
    }

    TreeGraph {
        adjacency,
        start_ids,
        warp_ids,
        valuable_ids,
        jewelry_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_shape_matches_upstream_data() {
        let g = incarnation_graph();
        // 8 class roots, 4 warps, 12 jewelry sockets in season data.
        assert_eq!(g.start_ids.len(), 8);
        assert_eq!(g.warp_ids.len(), 4);
        assert_eq!(g.jewelry_ids.len(), 12);
        // big (357) + jewelry (12) valuable candidates.
        assert_eq!(g.valuable_ids.len(), 369);
        // Every node id from the layout is present in adjacency.
        assert_eq!(g.adjacency.len(), 2134);
        // Roots connect to the rest of the tree.
        assert!(g.adjacency[&g.start_ids[0]].len() > 0);
        // Edges are symmetric.
        let a = g.start_ids[0];
        let b = g.adjacency[&a][0];
        assert!(g.adjacency[&b].contains(&a));
    }
}

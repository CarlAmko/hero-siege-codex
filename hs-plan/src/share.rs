//! HSPlanner share-code decoding: `https://.../?b=<lz-string>` → a
//! [`BuildPerformanceInput`]-shaped JSON the calc can consume.
//!
//! Wire schema (`frontend/utils/build/shareBuild.ts`, SCHEMA_VERSION 2):
//! `v` version, `c` class, `l` level, `a` attributes, `i` inventory,
//! `s` skill ranks, `ss` subskill ranks, `t` allocated tree nodes, `m`
//! active skill ids, `u` aura, `buf`/`ec`/`pc` condition toggles, `pt`
//! proc toggles, `kps` kills/sec, `se` season, plus `cs` custom stats,
//! `ts` tree sockets, `df` difficulty and `dp` disabled potions. Merc
//! fields (`mc`/`ms`/`mi`/`mda`) are parsed and reported but not part of
//! the calc input (the engine folds mercs in client-side).
//!
//! Robustness: the JS decoder treats *any* schema-validation failure as
//! "not a build code" (returns null), so we do the same — every field
//! parse is lenient and unknown versions are rejected rather than
//! guessed at.

use serde_json::{json, Map, Value};

use crate::lzstring::decompress_from_encoded_uri_component;

/// Extract the share code from a pasted URL or bare code: matches `[#&?]b=<code>`
/// like the frontend's `parseBuildCodeFromInput`, else treats the whole input
/// as the code.
pub fn parse_build_code_from_input(input: &str) -> &str {
    let trimmed = input.trim();
    let mut best: Option<&str> = None;
    let bytes = trimmed.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if (b == b'?' || b == b'#') && bytes.get(i + 1) == Some(&b'b') && bytes.get(i + 2) == Some(&b'=') {
            let start = i + 3;
            let end = bytes[start..]
                .iter()
                .position(|&c| c == b'&' || c.is_ascii_whitespace())
                .map(|o| start + o)
                .unwrap_or(bytes.len());
            best = Some(&trimmed[start..end]);
        }
    }
    best.unwrap_or(trimmed)
}

/// Decode a share code (or a full planner URL) into the calc's build-input JSON.
/// Returns `None` when the code does not decompress into a v2 payload.
pub fn decode_share(input: &str) -> Option<Value> {
    let code = parse_build_code_from_input(input);
    let js = decompress_from_encoded_uri_component(code)?;
    let raw: Value = serde_json::from_str(&js).ok()?;
    decode_payload(&raw)
}

/// Decode an already-decompressed payload (also usable for tests).
pub fn decode_payload(raw: &Value) -> Option<Value> {
    if raw.get("v").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    let obj = raw.as_object()?;

    let mut perf = Map::new();
    if let Some(c) = obj.get("c").and_then(Value::as_str) {
        perf.insert("classId".into(), json!(c));
    }
    if let Some(l) = obj.get("l").and_then(Value::as_u64) {
        perf.insert("level".into(), json!(l));
    }
    if let Some(a) = obj.get("a").and_then(Value::as_object) {
        perf.insert("allocatedAttrs".into(), Value::Object(a.clone()));
    }
    if let Some(i) = obj.get("i") {
        perf.insert("inventory".into(), i.clone());
    }
    if let Some(s) = obj.get("s").and_then(Value::as_object) {
        perf.insert("skillRanks".into(), Value::Object(s.clone()));
    }
    if let Some(ss) = obj.get("ss").and_then(Value::as_object) {
        perf.insert("subskillRanks".into(), Value::Object(ss.clone()));
    }
    if let Some(t) = obj.get("t").and_then(Value::as_array) {
        perf.insert("allocatedTreeNodes".into(), json!(t));
    }
    // `m` was serialized as string | string[] | null across versions.
    match obj.get("m") {
        Some(Value::Array(ids)) => {
            perf.insert("activeSkillIds".into(), json!(ids));
        }
        Some(Value::String(id)) => {
            perf.insert("activeSkillIds".into(), json!([id]));
        }
        _ => {
            if let Some(main) = obj.get("m").and_then(Value::as_str) {
                perf.insert("mainSkillId".into(), json!(main));
            }
        }
    }
    for (wire, camel) in [
        ("u", "activeAuraId"),
        ("buf", "activeBuffs"),
        ("ec", "enemyConditions"),
        ("pc", "playerConditions"),
        ("sp", "skillProjectiles"),
        ("er", "enemyResistances"),
        ("pt", "procToggles"),
        ("dp", "disabledPotions"),
        ("df", "difficulty"),
    ] {
        if let Some(v) = obj.get(wire) {
            if !v.is_null() {
                perf.insert(camel.into(), v.clone());
            }
        }
    }
    if let Some(kps) = obj.get("kps").and_then(Value::as_f64) {
        perf.insert("killsPerSec".into(), json!(kps));
    }
    if let Some(cs) = obj.get("cs").and_then(Value::as_array) {
        let stats: Vec<Value> = cs
            .iter()
            .filter_map(|s| {
                let k = s.get("k")?.as_str()?;
                let v = s.get("v")?.as_str()?;
                Some(json!({"statKey": k, "value": v}))
            })
            .collect();
        perf.insert("customStats".into(), Value::Array(stats));
    }
    if let Some(ts) = obj.get("ts").and_then(Value::as_object) {
        perf.insert("treeSocketed".into(), Value::Object(ts.clone()));
    }
    if let Some(se) = obj.get("se").and_then(Value::as_str) {
        perf.insert("season".into(), json!(se));
    }

    Some(json!({
        "perf": Value::Object(perf),
        "notes": obj.get("n").and_then(Value::as_str).unwrap_or(""),
        "season": obj.get("se").and_then(Value::as_str).unwrap_or(""),
        "merc": {
            "classId": obj.get("mc").and_then(Value::as_str),
            "skillRanks": obj.get("ms").cloned().unwrap_or(Value::Null),
            "inventory": obj.get("mi").cloned().unwrap_or(Value::Null),
            "disabledAuras": obj.get("mda").cloned().unwrap_or(Value::Null),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ground-truth vector from the verbatim reference implementation
    // (tools/lz-string.reference.js): compressToEncodedURIComponent of
    // a simple JSON document.
    const CODE: &str = "N4IgLgpgzmIFwgBYQDYoPYAIDu6BOKAJiADQgB28ALAEwC+QA";

    #[test]
    fn extracts_code_from_url() {
        assert_eq!(
            parse_build_code_from_input(&format!("https://zium1337.github.io/?b={CODE}")),
            CODE
        );
        assert_eq!(parse_build_code_from_input(CODE), CODE);
    }

    #[test]
    fn rejects_wrong_version() {
        let raw = json!({"v": 1, "c": "marksman"});
        assert!(decode_payload(&raw).is_none());
    }

    #[test]
    fn decodes_minimal_payload() {
        let code = {
            // simple v2 payload compressed by the reference tool
            let raw = json!({
                "v": 2, "c": "marksman", "l": 60,
                "a": {"strength": 10}, "s": {"gunner_drone": 20},
                "t": [0, 2], "m": ["gunner_drone"], "kps": 0, "se": "s10"
            });
            raw // real codes are tested end-to-end via decode_share below
        };
        let out = decode_payload(&code).expect("decodes");
        let perf = out.get("perf").unwrap();
        assert_eq!(perf["classId"], json!("marksman"));
        assert_eq!(perf["level"], json!(60));
        assert_eq!(perf["skillRanks"]["gunner_drone"], json!(20));
        assert_eq!(perf["activeSkillIds"], json!(["gunner_drone"]));
        assert_eq!(out["season"], json!("s10"));
    }

    #[test]
    fn decode_share_end_to_end_reference_vector() {
        // Reference-compressed v2 build payload (marksman, gunner_drone 20).
        let code = "N4IgbiBcBMA0IGMogLYEMBOBrAzugdiPADZQBsADPGlKDgC4YCm+A5vQBZQCMFAvvBy0QrAK758TDAH0AJhgD2kqNH7x6UANpVoAXXgotI8ZJnylTEPpBYADkMhUQOS5Ge8QfIA";
        let out = decode_share(&format!("https://zium1337.github.io/HSPlanner/?b={code}"))
            .expect("decodes");
        let perf = out.get("perf").unwrap();
        assert_eq!(perf["classId"], json!("marksman"));
        assert_eq!(perf["skillRanks"]["gunner_drone"], json!(20));
        assert_eq!(perf["allocatedTreeNodes"], json!([0, 2]));
        assert_eq!(out["season"], json!("s10"));
    }
}

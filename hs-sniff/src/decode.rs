//! Protocol decoder: JSON values, base64 blobs and URL-encoded bodies out of a
//! reassembled flush.
//!
//! Ported from `refs/hs-tracker/src-tauri/src/parser.rs`
//! (`extract_messages` and friends).

use base64::Engine as _;
use serde_json::Value;

use crate::reasm::{json_end, norm_eq, SCAN_BUDGET, MAX_SPAN};

/// String values that look like JSON are re-parsed, as the original does.
fn coerce(v: &Value) -> Value {
    if let Value::String(s) = v {
        let t = s.trim();
        if t.starts_with('{') || t.starts_with('[') {
            if let Ok(parsed) = serde_json::from_str(t) {
                return parsed;
            }
        }
    }
    v.clone()
}

/// Borrowing lookup — no clone, no coercion. Use this unless the value has to
/// be re-parsed from a JSON string.
pub fn field_ref<'a>(obj: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let map = obj.as_object()?;
    for n in names {
        if let Some(v) = map.get(*n) {
            return Some(v);
        }
    }
    map.iter()
        .find(|(k, _)| names.iter().any(|n| norm_eq(n, k)))
        .map(|(_, v)| v)
}

/// Normalized field lookup; string values that hold JSON are re-parsed.
pub fn field(obj: &Value, names: &[&str]) -> Option<Value> {
    field_ref(obj, names).map(coerce)
}

pub fn has(obj: &Value, names: &[&str]) -> bool {
    field_ref(obj, names).is_some()
}

/// The protocol writes whole numbers as floats (`"d": 5.0`, `"rs": 2032.0`),
/// so every numeric read has to accept both spellings.
pub fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => {
            let t = s.trim();
            t.parse::<i64>().ok().or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
        }
        _ => None,
    }
}

pub fn int_field(obj: &Value, names: &[&str]) -> i64 {
    field_ref(obj, names).and_then(as_int).unwrap_or(0)
}

/// A flag the game may send as a boolean or as a number.
pub fn as_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(other) => as_int(other) == Some(1),
        None => false,
    }
}

pub fn msg_text(obj: &Value) -> String {
    match field_ref(obj, &["message"]) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

pub fn b64_json(s: &str) -> Option<Value> {
    let t = s.trim();
    // Wire blobs may omit canonical padding; re-pad before decoding.
    let padded = match t.len() % 4 {
        2 => format!("{t}=="),
        3 => format!("{t}="),
        _ => t.to_string(),
    };
    if padded.len() % 4 != 0 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(padded.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Recursively unwrap string fields that are themselves base64-JSON
/// (S10 nests state blobs, e.g. `"s1": "eyJhIjo..."`). Depth-limited.
fn deep_unwrap(v: &Value, depth: u8) -> Value {
    if depth == 0 {
        return v.clone();
    }
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, val)| (k.clone(), deep_unwrap(val, depth - 1)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| deep_unwrap(x, depth - 1)).collect()),
        Value::String(s) => {
            if s.len() >= 16 && s.starts_with("ey") {
                if let Some(inner) = b64_json(s) {
                    return deep_unwrap(&inner, depth - 1);
                }
            }
            v.clone()
        }
        _ => v.clone(),
    }
}

/// JSON is scanned over the WHOLE buffer, so framing bytes between or inside
/// messages cannot cut one in half. The line-oriented formats (base64 blob,
/// query string) are still read per printable segment, which is how they are
/// framed.
pub fn extract_messages(buf: &[u8]) -> Vec<Value> {
    let mut out = extract_json_values(buf);
    out.extend(extract_b64_json_blobs(buf));
    for seg in buf.split(|b| *b < 0x20 || *b == 0x7f) {
        // binary noise splits into thousands of short segments; decide on the
        // raw bytes, before paying for a String
        let blob = seg.len() > 100;
        let query = seg.contains(&b'=') && seg.contains(&b'&');
        if !blob && !query {
            continue;
        }
        let s = String::from_utf8_lossy(seg);
        if blob {
            out.extend(base64_payload(&s));
        }
        if query {
            out.extend(query_payload(&s));
        }
    }
    // The blob scanners overlap by design (clean printable blob vs binary
    // envelope); drop exact duplicates so callers see each message once.
    let mut seen = std::collections::HashSet::new();
    out.retain(|v| seen.insert(v.to_string()));
    out
}

/// Base64-JSON blobs embedded in binary envelopes (S10 realtime protocol):
/// arbitrary framing bytes with `ey...` runs (base64 of `{`) inside. Used
/// for peer-state updates, character syncs and the like — shapes the plain
/// JSON scan never sees because they are wrapped in binary.
fn extract_b64_json_blobs(buf: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= buf.len() {
        // base64 of '{' is "ey" (also 'p'/'q' variants for whitespace); the
        // blobs observed on the wire start with ey followed by [A-Za-z0-9].
        let is_blob_start = buf[i] == b'e' && buf[i + 1] == b'y' && (buf[i + 2].is_ascii_alphanumeric() || buf[i + 2] == b'+' || buf[i + 2] == b'/');
        if !is_blob_start {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < buf.len() && (buf[j].is_ascii_alphanumeric() || buf[j] == b'+' || buf[j] == b'/' || buf[j] == b'=' || buf[j] == b'-') {
            j += 1;
        }
        let blob = &buf[i..j];
        // Blob must be non-trivial and base64 length-plausible.
        if blob.len() >= 24 && (blob.ends_with(b"=") || blob.len() % 4 != 1) {
            if let Some(v) = b64_json(std::str::from_utf8(blob).unwrap_or("")) {
                out.push(deep_unwrap(&v, 4));
            }
        }
        i = j.max(i + 1);
    }
    out
}

fn base64_payload(s: &str) -> Option<Value> {
    if s.len() <= 100 {
        return None;
    }
    if let Some(rest) = s.split_once("[INV]").map(|(_, r)| r) {
        return b64_json(rest);
    }
    if s.contains('&') {
        return None;
    }
    b64_json(s)
}

fn query_payload(s: &str) -> Option<Value> {
    if !s.contains('=') || !s.contains('&') {
        return None;
    }
    // start at the first key=, so protocol noise ahead of it is dropped
    let start = s.find(|c: char| c.is_ascii_alphanumeric() || c == '_')?;
    let map: serde_json::Map<String, Value> = form_urlencoded::parse(&s.as_bytes()[start..])
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, v)| (k.into_owned(), parse_query_value(v.into_owned())))
        .collect();
    (!map.is_empty()).then_some(Value::Object(map))
}

fn parse_query_value(v: String) -> Value {
    let t = v.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(parsed) = serde_json::from_str(t) {
            return parsed;
        }
    }
    Value::String(v)
}

/// Balanced-bracket scan: a flushed buffer often carries several concatenated
/// JSON messages, and a greedy first-to-last span keeps only the outermost.
///
/// An opener that never closes costs a walk to the end of the buffer before
/// the scan resumes at the next byte, so a buffer of them is quadratic in its
/// length. This runs on the capture thread between packet reads, so the
/// budget bounds one flush to a constant multiple of its length whatever the
/// bytes are.
fn extract_json_values(bytes: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut budget = bytes.len().saturating_mul(SCAN_BUDGET).saturating_add(MAX_SPAN);
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'{' || c == b'[' {
            if budget == 0 {
                break;
            }
            let mut walked = 0;
            let end = json_end(bytes, i, &mut walked);
            budget = budget.saturating_sub(walked);
            if let Some(end) = end {
                if let Ok(v) = serde_json::from_slice::<Value>(&bytes[i..=end]) {
                    let excluded = v
                        .as_object()
                        .is_some_and(|o| o.contains_key("inventory_charms") || o.contains_key("steam"));
                    if !excluded {
                        out.push(v);
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_concatenated_json() {
        let buf = b"noise{\"a\":1}{\"b\":2}tail[3,4]";
        let msgs = extract_messages(buf);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0], json!({"a": 1}));
        assert_eq!(msgs[1], json!({"b": 2}));
        assert_eq!(msgs[2], json!([3, 4]));
    }

    #[test]
    fn json_inside_strings_survives_framing() {
        let buf = b"\x01\x02{\"message\":\"has } brace inside\",\"status\":1}\x03";
        let msgs = extract_messages(buf);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["message"], "has } brace inside");
    }

    #[test]
    fn excluded_keys_are_dropped() {
        let msgs = extract_messages(b"{\"steam\":\"x\"}{\"ok\":1}");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["ok"], 1);
    }

    #[test]
    fn query_string_becomes_object() {
        let buf = b"identifier=abc&season=2&level=40.0";
        let msgs = extract_messages(buf);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["identifier"], "abc");
        assert_eq!(msgs[0]["season"], "2");
    }

    #[test]
    fn base64_blob_decodes() {
        // the blob path reads a PRINTABLE segment longer than 100 bytes, so
        // frame it with non-printable bytes the way the game's binary
        // framing does; printable padding would corrupt the base64
        let payload = serde_json::to_vec(&json!({"room": "Act_01_01", "mf": 250, "padding": "x".repeat(120)})).unwrap();
        use base64::Engine as _;
        let blob = base64::engine::general_purpose::STANDARD.encode(&payload);
        assert!(blob.len() > 100);
        let mut buf = vec![0x01, 0x02];
        buf.extend_from_slice(blob.as_bytes());
        buf.push(0x03);
        let msgs = extract_messages(&buf);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["room"], "Act_01_01");
    }

    #[test]
    fn numeric_coercion() {
        assert_eq!(as_int(&json!(5.0)), Some(5));
        assert_eq!(as_int(&json!("12")), Some(12));
        assert_eq!(as_int(&json!(12.7)), Some(12));
        assert_eq!(as_int(&json!(null)), None);
    }

    #[test]
    fn norm_key_lookup() {
        let obj = json!({"total_guild_xp": 15.0});
        assert!(has(&obj, &["totalGuildXp"]));
        assert_eq!(int_field(&obj, &["totalGuildXp"]), 15);
    }
}

//! lz-string decompression, ported from pieroxy's lz-string 1.4.5
//! (`decompressFromEncodedURIComponent` / `_decompress`) — the format HSPlanner
//! uses for its `?b=` share codes. WTFPL-licensed reference; this port keeps
//! the exact bit order so codes round-trip with the JS implementation.
//!
//! Only decompression is implemented: hs-plan reads planner codes, it does not
//! write them.
//!
//! Fidelity note: JS strings are UTF-16 code-unit sequences, so the internal
//! dictionary here holds `Vec<u32>` code units (not `char`s). Surrogate pairs
//! are only combined at the very end, mirroring `String.fromCharCode`.

const KEY_STR_URI_SAFE: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-$";

/// JS `getBaseValue(keyStrUriSafe, char)`: undefined for non-ASCII input.
fn base_value(code_unit: u32) -> Option<u32> {
    if code_unit > 127 {
        return None;
    }
    KEY_STR_URI_SAFE
        .iter()
        .position(|&c| c as u32 == code_unit)
        .map(|p| p as u32)
}

/// Port of `_decompress(length, 32, getNextValue)` with the URI-safe alphabet
/// (spaces treated as `+`, matching the JS entry point).
pub fn decompress_from_encoded_uri_component(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    let normalized = input.replace(' ', "+");
    let units: Vec<u16> = normalized.encode_utf16().collect();
    decompress(units.len(), 32, |index| {
        units.get(index).map(|&u| u as u32).and_then(base_value)
    })
}

/// Read `count` bits, LSB-first per the reference's `resb`/`power` loop,
/// refilling `val` through `next_value` whenever `position` underflows.
fn read_bits<F>(
    count: u32,
    reset_value: u32,
    val: &mut u32,
    position: &mut u32,
    index: &mut usize,
    next_value: &mut F,
) -> Option<u32>
where
    F: FnMut(usize) -> Option<u32>,
{
    let mut bits = 0u32;
    let mut power = 1u32;
    while power != 1u32 << count {
        let resb = *val & *position;
        *position >>= 1;
        if *position == 0 {
            *position = reset_value;
            *val = next_value(*index)?;
            *index += 1;
        }
        if resb > 0 {
            bits |= power;
        }
        power <<= 1;
    }
    Some(bits)
}

fn units_to_string(parts: &[Vec<u32>]) -> String {
    let mut units: Vec<u16> = Vec::new();
    for part in parts {
        units.extend(part.iter().map(|&u| u as u16));
    }
    // Pairs of surrogates combine exactly like String.fromCharCode would;
    // lone surrogates (never produced by well-formed codes) become U+FFFD.
    String::from_utf16_lossy(&units)
}

fn decompress<F>(length: usize, reset_value: u32, mut next_value: F) -> Option<String>
where
    F: FnMut(usize) -> Option<u32>,
{
    // data.val / data.position / data.index — mirrors the reference exactly.
    let mut val = next_value(0)?;
    let mut position: u32 = reset_value;
    let mut index: usize = 1;

    let mut dictionary: Vec<Vec<u32>> = vec![Vec::new(); 4];
    dictionary[0] = vec![0];
    dictionary[1] = vec![1];
    dictionary[2] = vec![2];
    let mut enlarge_in: u32 = 4;
    let mut dict_size: usize = 4;
    let mut num_bits: u32 = 3;

    let mut result: Vec<Vec<u32>> = Vec::new();
    let mut w: Vec<u32>;

    // First character: 2 code bits select the width.
    let bits = read_bits(2, reset_value, &mut val, &mut position, &mut index, &mut next_value)?;
    let c: Vec<u32> = match bits {
        0 => vec![read_bits(8, reset_value, &mut val, &mut position, &mut index, &mut next_value)?],
        1 => vec![read_bits(16, reset_value, &mut val, &mut position, &mut index, &mut next_value)?],
        _ => return Some(String::new()), // JS case 2: empty output
    };
    dictionary[3] = c.clone();
    w = c.clone();
    result.push(c);

    loop {
        if index > length {
            return None; // JS: data.index > length => ""
        }
        let bits = read_bits(num_bits, reset_value, &mut val, &mut position, &mut index, &mut next_value)?;
        let mut c = bits as usize;
        match c {
            0 => {
                let b = read_bits(8, reset_value, &mut val, &mut position, &mut index, &mut next_value)?;
                // JS: dictionary[dictSize++] = f(bits); c = dictSize-1;
                if dict_size < dictionary.len() {
                    dictionary[dict_size] = vec![b];
                } else {
                    dictionary.push(vec![b]);
                }
                dict_size += 1;
                c = dict_size - 1;
                enlarge_in -= 1;
            }
            1 => {
                let b = read_bits(16, reset_value, &mut val, &mut position, &mut index, &mut next_value)?;
                if dict_size < dictionary.len() {
                    dictionary[dict_size] = vec![b];
                } else {
                    dictionary.push(vec![b]);
                }
                dict_size += 1;
                c = dict_size - 1;
                enlarge_in -= 1;
            }
            2 => return Some(units_to_string(&result)),
            _ => {}
        }
        if enlarge_in == 0 {
            enlarge_in = 1 << num_bits;
            num_bits += 1;
        }
        // Entry resolution (JS: `dictionary[c]` truthiness vs dictSize check).
        let entry: Vec<u32> = if c < dictionary.len() && !dictionary[c].is_empty() {
            dictionary[c].clone()
        } else if c == dict_size {
            let mut e = w.clone();
            e.push(w[0]); // w + w.charAt(0)
            e
        } else {
            return None;
        };
        result.push(entry.clone());
        // JS: dictionary[dictSize++] = w + entry.charAt(0);
        let mut new_entry = w.clone();
        new_entry.push(entry[0]);
        if dict_size < dictionary.len() {
            dictionary[dict_size] = new_entry;
        } else {
            dictionary.push(new_entry);
        }
        dict_size += 1;
        enlarge_in -= 1;
        w = entry;
        if enlarge_in == 0 {
            enlarge_in = 1 << num_bits;
            num_bits += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors generated with the verbatim reference implementation
    // (tools/lz-string.reference.js, lz-string 1.4.5) and verified to
    // round-trip through its own decompressor before being stored here.
    #[test]
    fn decompress_simple_json() {
        let code = "N4IgLgpgzmIFwgBYQDYoPYAIDu6BOKAJiADQgB28ALAEwC+QA";
        let out = decompress_from_encoded_uri_component(code).expect("decompress");
        assert_eq!(out, "{\"test\":\"hello world\",\"n\":42}");
    }

    #[test]
    fn decompress_utf8() {
        let code = "OYJwPw+wBA4g9gFwVQKARUKemgac0HlRUCmAtnAFYCWUgPBuCR+1APIDSA-EA";
        let out = decompress_from_encoded_uri_component(code).expect("decompress");
        assert_eq!(out, "grüß Gott — 日本語 emoji 🎉 OK?");
    }

    #[test]
    fn decompress_repetition() {
        let code = "IY18ZXT1A";
        let out = decompress_from_encoded_uri_component(code).expect("decompress");
        assert_eq!(out, "a".repeat(64));
    }

    #[test]
    fn decompress_planner_payload() {
        let code = "N4IgbiBcBMA0IGMogLYEMBOBrAzugdiPADZQBsADPGlKDgC4YCm+A5vQBZQCMFAvvBy0QrAK758TDAH0AJhgD2kqNH7x6UANpVoAXXgotI8ZJnylTEPpBYADkMhUQOS5Ge8QfIA";
        let out = decompress_from_encoded_uri_component(code).expect("decompress");
        let raw: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(raw["v"], 2);
        assert_eq!(raw["c"], "marksman");
        assert_eq!(raw["s"]["gunner_drone"], 20);
    }

    #[test]
    fn empty_input_is_none() {
        assert!(decompress_from_encoded_uri_component("").is_none());
    }
}

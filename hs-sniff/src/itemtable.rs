//! Optional item-name table, loaded from a TSV file.
//!
//! hs-tracker bundles 1,491 datamined names in a generated `items.rs`. This
//! crate does not carry that table; instead the CLI accepts
//! `--table ITEMS.tsv` (columns: `type`, `id`, `weaponType`, `name`,
//! `rarity` — header optional) implementing the same identity resolution.

use std::collections::HashMap;
use hs_sniff::events::{ItemTable, Known};

/// `(type, id, weaponType) -> (name, rarity)`
pub struct HsCodexItems {
    by_identity: HashMap<(i64, i64, i64), Known>,
    by_name: HashMap<String, String>,
    muddled: std::collections::HashSet<String>,
}

impl HsCodexItems {
    pub fn new(rows: Vec<(i64, i64, i64, String, String)>) -> Self {
        let mut by_identity = HashMap::new();
        let mut by_name = HashMap::new();
        let mut seen: HashMap<String, Vec<String>> = HashMap::new();
        for (t, id, wt, name, rarity) in rows {
            let key = name.to_lowercase();
            seen.entry(key.clone()).or_default().push(rarity.clone());
            by_identity.insert((t, id, wt), Known { name: name.clone(), rarity: rarity.clone() });
            // A name that answers to two different items is dropped from the
            // by-name map entirely — the game has items that share a name.
            let entry = by_name.entry(key).or_insert_with(|| rarity.clone());
            if *entry != rarity {
                *entry = "Unknown".into();
            }
        }
        let muddled =
            seen.iter().filter(|(_, v)| v.iter().collect::<std::collections::HashSet<_>>().len() > 1)
                .map(|(k, _)| k.clone())
                .collect();
        Self { by_identity, by_name, muddled }
    }
}

impl ItemTable for HsCodexItems {
    fn by_identity(&self, t: i64, id: i64, wt: i64) -> Option<Known> {
        self.by_identity.get(&(t, id, wt)).cloned()
    }

    fn name_by_id(&self, t: i64, id: i64, wt: i64) -> Option<String> {
        self.by_identity.get(&(t, id, wt)).map(|k| k.name.clone())
    }

    fn rarity_by_name(&self, name: &str) -> Option<String> {
        self.by_name.get(&name.to_lowercase()).cloned()
    }

    fn muddled(&self, name: &str) -> bool {
        self.muddled.contains(&name.to_lowercase())
    }
}

/// Parse a TSV: `type  id  weaponType  name  rarity`, header optional.
pub fn parse_tsv(text: &str) -> Vec<(i64, i64, i64, String, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        if cols[0] == "type" {
            continue; // header
        }
        let (Ok(t), Ok(id), Ok(wt)) =
            (cols[0].trim().parse(), cols[1].trim().parse(), cols[2].trim().parse())
        else {
            continue;
        };
        rows.push((t, id, wt, cols[3].trim().to_string(), cols[4].trim().to_string()));
    }
    rows
}

/// Load and install a table from a file path.
pub fn load_from_file(path: &str) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let rows = parse_tsv(&text);
    if rows.is_empty() {
        return Err("no usable rows (need type/id/weaponType/name/rarity columns)".into());
    }
    let n = rows.len();
    hs_sniff_install(rows);
    Ok(n)
}

fn hs_sniff_install(rows: Vec<(i64, i64, i64, String, String)>) {
    // Installed by the CLI through `set_item_table`; kept here so the table
    // plumbing lives in one place.
    use std::sync::Arc as A;
    let table: A<dyn ItemTable> = A::new(HsCodexItems::new(rows));
    hs_sniff::events::set_item_table(Some(table));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tsv() {
        let rows = parse_tsv("type\tid\tweaponType\tname\trarity\n0\t0\t0\tHarlequinn's Crest\tSatanic");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].3, "Harlequinn's Crest");
    }

    #[test]
    fn muddled_names_are_flagged() {
        let table = HsCodexItems::new(vec![
            (3, 1, 1, "Angel".into(), "Set".into()),
            (5, 2, 0, "Angel".into(), "Heroic".into()),
        ]);
        assert!(table.muddled("angel"));
        assert_eq!(table.rarity_by_name("angel"), Some("Unknown".into()));
        assert_eq!(
            table.name_by_id(3, 1, 1),
            Some("Angel".into())
        );
    }
}

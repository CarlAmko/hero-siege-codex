//! Sweep analysis: turn one item's listings into an ask ladder and report the
//! structural mispricings in it.
//!
//! Pure — no fs, no network, no clock. `main.rs` does the fetching and reading.
//!
//! # The market is an order book, not a price
//!
//! Every listing is an ask of `(unit price, units)`, and one page is the *whole*
//! book for a fungible item (a tier-2 rune returns 62 listings; fetching page 2
//! returns nothing). So there is no sampling problem here — we hold every ask —
//! but there are three traps this module exists to avoid:
//!
//! 1. **`price` is a stack total.** Half the live rune rows have
//!    `price != unit_price`; a 13-unit stack of Mal at 24,500/unit reports
//!    `price = 318,500`. Banding `price` (which the game's own `avgPrice` does)
//!    averages stacks of different sizes and invents spreads that aren't there.
//!    Everything here is computed on [`MarketListing::unit_price_gold`].
//! 2. **A listing is not an observation.** Supply is `units`, so all statistics
//!    are unit-weighted — a 13-unit stack is 13 supply, not one vote.
//! 3. **The top of the book is where the outliers live.** The centre is a
//!    depth-weighted median; the mean and the max are never used.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::market::MarketListing;

/// One ask level: a unit price and the supply sitting at it.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceLevel {
    pub unit_price: i64,
    pub listings: usize,
    pub units: u64,
}

/// The full ask ladder for one item, cheapest unit price first.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemBook {
    pub name: String,
    pub levels: Vec<PriceLevel>,
}

impl ItemBook {
    /// Build the ladder from one item's listings. `None` when nothing is on
    /// sale.
    pub fn from_listings(name: impl Into<String>, listings: &[MarketListing]) -> Option<Self> {
        if listings.is_empty() {
            return None;
        }
        let mut levels: BTreeMap<i64, (usize, u64)> = BTreeMap::new();
        for row in listings {
            let entry = levels.entry(row.unit_price_gold()).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += row.units.max(1);
        }
        Some(ItemBook {
            name: name.into(),
            levels: levels
                .into_iter()
                .map(|(unit_price, (listings, units))| PriceLevel { unit_price, listings, units })
                .collect(),
        })
    }

    /// Distinct ask levels. "Mal is one wall at 9,000" is a different market
    /// from "Mal is 40 asks spread 6,700..25,000".
    pub fn listing_levels(&self) -> usize {
        self.levels.len()
    }

    /// Total listings behind the ladder.
    pub fn listings(&self) -> usize {
        self.levels.iter().map(|level| level.listings).sum()
    }

    /// Total supply on sale.
    pub fn total_units(&self) -> u64 {
        self.levels.iter().map(|level| level.units).sum()
    }

    /// The best ask — what you pay right now.
    pub fn best_ask(&self) -> Option<&PriceLevel> {
        self.levels.first()
    }

    /// The next distinct level up. The reference for "what would I undercut".
    pub fn next_ask(&self) -> Option<&PriceLevel> {
        self.levels.get(1)
    }

    /// How far the best ask sits below the next level, as a fraction
    /// (`0.088` = the next ask is 8.8% higher). `None` for a one-level book.
    pub fn cliff(&self) -> Option<f64> {
        let best = self.best_ask()?;
        let next = self.next_ask()?;
        if best.unit_price <= 0 {
            return None;
        }
        Some(next.unit_price as f64 / best.unit_price as f64 - 1.0)
    }

    /// Unit-weighted quantile of the unit price (`q` in 0.0..=1.0).
    pub fn quantile(&self, q: f64) -> Option<i64> {
        let total = self.total_units();
        if total == 0 {
            return None;
        }
        let target = ((total as f64) * q.clamp(0.0, 1.0)).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for level in &self.levels {
            seen += level.units;
            if seen >= target {
                return Some(level.unit_price);
            }
        }
        self.levels.last().map(|level| level.unit_price)
    }

    /// Median unit price across **distinct levels** — the fair-value reference.
    ///
    /// Each ask level is one seller's opinion, so this is robust to both
    /// failure modes: a single enormous stack cannot drag it (unit-weighting
    /// would), and one seller spamming many single-unit listings at one price
    /// counts once.
    pub fn price_centre(&self) -> Option<i64> {
        if self.levels.is_empty() {
            return None;
        }
        Some(self.levels[self.levels.len() / 2].unit_price)
    }

    /// Units available at or below `unit_price` — the position a buy at that
    /// price could actually take. A 90%-underpriced listing of a single unit is
    /// not a 90% opportunity.
    pub fn depth_at_or_below(&self, unit_price: i64) -> u64 {
        self.levels
            .iter()
            .filter(|level| level.unit_price <= unit_price)
            .map(|level| level.units)
            .sum()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "item": self.name,
            "levels": self.listing_levels(),
            "listings": self.listings(),
            "units": self.total_units(),
            "ask": self.best_ask().map(|level| level.unit_price),
            "ask_units": self.best_ask().map(|level| level.units),
            "next": self.next_ask().map(|level| level.unit_price),
            "centre": self.price_centre(),
            "unit_median": self.quantile(0.5),
            "p75": self.quantile(0.75),
            "cliff": self.cliff(),
        })
    }
}

/// A runeword's deterministic build cost: the runes it consumes. The stats a
/// runeword rolls vary, but the runes do not.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub ingredients: Vec<String>,
}

/// One actionable finding.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// The best ask sits well below the next level: buy the cheap one, then
    /// sell into the level above it. `units` is the depth at the ask, i.e. how
    /// much of the gap is actually takeable.
    Cliff { name: String, ask: i64, next: i64, gap: f64, units: u64 },
    /// The best ask sits well below the book's own depth-weighted centre.
    BelowCentre { name: String, ask: i64, centre: i64, ratio: f64 },
    /// A runeword sells for more than the runes it consumes: buy the runes,
    /// craft, sell the runeword. **This is the actionable direction** — runes
    /// are consumed by crafting and cannot be extracted back out, so the
    /// reverse is only information.
    ///
    /// The margin alone is a lie without depth. `crafts` is the binding limit
    /// (the shallowest ingredient's ask depth, or the runeword book's own
    /// supply) and `total` is `gap * crafts` — a 34,000% margin on one unit
    /// that you cannot sell is not a business.
    CraftMargin {
        name: String,
        item_ask: i64,
        item_listings: usize,
        rune_cost: i64,
        rune_cost_centre: i64,
        gap: i64,
        margin: f64,
        crafts: u64,
        total: i64,
    },
    /// A runeword is listed for less than its runes: the market values the
    /// inputs above the output. Not arbitrage — it means "do not craft this".
    CheaperThanRunes { name: String, item_ask: i64, rune_cost: i64, gap: i64 },
}

impl Signal {
    pub fn to_json(&self) -> Value {
        match self {
            Signal::Cliff { name, ask, next, gap, units } => json!({
                "kind": "cliff", "item": name, "ask": ask, "next": next,
                "cliff": gap, "units": units,
            }),
            Signal::BelowCentre { name, ask, centre, ratio } => json!({
                "kind": "below_centre", "item": name, "ask": ask,
                "centre": centre, "ratio": ratio,
            }),
            Signal::CraftMargin {
                name,
                item_ask,
                item_listings,
                rune_cost,
                rune_cost_centre,
                gap,
                margin,
                crafts,
                total,
            } => json!({
                "kind": "craft_margin", "item": name, "item_ask": item_ask,
                "item_listings": item_listings, "rune_cost": rune_cost,
                "rune_cost_centre": rune_cost_centre, "gap": gap, "margin": margin,
                "crafts": crafts, "total": total,
            }),
            Signal::CheaperThanRunes { name, item_ask, rune_cost, gap } => json!({
                "kind": "cheaper_than_runes", "item": name, "item_ask": item_ask,
                "rune_cost": rune_cost, "gap": gap,
            }),
        }
    }

    /// Ordering key within a signal family; bigger is stronger.
    fn strength(&self) -> f64 {
        match self {
            Signal::CraftMargin { total, .. } => *total as f64,
            Signal::CheaperThanRunes { .. } => -1.0,
            Signal::Cliff { gap, .. } => *gap,
            Signal::BelowCentre { ratio, .. } => {
                if *ratio > 0.0 {
                    1.0 / ratio - 1.0
                } else {
                    0.0
                }
            }
        }
    }
}

/// Firing thresholds. Deliberately conservative: a signal that fires on every
/// rune is noise, and a liquid rune ought not to fire at all.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// Fire `Cliff` when the next level is at least this much above the ask.
    pub cliff: f64,
    /// Fire `BelowCentre` when the ask is at most this fraction of the centre.
    pub centre_ratio: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { cliff: 0.25, centre_ratio: 0.6 }
    }
}

/// Structural signals (arithmetic) first, then price signals, each strongest
/// first.
pub fn analyze(books: &[ItemBook], recipes: &[Recipe], thresholds: Thresholds) -> Vec<Signal> {
    let by_name: BTreeMap<&str, &ItemBook> =
        books.iter().map(|book| (book.name.as_str(), book)).collect();

    let mut structural = Vec::new();
    let mut price = Vec::new();
    let mut informational = Vec::new();

    // A runeword's floor is its ingredients. Runes are consumed by crafting and
    // cannot be extracted again, so only one direction is a trade: cheap runes
    // plus a craft, sold as the runeword.
    for recipe in recipes {
        let Some(book) = by_name.get(recipe.name.as_str()) else { continue };
        let Some(item_ask) = book.best_ask() else { continue };
        let item_listings = book.listings();
        let item_units = book.total_units();
        // One seller's ask is not a market price, and it is the price you would
        // have to sell into.
        if item_listings < 2 {
            continue;
        }
        let mut cost_ask = 0i64;
        let mut cost_centre = 0i64;
        let mut depth = u64::MAX;
        let mut complete = true;
        for ingredient in &recipe.ingredients {
            let Some(ingredient_book) = by_name.get(ingredient.as_str()) else {
                complete = false;
                break;
            };
            let Some(level) = ingredient_book.best_ask() else {
                complete = false;
                break;
            };
            cost_ask += level.unit_price;
            cost_centre += ingredient_book.price_centre().unwrap_or(level.unit_price);
            depth = depth.min(level.units);
        }
        if !complete || cost_ask <= 0 {
            continue;
        }
        // You can only run as many crafts as both the cheap rune supply and the
        // runeword book can absorb.
        let crafts = depth.min(item_units);
        if item_ask.unit_price > cost_ask {
            let gap = item_ask.unit_price - cost_ask;
            structural.push(Signal::CraftMargin {
                name: recipe.name.clone(),
                item_ask: item_ask.unit_price,
                item_listings,
                rune_cost: cost_ask,
                rune_cost_centre: cost_centre,
                gap,
                margin: gap as f64 / cost_ask as f64,
                crafts,
                total: gap.saturating_mul(crafts as i64),
            });
        } else if item_ask.unit_price < cost_centre {
            // Compared against the robust cost: at the cheapest ask a rune can
            // always look cheap, which would make this fire on noise.
            informational.push(Signal::CheaperThanRunes {
                name: recipe.name.clone(),
                item_ask: item_ask.unit_price,
                rune_cost: cost_centre,
                gap: cost_centre - item_ask.unit_price,
            });
        }
    }

    for book in books {
        let Some(ask) = book.best_ask() else { continue };
        let mut flagged = false;
        if let (Some(next), Some(cliff)) = (book.next_ask(), book.cliff()) {
            if cliff >= thresholds.cliff {
                flagged = true;
                price.push(Signal::Cliff {
                    name: book.name.clone(),
                    ask: ask.unit_price,
                    next: next.unit_price,
                    gap: cliff,
                    units: ask.units,
                });
            }
        }
        // The centre comparison is the weaker, vaguer claim: once a book has a
        // real cliff, that is the specific reason and "below the centre" would
        // only restate it.
        if !flagged && book.listing_levels() > 1 {
            if let Some(centre) = book.price_centre() {
                let ratio = ask.unit_price as f64 / centre as f64;
                if ratio > 0.0 && ratio <= thresholds.centre_ratio {
                    price.push(Signal::BelowCentre {
                        name: book.name.clone(),
                        ask: ask.unit_price,
                        centre,
                        ratio,
                    });
                }
            }
        }
    }

    let by_strength = |a: &Signal, b: &Signal| {
        b.strength().partial_cmp(&a.strength()).unwrap_or(std::cmp::Ordering::Equal)
    };
    structural.sort_by(by_strength);
    price.sort_by(by_strength);
    informational.sort_by(by_strength);
    // Actionable first, then price structure, then the "do not craft" notes.
    structural.append(&mut price);
    structural.append(&mut informational);
    structural
}

/// Build recipes from `runewords.json` (name + rune ids) and `runes.json`
/// (id -> name). Both are plain arrays in this repo; an object with a single
/// array field is accepted too. Pure: the caller reads the files.
pub fn recipes_from_json(runewords_json: &str, runes_json: &str) -> Result<Vec<Recipe>, String> {
    let runes: Value =
        serde_json::from_str(runes_json).map_err(|e| format!("runes.json: {e}"))?;
    let runewords: Value =
        serde_json::from_str(runewords_json).map_err(|e| format!("runewords.json: {e}"))?;

    let mut id_to_name = BTreeMap::new();
    for row in rows_of(&runes) {
        if let (Some(id), Some(name)) = (
            row.get("id").and_then(Value::as_str),
            row.get("name").and_then(Value::as_str),
        ) {
            id_to_name.insert(id.to_string(), name.to_string());
        }
    }

    let mut recipes = Vec::new();
    for row in rows_of(&runewords) {
        let Some(name) = row.get("name").and_then(Value::as_str) else { continue };
        let ingredients: Vec<String> = row
            .get("runes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|id| id_to_name.get(id).cloned())
            .collect();
        if !ingredients.is_empty() {
            recipes.push(Recipe { name: name.to_string(), ingredients });
        }
    }
    Ok(recipes)
}

fn rows_of(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(map) => map
            .values()
            .find_map(|value| value.as_array())
            .map(|rows| rows.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(price: i64, unit_price: Option<f64>, units: u64) -> MarketListing {
        MarketListing { price, unit_price, units }
    }

    #[test]
    fn ladder_groups_levels_and_weights_by_units_not_listings() {
        // Three asks at 100 (1+1+2 units) and one at 200 (10 units). By listing
        // count the median is 100; by supply the centre is still 100, but the
        // p75 differs — the weighted view is what the market actually is.
        let book = ItemBook::from_listings(
            "Rune",
            &[
                listing(100, None, 1),
                listing(100, None, 1),
                listing(200, None, 2),
                listing(400, None, 10),
            ],
        )
        .unwrap();
        assert_eq!(book.listing_levels(), 3);
        assert_eq!(book.listings(), 4);
        assert_eq!(book.total_units(), 14);
        assert_eq!(book.best_ask().unwrap().unit_price, 100);
        assert_eq!(book.next_ask().unwrap().unit_price, 200);
        assert_eq!(book.next_ask().unwrap().units, 2);
        // 14 units: half is 7, which is reached inside the 10-unit 400 level.
        // The *price* centre is the level median (200); the depth-weighted
        // median is 400, because the supply really is up there.
        assert_eq!(book.price_centre(), Some(200));
        assert_eq!(book.quantile(0.5), Some(400));
        assert_eq!(book.quantile(0.25), Some(200));
        assert_eq!(book.depth_at_or_below(200), 4);
    }

    #[test]
    fn cliff_is_the_gap_to_the_next_level() {
        let book = ItemBook::from_listings("Rune", &[listing(100, None, 1), listing(150, None, 1)])
            .unwrap();
        assert!((book.cliff().unwrap() - 0.5).abs() < 1e-9);
        // A one-level book has nothing to undercut.
        let single = ItemBook::from_listings("Rune", &[listing(100, None, 1)]).unwrap();
        assert_eq!(single.cliff(), None);
        assert!(ItemBook::from_listings("Rune", &[]).is_none());
    }

    #[test]
    fn a_dense_ladder_does_not_fire_but_a_cliff_does() {
        let dense = ItemBook::from_listings(
            "Mal",
            &[listing(100, None, 1), listing(108, None, 1), listing(110, None, 1)],
        )
        .unwrap();
        // Mal-like: best ask only 8% under the next level.
        assert!((dense.cliff().unwrap() - 0.08).abs() < 0.011);

        let clifffy = ItemBook::from_listings(
            "Thin",
            &[listing(1000, None, 1), listing(4000, None, 1), listing(4100, None, 1)],
        )
        .unwrap();
        let signals = analyze(&[dense.clone(), clifffy], &[], Thresholds::default());
        assert_eq!(signals.len(), 1, "only the cliffy book should fire");
        match &signals[0] {
            Signal::Cliff { name, ask, next, units, .. } => {
                assert_eq!((name.as_str(), *ask, *next, *units), ("Thin", 1000, 4000, 1));
            }
            other => panic!("expected a cliff signal, got {other:?}"),
        }
    }

    #[test]
    fn stack_totals_do_not_inflate_the_book() {
        // The live Mal trap: a 13-unit stack at 24,500/unit reports price
        // 318,500. Banding `price` would make that the "highest", and sort it
        // above a single unit at 6,749.
        let rows = [
            listing(318_500, Some(24_500.0), 13),
            listing(7_333, Some(7_333.0), 1),
            listing(6_749, Some(6_749.0), 2),
        ];
        let book = ItemBook::from_listings("Mal", &rows).unwrap();
        assert_eq!(book.best_ask().unwrap().unit_price, 6_749);
        assert_eq!(book.next_ask().unwrap().unit_price, 7_333);
        assert_eq!(book.total_units(), 16);
        assert_eq!(book.price_centre(), Some(7_333));
        // The stack is 13 of the 16 units, so it owns the top, not the ask.
        assert_eq!(book.quantile(0.9), Some(24_500));
        // And note the trap in the other direction: unit-weighting alone would
        // put the *centre* at 24,500, inventing a discount for the ask. The
        // level median is why it does not.
    }

    #[test]
    fn cheap_runes_plus_a_craft_is_the_arbitrage() {
        // The live Malice shape: runes total 678, the runeword asks 2,500.
        let books = vec![
            ItemBook::from_listings("Malice", &[listing(2_500, None, 1), listing(2_600, None, 1)])
                .unwrap(),
            ItemBook::from_listings("Uth", &[listing(500, None, 1)]).unwrap(),
            ItemBook::from_listings("Ol", &[listing(89, None, 1)]).unwrap(),
            ItemBook::from_listings("Eth", &[listing(89, None, 1)]).unwrap(),
        ];
        let recipes = vec![Recipe {
            name: "Malice".into(),
            ingredients: vec!["Uth".into(), "Ol".into(), "Eth".into()],
        }];
        let signals = analyze(&books, &recipes, Thresholds::default());
        match &signals[0] {
            Signal::CraftMargin {
                name,
                item_ask,
                rune_cost,
                gap,
                margin,
                crafts,
                total,
                ..
            } => {
                assert_eq!((name.as_str(), *item_ask, *rune_cost, *gap), ("Malice", 2_500, 678, 1_822));
                assert!((margin - 1822.0 / 678.0).abs() < 1e-9);
                // One unit at each ingredient's ask is the binding limit here,
                // not the two runewords on sale.
                assert_eq!((*crafts, *total), (1, 1_822));
            }
            other => panic!("expected a craft margin, got {other:?}"),
        }
    }

    #[test]
    fn a_single_listing_runeword_is_not_a_market_price() {
        let books = vec![
            ItemBook::from_listings("Inner Fire", &[listing(555_555, None, 1)]).unwrap(),
            ItemBook::from_listings("Tor", &[listing(89, None, 1)]).unwrap(),
        ];
        let recipes = vec![Recipe { name: "Inner Fire".into(), ingredients: vec!["Tor".into()] }];
        assert!(analyze(&books, &recipes, Thresholds::default()).is_empty());
    }

    #[test]
    fn the_shallowest_ingredient_bounds_the_number_of_crafts() {
        let books = vec![
            ItemBook::from_listings("Malice", &[listing(2_500, None, 1), listing(2_600, None, 1)])
                .unwrap(),
            ItemBook::from_listings("Uth", &[listing(500, None, 40)]).unwrap(),
            ItemBook::from_listings("Ol", &[listing(89, None, 3)]).unwrap(),
        ];
        let recipes = vec![Recipe {
            name: "Malice".into(),
            ingredients: vec!["Uth".into(), "Ol".into()],
        }];
        let signals = analyze(&books, &recipes, Thresholds::default());
        match &signals[0] {
            // 3 from Ol, and only 2 runewords on sale to sell into.
            Signal::CraftMargin { crafts, total, .. } => assert_eq!((*crafts, *total), (2, 3_822)),
            other => panic!("expected a craft margin, got {other:?}"),
        }
    }

    #[test]
    fn a_runeword_under_its_runes_is_a_warning_not_a_trade() {
        // Runes are consumed by crafting and cannot be extracted, so an
        // expensive-rune/cheap-runeword book is not an opportunity.
        let books = vec![
            ItemBook::from_listings("Malice", &[listing(2_500, None, 1), listing(2_600, None, 1)])
                .unwrap(),
            ItemBook::from_listings("Uth", &[listing(4_000, None, 1)]).unwrap(),
            ItemBook::from_listings("Ol", &[listing(5_000, None, 1)]).unwrap(),
            ItemBook::from_listings("Eth", &[listing(3_000, None, 1)]).unwrap(),
        ];
        let recipes = vec![Recipe {
            name: "Malice".into(),
            ingredients: vec!["Uth".into(), "Ol".into(), "Eth".into()],
        }];
        let signals = analyze(&books, &recipes, Thresholds::default());
        match &signals[0] {
            Signal::CheaperThanRunes { name, item_ask, rune_cost, gap } => {
                assert_eq!((name.as_str(), *item_ask, *rune_cost, *gap), ("Malice", 2_500, 12_000, 9_500));
            }
            other => panic!("expected a cheaper-than-runes note, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_ingredient_suppresses_the_arbitrage_claim() {
        let books = vec![
            ItemBook::from_listings("Malice", &[listing(2_500, None, 1), listing(2_600, None, 1)])
                .unwrap(),
            ItemBook::from_listings("Uth", &[listing(4_000, None, 1)]).unwrap(),
        ];
        let recipes = vec![Recipe {
            name: "Malice".into(),
            ingredients: vec!["Uth".into(), "Ol".into()],
        }];
        assert!(analyze(&books, &recipes, Thresholds::default()).is_empty());
    }

    #[test]
    fn recipes_parse_the_repo_shape() {
        let runes = r#"[{"id":"rune_uth","name":"Uth","tier":1},
                        {"id":"rune_ol","name":"Ol","tier":1}]"#;
        let runewords = r#"[{"id":"rw_malice","name":"Malice","runes":["rune_uth","rune_ol"]},
                           {"id":"rw_empty","name":"Empty","runes":[]}]"#;
        let recipes = recipes_from_json(runewords, runes).unwrap();
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].name, "Malice");
        assert_eq!(recipes[0].ingredients, vec!["Uth", "Ol"]);
    }
}

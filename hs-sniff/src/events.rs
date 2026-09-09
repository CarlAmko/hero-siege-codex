//! Event extraction: decoded protocol messages -> [`GameEvent`]s.
//!
//! Ported from `refs/hs-tracker/src-tauri/src/parser.rs`
//! (`dict_to_events`, `item_sources`, `item_event` and friends). The datamined
//! item tables behind hs-tracker's `items.rs` are NOT bundled: resolution goes
//! through the optional [`ItemTable`] hook ([`set_item_table`]), and without
//! one, names come only from the packets themselves and rarity falls back to
//! what the packet claims.

use std::sync::RwLock;

use serde_json::{json, Value};

use crate::decode::{as_bool, as_int, b64_json, field, field_ref, has, int_field, msg_text};

// ---------------------------------------------------------------------------
// Item table hook (replaces hs-tracker's generated items.rs)
// ---------------------------------------------------------------------------

/// What an item table can answer about an identity.
#[derive(Debug, Clone)]
pub struct Known {
    pub name: String,
    pub rarity: String,
}

/// Hook for a datamined `(type, id, weaponType) -> (name, rarity)` table.
/// hs-sniff itself bundles none — see `hs-sniff/README.md`.
pub trait ItemTable: Send + Sync {
    fn by_identity(&self, item_type: i64, item_id: i64, weapon_type: i64) -> Option<Known>;
    /// Name for a `(type, id)` pair; `weapon_type` disambiguates weapons.
    fn name_by_id(&self, item_type: i64, item_id: i64, weapon_type: i64) -> Option<String>;
    /// The table's rarity for a display name, or `None` to fall back to the
    /// packet. Returning `Some("Unknown".into())` marks the name as ambiguous.
    fn rarity_by_name(&self, name: &str) -> Option<String>;
    /// True when the name is known but deliberately ambiguous.
    fn muddled(&self, name: &str) -> bool {
        self.rarity_by_name(name).map(|r| r == "Unknown").unwrap_or(false)
    }
}

static ITEM_TABLE: RwLock<Option<std::sync::Arc<dyn ItemTable>>> = RwLock::new(None);

/// Install the item table used for name/rarity resolution. Pass `None` to
/// uninstall. Existing sessions pick it up on the next event.
pub fn set_item_table(table: Option<std::sync::Arc<dyn ItemTable>>) {
    *ITEM_TABLE.write().unwrap() = table;
}

fn table() -> Option<std::sync::Arc<dyn ItemTable>> {
    ITEM_TABLE.read().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Constants shared with the stats layer
// ---------------------------------------------------------------------------

/// Packet rarity numbering: `(id, display name)`.
pub const RARITIES: &[(&str, &str)] = &[
    ("1", "Common"),
    ("2", "Superior"),
    ("3", "Rare"),
    ("4", "Set"),
    ("5", "Mythic"),
    ("6", "Satanic"),
    ("7", "Angelic"),
    ("8", "Blessed"),
    ("9", "Heroic"),
    ("10", "Unholy"),
];

/// The top grade an item can carry. Grades run 1..6 and are written D, C, B,
/// A, S, SS.
pub const SS_TIER: i64 = 6;

pub const RARE_RARITIES: &[&str] = &["Satanic", "Set", "Heroic", "Angelic", "Unholy"];

/// The item types whose id is their identity rather than a slot in a base
/// table: keys, collectibles, materials, socketables and vaults.
const SELF_NUMBERED: [i64; 5] = [12, 13, 14, 15, 19];
/// Relics. Type 16 holds the 156 of them and nothing else.
const RELIC: i64 = 16;
/// Consumables. Healing potions live here, and so do the two Codexes.
const CONSUMABLE: i64 = 11;
/// Eternity Codex (id 18) and Infernal Codex (id 23).
const ENTRY_CODEX_IDS: [i64; 2] = [18, 23];

fn is_entry_codex(item_type: i64, item_id: i64, name: &str) -> bool {
    if item_type == CONSUMABLE && ENTRY_CODEX_IDS.contains(&item_id) {
        return true;
    }
    let n = name.trim().to_lowercase();
    n == "eternity codex" || n == "infernal codex"
}

/// What only a market message carries. See `item_sources`.
const MARKET_FIELDS: &[&str] = &[
    "marketId",
    "market_id",
    "market_tokens",
    "marketTokens",
    "seller_name",
    "sellerName",
    "price",
];
const ITEM_DATA_FIELDS: &[&str] = &["itemData", "item_data"];
const PICKUP_FIELDS: &[&str] = &["pickup_add_data", "pickupAddData"];
/// The session credentials the client attaches to everything it asks for.
/// Their presence means the packet is ours going out, not the server's coming
/// back.
const CLIENT_ENVELOPE_FIELDS: &[&str] = &["identifier", "checksum"];
/// Where an item says whose slot it is in, or which spot on the floor it lies
/// on.
const OWNER_FIELDS: &[&str] = &["gd", "gid"];

const GOLD_FIELDS: &[&str] = &["currencyData", "currency_data"];
const XP_TOTAL_FIELDS: &[&str] =
    &["totalGuildXp", "total_guild_xp", "totalGuildExp", "total_guild_exp"];
const XP_GAIN_FIELDS: &[&str] = &["xp", "experienceGained", "experience_gained"];
const MAIL_FIELDS: &[&str] = &["newMail", "new_mail", "mail"];
const ITEM_WRAPPER_FIELDS: &[&str] = &["addedItemObject", "added_item_object"];
const ITEM_SIGNATURE_FIELDS: &[&str] = &["seed", "a", "itemId", "item_id", "gid"];
const ITEM_NAMED_SIGNATURE_FIELDS: &[&str] = &["seed", "itemId", "item_id", "gid"];
const ITEM_RARITY_FIELDS: &[&str] = &["rarity", "itemRarity", "item_rarity", "d"];
const SATANIC_ZONE_FIELDS: &[&str] = &["satanicZoneName", "satanic_zone_name"];
const REGION_ID_FIELDS: &[&str] =
    &["crossregion_identifier", "crossRegionIdentifier", "cross_region_identifier"];
const ACCOUNT_ID_FIELDS: &[&str] = &["unique_account_id", "uniqueAccountId"];
const ACCOUNT_SIGNATURE_FIELDS: &[&str] =
    &["name", "class", "class_id", "heroLevel", "herolevel", "season", "hardcore"];

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// The five purses, as the packets name them, plus a direct delta when the
/// packet reports one instead of a total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Currency {
    pub gss: i64,
    pub gsh: i64,
    pub gns: i64,
    pub gnh: i64,
    pub gbp: i64,
    pub delta: i64,
}

impl Currency {
    /// The one purse with anything in it, when exactly one has.
    pub fn only_purse(&self) -> Option<&'static str> {
        let mut found = None;
        for (name, value) in
            [("GSS", self.gss), ("GSH", self.gsh), ("GNS", self.gns), ("GNH", self.gnh), ("GBP", self.gbp)]
        {
            if value > 0 {
                if found.is_some() {
                    return None;
                }
                found = Some(name);
            }
        }
        found
    }

    pub fn for_mode(&self, mode: &str) -> i64 {
        match mode {
            "GSS" => self.gss,
            "GSH" => self.gsh,
            "GNS" => self.gns,
            "GNH" => self.gnh,
            "GBP" => self.gbp,
            _ => 0,
        }
    }
}

/// One normalized thing the game said.
#[derive(Debug, Clone, PartialEq)]
pub enum GameEvent {
    Gold(Currency),
    XpGain(i64),
    /// Inventory fingerprints that just left the player's bags. An item
    /// dropped and picked back up arrives as an ordinary addition — same
    /// shape, same named flag, same identity as a find. The fingerprint
    /// survives the round trip unchanged, so an addition of one the player has
    /// just let go of is a return.
    ItemsLetGo(Vec<String>),
    /// The account this client is logged in as. A fingerprint's second field
    /// is the account it was minted for, so this number is the whole of
    /// telling our items from other players'.
    WhoseAccount(String),
    Account {
        experience: i64,
        has_experience: bool,
        season: i64,
        hardcore: i64,
        blood_pact: i64,
        name: String,
        level: i64,
        herolevel: i64,
        difficulty: i64,
        /// Which grade of Hell, 1..5.
        hell_sub: i64,
        /// Which act the character is in, or 0 — see `act_of`.
        act: i64,
        kills: i64,
        /// every `statistic…` counter the save carries, by flattened name
        tallies: std::collections::HashMap<String, i64>,
    },
    Mail(bool),
    /// The room the character stands in, straight from the client's heartbeat.
    Room(String),
    /// What the heartbeat says about the character: magic find, the two
    /// levels, and whether the room it is in is the satanic zone.
    Vitals {
        mf: Option<i64>,
        level: i64,
        hlevel: i64,
        satanic_here: Option<bool>,
    },
    /// The client asking the server where the satanic zone is. It carries the
    /// identifier of the region it is asking on behalf of.
    ZoneRegion(String),
    /// A find the server put in chat: "Ragnar just found [Azazel's Despair]".
    /// The line goes to everybody on the shard, so who found it matters.
    Found {
        finder: String,
        name: String,
    },
    ItemAdded {
        rarity: Value,
        /// An Odyssey item the game did not flag as named.
        unscaled: bool,
        mf: bool,
        tier: i64,
        item_type: i64,
        item_id: i64,
        weapon_type: i64,
        seed: i64,
        name: String,
        announced: bool,
        amount: i64,
        fingerprint: String,
        /// The item's own hash: the same value at the drop and at the pickup,
        /// which is what ties the two sightings together.
        hash: String,
        /// Generated on the ground (the moment it drops), not picked up.
        ground: bool,
    },
    SatanicZone {
        zone: String,
        buffs: Vec<u8>,
        debuffs: Vec<u8>,
    },
}

impl GameEvent {
    /// Short machine-friendly kind tag, used by the CLI and MCP output.
    pub fn kind(&self) -> &'static str {
        match self {
            GameEvent::Gold(_) => "gold",
            GameEvent::XpGain(_) => "xp",
            GameEvent::ItemsLetGo(_) => "items_let_go",
            GameEvent::WhoseAccount(_) => "whose_account",
            GameEvent::Account { .. } => "account",
            GameEvent::Mail(_) => "mail",
            GameEvent::Room(_) => "room",
            GameEvent::Vitals { .. } => "vitals",
            GameEvent::ZoneRegion(_) => "zone_region",
            GameEvent::Found { .. } => "found",
            GameEvent::ItemAdded { .. } => "item_added",
            GameEvent::SatanicZone { .. } => "satanic_zone",
        }
    }

    /// JSON form for the CLI/MCP event stream.
    pub fn to_json(&self) -> Value {
        match self {
            GameEvent::Gold(c) => json!({
                "kind": "gold", "gss": c.gss, "gsh": c.gsh, "gns": c.gns,
                "gnh": c.gnh, "gbp": c.gbp, "delta": c.delta,
            }),
            GameEvent::XpGain(n) => json!({"kind": "xp", "amount": n}),
            GameEvent::ItemsLetGo(fps) => json!({"kind": "items_let_go", "fingerprints": fps}),
            GameEvent::WhoseAccount(a) => json!({"kind": "whose_account", "account": a}),
            GameEvent::Account { .. } => {
                let GameEvent::Account {
                    experience,
                    has_experience,
                    season,
                    hardcore,
                    blood_pact,
                    name,
                    level,
                    herolevel,
                    difficulty,
                    hell_sub,
                    act,
                    kills,
                    tallies,
                } = self
                else {
                    unreachable!()
                };
                json!({
                    "kind": "account", "name": name, "experience": experience,
                    "has_experience": has_experience, "season": season,
                    "hardcore": hardcore, "blood_pact": blood_pact,
                    "level": level, "herolevel": herolevel,
                    "difficulty": difficulty, "hell_sub": hell_sub,
                    "act": act, "kills": kills,
                    "tallies": tallies,
                })
            }
            GameEvent::Mail(present) => json!({"kind": "mail", "present": present}),
            GameEvent::Room(r) => json!({"kind": "room", "room": r}),
            GameEvent::Vitals { mf, level, hlevel, satanic_here } => json!({
                "kind": "vitals", "mf": mf, "level": level,
                "hlevel": hlevel, "satanic_here": satanic_here,
            }),
            GameEvent::ZoneRegion(r) => json!({"kind": "zone_region", "region": r}),
            GameEvent::Found { finder, name } => {
                json!({"kind": "found", "finder": finder, "name": name})
            }
            GameEvent::ItemAdded { .. } => {
                let GameEvent::ItemAdded {
                    rarity,
                    unscaled,
                    mf,
                    tier,
                    item_type,
                    item_id,
                    weapon_type,
                    seed,
                    name,
                    announced,
                    amount,
                    fingerprint,
                    hash,
                    ground,
                } = self
                else {
                    unreachable!()
                };
                json!({
                    "kind": "item_added", "name": name, "rarity": rarity,
                    "tier": tier, "item_type": item_type, "item_id": item_id,
                    "weapon_type": weapon_type, "seed": seed, "mf": mf,
                    "announced": announced, "amount": amount,
                    "fingerprint": fingerprint, "hash": hash, "ground": ground,
                    "unscaled": unscaled,
                })
            }
            GameEvent::SatanicZone { zone, buffs, debuffs } => json!({
                "kind": "satanic_zone", "zone": zone,
                "buffs": buffs, "debuffs": debuffs,
            }),
        }
    }
}

/// Aggregation-free running totals, for the CLI and the MCP summary tool.
/// The stats engine in hs-tracker goes much further; this stays deliberately
/// lean for the event-pipeline scope.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub packets_seen: u64,
    pub messages_decoded: u64,
    pub gold_delta: i64,
    pub xp_gain: i64,
    pub items_seen: u64,
    pub items_by_rarity: std::collections::BTreeMap<String, u64>,
    pub satanic_zone: Option<String>,
    pub room: Option<String>,
    pub account: Option<String>,
    pub character: Option<String>,
    pub level: i64,
    pub kills: i64,
}

impl Summary {
    pub fn observe(&mut self, event: &GameEvent) {
        match event {
            GameEvent::Gold(c) => self.gold_delta += c.delta,
            GameEvent::XpGain(n) => self.xp_gain += *n,
            GameEvent::ItemAdded { rarity, .. } => {
                self.items_seen += 1;
                let key = rarity_from_packet(rarity).unwrap_or_else(|| "Unknown".into());
                *self.items_by_rarity.entry(key).or_insert(0) += 1;
            }
            GameEvent::SatanicZone { zone, .. } => self.satanic_zone = Some(zone.clone()),
            GameEvent::Room(r) => self.room = Some(r.clone()),
            GameEvent::WhoseAccount(a) => self.account = Some(a.clone()),
            GameEvent::Account { name, level, kills, .. } => {
                self.character = Some(name.clone());
                self.level = *level;
                self.kills = *kills;
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Whether this packet is somebody talking.
///
/// A chat line carries who said it and where: a room, a colour for the name.
/// The server's own answers carry none of that — mail is
/// `{"message": "New mail!", "status": 1}` and nothing else.
fn is_chat(d: &Value) -> bool {
    has(d, &["chatRoom", "chat_room", "room"]) && has(d, &["nameColor", "name_color", "msgColor", "msg_color"])
}

/// Whether a line is about mail, rather than merely containing the letters.
fn says_mail(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.match_indices("mail").any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + 4..].chars().next();
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        // "mailbox" and "mails" are still about mail; "platemail" is not
        edge(before) && (edge(after) || lower[at..].starts_with("mailbox") || after == Some('s'))
    })
}

/// "No new mail", "You have no new mail." and "Mailbox empty" all mean empty.
fn mail_is_present(d: &Value) -> bool {
    let raw = field(d, MAIL_FIELDS);
    match raw {
        Some(Value::Bool(b)) => return b,
        Some(Value::Number(n)) => return n.as_i64().unwrap_or(0) > 0,
        _ => {}
    }
    let text = match raw {
        Some(Value::String(s)) => s,
        _ => msg_text(d),
    };
    let t = text.trim().to_lowercase();
    if t.is_empty() || ["0", "false", "none", "no", "clear"].contains(&t.as_str()) {
        return false;
    }
    if t.contains("no new mail") || t.contains("no mail") || t.contains("mailbox empty") {
        return false;
    }
    says_mail(&t) || t == "1" || t == "true" || t == "yes"
}

/// Case-insensitive search that stays in the ORIGINAL string: lowercasing can
/// change byte lengths and offsets taken from a lowered copy can slice
/// mid-character.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len())
        .find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n) && haystack.is_char_boundary(i))
}

/// The finder and what they found, out of "Ragnar just found [Azazel's Despair]".
fn announced_item_name(message: &str) -> Option<(String, String)> {
    const MARKER: &str = "just found [";
    let at = find_ascii_ci(message, MARKER)?;
    let start = at + MARKER.len();
    let end = message[start..].find(']')? + start;
    let name = message[start..end].trim();
    // whatever the line opens with, up to the marker; the game puts a colour
    // tag or a channel prefix in front of the name often enough
    let finder =
        message[..at].trim().rsplit(&[':', '>', ']'][..]).next().unwrap_or("").trim();
    (!name.is_empty()).then(|| (finder.to_string(), name.to_string()))
}

fn effect_ids(raw: Option<Value>) -> Vec<u8> {
    // the protocol writes whole numbers as floats ("2.0"), so read numbers
    // through the same coercion every other numeric field uses
    match raw {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| match b {
                Value::Number(_) => as_int(b),
                Value::String(s) => s.trim().parse().ok(),
                _ => None,
            })
            .filter_map(|n| u8::try_from(n).ok())
            .collect(),
        Some(Value::String(s)) => s
            .replace(',', "|")
            .split('|')
            .filter_map(|b| b.trim().parse().ok())
            .collect(),
        _ => vec![],
    }
}

/// The client's zone question, told apart from every other packet that carries
/// the same identifier by being nothing else: two fields, no message, no
/// payload.
fn zone_request_region(d: &Value) -> Option<String> {
    let o = d.as_object()?;
    if o.keys().filter(|k| k.as_str() != "_src").count() != 2 {
        return None;
    }
    if !has(d, ACCOUNT_ID_FIELDS) {
        return None;
    }
    match field(d, REGION_ID_FIELDS)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Who we are, out of the client's own requests. Only the client sends this
/// field: it is how a request is told from an answer elsewhere.
fn our_account(d: &Value) -> Option<String> {
    match field(d, ACCOUNT_ID_FIELDS)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Which act the character is in, out of the save. `act_previous[1]` tracks
/// the act without an exception, and a save arrives often.
fn act_of(d: &Value) -> i64 {
    let from = |v: Option<&Value>| match v {
        Some(Value::Array(xs)) => xs.get(1).and_then(as_int).unwrap_or(0),
        _ => 0,
    };
    let direct = from(field_ref(d, &["act_previous", "actPrevious"]));
    if direct > 0 {
        return direct;
    }
    // the same save arrives wrapped as well as bare
    match field_ref(d, &["slot_data", "slotData"]) {
        Some(slot) => from(field_ref(slot, &["act_previous", "actPrevious"])),
        None => 0,
    }
}

/// The counters the character save keeps beside experience and kills: bosses
/// killed, chests opened, floors cleared, deaths. Flattened to letters and
/// digits, so `statisticUberDamienKills` and `statistic_uber_damien_kills`
/// are one key.
fn tallies(obj: &Value) -> std::collections::HashMap<String, i64> {
    let mut out = std::collections::HashMap::new();
    let Some(map) = obj.as_object() else { return out };
    for (key, value) in map {
        let flat: String =
            key.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
        if flat.len() > "statistic".len() && flat.starts_with("statistic") {
            if let Some(n) = as_int(value) {
                out.insert(flat, n);
            }
        }
    }
    out
}

fn has_currency_totals(d: &Value) -> bool {
    ["GSS", "GSH", "GNS", "GNH", "GBP"].iter().any(|f| has(d, &[f]))
}

/// Guild XP is fifteen percent of the character XP that earned it, so the
/// share scales back up to what the character got. A quest reward is already
/// character XP; the save cannot take an over-credit back, so the
/// discriminator matters.
fn guild_share_to_character(guild: i64) -> i64 {
    let scaled = guild as f64 / 0.15;
    if !scaled.is_finite() || scaled >= i64::MAX as f64 {
        return 0;
    }
    scaled as i64
}

/// A quest paying out is not a guild share; `questId` is what says so.
fn is_quest_reward(d: &Value) -> bool {
    has(d, &["questId", "quest_id", "questID"])
}

/// XP gain is the first number in the message text, else the xp field.
fn xp_gain(d: &Value) -> i64 {
    let msg = msg_text(d);
    let digits: String =
        msg.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    if let Ok(n) = digits.parse() {
        return n;
    }
    int_field(d, XP_GAIN_FIELDS)
}

/// The inventory fingerprint ends with the item TYPE ("8-4653008-...-1" -> 1).
pub fn fingerprint_type(fingerprint: &str) -> Option<i64> {
    fingerprint.rsplit('-').next()?.parse().ok()
}

/// The account a fingerprint was made for — its second field.
pub fn fingerprint_account(fingerprint: &str) -> Option<&str> {
    let mut parts = fingerprint.split('-');
    let _kind = parts.next()?;
    let account = parts.next()?;
    // there has to be more after it, or this is not a fingerprint at all
    parts.next()?;
    (!account.is_empty() && account.bytes().all(|b| b.is_ascii_digit())).then_some(account)
}

/// The table's answer for a packet identity, or `None`.
pub fn known_item(name: &str, unscaled: bool, id: (i64, i64, i64)) -> Option<Known> {
    let t = table()?;
    if unscaled || name.is_empty() {
        return None;
    }
    t.by_identity(id.0, id.1, id.2)
        .filter(|k| name.eq_ignore_ascii_case(&k.name))
}

/// Rarity for an item, identity first, then the table by name, then the
/// packet's own claim.
pub fn resolve_rarity(packet: &Value, name: &str, unscaled: bool, id: (i64, i64, i64)) -> String {
    if let Some(k) = known_item(name, unscaled, id) {
        if !k.rarity.is_empty() {
            return k.rarity;
        }
    }
    let known = if name.is_empty() || unscaled { None } else { table().and_then(|t| t.rarity_by_name(name)) };
    if let Some(k) = known {
        return k;
    }
    if table().is_some_and(|t| t.muddled(name)) {
        return "Unknown".into();
    }
    rarity_from_packet(packet).unwrap_or_else(|| "Unknown".into())
}

/// The packet's own rarity claim, as a display name.
pub fn rarity_from_packet(rarity: &Value) -> Option<String> {
    // numbers arrive as floats ("d": 5.0) — normalise before matching
    let key = match as_int(rarity) {
        Some(n) => n.to_string(),
        None => match rarity {
            Value::String(s) => s.trim().to_string(),
            _ => return None,
        },
    };
    if let Some((_, name)) = RARITIES.iter().find(|(id, _)| *id == key) {
        return Some(name.to_string());
    }
    if key.is_empty() || key.parse::<i64>().is_ok() {
        return None;
    }
    let mut chars = key.chars();
    let titled = match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str().to_lowercase().as_str(),
        None => key.clone(),
    };
    RARITIES.iter().any(|(_, n)| *n == titled).then_some(titled)
}

fn is_item_like(v: &Value) -> bool {
    v.is_object() && (has(v, ITEM_SIGNATURE_FIELDS) || has(v, ITEM_RARITY_FIELDS))
}

/// itemData without a pickup/inventory route is a world-sync snapshot, not a
/// pickup — counting it would inflate everything.
fn is_inventory_item_data(d: &Value, item_data: &Value) -> bool {
    if has(item_data, PICKUP_FIELDS) {
        return true;
    }
    let route = match field(d, &["route", "__route"]) {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let ctx = format!("{} {}", route, msg_text(d)).to_lowercase();
    ctx.contains("inventory") || ctx.contains("pickup")
}

fn object_items(v: &Value) -> Vec<(Option<String>, Value)> {
    match v {
        Value::Object(map) => map
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(fp, item)| (Some(fp.clone()), item.clone()))
            .collect(),
        _ => vec![],
    }
}

/// Every shape a pickup can arrive in, in the order the reference client
/// checks. The bool marks a GROUND drop (generated near the player) as opposed
/// to an inventory addition (the pickup itself).
fn item_sources(d: &Value) -> Vec<(Option<String>, Value, bool)> {
    // A trip to the market is not a find: taking an item back off the trade
    // board answers with the item in full, the shape of a drop answer. The
    // market marks are the discriminator.
    if MARKET_FIELDS.iter().any(|f| has(d, &[f])) {
        return vec![];
    }
    let own_fp = match field(d, &["addedItemFingerprint", "added_item_fingerprint", "fingerprint"]) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let ops = field(d, &["operations"]).unwrap_or(Value::Null);

    // Splitting a stack is not finding anything: a split answers with the new
    // half as `itemsAdded`, under a fingerprint that has never been seen —
    // the same shape a pickup arrives in.
    if has(&ops, &["split"]) {
        return vec![];
    }

    let pickups = |v: Vec<(Option<String>, Value)>| -> Vec<(Option<String>, Value, bool)> {
        v.into_iter().map(|(fp, item)| (fp, item, false)).collect()
    };

    if let Some(add) = field(&ops, &["add"]) {
        return pickups(object_items(&add));
    }
    // stacked pickups (keys, materials): { stack: { <fp>: { pickup_add_data: {...} } } }
    if let Some(Value::Object(stacked)) = field(&ops, &["stack"]) {
        return stacked
            .iter()
            .filter_map(|(fp, v)| field(v, PICKUP_FIELDS).map(|item| (Some(fp.clone()), item, false)))
            .collect();
    }
    if let Some(added) = field(d, &["itemsAdded", "items_added"]) {
        return pickups(object_items(&added));
    }
    if let Some(item_data) = field(d, ITEM_DATA_FIELDS) {
        if is_inventory_item_data(d, &item_data) {
            if let Some(pickup) = field(&item_data, PICKUP_FIELDS) {
                return vec![(own_fp, pickup, false)];
            }
            let nested: Vec<(Option<String>, Value)> = match &item_data {
                Value::Object(map) => map
                    .iter()
                    .filter_map(|(fp, v)| field(v, PICKUP_FIELDS).map(|item| (Some(fp.clone()), item)))
                    .collect(),
                _ => vec![],
            };
            if !nested.is_empty() {
                return pickups(nested);
            }
            if is_item_like(&item_data) {
                return vec![(own_fp, item_data, false)];
            }
            return pickups(object_items(&item_data));
        }
        // Unrouted itemData is the server answering "here is what dropped",
        // which holds only while the server is the one talking. This client
        // puts an item in the same field when it posts one to the market.
        if has(d, CLIENT_ENVELOPE_FIELDS) {
            return vec![];
        }
        // Only `c == 1` items are named ones: their ids come from the unique
        // item space, while `c == 0` drops are ordinary bases numbered 0..20.
        let candidates = if is_item_like(&item_data) {
            vec![(own_fp, item_data)]
        } else {
            object_items(&item_data)
        };
        // A quest's reward is not lying on the floor either: it goes into the
        // save's `fortune_item`. The discriminator is that a thing in the
        // world says where it is — the world's id for its spot, a map
        // position, or nothing. And nothing means it is not on the floor.
        return candidates
            .into_iter()
            .filter(|(fp, item)| {
                int_field(item, &["c"]) == 1
                    || fingerprint_type(fp.as_deref().unwrap_or("")).is_some_and(|t| {
                        t == RELIC
                            || SELF_NUMBERED.contains(&t)
                            || (t == CONSUMABLE && ENTRY_CODEX_IDS.contains(&int_field(item, &["b"])))
                    })
            })
            .filter(|(_, item)| !belongs_to_a_player(item))
            .filter(|(_, item)| lies_on_the_floor(item))
            .map(|(fp, item)| (fp, item, true))
            .collect();
    }
    if let Some(wrapped) = field(d, ITEM_WRAPPER_FIELDS) {
        return vec![(own_fp, wrapped, false)];
    }
    // Bare item payload. The short format ("a"/"d"/"b") only ever arrives
    // inside a container keyed by fingerprint, so at top level we demand a
    // spelled-out identity field — single letters are common everywhere else.
    if has(d, ITEM_NAMED_SIGNATURE_FIELDS) && has(d, ITEM_RARITY_FIELDS) {
        return vec![(own_fp, d.clone(), false)];
    }
    vec![]
}

/// Whether this item is in somebody's slot rather than lying on the ground.
///
/// The server answers "here is what dropped" and "here is what the merchant
/// has" in the same shape. The item's own account of where it is
/// discriminates: on the ground it has a place on the map, in a shop window it
/// has a player.
///
/// ```text
///   somewhere in the world   "gd": 2422649   or   "gd": {"pos": [11, 0]}
///   in somebody's slot       "gd": {"player": 0}
/// ```
fn belongs_to_a_player(item: &Value) -> bool {
    OWNER_FIELDS
        .iter()
        .filter_map(|f| field_ref(item, &[*f]))
        .any(|v| matches!(v, Value::Object(map) if map.contains_key("player")))
}

/// Whether the item says where in the world it is.
fn lies_on_the_floor(item: &Value) -> bool {
    OWNER_FIELDS.iter().any(|f| field_ref(item, &[*f]).is_some())
}

fn item_events(d: &Value) -> Vec<GameEvent> {
    // What left the bags, before what entered them: both arrive in one
    // message when an item is moved, and the engine has to know the item is
    // coming back before it is told that it arrived.
    let fps: Vec<String> =
        match field(d, &["operations"]).as_ref().and_then(|o| field(o, &["remove"])) {
            Some(Value::Array(gone)) => gone.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Some(Value::Object(gone)) => gone.keys().cloned().collect(),
            _ => vec![],
        };
    let mut out = match fps.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>() {
        fps if fps.is_empty() => vec![],
        fps => vec![GameEvent::ItemsLetGo(fps)],
    };
    out.extend(
        item_sources(d)
            .into_iter()
            .filter(|(_, item, _)| item.is_object())
            .map(|(fp, item, ground)| item_event(&item, fp.as_deref(), ground)),
    );
    out
}

fn item_event(obj: &Value, fingerprint: Option<&str>, ground: bool) -> GameEvent {
    let fp_type = fingerprint.and_then(fingerprint_type);
    let short_id = int_field(obj, &["b"]);
    let explicit_type = int_field(obj, &["type", "itemType", "item_type"]);
    let item_type = if explicit_type != 0 { explicit_type } else { fp_type.unwrap_or(short_id) };
    let explicit_id = int_field(obj, &["id", "itemId", "item_id"]);
    let item_id = if explicit_id != 0 {
        explicit_id
    } else if fp_type.is_some() {
        short_id
    } else {
        int_field(obj, &["gid"])
    };
    let weapon_type = {
        let wt = int_field(obj, &["weapon_type", "weaponType"]);
        if wt != 0 {
            wt
        } else if item_type == 3 {
            int_field(obj, &["j"])
        } else {
            0
        }
    };
    let explicit_name = match field(obj, &["name", "itemName", "item_name", "label"]) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    // Odyssey keeps its own item space, and its packet says so: it carries an
    // `h` that no seasonal item sends. Its `d` is not a rarity on the scale
    // the rest of the game uses.
    let odyssey = has(obj, &["h"]);
    let claimed = field(obj, ITEM_RARITY_FIELDS).unwrap_or_else(|| json!(0));
    // An ordinary base cannot carry a named item's rarity, and `c` is the
    // game's own flag for which id space an item came from.
    let plain_base = has(obj, &["c"]) && int_field(obj, &["c"]) == 0;
    let named_only = rarity_from_packet(&claimed)
        .is_some_and(|r| RARE_RARITIES.contains(&r.as_str()));
    let rarity = if odyssey || (plain_base && named_only) { Value::Null } else { claimed };
    // A name read out of the tables is a guess about which item this is, and
    // a guess must not become evidence about what it is worth. Keys, gems,
    // runes and materials are the exception: for those types `c == 0` is
    // simply what they are and the id IS the identity.
    let named_flag = int_field(obj, &["c"]) == 1;
    let claimed_tier = int_field(obj, &["tier", "n"]);
    let unnamed_top = !named_flag && claimed_tier >= SS_TIER;
    let tier = if odyssey || unnamed_top || !(1..=SS_TIER).contains(&claimed_tier) {
        0
    } else {
        claimed_tier
    };
    let resource = SELF_NUMBERED.contains(&item_type);
    let worth_naming = rarity_from_packet(&rarity)
        .is_some_and(|r| RARE_RARITIES.contains(&r.as_str()));
    let name = if !explicit_name.is_empty() {
        explicit_name
    } else if named_flag || worth_naming || resource || is_entry_codex(item_type, item_id, "") {
        table()
            .and_then(|t| t.name_by_id(item_type, item_id, weapon_type))
            .unwrap_or_default()
    } else {
        String::new()
    };
    GameEvent::ItemAdded {
        rarity,
        unscaled: odyssey && !named_flag,
        mf: int_field(obj, &["mf_drop", "mfDrop", "m"]) == 1,
        tier,
        item_type,
        item_id,
        weapon_type,
        seed: int_field(obj, &["seed", "a"]),
        name,
        announced: false,
        amount: int_field(obj, &["amount", "o"]).max(1),
        fingerprint: fingerprint.unwrap_or_default().to_string(),
        hash: match field(obj, &["sh"]) {
            Some(Value::String(h)) => h,
            _ => String::new(),
        },
        ground,
    }
}

/// One packet can carry several things at once (currency + items + zone), so
/// every matching rule contributes; matching only the first loses events.
fn dict_to_events(d: &Value) -> Vec<GameEvent> {
    if d.as_object().is_none_or(|o| o.contains_key("steam")) {
        return vec![];
    }
    let mut events = Vec::new();
    let message = msg_text(d).to_lowercase();

    let wrapped_currency = field(d, GOLD_FIELDS);
    let gold_delta = int_field(d, &["goldAmount", "gold_amount", "amount_gold"]);
    if wrapped_currency.is_some() || has_currency_totals(d) || gold_delta > 0 {
        let c = wrapped_currency.unwrap_or_else(|| d.clone());
        events.push(GameEvent::Gold(Currency {
            gss: int_field(&c, &["GSS", "gss"]),
            gsh: int_field(&c, &["GSH", "gsh"]),
            gns: int_field(&c, &["GNS", "gns"]),
            gnh: int_field(&c, &["GNH", "gnh"]),
            gbp: int_field(&c, &["GBP", "gbp"]),
            delta: gold_delta.max(0),
        }));
    }
    if has(d, XP_TOTAL_FIELDS) {
        events.push(GameEvent::XpGain(guild_share_to_character(xp_gain(d))));
    }
    // The client's heartbeat, base64'd, in either of the two shapes it takes.
    // It arrives every few seconds, where the character save arrives only when
    // the game saves.
    if let Some(Value::String(blob)) = field(d, &["game_state", "gameState"]) {
        if let Some(state) = b64_json(&blob) {
            if let Some(room) = field_ref(&state, &["room"]).and_then(|v| v.as_str()) {
                if !room.is_empty() {
                    events.push(GameEvent::Room(room.to_string()));
                }
            }
            // Only what this packet actually carries. A field it does not
            // mention is `None`, not zero: the heartbeat has grown a second
            // shape before and it will again.
            let mf = has(&state, &["mf"]).then(|| int_field(&state, &["mf"]));
            let level = int_field(&state, &["level"]);
            let hlevel = int_field(&state, &["hlevel", "heroLevel", "herolevel"]);
            if mf.is_some() || level > 0 || hlevel > 0 {
                events.push(GameEvent::Vitals {
                    mf,
                    level,
                    hlevel,
                    // the game says outright whether this room is the satanic
                    // one; comparing zone codes was always a guess at it
                    satanic_here: has(&state, &["sz"]).then(|| as_bool(field_ref(&state, &["sz"]))),
                });
            }
        }
    }
    // A whole word, not a substring: "Lost Master's Platemail" is a Set item
    // announced to the whole shard, and matched as a substring, a stranger's
    // drop rang the mail chime.
    if !is_chat(d) && (says_mail(&message) || has(d, MAIL_FIELDS)) {
        events.push(GameEvent::Mail(mail_is_present(d)));
    }
    // server chat announcement: "Someone just found [Item Name]"
    if let Some((finder, name)) = announced_item_name(&msg_text(d)) {
        events.push(GameEvent::Found { finder, name });
    }
    events.extend(item_events(d));
    if let Some(region) = zone_request_region(d) {
        events.push(GameEvent::ZoneRegion(region));
    }
    if let Some(account) = our_account(d) {
        events.push(GameEvent::WhoseAccount(account));
    }
    if has(d, SATANIC_ZONE_FIELDS) {
        let zone = match field(d, SATANIC_ZONE_FIELDS) {
            Some(Value::String(s)) => s,
            Some(v) => v.to_string(),
            None => String::new(),
        };
        let buffs = effect_ids(field(
            d,
            &["buffs", "satanicZoneBuffs", "satanic_zone_buffs", "zoneBuffs", "zone_buffs"],
        ));
        let debuffs = effect_ids(field(
            d,
            &["debuffs", "satanicZoneDebuffs", "satanic_zone_debuffs", "zoneDebuffs", "zone_debuffs"],
        ));
        events.push(GameEvent::SatanicZone { zone, buffs, debuffs });
    }

    let full_account = has(d, &["experience"]) && has(d, ACCOUNT_SIGNATURE_FIELDS);
    // login identity payload: no experience/talents, but carries name, uid,
    // cross-region id, season and hardcore (and is not a nearby-player list)
    let identity_account = !full_account
        && has(d, &["name"])
        && has(d, &["accountUID", "accountUid", "unique_id", "uniqueId"])
        && has(d, REGION_ID_FIELDS)
        && !has(d, &["platformUserName", "platform_user_name", "nameColor", "name_color", "slot"]);
    if (full_account || identity_account) && has(d, &["season"]) && has(d, &["hardcore"]) {
        let name = match field(d, &["name"]) {
            Some(Value::String(s)) => s,
            _ => String::new(),
        };
        events.push(GameEvent::Account {
            experience: int_field(d, &["experience"]),
            has_experience: has(d, &["experience"]),
            season: int_field(d, &["season"]),
            hardcore: int_field(d, &["hardcore"]),
            blood_pact: int_field(d, &["blood_pact", "bloodPact"]),
            name,
            level: int_field(d, &["level"]),
            herolevel: int_field(d, &["heroLevel", "herolevel"]),
            difficulty: int_field(d, &["difficulty"]),
            hell_sub: int_field(d, &["hell_subdifficulty", "hellSubdifficulty"]),
            act: act_of(d),
            kills: int_field(
                d,
                &[
                    "statisticTotalMonsterKills",
                    "statistic_total_monster_kills",
                    "totalMonsterKills",
                    "total_monster_kills",
                ],
            ),
            tallies: tallies(d),
        });
    // The guild share of somebody else's experience — the one event with no
    // shape of its own to check against, just a field called `xp`. The capture
    // filter takes every plaintext byte on the machine, not only the game's,
    // so a stray key from another program can reach here and be credited as
    // experience. Hence the same standard as the item path: it has to arrive
    // in something shaped like a game message, and every one of those carries
    // a `status` or a `message`.
    } else if !full_account
        && !identity_account
        && (has(d, &["status"]) || !msg_text(d).is_empty())
        && has(d, XP_GAIN_FIELDS)
        && !has(d, XP_TOTAL_FIELDS)
    {
        let raw = xp_gain(d);
        events.push(GameEvent::XpGain(if is_quest_reward(d) { raw } else { guild_share_to_character(raw) }));
    }
    events
}

fn walk_dicts(v: &Value, events: &mut Vec<GameEvent>) {
    match v {
        Value::Object(_) => events.extend(dict_to_events(v)),
        Value::Array(items) => {
            for item in items {
                walk_dicts(item, events);
            }
        }
        _ => {}
    }
}

/// Extract every event from already-decoded messages.
pub fn events_from_messages(messages: &[Value]) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for m in messages {
        walk_dicts(m, &mut events);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(msg: Value) -> Vec<GameEvent> {
        events_from_messages(&[msg])
    }

    #[test]
    fn gold_from_currency_data() {
        let events = ev(json!({
            "message": "save ok", "status": 1,
            "currencyData": {"GSS": 1234.0}
        }));
        assert!(matches!(events.first(), Some(GameEvent::Gold(c)) if c.gss == 1234));
    }

    #[test]
    fn gold_delta() {
        let events = ev(json!({"message": "gold", "status": 1, "goldAmount": 250.0}));
        assert!(matches!(events.first(), Some(GameEvent::Gold(c)) if c.delta == 250));
    }

    #[test]
    fn guild_xp_scaled_but_quest_not() {
        let share = ev(json!({"message": "guild xp 150", "status": 1, "xp": 150.0}));
        assert!(matches!(share.first(), Some(GameEvent::XpGain(n)) if *n == 1000));
        let quest = ev(json!({"message": "quest!", "status": 1, "xp": 150.0, "questId": 9}));
        assert!(matches!(quest.first(), Some(GameEvent::XpGain(n)) if *n == 150));
    }

    #[test]
    fn mail_word_not_platemail() {
        let mail = ev(json!({"message": "New mail!", "status": 1}));
        assert!(matches!(mail.first(), Some(GameEvent::Mail(true))));
        let set_drop = ev(json!({"message": "just found [Lost Master's Platemail]"}));
        assert!(matches!(set_drop.first(), Some(GameEvent::Found { .. })));
        assert!(!set_drop.iter().any(|e| matches!(e, GameEvent::Mail(_))));
    }

    #[test]
    fn satanic_zone_event() {
        let events = ev(json!({
            "satanicZoneName": "Blood Moors",
            "buffs": [1, 2.0], "debuffs": "3,4"
        }));
        assert!(matches!(
            events.first(),
            Some(GameEvent::SatanicZone { zone, buffs, debuffs })
                if zone == "Blood Moors" && buffs == &vec![1, 2] && debuffs == &vec![3, 4]
        ));
    }

    #[test]
    fn heartbeat_room_and_vitals() {
        let state = serde_json::to_string(&json!({"room": "Act_02_04", "mf": 320.0, "level": 88.0, "hlevel": 21, "sz": true})).unwrap();
        use base64::Engine as _;
        let blob = base64::engine::general_purpose::STANDARD.encode(state);
        let events = ev(json!({"message": "state", "status": 1, "game_state": blob}));
        assert!(events.iter().any(|e| matches!(e, GameEvent::Room(r) if r == "Act_02_04")));
        assert!(events.iter().any(|e| matches!(e, GameEvent::Vitals { mf: Some(320), satanic_here: Some(true), .. })));
    }

    #[test]
    fn account_save_full() {
        let events = ev(json!({
            "name": "Buffy", "class": 2, "heroLevel": 26, "season": 9,
            "hardcore": 0, "experience": 51234.0, "level": 100,
            "difficulty": 1, "act_previous": [1, 6],
            "statisticTotalMonsterKills": 4321.0,
            "statisticUberDamienKills": 3.0
        }));
        assert!(matches!(
            events.first(),
            Some(GameEvent::Account { act: 6, kills: 4321, .. })
        ));
    }

    #[test]
    fn items_added_from_operations() {
        let events = ev(json!({
            "message": "pickup", "status": 1,
            "operations": {"add": {
                "99-123-abc-6": {"a": 77.0, "d": 6, "b": 5, "c": 1, "name": "Harlequinn's Crest"}
            }}
        }));
        assert!(matches!(
            events.first(),
            Some(GameEvent::ItemAdded { item_type: 6, name, .. }) if name == "Harlequinn's Crest"
        ));
    }

    #[test]
    fn ground_drop_only_when_on_the_floor() {
        // named item lying on the ground
        let drop = ev(json!({
            "itemData": {"8-1-abc-1": {"a": 5.0, "d": 6, "c": 1, "gd": 2422649.0}}
        }));
        assert!(matches!(drop.first(), Some(GameEvent::ItemAdded { ground: true, .. })));
        // in somebody's slot: refused
        let held = ev(json!({
            "itemData": {"8-1-abc-1": {"a": 5.0, "d": 6, "c": 1, "gd": {"player": 0}}}
        }));
        assert!(matches!(held.first(), None));
        // nowhere: a fortune item, refused
        let fortune = ev(json!({
            "itemData": {"8-1-abc-1": {"a": 5.0, "d": 6, "c": 1}}
        }));
        assert!(matches!(fortune.first(), None));
    }

    #[test]
    fn market_message_is_not_a_find() {
        let events = ev(json!({
            "message": "Removal success", "marketId": "138066",
            "itemData": {"8-1-abc-1": {"a": 5.0, "d": 6, "c": 1, "gd": 99.0}}
        }));
        assert!(events.is_empty());
    }

    #[test]
    fn stack_split_is_not_a_find() {
        let events = ev(json!({
            "message": "split", "status": 1,
            "operations": {"split": {"99-1-abc-12": {"sh": "cf02e71d8341"}}}
        }));
        assert!(events.iter().all(|e| !matches!(e, GameEvent::ItemAdded { .. })));
    }

    #[test]
    fn zone_question_detected() {
        let events = ev(json!({"unique_account_id": "99-123", "crossregion_identifier": "eu-1"}));
        assert!(matches!(events.first(), Some(GameEvent::ZoneRegion(r)) if r == "eu-1"));
    }

    #[test]
    fn chat_is_not_mail() {
        let events = ev(json!({
            "message": "anyone at the mailbox?", "chatRoom": "town",
            "nameColor": "#ffffff"
        }));
        assert!(events.is_empty());
    }

    #[test]
    fn items_let_go_before_add() {
        let events = ev(json!({
            "message": "move", "status": 1,
            "operations": {
                "remove": ["99-1-fp1-8"],
                "add": {"99-1-fp1-8": {"a": 1.0, "d": 1, "c": 1}}
            }
        }));
        assert!(matches!(events.first(), Some(GameEvent::ItemsLetGo(_))));
        assert!(events.iter().any(|e| matches!(e, GameEvent::ItemAdded { .. })));
    }

    #[test]
    fn fingerprint_helpers() {
        assert_eq!(fingerprint_type("8-4653008-abc-1"), Some(1));
        assert_eq!(fingerprint_account("99-4964607-1a03b93f92c-10"), Some("4964607"));
    }

    #[test]
    fn summary_counts() {
        let mut s = Summary::default();
        s.observe(&GameEvent::XpGain(120));
        s.observe(&GameEvent::Gold(Currency { delta: 50, ..Default::default() }));
        s.observe(&GameEvent::ItemAdded {
            rarity: json!(6),
            name: "x".into(),
            unscaled: false,
            mf: false,
            tier: 0,
            item_type: 6,
            item_id: 1,
            weapon_type: 0,
            seed: 0,
            announced: false,
            amount: 1,
            fingerprint: String::new(),
            hash: String::new(),
            ground: false,
        });
        s.observe(&GameEvent::ItemAdded {
            rarity: json!(6),
            name: "y".into(),
            unscaled: false,
            mf: false,
            tier: 0,
            item_type: 6,
            item_id: 2,
            weapon_type: 0,
            seed: 0,
            announced: false,
            amount: 1,
            fingerprint: String::new(),
            hash: String::new(),
            ground: false,
        });
        assert_eq!(s.xp_gain, 120);
        assert_eq!(s.gold_delta, 50);
        assert_eq!(s.items_seen, 2);
        assert_eq!(s.items_by_rarity.get("Satanic"), Some(&2));
    }
}

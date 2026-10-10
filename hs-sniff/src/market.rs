//! Hero Siege market client: turn a plain-text price question into a live
//! listing query and a price summary.
//!
//! The game's market is **HTTPS**, so a passive capture cannot read prices off
//! the wire — the sniffer only ever sees the game's own outgoing request
//! (its session context, in the clear) and never the TLS answer. Following the
//! Hero Siege Companion, this module therefore re-issues the query *as the
//! game would*: it signs the request with the checksum/multipass the client
//! uses and posts it to the same endpoint.
//!
//! Pipeline:
//! - [`SessionContext`] — the six fields the client attaches to market calls,
//!   accumulated from [`crate::events`] as they pass.
//! - [`parse_market_query`] — plain text plus a [`MarketCatalog`] -> filters.
//! - [`build_market_body`] — filters + context -> the exact form body.
//! - [`fetch_market`] — POST it; [`parse_market_response`] decodes the zlib
//!   blob into listings and a currency-safe price band.
//!
//! Nothing here is needed for the passive event pipeline; it lives behind the
//! `market` CLI command and the `hs_market` MCP tool.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Wire constants (the client's own, reproduced)
// ---------------------------------------------------------------------------

pub const MARKET_HOST: &str = "hsmarket.panicartstudios.com";
pub const MARKET_PATH: &str = "/market/herosiege_api_bootstrap.php";
pub const FETCH_ITEMS_API_SCRIPT: &str = "market/market_fetch_items";

/// Salt mixed into the per-account checksum (`market-checksum.ts`).
const CHECKSUM_SALT: &str = "qmR9xbft3ZPvE3Ab";
/// Salt mixed into the route signature (`market-multipass.ts`).
const MULTIPASS_SALT: &str = "IoksbAf67bHlX10snbfB4be3x0z9";

/// Bits packing a market item mask: `repo<<30 | weaponType<<22 | type<<12 | id`.
pub const MASK_REPOSITORY: u64 = 1 << 30;
pub const MASK_WEAPON_TYPE: u64 = 1 << 22;
pub const MASK_ITEM_TYPE: u64 = 1 << 12;
pub const MASK_MAX: u64 = u32::MAX as u64;
const MAX_TYPE: u64 = 0x3ff;
const MAX_GAME_ID: u64 = 0xfff;
const MAX_RUNEWORD_ID: u32 = 100;
const MAX_STAT_FILTERS: usize = 16;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Session context
// ---------------------------------------------------------------------------

/// The session credentials the client attaches to its market requests. All six
/// come out of the game's own outgoing packets; `checksum` is derived, never
/// captured. Fields accumulate: a packet may carry only some.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionContext {
    pub account_id: Option<String>,
    pub unique_account_id: Option<String>,
    pub crossregion_identifier: Option<String>,
    pub season: Option<String>,
    pub hardcore: Option<String>,
    pub beta: Option<String>,
    /// The session token the client attaches to its requests. Sensitive; never
    /// rendered into a log or a tool result (see [`SessionContext::to_json`]).
    pub identifier: Option<String>,
    /// The checksum seen on the game's own **market** request. Kept as a
    /// fallback/diagnostic: the server accepts the derived checksum (the
    /// Companion's formula still holds), and the native sell-poll checksum is
    /// bound to a different route, so it is not preferred.
    pub native_checksum: Option<String>,
}

/// The six field names, in the order a caller should report missing ones.
pub const SESSION_FIELDS: [&str; 6] =
    ["account_id", "unique_account_id", "crossregion_identifier", "season", "hardcore", "beta"];

impl SessionContext {
    /// Merge another sighting in, filling blanks. Never overwrites a value that
    /// is already known with a different one silently — the newest sighting
    /// wins only for the fields it actually carries, which is what a fresh
    /// in-game market interaction produces.
    pub fn observe(&mut self, other: &SessionContext) {
        // Prefer the region-qualified account id: the market body carries it,
        // and a later unqualified mailbox packet must not downgrade it.
        if let Some(value) = &other.account_id {
            let qualified_existing = self.account_id.as_deref().is_some_and(|s| s.contains('-'));
            if !qualified_existing || value.contains('-') {
                self.account_id = Some(value.clone());
            }
        }
        for (slot, value) in [
            (&mut self.unique_account_id, &other.unique_account_id),
            (&mut self.crossregion_identifier, &other.crossregion_identifier),
            (&mut self.season, &other.season),
            (&mut self.hardcore, &other.hardcore),
            (&mut self.beta, &other.beta),
            (&mut self.identifier, &other.identifier),
            (&mut self.native_checksum, &other.native_checksum),
        ] {
            if let Some(v) = value {
                *slot = Some(v.clone());
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        SESSION_FIELDS.iter().all(|f| self.field(f).is_none())
    }

    fn field(&self, name: &str) -> Option<&String> {
        match name {
            "account_id" => self.account_id.as_ref(),
            "unique_account_id" => self.unique_account_id.as_ref(),
            "crossregion_identifier" => self.crossregion_identifier.as_ref(),
            "season" => self.season.as_ref(),
            "hardcore" => self.hardcore.as_ref(),
            "beta" => self.beta.as_ref(),
            _ => None,
        }
    }

    /// Which of the six are still unknown — what the operator must capture by
    /// opening the in-game Market once.
    pub fn missing(&self) -> Vec<&'static str> {
        SESSION_FIELDS.iter().copied().filter(|f| self.field(f).is_none()).collect()
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }

    /// Whether a market request can be built: all six fields the server asks
    /// for, so both the body and the derived checksum are complete.
    pub fn can_sign(&self) -> bool {
        self.is_complete()
    }

    /// The checksum to sign with. The derived one is correct for
    /// `market_fetch_items` (verified against the live server); the native
    /// sell-poll checksum is a last resort only.
    pub fn checksum_for_request(&self) -> Option<String> {
        self.checksum().or_else(|| self.native_checksum.clone())
    }

    /// The raw account id: the native checksum strips any region prefix
    /// (`eu-12345` -> `12345`).
    pub fn raw_account_id(&self) -> Option<&str> {
        let id = self.account_id.as_deref()?;
        Some(&id[id.rfind('-').map(|i| i + 1).unwrap_or(0)..])
    }

    /// The per-account request checksum. Needs `account_id`, `season`,
    /// `hardcore` and `beta`.
    pub fn checksum(&self) -> Option<String> {
        let account = self.raw_account_id()?;
        let season = self.season.as_deref()?;
        let hardcore = self.hardcore.as_deref()?;
        let beta = self.beta.as_deref()?;
        Some(sha256_hex(format!("{account}{CHECKSUM_SALT}{season}{hardcore}{beta}").as_bytes()))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "account_id": self.account_id,
            "unique_account_id": self.unique_account_id,
            "crossregion_identifier": self.crossregion_identifier,
            "season": self.season,
            "hardcore": self.hardcore,
            "beta": self.beta,
            // The session token and the market checksum are credentials: report
            // only whether they are known, never their values.
            "identifier_present": self.identifier.is_some(),
            "native_checksum_present": self.native_checksum.is_some(),
            "signing": if self.checksum().is_some() {
                "derived"
            } else if self.native_checksum.is_some() {
                "native"
            } else {
                "none"
            },
            "ready": self.can_sign(),
            "missing": self.missing(),
        })
    }
}

/// The route signature, derived only from the endpoint name.
pub fn market_multipass() -> String {
    sha256_hex(format!("{MULTIPASS_SALT}{FETCH_ITEMS_API_SCRIPT}").as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Query shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketTarget {
    /// A packed normal/unique item identity.
    ItemMask(u64),
    /// A runeword repository id (1..=100).
    Runeword(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatFilter {
    pub stat_id: u32,
    pub minimum: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketQuery {
    pub target: MarketTarget,
    pub min_sockets: Option<u8>,
    pub max_sockets: Option<u8>,
    pub stat_filters: Vec<StatFilter>,
}

impl MarketQuery {
    pub fn item_mask(mask: u64) -> Self {
        Self { target: MarketTarget::ItemMask(mask), min_sockets: None, max_sockets: None, stat_filters: vec![] }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "target": match self.target {
                MarketTarget::ItemMask(mask) => json!({"kind": "item_mask", "value": mask}),
                MarketTarget::Runeword(id) => json!({"kind": "runeword", "value": id}),
            },
            "min_sockets": self.min_sockets,
            "max_sockets": self.max_sockets,
            "stat_filters": self.stat_filters.iter()
                .map(|f| json!({"stat_id": f.stat_id, "minimum": f.minimum}))
                .collect::<Vec<_>>(),
        })
    }
}

/// Pack an identity into the market mask the server matches on.
pub fn item_mask(repository: &str, item_type: u64, game_id: u64, weapon_type: u64) -> Result<u64, MarketError> {
    if item_type > MAX_TYPE || game_id > MAX_GAME_ID || weapon_type > 0xff {
        return Err(MarketError::BadTarget(format!(
            "identity out of range: type {item_type}, id {game_id}, weaponType {weapon_type}"
        )));
    }
    let repo = if repository.eq_ignore_ascii_case("unique") { 1 } else { 0 };
    Ok(repo * MASK_REPOSITORY + weapon_type * MASK_WEAPON_TYPE + item_type * MASK_ITEM_TYPE + game_id)
}

// ---------------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------------

/// Build the exact `application/x-www-form-urlencoded` body the client posts to
/// the market. Pure; the caller decides whether to send it.
pub fn build_market_body(ctx: &SessionContext, query: &MarketQuery) -> Result<String, MarketError> {
    if !ctx.can_sign() {
        return Err(MarketError::IncompleteContext(ctx.missing()));
    }
    let checksum = ctx.checksum_for_request().ok_or_else(|| MarketError::IncompleteContext(ctx.missing()))?;    if let Some(min) = query.min_sockets {
        if min > 6 {
            return Err(MarketError::BadSockets(format!("minimum {min} > 6")));
        }
    }
    if let Some(max) = query.max_sockets {
        if max > 6 {
            return Err(MarketError::BadSockets(format!("maximum {max} > 6")));
        }
    }
    if let (Some(min), Some(max)) = (query.min_sockets, query.max_sockets) {
        if min > max {
            return Err(MarketError::BadSockets(format!("minimum {min} exceeds maximum {max}")));
        }
    }
    if query.stat_filters.len() > MAX_STAT_FILTERS {
        return Err(MarketError::BadStat(format!("more than {MAX_STAT_FILTERS} stat filters")));
    }

    let mut out = form_urlencoded::Serializer::new(String::new());
    for (field, value) in [
        ("account_id", ctx.account_id.as_deref()),
        ("unique_account_id", ctx.unique_account_id.as_deref()),
        ("identifier", ctx.identifier.as_deref()),
        ("crossregion_identifier", ctx.crossregion_identifier.as_deref()),
        ("season", ctx.season.as_deref()),
        ("hardcore", ctx.hardcore.as_deref()),
        ("beta", ctx.beta.as_deref()),
    ] {
        if let Some(value) = value {
            out.append_pair(field, value);
        }
    }
    out.append_pair("checksum", &checksum);
    out.append_pair("multipass", &market_multipass());
    out.append_pair("api_script", FETCH_ITEMS_API_SCRIPT);
    out.append_pair("item_sort", "2");
    out.append_pair("scroll_page", "0");
    out.append_pair("page_id", "99999999");
    out.append_pair("get_highest_id", "1");
    match query.target {
        MarketTarget::ItemMask(mask) => {
            out.append_pair("filter_masks", &json!([mask]).to_string());
        }
        // A runeword query selects by runeword id, and `filter_masks` must be
        // **absent** rather than an empty array: the server answers an explicit
        // `filter_masks=[]` with HTTP 500. Verified live against
        // market/market_fetch_items (runeword 51 -> 8 listings).
        MarketTarget::Runeword(id) => {
            if id == 0 || id > MAX_RUNEWORD_ID {
                return Err(MarketError::BadTarget(format!("runeword id {id} out of range")));
            }
            out.append_pair("filter_runeword", &id.to_string());
        }
    };
    if let Some(min) = query.min_sockets {
        out.append_pair("filter_sockets_min", &min.to_string());
    }
    if let Some(max) = query.max_sockets {
        out.append_pair("filter_sockets_max", &max.to_string());
    }
    if !query.stat_filters.is_empty() {
        let clauses: Vec<Value> = query
            .stat_filters
            .iter()
            .map(|f| json!({"statId": f.stat_id, "filter": 2, "statValue": f.minimum}))
            .collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&clauses).unwrap_or_default(),
        );
        out.append_pair("stat_filter", &encoded);
    }
    Ok(out.finish())
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MarketListing {
    /// Gold total for the whole listing. This is a **stack total**, not a unit
    /// price: half the live rune rows carry `price != unit_price`.
    pub price: i64,
    /// Per-unit gold price as the server reports it, when present.
    pub unit_price: Option<f64>,
    /// Units in this listing. The server only sometimes labels it, so it is
    /// otherwise derived from `price / unit_price` and defaults to 1.
    pub units: u64,
    /// Server-side listing id. Monotonic, so it doubles as a recency key and is
    /// what the observation corpus diffs on to infer sales. 0 when absent.
    pub market_id: u64,
}

impl MarketListing {
    /// Per-unit gold price — the only comparable price across a market where
    /// the same rune is listed as 1 unit and as a 13-unit stack. A single-unit
    /// listing has `unit_price == price`; when the server omits it, the stack
    /// price is the unit price.
    pub fn unit_price_gold(&self) -> i64 {
        match self.unit_price {
            Some(unit) if unit > 0.0 => unit.round() as i64,
            _ => self.price,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketResult {
    /// Gold listings only, in the order the server returned them.
    pub listings: Vec<MarketListing>,
    pub total_matches: Option<u64>,
    /// Decoded rows before gold-only filtering, so the count stays honest.
    pub returned_count: u64,
}

/// A price band for one currency. The market is gold-only, but the shape keeps
/// currencies separate so a barter row can never be averaged into a gold one.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceBand {
    pub currency: &'static str,
    pub count: usize,
    pub lowest: i64,
    pub median: i64,
    pub highest: i64,
}

impl MarketResult {
    /// Cheapest **unit** price first — the ask ladder. Sorting by `price`
    /// instead would rank a 13-unit stack above a single cheaper unit.
    pub fn by_price(&self) -> Vec<&MarketListing> {
        let mut rows: Vec<&MarketListing> = self.listings.iter().collect();
        rows.sort_by_key(|row| row.unit_price_gold());
        rows
    }

    /// Summary over every gold listing on the page, in per-unit gold. Banding
    /// `price` would average stacks of different sizes together and inflate
    /// every figure (the game's own `avgPrice` does exactly that).
    pub fn band(&self) -> Option<PriceBand> {
        if self.listings.is_empty() {
            return None;
        }
        let mut prices: Vec<i64> = self.listings.iter().map(|row| row.unit_price_gold()).collect();
        prices.sort_unstable();
        Some(PriceBand {
            currency: "gold",
            count: prices.len(),
            lowest: prices[0],
            median: prices[prices.len() / 2],
            highest: prices[prices.len() - 1],
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "listings": self.listings.iter()
                .map(|row| json!({"market_id": row.market_id, "price": row.price,
                                 "unit_price": row.unit_price, "units": row.units}))
                .collect::<Vec<_>>(),
            "total_matches": self.total_matches,
            "returned_count": self.returned_count,
            "band": self.band().map(|b| json!({
                "currency": b.currency, "count": b.count,
                "lowest": b.lowest, "median": b.median, "highest": b.highest,
            })),
        })
    }
}

/// Decode a market response body. `status` is the HTTP status the transport saw.
pub fn parse_market_response(body: &[u8], status: u16) -> Result<MarketResult, MarketError> {
    if status != 200 {
        return Err(MarketError::HttpStatus(status));
    }
    let envelope: Value = serde_json::from_slice(body)
        .map_err(|e| MarketError::Malformed(format!("response is not JSON: {e}")))?;
    let object = envelope
        .as_object()
        .ok_or_else(|| MarketError::Malformed("response envelope is not an object".into()))?;
    if object.get("status").and_then(Value::as_i64) != Some(1) {
        return Err(MarketError::ServerRejected(classify_server_reason(object)));
    }
    let items = object
        .get("items")
        .and_then(Value::as_str)
        .ok_or_else(|| MarketError::Malformed("response has no `items` blob".into()))?;
    let decoded = inflate_items(items)?;
    let rows = decoded
        .as_array()
        .ok_or_else(|| MarketError::Malformed("decoded `items` is not an array".into()))?;
    let returned_count = rows.len() as u64;

    let mut listings = Vec::new();
    for row in rows {
        let Some(object) = row.as_object() else { continue };
        let Some(price) = object.get("price").and_then(Value::as_i64) else { continue };
        if price < 0 {
            continue;
        }
        // Item-priced rows have a non-empty `price_items`; this build has no
        // barter projection, so they are dropped rather than mislabelled gold.
        if let Some(items) = object.get("price_items") {
            match items.as_array() {
                Some(rows) if rows.is_empty() => {}
                _ => continue,
            }
        }
        let unit_price = object
            .get("unit_price")
            .or_else(|| object.get("unitPrice"))
            .and_then(parse_unit_price);
        let units = object
            .get("units")
            .and_then(Value::as_u64)
            .or_else(|| {
                // `price` covers the whole stack, so units = price / unit price.
                let unit = unit_price?;
                if unit <= 0.0 {
                    return None;
                }
                let n = (price as f64 / unit).round();
                let exact = (price as f64 - unit * n).abs() <= 1.0;
                (n >= 1.0 && exact).then_some(n as u64)
            })
            .unwrap_or(1);
        let market_id = object.get("market_id").and_then(Value::as_u64).unwrap_or(0);
        listings.push(MarketListing { price, unit_price, units, market_id });
    }
    let total_matches = object.get("itemCount").and_then(Value::as_u64);
    Ok(MarketResult { listings, total_matches, returned_count })
}

fn parse_unit_price(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite() && *v >= 0.0),
        // The retained responses encode unit_price as bounded decimal text.
        Value::String(s) if s.len() <= 40 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.') => {
            s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
        }
        _ => None,
    }
}

fn inflate_items(encoded: &str) -> Result<Value, MarketError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|e| MarketError::Malformed(format!("`items` is not base64: {e}")))?;
    let reader = flate2::read::ZlibDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    reader
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut out)
        .map_err(|e| MarketError::Malformed(format!("`items` is not zlib: {e}")))?;
    serde_json::from_slice(&out).map_err(|e| MarketError::Malformed(format!("`items` is not JSON: {e}")))
}

/// Classify why the server rejected a request, without retaining its text.
fn classify_server_reason(object: &Map<String, Value>) -> String {
    let message = [object.get("message"), object.get("error")]
        .iter()
        .filter_map(|v| v.and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let reason = if message.contains("checksum") {
        "checksum"
    } else if message.contains("multipass") {
        "multipass"
    } else if message.contains("account") {
        "account"
    } else if message.contains("session") || message.contains("expired") || message.contains("crossregion") {
        "session"
    } else if message.contains("rate limit") || message.contains("too many") {
        "rate-limit"
    } else {
        "other"
    };
    reason.to_string()
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Sign and POST the query, then decode the response. Blocking; callers on an
/// async runtime should run it on a worker.
pub fn fetch_market(ctx: &SessionContext, query: &MarketQuery) -> Result<MarketResult, MarketError> {
    let body = build_market_body(ctx, query)?;
    let url = format!("https://{MARKET_HOST}{MARKET_PATH}");
    let response = ureq::post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept-Encoding", "identity")
        .timeout(REQUEST_TIMEOUT)
        .send_string(&body);
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => return Err(MarketError::HttpStatus(status)),
        Err(error) => return Err(MarketError::Transport(error.to_string())),
    };
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| MarketError::Transport(e.to_string()))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(MarketError::Malformed("response exceeded 4 MiB".into()));
    }
    parse_market_response(&bytes, status)
}

// ---------------------------------------------------------------------------
// Cooldown + result cache
// ---------------------------------------------------------------------------

/// Minimum gap between two live market fetches. The server is a live game
/// service; a client that spams it is a bad citizen.
pub const MARKET_COOLDOWN_MS: u64 = 10_000;
/// How long a fetched page is served from cache.
pub const MARKET_CACHE_TTL_MS: u64 = 30_000;
const MARKET_CACHE_MAX: usize = 64;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The result of asking for prices: either a page, or a refusal to fetch again
/// so soon.
#[derive(Debug, Clone, PartialEq)]
pub enum MarketOutcome {
    Ready { result: MarketResult, observed_at: u64, cached: bool },
    Cooldown { next_allowed_at: u64 },
}

impl MarketOutcome {
    pub fn to_json(&self) -> Value {
        match self {
            MarketOutcome::Ready { result, observed_at, cached } => json!({
                "status": "ok",
                "observed_at": observed_at,
                "cached": cached,
                "market": result.to_json(),
            }),
            MarketOutcome::Cooldown { next_allowed_at } => json!({
                "status": "cooldown",
                "next_allowed_at": next_allowed_at,
            }),
        }
    }
}

struct CacheEntry {
    result: MarketResult,
    observed_at: u64,
    expires_at: u64,
}

/// A market client with a 10-second fetch cooldown and a 30-second result
/// cache. Identical queries inside the TTL are served from cache; any other
/// query inside the cooldown is refused rather than sent.
pub struct MarketClient {
    cache: HashMap<String, CacheEntry>,
    next_allowed_at: u64,
    clock: Box<dyn Fn() -> u64 + Send>,
}

impl Default for MarketClient {
    fn default() -> Self {
        Self::with_clock(now_ms)
    }
}

impl MarketClient {
    /// A client reading time from `clock` (epoch ms), for tests.
    pub fn with_clock(clock: impl Fn() -> u64 + Send + 'static) -> Self {
        Self { cache: HashMap::new(), next_allowed_at: 0, clock: Box::new(clock) }
    }

    pub fn next_allowed_at(&self) -> u64 {
        self.next_allowed_at
    }

    /// Serve from cache, refuse inside the cooldown, else fetch.
    pub fn search(&mut self, ctx: &SessionContext, query: &MarketQuery) -> Result<MarketOutcome, MarketError> {
        let now = (self.clock)();
        let key = cache_key(ctx, query);
        if let Some(entry) = self.cache.get(&key) {
            if entry.expires_at > now {
                return Ok(MarketOutcome::Ready {
                    result: entry.result.clone(),
                    observed_at: entry.observed_at,
                    cached: true,
                });
            }
            self.cache.remove(&key);
        }
        if now < self.next_allowed_at {
            return Ok(MarketOutcome::Cooldown { next_allowed_at: self.next_allowed_at });
        }
        let result = fetch_market(ctx, query)?;
        self.next_allowed_at = now + MARKET_COOLDOWN_MS;
        self.cache.insert(
            key,
            CacheEntry {
                result: result.clone(),
                observed_at: now,
                expires_at: now + MARKET_CACHE_TTL_MS,
            },
        );
        while self.cache.len() > MARKET_CACHE_MAX {
            match self.cache.keys().next().cloned() {
                Some(oldest) => {
                    self.cache.remove(&oldest);
                }
                None => break,
            }
        }
        Ok(MarketOutcome::Ready { result, observed_at: now, cached: false })
    }
}

/// The cache scope is the account/region pair plus the full filter set, so a
/// new session never serves another session's prices.
fn cache_key(ctx: &SessionContext, query: &MarketQuery) -> String {
    let scope = format!(
        "{}|{}|{}",
        ctx.account_id.as_deref().unwrap_or(""),
        ctx.crossregion_identifier.as_deref().unwrap_or(""),
        ctx.season.as_deref().unwrap_or("")
    );
    let target = match query.target {
        MarketTarget::ItemMask(mask) => format!("m{mask}"),
        MarketTarget::Runeword(id) => format!("r{id}"),
    };
    let stats: Vec<String> =
        query.stat_filters.iter().map(|f| format!("{}>={}", f.stat_id, f.minimum)).collect();
    format!("{scope}|{target}|{:?}|{:?}|{}", query.min_sockets, query.max_sockets, stats.join(","))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketError {
    /// The sniffer has not seen all six session fields yet.
    IncompleteContext(Vec<&'static str>),
    BadTarget(String),
    BadSockets(String),
    BadStat(String),
    UnknownName { name: String, suggestions: Vec<String> },
    Transport(String),
    HttpStatus(u16),
    ServerRejected(String),
    Malformed(String),
}

impl std::fmt::Display for MarketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarketError::IncompleteContext(missing) => write!(
                f,
                "market session context incomplete (missing {}); open the in-game Market once with capture running",
                missing.join(", ")
            ),
            MarketError::BadTarget(why) => write!(f, "bad market target: {why}"),
            MarketError::BadSockets(why) => write!(f, "bad socket filter: {why}"),
            MarketError::BadStat(why) => write!(f, "bad stat filter: {why}"),
            MarketError::UnknownName { name, suggestions } => {
                write!(f, "no item or runeword matches {name:?}")?;
                if !suggestions.is_empty() {
                    write!(f, "; did you mean {}", suggestions.join(", "))?;
                }
                Ok(())
            }
            MarketError::Transport(why) => write!(f, "market unreachable: {why}"),
            MarketError::HttpStatus(status) => write!(f, "market returned HTTP {status}"),
            MarketError::ServerRejected(reason) => write!(f, "market rejected the request ({reason})"),
            MarketError::Malformed(why) => write!(f, "unreadable market response: {why}"),
        }
    }
}

impl std::error::Error for MarketError {}

// ---------------------------------------------------------------------------
// Catalog: name -> target
// ---------------------------------------------------------------------------

/// What the plain-text parser needs to know about the game's items. The bundled
/// implementation is [`MarketCatalogTable`].
pub trait MarketCatalog {
    /// An unambiguous market target for an exact (normalized) name.
    fn target_by_name(&self, name: &str) -> Option<MarketTarget>;
    /// Up to `limit` names containing `needle`, for a "did you mean".
    fn names_like(&self, needle: &str, limit: usize) -> Vec<String>;
    /// A stat id for an exact (normalized) display name.
    fn stat_by_name(&self, name: &str) -> Option<u32>;
    /// Up to `limit` stat names containing `needle`.
    fn stat_names_like(&self, needle: &str, limit: usize) -> Vec<String>;
}

/// The compiled-in catalog, generated from the vendored Companion identity
/// extract by `tools/gen-market-catalog.mjs`.
pub struct MarketCatalogTable {
    items: HashMap<String, MarketTarget>,
    item_names: Vec<String>,
    /// Normalized name -> item type, so a sweep can watch a whole bucket (16 is
    /// the materials/consumables type).
    item_type_of: HashMap<String, u64>,
    runewords: HashMap<String, u32>,
    runeword_names: Vec<String>,
    stats: HashMap<String, u32>,
    stat_names: Vec<String>,
}

impl Default for MarketCatalogTable {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketCatalogTable {
    /// Load the embedded tables.
    pub fn new() -> Self {
        let mut items = HashMap::new();
        let mut item_names = Vec::new();
        let mut item_type_of = HashMap::new();
        for line in include_str!("../data/market-items.tsv").lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 5 {
                continue;
            }
            let (Ok(item_type), Ok(game_id), Ok(weapon_type)) =
                (cols[1].parse::<u64>(), cols[2].parse::<u64>(), cols[3].parse::<u64>())
            else {
                continue;
            };
            if let Ok(mask) = item_mask(cols[0], item_type, game_id, weapon_type) {
                let key = normalize_name(cols[4]);
                // Two items sharing a name (unique vs normal) are dropped
                // rather than guessed; the operator can use `mask:`.
                match items.get(&key) {
                    Some(existing) if existing != &MarketTarget::ItemMask(mask) => {
                        items.remove(&key);
                        item_type_of.remove(&key);
                    }
                    Some(_) => {}
                    None => {
                        items.insert(key.clone(), MarketTarget::ItemMask(mask));
                        item_type_of.insert(key, item_type);
                        item_names.push(cols[4].to_string());
                    }
                }
            }
        }
        let mut runewords = HashMap::new();
        let mut runeword_names = Vec::new();
        for line in include_str!("../data/market-runewords.tsv").lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 2 {
                continue;
            }
            if let Ok(id) = cols[0].parse::<u32>() {
                runewords.insert(normalize_name(cols[1]), id);
                runeword_names.push(cols[1].to_string());
            }
        }
        // A name that is both an item and a runeword prefers the runeword: a
        // player asking for "Grief" means the runeword.
        for name in &runeword_names {
            let key = normalize_name(name);
            if items.remove(&key).is_some() {
                item_names.retain(|n| normalize_name(n) != key);
                item_type_of.remove(&key);
            }
        }
        let mut stats = HashMap::new();
        let mut stat_names = Vec::new();
        for line in include_str!("../data/market-stats.tsv").lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 2 {
                continue;
            }
            if let Ok(id) = cols[0].parse::<u32>() {
                let key = normalize_name(cols[1]);
                stats.entry(key).or_insert(id);
                stat_names.push(cols[1].to_string());
            }
        }
        item_names.sort();
        runeword_names.sort();
        Self { items, item_names, item_type_of, runewords, runeword_names, stats, stat_names }
    }

    /// Names of catalog items in one item type, for building a sweep watchlist.
    /// Type 16 is the materials/consumables bucket; 15 is where runes live.
    pub fn names_of_item_type(&self, item_type: u64) -> Vec<String> {
        self.item_names
            .iter()
            .filter(|name| self.item_type_of.get(&normalize_name(name)) == Some(&item_type))
            .cloned()
            .collect()
    }

    fn runeword_by_name(&self, name: &str) -> Option<u32> {
        self.runewords.get(&normalize_name(name)).copied()
    }

    fn names_matching(haystack: &[String], needle: &str, limit: usize) -> Vec<String> {
        let needle = normalize_name(needle);
        if needle.is_empty() || limit == 0 {
            return vec![];
        }
        let direct: Vec<String> = haystack
            .iter()
            .filter(|name| normalize_name(name).contains(&needle))
            .take(limit)
            .cloned()
            .collect();
        if !direct.is_empty() {
            return direct;
        }
        // No substring hit: fall back to the longest words, so a typo in one
        // word still surfaces "Harlequinn's Crest" for "harlequin crestt".
        let mut words: Vec<&str> = needle.split_whitespace().filter(|w| w.len() >= 4).collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.len()));
        for word in words {
            let hits: Vec<String> = haystack
                .iter()
                .filter(|name| normalize_name(name).contains(word))
                .take(limit)
                .cloned()
                .collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        vec![]
    }
}

impl MarketCatalog for MarketCatalogTable {
    fn target_by_name(&self, name: &str) -> Option<MarketTarget> {
        if let Some(id) = self.runeword_by_name(name) {
            return Some(MarketTarget::Runeword(id));
        }
        self.items.get(&normalize_name(name)).cloned()
    }

    fn names_like(&self, needle: &str, limit: usize) -> Vec<String> {
        let mut out = Self::names_matching(&self.runeword_names, needle, limit);
        out.extend(Self::names_matching(&self.item_names, needle, limit.saturating_sub(out.len())));
        out
    }

    fn stat_by_name(&self, name: &str) -> Option<u32> {
        self.stats.get(&normalize_name(name)).copied()
    }

    fn stat_names_like(&self, needle: &str, limit: usize) -> Vec<String> {
        Self::names_matching(&self.stat_names, needle, limit)
    }
}

/// Fold a display name to a lookup key: case, punctuation and spacing all vary
/// between the game's tables and a player's typing.
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Plain-text parsing
// ---------------------------------------------------------------------------

const LEAD_FILLER: &[&str] =
    &["how", "much", "is", "are", "was", "what", "whats", "what's", "the", "a", "an", "price",
      "prices", "going", "rate", "of", "for", "check", "value", "worth", "cost", "costs",
      "currently", "now", "market", "on", "current", "with"];
const TRAIL_FILLER: &[&str] =
    &["going", "for", "price", "prices", "now", "currently", "today", "worth", "cost", "costs",
      "rate", "with", "that", "has", "having", "and"];
const MIN_WORDS: &[&str] = &["min", "minimum", "at", "least", ">=", ">", "over", "above"];
const MAX_WORDS: &[&str] = &["max", "maximum", "at", "most", "<=", "<", "under", "below", "up", "to"];

/// Turn a plain-text price question into a market query.
///
/// Structured clauses (`mask:123`, `runeword:12`, `sockets:2-4`,
/// `min-sockets:2`, `max-sockets:4`, `<stat> >= 20`, `stat:5>=10`) are pulled
/// out; whatever text is left names the item or runeword.
pub fn parse_market_query(text: &str, catalog: &dyn MarketCatalog) -> Result<MarketQuery, MarketError> {
    let tokens = tokenize(text);
    let mut consumed = vec![false; tokens.len()];
    let mut target: Option<MarketTarget> = None;
    let mut min_sockets = None;
    let mut max_sockets = None;
    let mut stat_filters: Vec<StatFilter> = Vec::new();

    // 1. Explicit `key:value` clauses.
    for (index, token) in tokens.iter().enumerate() {
        let Some((key, value)) = split_clause(token) else { continue };
        match key.as_str() {
            "mask" => {
                let mask = value.parse::<u64>().map_err(|_| MarketError::BadTarget(format!("mask {value:?} is not a number")))?;
                if mask > MASK_MAX {
                    return Err(MarketError::BadTarget(format!("mask {mask} exceeds 32 bits")));
                }
                set_target(&mut target, MarketTarget::ItemMask(mask))?;
                consumed[index] = true;
            }
            "runeword" => {
                let id = match value.parse::<u32>() {
                    Ok(id) => id,
                    Err(_) => catalog
                        .names_like(&value, 1)
                        .first()
                        .and_then(|name| catalog.target_by_name(name))
                        .and_then(|t| match t {
                            MarketTarget::Runeword(id) => Some(id),
                            _ => None,
                        })
                        .ok_or_else(|| MarketError::BadTarget(format!("unknown runeword {value:?}")))?,
                };
                set_target(&mut target, MarketTarget::Runeword(id))?;
                consumed[index] = true;
            }
            "item" => {
                set_target(&mut target, resolve_name(&value, catalog)?)?;
                consumed[index] = true;
            }
            "min-sockets" | "minsockets" | "min_sockets" => {
                min_sockets = Some(parse_socket(&value)?);
                consumed[index] = true;
            }
            "max-sockets" | "maxsockets" | "max_sockets" => {
                max_sockets = Some(parse_socket(&value)?);
                consumed[index] = true;
            }
            "stat" => {
                let (id, minimum) = parse_stat_clause(&value)?;
                push_stat(&mut stat_filters, id, minimum)?;
                consumed[index] = true;
            }
            _ => {}
        }
    }

    // 2. "N sockets" / "N-M sockets", with min/max/exactly qualifiers.
    extract_sockets(&tokens, &mut consumed, &mut min_sockets, &mut max_sockets)?;

    // 3. Named stat minimums: "<stat> >= 20", "min <stat> 20".
    extract_stats(&tokens, &mut consumed, catalog, &mut stat_filters)?;

    // 4. Whatever is left names the target.
    if target.is_none() {
        let phrase = collect_unconsumed(&tokens, &consumed);
        let name = strip_filler(&phrase, catalog)
            .ok_or_else(|| MarketError::UnknownName { name: phrase.clone(), suggestions: catalog.names_like(&phrase, 5) })?;
        target = Some(resolve_name(&name, catalog)?);
    }

    let target = target.expect("target resolved or error returned");
    Ok(MarketQuery { target, min_sockets, max_sockets, stat_filters })
}

fn set_target(slot: &mut Option<MarketTarget>, target: MarketTarget) -> Result<(), MarketError> {
    if let Some(existing) = slot {
        if existing != &target {
            return Err(MarketError::BadTarget("more than one item target given".into()));
        }
    }
    *slot = Some(target);
    Ok(())
}

fn resolve_name(name: &str, catalog: &dyn MarketCatalog) -> Result<MarketTarget, MarketError> {
    catalog.target_by_name(name).ok_or_else(|| MarketError::UnknownName {
        name: name.to_string(),
        suggestions: catalog.names_like(name, 5),
    })
}

fn push_stat(filters: &mut Vec<StatFilter>, stat_id: u32, minimum: i64) -> Result<(), MarketError> {
    if filters.iter().any(|f| f.stat_id == stat_id) {
        return Err(MarketError::BadStat(format!("stat {stat_id} given twice")));
    }
    filters.push(StatFilter { stat_id, minimum });
    Ok(())
}

fn split_clause(token: &str) -> Option<(String, String)> {
    let (key, value) = token.split_once([':', '='])?;
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key.to_ascii_lowercase(), value.to_string()))
}

fn parse_socket(value: &str) -> Result<u8, MarketError> {
    let n: u8 = value.parse().map_err(|_| MarketError::BadSockets(format!("{value:?} is not a count")))?;
    if n > 6 {
        return Err(MarketError::BadSockets(format!("{n} sockets exceeds the 0-6 range")));
    }
    Ok(n)
}

/// `stat:5>=10`, `stat:5:10`, `stat:5-10` — the id then the minimum.
fn parse_stat_clause(value: &str) -> Result<(u32, i64), MarketError> {
    let split = value
        .split_once(">=")
        .or_else(|| value.split_once(':'))
        .or_else(|| value.split_once('='));
    let (id, minimum) = match split {
        Some((id, minimum)) => (id, minimum),
        None => (value, "0"),
    };
    let id: u32 = id.parse().map_err(|_| MarketError::BadStat(format!("{id:?} is not a stat id")))?;
    let minimum: i64 = minimum
        .trim_start_matches('=')
        .parse()
        .map_err(|_| MarketError::BadStat(format!("{minimum:?} is not a stat minimum")))?;
    Ok((id, minimum))
}

fn extract_sockets(
    tokens: &[String],
    consumed: &mut [bool],
    min_sockets: &mut Option<u8>,
    max_sockets: &mut Option<u8>,
) -> Result<(), MarketError> {
    for (index, token) in tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        if !lower.starts_with("socket") || consumed[index] {
            continue;
        }
        // The count is in the token before ("2 sockets") or inside it
        // ("sockets:2", "sockets=2").
        let mut count = None;
        if let Some((key, value)) = split_clause(token) {
            if key.starts_with("socket") {
                count = Some(value);
            }
        } else if index > 0 {
            let previous = tokens[index - 1].to_ascii_lowercase();
            if is_number(&previous) || previous.contains('-') && previous.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                count = Some(tokens[index - 1].clone());
                consumed[index - 1] = true;
            } else if MIN_WORDS.contains(&previous.as_str()) || MAX_WORDS.contains(&previous.as_str()) {
                // "min 2 sockets": the qualifier is two back.
                if index >= 2 && is_number(&tokens[index - 2]) {
                    count = Some(tokens[index - 2].clone());
                    consumed[index - 2] = true;
                }
            }
        }
        consumed[index] = true;
        let Some(count) = count else {
            return Err(MarketError::BadSockets("socket clue has no count (try `sockets:2-4`)".into()));
        };
        let qualifier = if index > 0 { tokens[index - 1].to_ascii_lowercase() } else { String::new() };
        let exactly = qualifier == "exactly" || qualifier == "exact";
        if let Some((lo, hi)) = count.split_once('-') {
            *min_sockets = Some(parse_socket(lo)?);
            *max_sockets = Some(parse_socket(hi)?);
        } else if MAX_WORDS.contains(&qualifier.as_str()) {
            *max_sockets = Some(parse_socket(&count)?);
        } else if exactly {
            *min_sockets = Some(parse_socket(&count)?);
            *max_sockets = Some(parse_socket(&count)?);
        } else {
            *min_sockets = Some(parse_socket(&count)?);
        }
    }
    Ok(())
}

fn extract_stats(
    tokens: &[String],
    consumed: &mut [bool],
    catalog: &dyn MarketCatalog,
    filters: &mut Vec<StatFilter>,
) -> Result<(), MarketError> {
    for index in 0..tokens.len() {
        if consumed[index] {
            continue;
        }
        let lower = tokens[index].to_ascii_lowercase();
        if !MIN_WORDS.contains(&lower.as_str()) {
            continue;
        }
        // Find the minimum: a number after the operator that no earlier clause
        // already claimed (a consumed number belongs to a socket clause).
        let Some((number_index, value)) = (index + 1..tokens.len())
            .find_map(|i| (!consumed[i]).then(|| number_at(&tokens[i]).map(|n| (i, n))).flatten())
        else {
            continue;
        };
        // The stat name is the longest recognized phrase ending just before the
        // operator, or after it when the number comes first ("magic find >= 20").
        let Some((name, name_range)) = best_stat_name(tokens, index, number_index, catalog) else {
            // Not a stat clause after all ("min 2 sockets" reaches here if the
            // socket extractor did not, and is simply left alone).
            continue;
        };
        let stat_id = catalog
            .stat_by_name(&name)
            .ok_or_else(|| MarketError::BadStat(format!("unknown stat {name:?}")))?;
        push_stat(filters, stat_id, value)?;
        for i in name_range {
            consumed[i] = true;
        }
        for i in index..=number_index {
            consumed[i] = true;
        }
    }
    Ok(())
}

/// The longest catalog-recognized run of words adjacent to the operator. If the
/// number precedes the name ("20 magic find"), the run after it is used; else
/// the run before it.
fn best_stat_name(
    tokens: &[String],
    operator: usize,
    number_index: usize,
    catalog: &dyn MarketCatalog,
) -> Option<(String, std::ops::Range<usize>)> {
    // Name before the operator: "magic find >= 20".
    for width in (1..=4).rev() {
        if operator < width {
            continue;
        }
        let start = operator - width;
        if (start..operator).any(|i| tokens[i].is_empty()) {
            continue;
        }
        let phrase = tokens[start..operator].join(" ");
        if catalog.stat_by_name(&phrase).is_some() {
            return Some((phrase, start..operator));
        }
    }
    // Name after the number: "20 magic find" / "min 20 magic find".
    for width in (1..=4).rev() {
        if number_index + width >= tokens.len() {
            continue;
        }
        let end = number_index + width;
        let phrase = tokens[number_index + 1..=end].join(" ");
        if catalog.stat_by_name(&phrase).is_some() {
            return Some((phrase, number_index + 1..end + 1));
        }
    }
    None
}

fn number_at(token: &str) -> Option<i64> {
    let cleaned = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '-');
    let digits = cleaned.trim_start_matches('-');
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    cleaned.parse().ok()
}

fn is_number(token: &str) -> bool {
    number_at(token).is_some()
}

/// Whole-phrase name resolution, then trimming filler from either end until
/// the catalog recognizes what is left.
fn strip_filler(phrase: &str, catalog: &dyn MarketCatalog) -> Option<String> {
    if catalog.target_by_name(phrase).is_some() {
        return Some(phrase.to_string());
    }
    let mut words: Vec<String> = phrase.split_whitespace().map(str::to_string).collect();
    while !words.is_empty() {
        let joined = words.join(" ");
        if catalog.target_by_name(&joined).is_some() {
            return Some(joined);
        }
        let first = words[0].to_ascii_lowercase();
        let last = words[words.len() - 1].to_ascii_lowercase();
        if LEAD_FILLER.contains(&first.as_str()) {
            words.remove(0);
        } else if TRAIL_FILLER.contains(&last.as_str()) {
            words.pop();
        } else {
            break;
        }
    }
    None
}

fn collect_unconsumed(tokens: &[String], consumed: &[bool]) -> String {
    tokens
        .iter()
        .zip(consumed)
        .filter(|(_, consumed)| !**consumed)
        .map(|(token, _)| token.trim_matches(|c: char| c == '?' || c == '.' || c == ',').to_string())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split on whitespace, dropping a trailing `?`/`.`/`,` but keeping `>=`, `-`
/// ranges and `key:value` intact.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|raw| raw.trim_matches(|c: char| c == '?' || c == '!' || c == '.' || c == ',').to_string())
        .filter(|token| !token.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SessionContext {
        SessionContext {
            account_id: Some("eu-12345".into()),
            unique_account_id: Some("99-777".into()),
            crossregion_identifier: Some("eu-1".into()),
            season: Some("10".into()),
            hardcore: Some("0".into()),
            beta: Some("0".into()),
            ..Default::default()
        }
    }

    #[test]
    fn derived_checksum_wins_over_the_native_one() {
        let ctx = SessionContext {
            account_id: Some("3-2304".into()),
            unique_account_id: Some("180729201".into()),
            crossregion_identifier: Some("7695739231".into()),
            season: Some("11".into()),
            hardcore: Some("0".into()),
            beta: Some("0".into()),
            identifier: Some("VsJbIajeBw93248X8udkEU4LTjAiprkJ".into()),
            native_checksum: Some("e2137c7d2b54197cdda2f40c9cd7c2500d3a6ab5058f58a2e501570f02f167a7".into()),
        };
        assert!(ctx.can_sign());
        let body = build_market_body(&ctx, &MarketQuery::item_mask(0)).unwrap();
        let form: HashMap<_, _> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        assert_eq!(form["checksum"], sha256_hex(b"2304qmR9xbft3ZPvE3Ab1100"));
        assert_eq!(form["account_id"], "3-2304");
        assert_eq!(form["identifier"], "VsJbIajeBw93248X8udkEU4LTjAiprkJ");
        // The session token must not leak through JSON.
        assert!(!ctx.to_json().to_string().contains("VsJbIajeBw"));
    }

    #[test]
    fn cache_hit_is_served_without_a_fetch() {
        let mut client = MarketClient::with_clock(|| 1_000);
        let query = MarketQuery::item_mask(7);
        client.cache.insert(
            cache_key(&ctx(), &query),
            CacheEntry {
                result: MarketResult { listings: vec![], total_matches: None, returned_count: 0 },
                observed_at: 1_000,
                expires_at: 2_000,
            },
        );
        match client.search(&ctx(), &query).unwrap() {
            MarketOutcome::Ready { cached, .. } => assert!(cached),
            other => panic!("expected a cached hit, got {other:?}"),
        }
    }

    #[test]
    fn cooldown_refuses_a_different_query() {
        let mut client = MarketClient::with_clock(|| 1_000);
        client.next_allowed_at = 5_000;
        assert_eq!(
            client.search(&ctx(), &MarketQuery::item_mask(7)),
            Ok(MarketOutcome::Cooldown { next_allowed_at: 5_000 })
        );
    }

    #[test]
    fn cache_key_distinguishes_filters_and_scope() {
        let base = MarketQuery::item_mask(7);
        let mut sockets = base.clone();
        sockets.min_sockets = Some(2);
        assert_ne!(cache_key(&ctx(), &base), cache_key(&ctx(), &sockets));
        let mut other = ctx();
        other.season = Some("99".into());
        assert_ne!(cache_key(&ctx(), &base), cache_key(&other, &base));
    }

    #[test]
    fn checksum_uses_the_raw_account_suffix() {
        // Recompute independently: sha256("12345" + salt + "10" + "0" + "0").
        let expected = sha256_hex(format!("12345{CHECKSUM_SALT}1000").as_bytes());
        assert_eq!(ctx().checksum().as_deref(), Some(expected.as_str()));
        assert_eq!(ctx().raw_account_id(), Some("12345"));
    }

    #[test]
    fn missing_fields_are_reported() {
        let mut partial = SessionContext::default();
        partial.account_id = Some("1".into());
        assert_eq!(
            partial.missing(),
            vec!["unique_account_id", "crossregion_identifier", "season", "hardcore", "beta"]
        );
        assert!(build_market_body(&partial, &MarketQuery::item_mask(0)).is_err());
    }

    #[test]
    fn body_carries_the_signed_fields() {
        let body = build_market_body(&ctx(), &MarketQuery::item_mask(1 << 30)).unwrap();
        let form: HashMap<_, _> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        assert_eq!(form["account_id"], "eu-12345");
        assert_eq!(form["season"], "10");
        assert_eq!(form["api_script"], FETCH_ITEMS_API_SCRIPT);
        assert_eq!(form["filter_masks"], format!("[{}]", 1u64 << 30));
        assert!(form["checksum"].len() == 64);
        assert_eq!(form["multipass"], market_multipass());
        assert!(!form.contains_key("filter_runeword"));
    }

    #[test]
    fn runeword_body_uses_the_selector_not_a_mask() {
        let query = MarketQuery {
            target: MarketTarget::Runeword(7),
            min_sockets: Some(2),
            max_sockets: Some(4),
            stat_filters: vec![StatFilter { stat_id: 284, minimum: 20 }],
        };
        let body = build_market_body(&ctx(), &query).unwrap();
        let form: HashMap<_, _> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
        assert!(
            form.get("filter_masks").is_none(),
            "an empty filter_masks is an HTTP 500; the key must be absent"
        );
        assert_eq!(form["filter_runeword"], "7");
        assert_eq!(form["filter_sockets_min"], "2");
        assert_eq!(form["filter_sockets_max"], "4");
        let clauses: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD.decode(form["stat_filter"].as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!(clauses, json!([{"statId": 284, "filter": 2, "statValue": 20}]));
    }

    #[test]
    fn item_mask_packing_matches_the_client() {
        // Harlequinn's Crest: unique helmet, type 0, id 0, weaponType 0.
        assert_eq!(item_mask("unique", 0, 0, 0).unwrap(), 1 << 30);
        assert_eq!(item_mask("normal", 0, 0, 0).unwrap(), 0);
        // Pearlescento Jewel: normal, type 15, id 94, weaponType 0.
        assert_eq!(item_mask("normal", 15, 94, 0).unwrap(), 15 * (1 << 12) + 94);
    }

    fn response(listings: Value, item_count: u64) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use std::io::Write as _;
        let raw = serde_json::to_vec(&listings).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        let deflated = encoder.finish().unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(deflated);
        json!({"status": 1, "items": encoded, "itemCount": item_count}).to_string().into_bytes()
    }

    #[test]
    fn parses_listings_and_bands_without_currency_mixing() {
        let body = response(
            json!([
                {"price": 500, "price_items": []},
                {"price": 100, "unit_price": "12.5"},
                {"price": 900, "price_items": [{"id": 1}]}, // item-priced: dropped
                {"price": -1},                              // malformed: dropped
            ]),
            42,
        );
        let result = parse_market_response(&body, 200).unwrap();
        assert_eq!(result.listings.len(), 2);
        assert_eq!(result.returned_count, 4);
        assert_eq!(result.total_matches, Some(42));
        let band = result.band().unwrap();
        // Per unit: row 0 is a single unit at 500, row 1 is 8 units at 12.5.
        assert_eq!(band.currency, "gold");
        assert_eq!(band.count, 2);
        assert_eq!((band.lowest, band.median, band.highest), (13, 500, 500));
        assert_eq!(result.listings[1].unit_price, Some(12.5));
        assert_eq!(result.listings[1].units, 8);
        assert_eq!(result.listings[1].unit_price_gold(), 13);
        assert_eq!(result.listings[0].units, 1);
    }

    #[test]
    fn server_rejection_is_classified_without_echoing_text() {
        let body = json!({"status": 0, "message": "Bad checksum for account eu-1"}).to_string();
        assert_eq!(
            parse_market_response(body.as_bytes(), 200),
            Err(MarketError::ServerRejected("checksum".into()))
        );
        assert_eq!(parse_market_response(b"", 403), Err(MarketError::HttpStatus(403)));
    }

    #[test]
    fn natural_language_finds_the_item_and_its_sockets() {
        let catalog = MarketCatalogTable::new();
        let query = parse_market_query(
            "how much is a 2 socket Harlequinn's Crest going for?",
            &catalog,
        )
        .unwrap();
        assert_eq!(query.target, MarketTarget::ItemMask(1 << 30));
        assert_eq!(query.min_sockets, Some(2));
        assert_eq!(query.max_sockets, None);
    }

    #[test]
    fn natural_language_handles_a_leading_the() {
        let catalog = MarketCatalogTable::new();
        let query = parse_market_query("price of The Bucket", &catalog).unwrap();
        assert_eq!(query.target, MarketTarget::ItemMask((1 << 30) + 3));
    }

    #[test]
    fn natural_language_matches_a_runeword_by_name() {
        let catalog = MarketCatalogTable::new();
        let query = parse_market_query("what is Grief going for", &catalog).unwrap();
        match query.target {
            MarketTarget::Runeword(id) => assert!(id > 0 && id <= 100),
            other => panic!("expected a runeword, got {other:?}"),
        }
    }

    #[test]
    fn natural_language_reads_a_named_stat_minimum() {
        let catalog = MarketCatalogTable::new();
        let query = parse_market_query("Harlequinn's Crest with magic find >= 20", &catalog).unwrap();
        assert_eq!(query.target, MarketTarget::ItemMask(1 << 30));
        assert_eq!(query.stat_filters, vec![StatFilter { stat_id: 284, minimum: 20 }]);
    }

    #[test]
    fn explicit_mask_works_without_a_name() {
        let catalog = MarketCatalogTable::new();
        let query = parse_market_query("mask:1234 sockets:2-4", &catalog).unwrap();
        assert_eq!(query.target, MarketTarget::ItemMask(1234));
        assert_eq!((query.min_sockets, query.max_sockets), (Some(2), Some(4)));
    }

    #[test]
    fn unknown_names_suggest_near_misses() {
        let catalog = MarketCatalogTable::new();
        let error = parse_market_query("harlequin crestt", &catalog).unwrap_err();
        match error {
            MarketError::UnknownName { suggestions, .. } => assert!(!suggestions.is_empty()),
            other => panic!("expected UnknownName, got {other:?}"),
        }
    }
}

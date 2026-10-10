//! hs-sniff CLI.
//!
//! ```text
//! hs-sniff watch              live capture, streaming events + rolling summary
//! hs-sniff replay FILE.pcap   run the pipeline over a capture file
//! hs-sniff serve              MCP stdio server
//! hs-sniff sniff-stdin        read a raw TCP payload (or pcap) from stdin and print events
//! ```

use std::io::Read;
use std::sync::Arc;

use hs_sniff::events::{events_from_messages, GameEvent, Summary};
use hs_sniff::market::{self, MarketCatalog, MarketCatalogTable};
use hs_sniff::sweep;
use hs_sniff::mcp::SharedState;
use hs_sniff::reasm::Reassembler;
use hs_sniff::wire::{parse_pcap_bytes, ReplaySource};
use hs_sniff::{decode, mcp};

mod itemtable;

struct Options {
    cmd: String,
    file: Option<String>,
    query: Option<String>,
    limit: u64,
    table: Option<String>,
    log: Option<String>,
    /// `market`: capture live to collect the session context.
    live: bool,
    /// `market --live`: how long to wait for a complete context, in seconds.
    timeout: u64,
    json: bool,
    /// `sweep`: directory holding runes.json / runewords.json.
    data_dir: Option<String>,
    /// `sweep`: milliseconds between market fetches.
    delay: u64,
    /// `sweep`: watchlist selectors.
    runes: bool,
    runewords: bool,
    materials: bool,
    names: Vec<String>,
    masks: Vec<u64>,
    /// `sweep`: JSONL corpus of observed listings.
    observations: Option<String>,
    /// `sweep`: skip recording and diffing altogether.
    no_observations: bool,
}

fn parse_args(args: &[String]) -> Options {
    let mut opts = Options {
        cmd: "help".into(),
        file: None,
        query: None,
        limit: u64::MAX,
        table: None,
        log: None,
        live: false,
        timeout: 30,
        json: false,
        data_dir: None,
        delay: 1000,
        runes: false,
        runewords: false,
        materials: false,
        names: Vec::new(),
        masks: Vec::new(),
        observations: None,
        no_observations: false,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "watch" | "replay" | "serve" | "sniff-stdin" | "market" | "context" | "sweep" => {
                opts.cmd = args[i].clone()
            }
            "help" | "--help" | "-h" => opts.cmd = "help".into(),
            "--live" => opts.live = true,
            "--json" => opts.json = true,
            "--runes" => opts.runes = true,
            "--runewords" => opts.runewords = true,
            "--materials" => opts.materials = true,
            "--name" if i + 1 < args.len() => {
                opts.names.push(args[i + 1].clone());
                i += 1;
            }
            "--mask" if i + 1 < args.len() => {
                if let Ok(mask) = args[i + 1].parse::<u64>() {
                    opts.masks.push(mask);
                }
                i += 1;
            }
            "--data-dir" if i + 1 < args.len() => {
                opts.data_dir = Some(args[i + 1].clone());
                i += 1;
            }
            "--delay" if i + 1 < args.len() => {
                opts.delay = args[i + 1].parse().unwrap_or(1000);
                i += 1;
            }
            "--observations" if i + 1 < args.len() => {
                opts.observations = Some(args[i + 1].clone());
                i += 1;
            }
            "--no-observations" => opts.no_observations = true,
            "--table" if i + 1 < args.len() => {
                opts.table = Some(args[i + 1].clone());
                i += 1;
            }
            "--log" if i + 1 < args.len() => {
                opts.log = Some(args[i + 1].clone());
                i += 1;
            }
            "--replay" if i + 1 < args.len() => {
                opts.file = Some(args[i + 1].clone());
                i += 1;
            }
            "--timeout" if i + 1 < args.len() => {
                opts.timeout = args[i + 1].parse().unwrap_or(30);
                i += 1;
            }
            "--limit" if i + 1 < args.len() => {
                opts.limit = args[i + 1].parse().unwrap_or(u64::MAX);
                i += 1;
            }
            other if !other.starts_with('-') => positional.push(other.to_string()),
            _ => {}
        }
        i += 1;
    }
    match opts.cmd.as_str() {
        "replay" if opts.file.is_none() => opts.file = positional.first().cloned(),
        "market" => opts.query = Some(positional.join(" ")).filter(|q| !q.trim().is_empty()),
        _ => {}
    }
    opts
}

fn print_event(ev: &GameEvent) {
    println!("{}", ev.to_json());
}

fn print_summary(s: &Summary) {
    println!(
        "--- gold Δ {} | xp {} | items {} ({:?}) | zone {:?} | room {:?} | char {:?} lvl {} kills {} ---",
        s.gold_delta,
        s.xp_gain,
        s.items_seen,
        s.items_by_rarity,
        s.satanic_zone,
        s.room,
        s.character,
        s.level,
        s.kills
    );
}

/// Run the whole pipeline over raw frame payloads and print/ingest events.
fn run_payloads<'a>(
    payloads: impl Iterator<Item = (Option<std::net::IpAddr>, &'a [u8])>,
    shared: Option<&Arc<SharedState>>,
    limit: u64,
) -> Summary {
    let mut asm = Reassembler::default();
    let mut printed = 0usize;
    let mut totals = Summary::default();
    let mut idle = std::time::Instant::now();

    let feed = |flushed: &[u8], totals: &mut Summary, printed: &mut usize| {
        for m in decode::extract_messages(flushed) {
            for ev in events_from_messages(&[m]) {
                totals.observe(&ev);
                if let Some(s) = shared {
                    s.ingest(&ev);
                }
                if (*printed as u64) < limit {
                    print_event(&ev);
                }
                *printed += 1;
            }
        }
    };
    for (src, data) in payloads {
        if let (Some(src), Some((sp, dp, ack))) = (src, tcp_parts(data)) {
            if let Some(flushed) = asm.push((src, sp, dp), ack, data) {
                feed(&flushed, &mut totals, &mut printed);
            }
        } else {
            // no TCP/IP framing (raw flush log): treat as a complete flush
            feed(data, &mut totals, &mut printed);
        }
        if idle.elapsed() >= std::time::Duration::from_millis(250) {
            idle = std::time::Instant::now();
            for (_src, flushed) in asm.drain_idle() {
                feed(&flushed, &mut totals, &mut printed);
            }
        }
    }
    // end of input: there is no "later" to wait for
    for (_src, flushed) in asm.drain_all() {
        feed(&flushed, &mut totals, &mut printed);
    }
    if limit > 0 {
        print_summary(&totals);
    }
    totals
}

/// Decode a whole capture file into a session summary, printing nothing.
fn summary_from_file(path: &str) -> Result<Summary, String> {
    let source = ReplaySource::from_path(path).map_err(|e| e.to_string())?;
    let summary = match source {
        ReplaySource::Pcap(frames) => {
            run_payloads(frames.iter().map(|f| (f.src, f.data.as_slice())), None, 0)
        }
        ReplaySource::Jsonl(rows) => {
            run_payloads(rows.iter().map(|(src, data)| (*src, data.as_slice())), None, 0)
        }
    };
    Ok(summary)
}

/// Where the market session context comes from: a capture file, or a bounded
/// live capture. Never a hand-typed credential.
fn acquire_session(opts: &Options) -> Summary {
    if let Some(path) = &opts.file {
        return summary_from_file(path)
            .unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
    }
    if opts.live {
        #[cfg(feature = "live")]
        {
            return session_from_live(opts.timeout).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
        }
        #[cfg(not(feature = "live"))]
        {
            eprintln!("this build has no live capture (rebuild with --features live)");
            std::process::exit(1);
        }
    }
    eprintln!("a market query needs a session-context source:");
    eprintln!("  --replay FILE   read the six fields out of a capture file");
    eprintln!("  --live          capture live until they are complete");
    std::process::exit(2);
}

#[cfg(feature = "live")]
fn session_from_live(timeout_secs: u64) -> Result<Summary, String> {
    let live = hs_sniff::capture::LiveCapture::start()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut summary = Summary::default();
    let total = market::SESSION_FIELDS.len();
    let mut reported = 0usize;
    eprintln!(
        "capturing for up to {timeout_secs}s — open the in-game Market once so the session context can be collected"
    );
    while std::time::Instant::now() < deadline {
        if let Some(event) = live.try_recv() {
            summary.observe(&event);
            if summary.session.is_complete() {
                eprintln!("session context ready — querying the market");
                break;
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let have = total - summary.session.missing().len();
        if have > reported {
            reported = have;
            eprintln!("  captured {have}/{total} fields (missing: {})", summary.session.missing().join(", "));
        }
    }
    Ok(summary)
}

fn describe_query(query: &market::MarketQuery) -> String {
    let mut parts = vec![match query.target {
        market::MarketTarget::ItemMask(mask) => format!("item mask {mask}"),
        market::MarketTarget::Runeword(id) => format!("runeword {id}"),
    }];
    match (query.min_sockets, query.max_sockets) {
        (Some(min), Some(max)) if min == max => parts.push(format!("{min} sockets")),
        (Some(min), Some(max)) => parts.push(format!("{min}-{max} sockets")),
        (Some(min), None) => parts.push(format!("at least {min} sockets")),
        (None, Some(max)) => parts.push(format!("at most {max} sockets")),
        (None, None) => {}
    }
    for filter in &query.stat_filters {
        parts.push(format!("stat {} >= {}", filter.stat_id, filter.minimum));
    }
    parts.join(", ")
}

fn print_market_response(query: &market::MarketQuery, outcome: &market::MarketOutcome, json: bool) {
    if json {
        let value = serde_json::json!({"query": query.to_json(), "result": outcome.to_json()});
        println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
        return;
    }
    eprintln!("query: {}", describe_query(query));
    match outcome {
        market::MarketOutcome::Cooldown { next_allowed_at } => {
            println!("market cooldown — next fetch allowed at {next_allowed_at} (epoch ms)");
        }
        market::MarketOutcome::Ready { result, observed_at, cached } => {
            if *cached {
                eprintln!("(cached result, observed {observed_at})");
            }
            print_market_result(result);
        }
    }
}

fn print_market_result(result: &market::MarketResult) {
    match result.band() {
        None => println!("no gold listings on this page ({} row(s) decoded)", result.returned_count),
        Some(band) => {
            if let Some(total) = result.total_matches {
                println!("gold market — {total} matching, showing {} on this page", band.count);
            } else {
                println!("gold market — {} listing(s) on this page", band.count);
            }
            println!("  lowest {}  median {}  highest {}  (gold)", band.lowest, band.median, band.highest);
            for row in result.by_price().iter().take(10) {
                match row.unit_price {
                    Some(unit) => println!("  {} gold  ({} / unit)", row.price, unit),
                    None => println!("  {} gold", row.price),
                }
            }
        }
    }
}

/// (src_port, dst_port, ack) out of a stripped IPv4/IPv6+TCP frame.
fn tcp_parts(ip: &[u8]) -> Option<(u16, u16, u32)> {
    let ihl = match ip.first()? >> 4 {
        4 => ((*ip.first()? & 0x0f) as usize) * 4,
        6 => 40,
        _ => return None,
    };
    if ip.len() < ihl + 20 {
        return None;
    }
    let t = &ip[ihl..];
    let sp = u16::from_be_bytes([t[0], t[1]]);
    let dp = u16::from_be_bytes([t[2], t[3]]);
    let ack = u32::from_be_bytes([t[8], t[9], t[10], t[11]]);
    Some((sp, dp, ack))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = parse_args(&args);

    if opts.cmd == "help" {
        print_help();
        return;
    }

    // Install the item-name table when the user supplied one.
    if let Some(path) = &opts.table {
        match itemtable::load_from_file(path) {
            Ok(n) => eprintln!("loaded {n} item names from {path}"),
            Err(e) => eprintln!("warning: could not load item table {path}: {e}"),
        }
    }

    match opts.cmd.as_str() {
        "replay" => {
            let Some(path) = opts.file else {
                eprintln!("replay needs a file: hs-sniff replay capture.pcap");
                std::process::exit(2);
            };
            match ReplaySource::from_path(&path) {
                Ok(ReplaySource::Pcap(frames)) => {
                    eprintln!("{} frames from {path}", frames.len());
                    let payloads = frames.iter().map(|f| (f.src, f.data.as_slice()));
                    run_payloads(payloads, None, opts.limit);
                }
                Ok(ReplaySource::Jsonl(rows)) => {
                    eprintln!("{} rows from {path}", rows.len());
                    let payloads = rows.iter().map(|(src, data)| (*src, data.as_slice()));
                    run_payloads(payloads, None, opts.limit);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "sniff-stdin" => {
            let mut bytes = Vec::new();
            if std::io::stdin().read_to_end(&mut bytes).is_err() {
                eprintln!("error: could not read stdin");
                std::process::exit(1);
            }
            if parse_pcap_bytes(&bytes).is_ok() {
                let frames = parse_pcap_bytes(&bytes).unwrap();
                let payloads = frames.iter().map(|f| (f.src, f.data.as_slice()));
                run_payloads(payloads, None, opts.limit);
            } else {
                run_payloads(std::iter::once((None, bytes.as_slice())), None, opts.limit);
            }
        }
        "context" => {
            let session = acquire_session(&opts).session;
            println!("{}", serde_json::to_string_pretty(&session.to_json()).unwrap_or_default());
            if !session.can_sign() {
                eprintln!(
                    "open the in-game Market once with capture running to fill the missing fields"
                );
            }
        }
        "market" => {
            let Some(text) = opts.query.clone() else {
                eprintln!("market needs a query: hs-sniff market \"2 socket Harlequinn's Crest\"");
                std::process::exit(2);
            };
            let session = acquire_session(&opts).session;
            if !session.can_sign() {
                eprintln!("market session context incomplete: missing {}", session.missing().join(", "));
                eprintln!("open the in-game Market once with capture running, then retry");
                std::process::exit(3);
            }
            let catalog = MarketCatalogTable::new();
            let query = match market::parse_market_query(&text, &catalog) {
                Ok(query) => query,
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(2);
                }
            };
            match market::MarketClient::default().search(&session, &query) {
                Ok(outcome) => print_market_response(&query, &outcome, opts.json),
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(1);
                }
            }
        }
        "sweep" => run_sweep(&opts),
        "watch" => {
            #[cfg(feature = "live")]
            {
                watch(opts.log);
            }
            #[cfg(not(feature = "live"))]
            {
                eprintln!("this build has no live capture (rebuild with --features live)");
                std::process::exit(1);
            }
        }
        "serve" => {
            let shared = Arc::new(SharedState::default());
            #[cfg(feature = "live")]
            {
                // An optional live capture in the background; MCP tools work
                // with or without it (hs_replay feeds the same buffers).
                if std::env::var("HS_SNIFF_LIVE").as_deref() == Ok("1") {
                    match hs_sniff::capture::LiveCapture::start() {
                        Ok(live) => {
                            let shared2 = shared.clone();
                            std::thread::spawn(move || loop {
                                match live.try_recv() {
                                    Some(ev) => shared2.ingest(&ev),
                                    None => std::thread::sleep(std::time::Duration::from_millis(50)),
                                }
                            });
                            // the thread owns the capture for the process lifetime
                        }
                        Err(e) => eprintln!("live capture unavailable: {e}"),
                    }
                }
            }
            mcp::serve(shared, String::new);
        }
        _ => print_help(),
    }
}

#[cfg(feature = "live")]
fn watch(log_path: Option<String>) {
    use std::time::Duration;
    let live = match hs_sniff::capture::LiveCapture::start_with_log(log_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("capture error: {e}");
            eprintln!("Linux: run with cap_net_raw (setcap 'cap_net_raw+eip' on the binary) or as root.");
            eprintln!("Windows: install Npcap with WinPcap API-compatible Mode.");
            std::process::exit(1);
        }
    };
    eprintln!("capturing — Ctrl-C to stop");
    let mut totals = Summary::default();
    let mut last = std::time::Instant::now();
    loop {
        match live.try_recv() {
            Some(ev) => {
                totals.observe(&ev);
                print_event(&ev);
            }
            None => {
                if last.elapsed() >= Duration::from_secs(10) {
                    last = std::time::Instant::now();
                    let st = live.status();
                    print_summary(&totals);
                    eprintln!(
                        "[{}] packets {} msgs {} hosts {} — {}",
                        st.interfaces.join(","),
                        st.packets_seen,
                        st.messages_decoded,
                        st.hosts,
                        st.note
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sweep
// ---------------------------------------------------------------------------

/// Watchlist data read from the repo's `data/` directory.
struct SweepData {
    rune_names: Vec<String>,
    runeword_names: Vec<String>,
    recipes: Vec<sweep::Recipe>,
}

fn json_names(json: &str) -> Vec<String> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let rows = match &value {
        serde_json::Value::Array(rows) => rows.clone(),
        serde_json::Value::Object(map) => map
            .values()
            .find_map(|value| value.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    rows.iter()
        .filter_map(|row| row.get("name").and_then(|name| name.as_str()).map(str::to_string))
        .collect()
}

fn load_sweep_data(dir: &str) -> Result<SweepData, String> {
    let runes_path = std::path::Path::new(dir).join("runes.json");
    let runewords_path = std::path::Path::new(dir).join("runewords.json");
    let runes =
        std::fs::read_to_string(&runes_path).map_err(|e| format!("{}: {e}", runes_path.display()))?;
    let runewords = std::fs::read_to_string(&runewords_path)
        .map_err(|e| format!("{}: {e}", runewords_path.display()))?;
    let recipes = sweep::recipes_from_json(&runewords, &runes)?;
    Ok(SweepData {
        rune_names: json_names(&runes),
        runeword_names: json_names(&runewords),
        recipes,
    })
}

fn run_sweep(opts: &Options) {
    let session = acquire_session(opts).session;
    if !session.can_sign() {
        eprintln!("sweep session context incomplete: missing {}", session.missing().join(", "));
        eprintln!("open the in-game Market once with capture running, then retry");
        std::process::exit(3);
    }

    let data_dir = opts.data_dir.clone().unwrap_or_else(|| "../data".to_string());
    let data = match load_sweep_data(&data_dir) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("--data-dir must hold runes.json and runewords.json (the repo's data/)");
            std::process::exit(2);
        }
    };

    let catalog = MarketCatalogTable::new();
    let default_watchlist = !opts.runes
        && !opts.runewords
        && !opts.materials
        && opts.names.is_empty()
        && opts.masks.is_empty();

    let mut wanted: Vec<String> = opts.names.clone();
    if opts.runes || default_watchlist {
        wanted.extend(data.rune_names.iter().cloned());
    }
    if opts.runewords || default_watchlist {
        wanted.extend(data.runeword_names.iter().cloned());
    }
    if opts.materials {
        wanted.extend(catalog.names_of_item_type(16));
    }

    let mut targets: Vec<(String, market::MarketTarget)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in wanted {
        if !seen.insert(name.clone()) {
            continue;
        }
        match catalog.target_by_name(&name) {
            Some(target) => targets.push((name, target)),
            None => eprintln!("skip: no market identity for {name:?}"),
        }
    }
    for mask in &opts.masks {
        let name = format!("mask:{mask}");
        if seen.insert(name.clone()) {
            targets.push((name, market::MarketTarget::ItemMask(*mask)));
        }
    }
    targets.truncate(opts.limit as usize);
    if targets.is_empty() {
        eprintln!("nothing to sweep");
        std::process::exit(2);
    }

    let delay = std::time::Duration::from_millis(opts.delay);
    eprintln!(
        "sweeping {} items, {} recipes from {}, {}ms apart (~{}s)",
        targets.len(),
        data.recipes.len(),
        data_dir,
        opts.delay,
        targets.len() as u64 * opts.delay / 1000
    );

    let mut books: Vec<sweep::ItemBook> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut observations: Vec<sweep::Observation> = Vec::new();

    // The corpus is the diff baseline: the previous sweep's state is the most
    // recent row per listing id. Read before appending, so the current sweep
    // never diffs against itself.
    let observations_path = opts
        .observations
        .clone()
        .unwrap_or_else(|| "../data/market-observations.jsonl".to_string());
    let previous = if opts.no_observations {
        Vec::new()
    } else {
        // Only diff items we are sweeping this run: dropping an item from the
        // watchlist would otherwise read as all of its listings having sold.
        let watched: std::collections::HashSet<&str> =
            targets.iter().map(|(name, _)| name.as_str()).collect();
        sweep::latest_state(&read_observations(&observations_path))
            .into_iter()
            .filter(|row| watched.contains(row.item.as_str()))
            .collect()
    };
    let had_baseline = !previous.is_empty();
    let ts = now_ms();
    for (index, (name, target)) in targets.iter().enumerate() {
        if index > 0 {
            std::thread::sleep(delay);
        }
        let query = market::MarketQuery {
            target: target.clone(),
            min_sockets: None,
            max_sockets: None,
            stat_filters: Vec::new(),
        };
        let mut attempt = 0u32;
        loop {
            match market::fetch_market(&session, &query) {
                Ok(result) => {
                    for row in &result.listings {
                        observations.push(sweep::Observation::from_listing(ts, name, row));
                    }
                    match sweep::ItemBook::from_listings(name.clone(), &result.listings) {
                        Some(book) => books.push(book),
                        None => skipped.push((name.clone(), "nothing on sale".into())),
                    }
                    break;
                }
                Err(market::MarketError::ServerRejected(reason))
                    if reason == "rate-limit" && attempt < 2 =>
                {
                    attempt += 1;
                    let backoff = delay * 10 * attempt;
                    eprintln!("rate limited on {name}; backing off {}s", backoff.as_secs());
                    std::thread::sleep(backoff);
                }
                Err(error) => {
                    skipped.push((name.clone(), error.to_string()));
                    break;
                }
            }
        }
        eprintln!("[{}/{}] {name}", index + 1, targets.len());
    }

    let signals = sweep::analyze(&books, &data.recipes, sweep::Thresholds::default());
    let delta = sweep::diff_observations(&previous, &observations);
    if !opts.no_observations {
        match append_observations(&observations_path, &observations) {
            Ok(()) => {
                eprintln!("recorded {} listings to {observations_path}", observations.len())
            }
            Err(error) => eprintln!("warning: could not record observations: {error}"),
        }
    }
    if opts.json {
        let payload = serde_json::json!({
            "books": books.iter().map(|book| book.to_json()).collect::<Vec<_>>(),
            "signals": signals.iter().map(|signal| signal.to_json()).collect::<Vec<_>>(),
            "skipped": skipped.iter()
                .map(|(item, reason)| serde_json::json!({"item": item, "reason": reason}))
                .collect::<Vec<_>>(),
            "observations": {
                "path": if opts.no_observations {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(observations_path)
                },
                "baseline": had_baseline,
                "recorded": observations.len(),
            },
            "delta": delta.to_json(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return;
    }
    print_sweep_table(&books, &signals, &skipped);
    if !opts.no_observations {
        print_changes(&delta, had_baseline);
    }
}

/// Read the observation corpus, tolerating a torn final line.
fn read_observations(path: &str) -> Vec<sweep::Observation> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| sweep::Observation::from_json(&value))
        .collect()
}

fn append_observations(path: &str, rows: &[sweep::Observation]) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    for row in rows {
        writeln!(file, "{}", row.to_json()).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// What left the market since the last sweep. A vanished listing may have sold,
/// been cancelled, or simply expired — the game exposes no trade feed, so this
/// is the only market-wide evidence there is.
fn print_changes(delta: &sweep::Delta, had_baseline: bool) {
    println!("\nCHANGES");
    if !had_baseline {
        println!("  first sweep — recorded as the baseline, nothing to diff against");
        return;
    }
    let sales = delta.vanished_by_item();
    if sales.is_empty() {
        println!("  nothing left the market");
    } else {
        println!("  gone since last sweep (sold, cancelled or expired):");
        for stat in sales.iter().take(12) {
            println!(
                "    {:<22} {:>3} listings {:>7} units  median {:>9}  top {:>9}",
                stat.item, stat.count, stat.units, stat.median_unit_price, stat.highest_unit_price
            );
        }
    }
    println!(
        "  new listings: {}   repriced: {}",
        delta.appeared.len(),
        delta.repriced.len()
    );
}

fn print_sweep_table(
    books: &[sweep::ItemBook],
    signals: &[sweep::Signal],
    skipped: &[(String, String)],
) {
    println!(
        "{:<22} {:>6} {:>5} {:>6} {:>10} {:>10} {:>10} {:>7}",
        "item", "levels", "asks", "units", "ask", "next", "centre", "cliff"
    );
    let mut rows: Vec<&sweep::ItemBook> = books.iter().collect();
    rows.sort_by_key(|book| book.best_ask().map(|level| level.unit_price).unwrap_or(i64::MAX));
    for book in rows {
        let cell = |value: Option<i64>| value.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "{:<22} {:>6} {:>5} {:>6} {:>10} {:>10} {:>10} {:>7}",
            book.name,
            book.listing_levels(),
            book.listings(),
            book.total_units(),
            cell(book.best_ask().map(|level| level.unit_price)),
            cell(book.next_ask().map(|level| level.unit_price)),
            cell(book.price_centre()),
            book.cliff().map(|c| format!("{:.0}%", c * 100.0)).unwrap_or_else(|| "-".into()),
        );
    }

    println!("\nSIGNALS");
    if signals.is_empty() {
        println!("  none — every book is tightly priced");
    }
    for signal in signals {
        match signal {
            sweep::Signal::CraftMargin { name, item_ask, rune_cost, gap, crafts, total, .. } => println!(
                "  CRAFT MARGIN {name:<20} runes {rune_cost:>9}  item {item_ask:>9}  +{gap:>9}  x{crafts} = {total:>10}"
            ),
            sweep::Signal::Cliff { name, ask, next, gap, units } => println!(
                "  CLIFF        {name:<20} ask {ask:>9}  next {next:>9}  +{:.0}%  depth {units}",
                gap * 100.0
            ),
            sweep::Signal::BelowCentre { name, ask, centre, ratio } => println!(
                "  BELOW CENTRE {name:<20} ask {ask:>9}  centre {centre:>9}  ({:.0}% of centre)",
                ratio * 100.0
            ),
            sweep::Signal::CheaperThanRunes { name, item_ask, rune_cost, gap } => println!(
                "  DON'T CRAFT  {name:<20} runes {rune_cost:>9}  item {item_ask:>9}  item is {gap} cheaper"
            ),
        }
    }

    if !skipped.is_empty() {
        println!("\nno listings / errors ({}):", skipped.len());
        for (name, reason) in skipped {
            println!("  {name:<22} {reason}");
        }
    }
    println!("\nprices are per unit; `cliff` is how far the best ask sits below the next level.");
}

fn print_help() {
    println!(
        "hs-sniff — passive Hero Siege traffic reader

USAGE:
  hs-sniff watch [--log FLUSH.jsonl]
                                live capture, streaming events (needs libpcap/Npcap rights)
  hs-sniff replay FILE          decode a pcap capture or hs-sniff jsonl log
  hs-sniff sniff-stdin < FILE   decode a raw payload or pcap from stdin
  hs-sniff serve                MCP stdio server (set HS_SNIFF_LIVE=1 to also capture)
  hs-sniff context --replay FILE | --live [--timeout SECS]
                                show the market session context sniffed so far
  hs-sniff market \"QUERY\" --replay FILE | --live [--timeout SECS] [--json]
                                live market prices for a plain-text query, e.g.
                                \"2 socket Harlequinn's Crest\", \"Grief\", \"min 2 sockets mask:1234\"
  hs-sniff sweep [--runes] [--runewords] [--materials] [--name ITEM] [--mask N]
                 --replay FILE | --live [--timeout SECS]
                 [--data-dir DIR] [--delay MS] [--limit N] [--json]
                 [--observations FILE] [--no-observations]
                                sweep fungible items and report structural mispricings
                                (default watchlist: runes + runewords)
  hs-sniff --table ITEMS.tsv replay FILE
                                load a name/rarity table (type<TAB>id<TAB>wt<TAB>name<TAB>rarity)

The game is read passively: nothing is injected, nothing is decrypted. The
market is HTTPS, so `market` re-issues the game's own query using the session
context it sniffed — the game must have opened its Market once while capturing.

`sweep` treats each item as an order book and reports on **unit** prices: `price`
is a stack total, so banding it (as the game's own avgPrice does) compares
stacks of different sizes. A dense book should produce no signal."
    );
}

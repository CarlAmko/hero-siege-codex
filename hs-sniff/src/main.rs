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
use hs_sniff::market::{self, MarketCatalogTable};
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
    };
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "watch" | "replay" | "serve" | "sniff-stdin" | "market" | "context" => {
                opts.cmd = args[i].clone()
            }
            "help" | "--help" | "-h" => opts.cmd = "help".into(),
            "--live" => opts.live = true,
            "--json" => opts.json = true,
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
  hs-sniff --table ITEMS.tsv replay FILE
                                load a name/rarity table (type<TAB>id<TAB>wt<TAB>name<TAB>rarity)

The game is read passively: nothing is injected, nothing is decrypted. The
market is HTTPS, so `market` re-issues the game's own query using the session
context it sniffed — the game must have opened its Market once while capturing."
    );
}

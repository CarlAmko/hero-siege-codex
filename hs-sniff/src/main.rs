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
use hs_sniff::mcp::SharedState;
use hs_sniff::reasm::Reassembler;
use hs_sniff::wire::{parse_pcap_bytes, ReplaySource};
use hs_sniff::{decode, mcp};

mod itemtable;

struct Options {
    cmd: String,
    file: Option<String>,
    limit: u64,
    table: Option<String>,
    log: Option<String>,
}

fn parse_args(args: &[String]) -> Options {
    let mut opts =
        Options { cmd: "help".into(), file: None, limit: u64::MAX, table: None, log: None };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "watch" | "replay" | "serve" | "sniff-stdin" => opts.cmd = args[i].clone(),
            "help" | "--help" | "-h" => opts.cmd = "help".into(),
            "--table" if i + 1 < args.len() => {
                opts.table = Some(args[i + 1].clone());
                i += 1;
            }
            "--log" if i + 1 < args.len() => {
                opts.log = Some(args[i + 1].clone());
                i += 1;
            }
            "--limit" if i + 1 < args.len() => {
                opts.limit = args[i + 1].parse().unwrap_or(u64::MAX);
                i += 1;
            }
            other => {
                if !other.starts_with('-') && opts.file.is_none() && opts.cmd == "replay" {
                    opts.file = Some(other.to_string());
                }
            }
        }
        i += 1;
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
) -> usize {
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
    print_summary(&totals);
    printed
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
  hs-sniff --table ITEMS.tsv replay FILE
                                load a name/rarity table (type<TAB>id<TAB>wt<TAB>name<TAB>rarity)

The game is read passively: nothing is injected, nothing is decrypted."
    );
}

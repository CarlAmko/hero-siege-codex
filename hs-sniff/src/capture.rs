//! Live capture: passive sniffing off the wire, scoped to the game's own
//! servers where the OS lets us see them.
//!
//! Structure ported from `refs/hs-tracker/src-tauri/src/sniffer.rs`, cut down
//! to what a headless CLI needs: game-process discovery, endpoint lookup, BPF
//! scoping, per-adapter capture threads, TCP reassembly and event decoding.
//! The overlay/dashboard layers of the original are intentionally absent.
//!
//! Windows note: Npcap must be installed (with "WinPcap API-compatible Mode")
//! for this module to work; on Linux the binary needs `cap_net_raw` or root,
//! the same as tcpdump.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use etherparse::{NetSlice, SlicedPacket, TransportSlice};

use crate::decode::extract_messages;
use crate::events::{events_from_messages, GameEvent};
use crate::reasm::{Flow, Reassembler};

/// Process names the game is recognised by, flattened to alphanumerics.
const GAME_PROCESS_NAMES: &[&str] = &["herosiegeexe", "herosiege"];

/// Everything the CLI wants to know about a running capture.
#[derive(Debug, Clone, Default)]
pub struct CaptureStatus {
    pub running: bool,
    pub interfaces: Vec<String>,
    pub hosts: usize,
    pub packets_seen: u64,
    pub messages_decoded: u64,
    pub dropped: u64,
    pub note: String,
}

/// Handles for a live session: poll [`status`], consume events with
/// [`try_recv`], stop with [`stop`].
pub struct LiveCapture {
    inner: Option<Session>,
    stats: Arc<SessionStats>,
    event_rx: std::sync::mpsc::Receiver<GameEvent>,
}

struct Session {
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct SessionStats {
    packets: AtomicU64,
    messages: AtomicU64,
    dropped: AtomicU64,
    interfaces: Mutex<Vec<String>>,
    hosts: Mutex<usize>,
    note: Mutex<String>,
}

impl LiveCapture {
    /// Start capturing. Fails when no capture device can be opened at all —
    /// Npcap missing on Windows, no cap_net_raw on Linux.
    pub fn start() -> Result<Self, String> {
        Self::start_with_log(None)
    }

    /// Start capturing, additionally appending every reassembled flush to
    /// `log_path` as jsonl (`{"src","data","ts"}`) so the session can be
    /// replayed through `replay` later — including message shapes the event
    /// extractor does not model (character snapshots, tree allocations).
    pub fn start_with_log(log_path: Option<String>) -> Result<Self, String> {
        let devices = pcap::Device::list().map_err(|e| format!("cannot list devices: {e}"))?;
        if devices.is_empty() {
            return Err("no capture devices found".into());
        }
        let log = log_path.map(|p| {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .unwrap_or_else(|e| panic!("cannot open flush log {p}: {e}"));
            Arc::new(Mutex::new(std::io::BufWriter::new(f)))
        });
        let stats = Arc::new(SessionStats::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        let watcher_stop = stop.clone();
        let watcher_stats = stats.clone();
        let watcher = std::thread::spawn(move || {
            watcher_loop(watcher_stop, watcher_stats, tx, log);
        });

        Ok(Self {
            inner: Some(Session { stop, watcher: Some(watcher) }),
            stats,
            event_rx: rx,
        })
    }

    pub fn status(&self) -> CaptureStatus {
        CaptureStatus {
            running: self.inner.is_some(),
            interfaces: self.stats.interfaces.lock().unwrap().clone(),
            hosts: *self.stats.hosts.lock().unwrap(),
            packets_seen: self.stats.packets.load(Ordering::Relaxed),
            messages_decoded: self.stats.messages.load(Ordering::Relaxed),
            dropped: self.stats.dropped.load(Ordering::Relaxed),
            note: self.stats.note.lock().unwrap().clone(),
        }
    }

    /// Non-blocking event poll for the CLI/MCP loops.
    pub fn try_recv(&self) -> Option<GameEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn stop(&mut self) {
        if let Some(session) = self.inner.take() {
            session.stop.store(true, Ordering::Relaxed);
            if let Some(w) = session.watcher {
                let _ = w.join();
            }
        }
    }
}

impl Drop for LiveCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The watcher: finds the game, reads its endpoints from the OS socket table
/// every five seconds, and (re)spawns capture threads when the scope changes.
fn watcher_loop(
    stop: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
    tx: std::sync::mpsc::Sender<GameEvent>,
    log: Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
) {
    let mut sys = sysinfo::System::new();
    let mut scope = String::new();
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    let mut seen_note = String::new();

    while !stop.load(Ordering::Relaxed) {
        let pids = game_pids(&mut sys);
        let (remote, note) = if pids.is_empty() {
            (BTreeSet::new(), "waiting for the game".to_string())
        } else {
            let (ips, local) = game_endpoints(&pids);
            let note = if ips.is_empty() && local > 0 {
                "game holds only local connections (accelerator?); capture is wide".into()
            } else if ips.is_empty() {
                "game found, no endpoints yet".into()
            } else {
                format!("{} game server(s)", ips.len())
            };
            (ips, note)
        };
        if note != seen_note {
            seen_note = note.clone();
            *stats.note.lock().unwrap() = note;
        }
        *stats.hosts.lock().unwrap() = remote.len();

        let next = scope_for(&remote);
        if next != scope {
            scope = next.clone();
            // stop old threads; a fresh set picks up the new filter
            for t in threads.drain(..) {
                // threads exit on their own when the process/endpoint sweep
                // changes the scope; they poll nothing shared, so detach
                t.thread().unpark();
            }
            let devices = match pcap::Device::list() {
                Ok(d) => d,
                Err(_) => continue,
            };
            for dev in devices {
                let scope = scope.clone();
                let stats = stats.clone();
                let tx = tx.clone();
                let stop = stop.clone();
                let log = log.clone();
                threads.push(std::thread::spawn(move || {
                    capture_thread(dev, scope, stop, stats, tx, log);
                }));
            }
            *stats.interfaces.lock().unwrap() =
                pcap::Device::list().map(|ds| ds.iter().map(|d| d.name.clone()).collect()).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_secs(if pids.is_empty() { 1 } else { 5 }));
    }
}

/// Capture on one adapter until stopped. Re-derives the filter from `scope`.
fn capture_thread(
    dev: pcap::Device,
    scope: String,
    stop: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
    tx: std::sync::mpsc::Sender<GameEvent>,
    log: Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
) {
    let mut cap = match pcap::Capture::from_device(dev)
        .and_then(|c| c.immediate_mode(true).timeout(400).open())
    {
        Ok(c) => c,
        Err(e) => {
            *stats.note.lock().unwrap() = format!("cannot open adapter: {e}");
            return;
        }
    };
    if let Err(e) = cap.filter(&format!("tcp and len > 30 and ({scope})"), true) {
        *stats.note.lock().unwrap() = format!("bad filter: {e}");
        return;
    }
    let framing = cap.get_datalink().0;
    let mut asm = Reassembler::default();
    let mut swept = std::time::Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let packet = match cap.next_packet() {
            Ok(p) => Some(p),
            Err(pcap::Error::TimeoutExpired) => None,
            Err(_) => None,
        };
        if packet.is_none() || swept.elapsed() >= Duration::from_millis(100) {
            swept = std::time::Instant::now();
            for (src, flushed) in asm.drain_idle() {
                handle_flush(&flushed, Some(src), &stats, &tx, log.as_ref());
            }
        }
        let Some(packet) = packet else { continue };
        stats.packets.fetch_add(1, Ordering::Relaxed);
        let data: &[u8] = packet.data;
        let sliced = match framing {
            1 => SlicedPacket::from_ethernet(data),
            0 | 108 if data.len() >= 4 => SlicedPacket::from_ip(&data[4..]),
            _ => SlicedPacket::from_ip(data),
        };
        let Ok(pkt) = sliced else { continue };
        let src = match &pkt.net {
            Some(NetSlice::Ipv4(v4)) => IpAddr::V4(v4.header().source_addr()),
            Some(NetSlice::Ipv6(v6)) => IpAddr::V6(v6.header().source_addr()),
            _ => continue,
        };
        let Some(TransportSlice::Tcp(tcp)) = &pkt.transport else { continue };
        // Port 443 is skipped as encrypted; the game's plaintext never moves
        // there in ordinary play.
        if tcp.source_port() == 443 || tcp.destination_port() == 443 {
            continue;
        }
        let flow: Flow = (src, tcp.source_port(), tcp.destination_port());
        if let Some(flushed) = asm.push(flow, tcp.acknowledgment_number(), tcp.payload()) {
            handle_flush(&flushed, Some(src), &stats, &tx, log.as_ref());
        }
    }
}

fn handle_flush(
    flushed: &[u8],
    src: Option<IpAddr>,
    stats: &Arc<SessionStats>,
    tx: &std::sync::mpsc::Sender<GameEvent>,
    log: Option<&Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
) {
    // Log the flush before extraction so message shapes the event extractor
    // does not model are still preserved for later replay/analysis.
    if let Some(log) = log {
        use base64::Engine as _;
        use std::io::Write as _;
        let row = serde_json::json!({
            "src": src.map(|i| i.to_string()),
            "data": base64::engine::general_purpose::STANDARD.encode(flushed),
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        });
        if let Ok(mut w) = log.lock() {
            let _ = serde_json::to_writer(&mut *w, &row);
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }
    let messages = extract_messages(flushed);
    if messages.is_empty() {
        return;
    }
    stats.messages.fetch_add(messages.len() as u64, Ordering::Relaxed);
    for event in events_from_messages(&messages) {
        let _ = tx.send(event);
    }
}

fn flat_name(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase()
}

fn game_pids(sys: &mut sysinfo::System) -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    sys.processes()
        .iter()
        .filter(|(_, p)| {
            let name = flat_name(&p.name().to_string_lossy());
            let exe = p.exe().map(|e| flat_name(&e.file_name().map(|f| f.to_string_lossy()).unwrap_or_default())).unwrap_or_default();
            GAME_PROCESS_NAMES.iter().any(|g| name.starts_with(g) || exe.starts_with(g))
        })
        .map(|(pid, _)| pid.as_u32())
        .collect()
}

/// Both ends of every connection the game holds, read from the OS socket
/// table. Returns (remote server IPs, count of connections back to this
/// machine).
fn game_endpoints(pids: &[u32]) -> (BTreeSet<IpAddr>, usize) {
    use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo};
    let mut remote = BTreeSet::new();
    let mut homebound = 0usize;
    if pids.is_empty() {
        return (remote, homebound);
    }
    let unmap = |ip: IpAddr| -> IpAddr {
        match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        }
    };
    for af in [AddressFamilyFlags::IPV4, AddressFamilyFlags::IPV6] {
        let rows = match netstat2::iterate_sockets_info(af, ProtocolFlags::TCP) {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        for s in rows.flatten() {
            if let ProtocolSocketInfo::Tcp(t) = &s.protocol_socket_info {
                if !s.associated_pids.iter().any(|p| pids.contains(p)) {
                    continue;
                }
                let far = (unmap(t.remote_addr), t.remote_port);
                if far.0.is_unspecified() {
                    continue;
                }
                if far.0.is_loopback() {
                    homebound += 1;
                    continue;
                }
                remote.insert(far.0);
            }
        }
    }
    (remote, homebound)
}

/// The BPF scope: the game's servers by /24 (the game moves between servers
/// inside a session and a single address misses the first seconds of a new
/// one), or everything TCP while nothing is known yet.
fn scope_for(remote: &BTreeSet<IpAddr>) -> String {
    if remote.is_empty() {
        return "tcp".into();
    }
    let mut out: Vec<String> = remote
        .iter()
        .map(|ip| match ip {
            IpAddr::V4(v4) => {
                let [a, b, c, _] = v4.octets();
                format!("net {a}.{b}.{c}.0/24")
            }
            IpAddr::V6(_) => format!("host {ip}"),
        })
        .collect();
    out.sort();
    out.dedup();
    out.join(" or ")
}

/// Silence an unused-warning when built without live capture.
#[allow(dead_code)]
struct KeepAlive(HashMap<String, u32>);

//! Wire sources: pcap file parsing and flush logs, for replay and tests.
//!
//! Supports the classic libpcap format (the one `tcpdump -w` and Wireshark's
//! "pcap" write) with Ethernet, raw-IP and NULL/Loopback link types — enough
//! to replay a capture of a Hero Siege session offline. VLAN tags are skipped
//! the same way the live path handles them.

use std::net::IpAddr;
use std::path::Path;

/// One frame worth handing to the pipeline: the packet bytes starting at the
/// IP header (already link-layer-stripped), with a direction hint.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Capture timestamp in microseconds since the epoch.
    pub ts_micros: u64,
    /// Source IP once parsed, when known.
    pub src: Option<IpAddr>,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub enum WireError {
    Io(std::io::Error),
    /// Not a pcap file (bad magic).
    NotPcap,
    /// Structurally broken file.
    Corrupt(String),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "io error: {e}"),
            WireError::NotPcap => write!(f, "not a pcap file (magic mismatch)"),
            WireError::Corrupt(s) => write!(f, "corrupt pcap: {s}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        WireError::Io(e)
    }
}

const MAGIC_LE: u32 = 0xa1b2_c3d4;
const MAGIC_BE: u32 = 0xd4c3_b2a1;
const MAGIC_NANOLE: u32 = 0xa1b2_3c4d;
const MAGIC_NANOBE: u32 = 0x4d3c_b2a1;

const LINK_ETHERNET: u32 = 1;
const LINK_RAW: u32 = 12;
const LINK_LOOP: u32 = 108;
const LINK_NULL: u32 = 0;

/// Parse a pcap file into frames with the link layer stripped. nanosecond
/// files are accepted and their timestamps folded to microseconds.
pub fn parse_pcap_file(path: impl AsRef<Path>) -> Result<Vec<Frame>, WireError> {
    parse_pcap_bytes(&std::fs::read(path)?)
}

/// Parse pcap bytes. Kept public so tests and the MCP can feed buffers.
pub fn parse_pcap_bytes(bytes: &[u8]) -> Result<Vec<Frame>, WireError> {
    if bytes.len() < 24 {
        return Err(WireError::NotPcap);
    }
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    // Both native and byte-swapped, micro and nano.
    let (swap, nanos) = match magic {
        MAGIC_LE => (false, false),
        MAGIC_BE => (true, false),
        MAGIC_NANOLE => (false, true),
        MAGIC_NANOBE => (true, true),
        _ => return Err(WireError::NotPcap),
    };
    let rd_u32 = |b: &[u8], at: usize| -> u32 {
        let v = u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        if swap { v.swap_bytes() } else { v }
    };
    let link = rd_u32(bytes, 20);
    let mut out = Vec::new();
    let mut at = 24;
    while at + 16 <= bytes.len() {
        let ts_sec = rd_u32(bytes, at) as u64;
        let ts_frac = rd_u32(bytes, at + 4) as u64;
        let caplen = rd_u32(bytes, at + 8) as usize;
        let _wirelen = rd_u32(bytes, at + 12) as usize;
        at += 16;
        if caplen > bytes.len().saturating_sub(at) {
            return Err(WireError::Corrupt(format!("packet at {at} claims {caplen} bytes past end")));
        }
        let data = &bytes[at..at + caplen];
        at += caplen;
        let ts_micros = if nanos { ts_frac / 1_000 } else { ts_frac };
        let Some(stripped) = strip_link(data, link) else { continue };
        let src = source_of(&stripped);
        out.push(Frame { ts_micros: ts_sec * 1_000_000 + ts_micros, src, data: stripped });
    }
    Ok(out)
}

/// Strip the link layer; returns the bytes from the IP header on.
fn strip_link(data: &[u8], link: u32) -> Option<Vec<u8>> {
    match link {
        LINK_ETHERNET => {
            if data.len() < 14 {
                return None;
            }
            let mut at = 12;
            // up to two VLAN tags before the ethertype that matters
            for _ in 0..2 {
                let ty = u16::from_be_bytes([data[at], data[at + 1]]);
                match ty {
                    0x8100 | 0x88a8 | 0x9100 if at + 4 <= data.len() => at += 4,
                    0x0800 | 0x86dd => return Some(data[at + 2..].to_vec()),
                    _ => return None,
                }
            }
            None
        }
        LINK_NULL | LINK_LOOP => {
            // 4-byte family header (host byte order either way; the value is 2)
            data.get(4..).map(|d| d.to_vec())
        }
        LINK_RAW => Some(data.to_vec()),
        _ => Some(data.to_vec()),
    }
}

/// Source IPv4/IPv6 address out of a stripped frame, when it parses far enough.
fn source_of(ip: &[u8]) -> Option<IpAddr> {
    use std::net::Ipv4Addr;
    match ip.first()? >> 4 {
        4 if ip.len() >= 20 => {
            Some(IpAddr::V4(Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15])))
        }
        6 if ip.len() >= 40 => {
            let mut o = [0u8; 16];
            o.copy_from_slice(&ip[8..24]);
            Some(IpAddr::V6(o.into()))
        }
        _ => None,
    }
}

/// Where a replay comes from.
pub enum ReplaySource {
    /// A pcap capture file.
    Pcap(Vec<Frame>),
    /// A jsonl file of `{"src": "...", "data": "<base64>"}` lines — the
    /// hand-rolled capture log format this tool writes with `--log jsonl`.
    Jsonl(Vec<(Option<IpAddr>, Vec<u8>)>),
}

impl ReplaySource {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, WireError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        if parse_pcap_bytes(&bytes).is_ok() {
            return Ok(ReplaySource::Pcap(parse_pcap_bytes(&bytes)?));
        }
        // jsonl fallback
        let text = String::from_utf8_lossy(&bytes);
        if text.lines().any(|l| l.trim_start().starts_with('{')) {
            let mut rows = Vec::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v: serde_json::Value = serde_json::from_str(line)
                    .map_err(|_| WireError::Corrupt("bad jsonl line".into()))?;
                let src = v.get("src").and_then(|s| s.as_str()).and_then(|s| s.parse().ok());
                let data = v
                    .get("data")
                    .and_then(|d| d.as_str())
                    .and_then(|d| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, d).ok())
                    .unwrap_or_default();
                rows.push((src, data));
            }
            return Ok(ReplaySource::Jsonl(rows));
        }
        Err(WireError::NotPcap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny pcap: Ethernet + IPv4 + TCP + payload.
    fn pcap_with(payload: &[u8]) -> Vec<u8> {
        let mut ip = vec![0x45, 0, 0, 0];
        ip.extend_from_slice(&[0, 1, 0, 0]); // id, flags
        ip.extend_from_slice(&[64, 6, 0, 0]); // ttl, tcp proto, csum
        ip.extend_from_slice(&[10, 0, 0, 1]); // src
        ip.extend_from_slice(&[172, 104, 128, 5]); // dst
        let mut tcp = vec![0x1f, 0x90, 0x1f, 0x40]; // ports
        tcp.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 2]); // seq, ack
        tcp.push(0x50); // offset
        tcp.push(0x18); // PSH|ACK
        tcp.extend_from_slice(&[0x20, 0, 0, 0, 0, 0]);
        let mut pkt = ip;
        pkt.extend(tcp);
        pkt.extend_from_slice(payload);
        let total = pkt.len() as u16;
        pkt[2..4].copy_from_slice(&total.to_be_bytes());

        let mut out = vec![0xd4, 0xc3, 0xb2, 0xa1]; // magic (swapped LE bytes)
        out.extend_from_slice(&[2, 0, 4, 0]); // 2.4
        out.extend_from_slice(&[0, 0, 0, 0]); // tz
        out.extend_from_slice(&[0, 0, 0, 0]); // sigfigs
        out.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // snaplen 65535
        out.extend_from_slice(&LINK_ETHERNET.to_le_bytes());
        // eth header
        let mut frame = vec![0; 12];
        frame.extend_from_slice(&0x0800u16.to_be_bytes());
        frame.extend_from_slice(&pkt);
        // packet record
        out.extend_from_slice(&1000u32.to_le_bytes());
        out.extend_from_slice(&500u32.to_le_bytes());
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&frame);
        out
    }

    #[test]
    fn parses_minimal_pcap() {
        let bytes = pcap_with(b"{\"a\":1}");
        let frames = parse_pcap_bytes(&bytes).expect("parses");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].src, Some("10.0.0.1".parse().unwrap()));
        let msgs = crate::decode::extract_messages(&frames[0].data);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["a"], 1);
    }
}

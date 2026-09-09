//! TCP flow reassembly.
//!
//! Ported from `refs/hs-tracker/src-tauri/src/parser.rs` (the `Reassembler`
//! there): payloads are buffered per flow and ack, and flushed when the ack
//! from that flow changes. A message straddling two flushes would be lost, so
//! an unterminated JSON tail is carried over to the next flush of the same
//! flow.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// One side of one TCP connection: source address and both ports.
///
/// The game holds several connections to the same server at once — a busy one
/// (the world) and a quiet one (character saves). Keyed by address alone they
/// share a buffer, and during a fight the world traffic shreds the save being
/// assembled — exactly when the counters must not stop moving.
pub type Flow = (IpAddr, u16, u16);

const BUF_CAP: usize = 1 << 20;
/// The longest a single JSON value may span. Nothing the game sends is near
/// it: the biggest message in a 16,104-message capture is 35,674 bytes.
pub(crate) const MAX_SPAN: usize = 256 << 10;
/// What all the scans over one buffer may read between them, as a multiple of
/// its length. Openers that lead nowhere are what made the scan quadratic, and
/// this is the ceiling that stops a buffer of them from stalling capture.
pub(crate) const SCAN_BUDGET: usize = 8;
/// A carried tail is the truncated end of one message, so the cap has to clear
/// the largest the game sends.
const CARRY_CAP: usize = 64 << 10;
const CARRY_ROUNDS: u8 = 3;
const BUF_TTL: Duration = Duration::from_secs(15);
/// What we send is only flushed when the ack changes, and the ack only changes
/// when the server sends something back. Character saves — the one source of
/// kills and experience — would then sit here until the next server burst, so
/// the counters would move only on a zone change. A quiet buffer is flushed on
/// its own.
const IDLE_FLUSH: Duration = Duration::from_millis(250);

struct Pending {
    data: Vec<u8>,
    at: Instant,
}

/// Buffers payloads per flow and ack and yields whole flushes.
#[derive(Default)]
pub struct Reassembler {
    bufs: HashMap<(Flow, u32), Pending>,
    last_ack: HashMap<Flow, u32>,
    carry: HashMap<Flow, (Vec<u8>, u8)>,
}

impl Reassembler {
    /// Hand over one segment's payload. Returns a flush when the ack from this
    /// flow changed, which marks the end of one side of a burst.
    pub fn push(&mut self, flow: Flow, ack: u32, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.is_empty() {
            return None;
        }
        self.evict_stale();
        let last = *self.last_ack.entry(flow).or_insert(ack);
        let buf = self
            .bufs
            .entry((flow, ack))
            .or_insert_with(|| Pending { data: Vec::new(), at: Instant::now() });
        if buf.data.len() < BUF_CAP {
            buf.data.extend_from_slice(payload);
        }
        buf.at = Instant::now();
        if ack == last {
            return None;
        }
        self.last_ack.insert(flow, ack);
        let flushed = self.bufs.remove(&(flow, last))?;
        Some(self.finish(flow, flushed.data))
    }

    /// Flush every non-empty buffer regardless of age — for end-of-input,
    /// where there is no "later" to wait for.
    pub fn drain_all(&mut self) -> Vec<(IpAddr, Vec<u8>)> {
        let keys: Vec<(Flow, u32)> =
            self.bufs.iter().filter(|(_, b)| !b.data.is_empty()).map(|(k, _)| *k).collect();
        keys.into_iter()
            .filter_map(|key| {
                let pending = self.bufs.remove(&key)?;
                Some((key.0 .0, self.finish(key.0, pending.data)))
            })
            .collect()
    }

    /// Buffers nobody has added to for a moment, so a stream that only talks
    /// one way still gets read. Returns (source, flushed-bytes) pairs.
    pub fn drain_idle(&mut self) -> Vec<(IpAddr, Vec<u8>)> {
        let now = Instant::now();
        let ripe: Vec<(Flow, u32)> = self
            .bufs
            .iter()
            .filter(|(_, b)| !b.data.is_empty() && now.duration_since(b.at) >= IDLE_FLUSH)
            .map(|(k, _)| *k)
            .collect();
        ripe
            .into_iter()
            .filter_map(|key| {
                let pending = self.bufs.remove(&key)?;
                Some((key.0 .0, self.finish(key.0, pending.data)))
            })
            .collect()
    }

    fn finish(&mut self, flow: Flow, flushed: Vec<u8>) -> Vec<u8> {
        // Stitch the previous tail back on. The flush count travels in the map
        // with it — derived from whether the tail was empty, it would stay at
        // 0 or 1 and never reach CARRY_ROUNDS.
        //
        // Giving up keeps the bytes and parses them rather than dropping the
        // tail: a stray `[` or `{` in binary framing is followed by whatever
        // the stream sent next, so discarding the tail discards any message
        // sitting behind the opener.
        let (mut data, rounds, giving_up) = match self.carry.remove(&flow) {
            Some((tail, rounds)) if rounds < CARRY_ROUNDS => (tail, rounds, false),
            Some((tail, _)) => (tail, 0, true),
            None => (Vec::new(), 0, false),
        };
        data.extend_from_slice(&flushed);

        // Is the unterminated value at the end a real message, or a framing
        // byte that happens to be a brace? Bracket structure cannot answer
        // that. A tail that ends at the opener itself is an ordinary
        // truncation; the byte after the opener is the discriminator — see
        // `opens_a_value`.
        let cut = unterminated_start(&data);
        let truncated =
            !giving_up && cut < data.len() && data.len() - cut <= CARRY_CAP && opens_a_value(&data[cut..]);
        if truncated {
            let tail = data.split_off(cut);
            self.carry.insert(flow, (tail, rounds + 1));
        }
        data
    }

    fn evict_stale(&mut self) {
        // A flow that flushes cleanly leaves no buffer behind, so its ack and
        // carry entries would otherwise outlive every sweep keyed on `bufs`.
        if self.bufs.len() <= 64 && self.last_ack.len() <= 512 && self.carry.len() <= 512 {
            return;
        }
        let now = Instant::now();
        self.bufs.retain(|_, b| now.duration_since(b.at) < BUF_TTL);
        let live: HashSet<Flow> = self.bufs.keys().map(|(flow, _)| *flow).collect();
        self.last_ack.retain(|flow, _| live.contains(flow));
        self.carry.retain(|flow, _| live.contains(flow));
        if self.bufs.len() > 512 {
            self.bufs.clear();
            self.last_ack.clear();
            self.carry.clear();
        }
    }
}

/// Whether an unterminated opener reads as the beginning of a real value.
///
/// The game's messages are JSON objects whose first member is a quoted key,
/// while the framing bytes a stray brace lives in are binary and can be
/// followed by anything. So an object must be followed by a quote or its own
/// close, and an array by something that can start a value.
fn opens_a_value(tail: &[u8]) -> bool {
    let Some(&opener) = tail.first() else { return false };
    let mut i = 1;
    while tail.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let Some(&next) = tail.get(i) else { return true };
    match opener {
        b'{' => next == b'"' || next == b'}',
        b'[' => matches!(
            next,
            b'"' | b'{' | b'[' | b']' | b'-' | b't' | b'f' | b'n' | b'0'..=b'9'
        ),
        _ => false,
    }
}

/// Index where a JSON value starts that never closes in this buffer, so the
/// caller can keep it for the next chunk. `len()` when everything is complete.
fn unterminated_start(buf: &[u8]) -> usize {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'{' || buf[i] == b'[' {
            match matching_json_end(buf, i) {
                Some(end) => i = end + 1,
                None => return i,
            }
        } else {
            i += 1;
        }
    }
    buf.len()
}

/// `totalGuildXp` and `total_guild_xp` are the same key: compare the
/// alphanumeric-lowercase forms without building them (this runs for every key
/// of every packet, several times per packet).
pub fn norm_eq(a: &str, b: &str) -> bool {
    let (mut ai, mut bi) = (
        a.bytes().filter(u8::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()),
        b.bytes().filter(u8::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()),
    );
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return true,
            (x, y) if x == y => continue,
            _ => return false,
        }
    }
}

/// Where the value opened at `start` closes, and how many bytes were read to
/// find that out — the caller that scans from every position in a buffer needs
/// the second number to keep the whole scan linear.
///
/// The walk stops at `MAX_SPAN`: the longest message in a full session's
/// capture is 35,674 bytes, so a span still open a quarter of a megabyte later
/// is a framing byte being chased, not a message being read.
pub(crate) fn json_end(b: &[u8], start: usize, walked: &mut usize) -> Option<usize> {
    let stop = b.len().min(start.saturating_add(MAX_SPAN));
    let mut stack = vec![b[start]];
    let mut in_str = false;
    let mut esc = false;
    let mut found = None;
    let mut i = start + 1;
    while i < stop {
        let c = b[i];
        i += 1;
        if esc {
            esc = false;
            continue;
        }
        match c {
            b'\\' => esc = true,
            b'"' => in_str = !in_str,
            b'{' | b'[' if !in_str => stack.push(c),
            b'}' | b']' if !in_str => {
                let Some(&open) = stack.last() else { break };
                if (c == b'}') != (open == b'{') {
                    break;
                }
                stack.pop();
                if stack.is_empty() {
                    found = Some(i - 1);
                    break;
                }
            }
            _ => {}
        }
    }
    *walked += i - start;
    found
}

fn matching_json_end(b: &[u8], start: usize) -> Option<usize> {
    json_end(b, start, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn flow(port: u16) -> Flow {
        (IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port, 7777)
    }

    #[test]
    fn flushes_when_ack_changes() {
        let mut r = Reassembler::default();
        assert!(r.push(flow(1), 100, b"{\"a\":1}").is_none());
        // the flush returns the buffer accumulated under the OLD ack; the
        // bytes that arrived with the new ack wait for the next flush
        let out = r.push(flow(1), 200, b"noise").expect("flush on ack change");
        let msgs = crate::decode::extract_messages(&out);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["a"], 1);
    }

    #[test]
    fn flows_do_not_share_buffers() {
        let mut r = Reassembler::default();
        r.push(flow(1), 100, b"{\"world\":1}");
        r.push(flow(2), 100, b"{\"save\":1}");
        let f1 = r.push(flow(1), 200, b"x").expect("flush flow 1");
        let f2 = r.push(flow(2), 200, b"x").expect("flush flow 2");
        let msgs = crate::decode::extract_messages(&[f1, f2].concat());
        assert!(msgs.iter().any(|m| m.get("save") == Some(&serde_json::json!(1))));
        assert!(msgs.iter().any(|m| m.get("world") == Some(&serde_json::json!(1))));
    }

    #[test]
    fn tail_carries_over() {
        let mut r = Reassembler::default();
        r.push(flow(1), 100, b"{\"items\":[1,2");
        // first flush: the tail is unterminated and is carried, not dropped
        let out1 = r.push(flow(1), 200, b"]}").expect("flush");
        assert!(crate::decode::extract_messages(&out1).is_empty());
        // second flush stitches the carried tail onto the continued stream
        let out2 = r.push(flow(1), 300, b"x").expect("flush 2");
        let msgs = crate::decode::extract_messages(&out2);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["items"], serde_json::json!([1, 2]));
    }

    #[test]
    fn idle_flush_releases_quiet_buffer() {
        let mut r = Reassembler::default();
        r.push(flow(1), 100, b"{\"k\":0}");
        std::thread::sleep(Duration::from_millis(280));
        let idle = r.drain_idle();
        assert!(!idle.is_empty());
    }
}

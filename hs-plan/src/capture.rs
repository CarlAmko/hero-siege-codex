//! Screenshot capture pipeline for the planner UI bridge:
//! base64 PNG → preprocess (invert/upscale, mirroring upstream's ocr.rs) →
//! tesseract OCR → vendored tooltip parser → pending-capture queue.
//!
//! Upstream HSPlanner runs neural OCR (ocrs/rten) inside Tauri with the models
//! embedded at build time; those binaries aren't in the repo, so here we shell
//! out to tesseract (already installed) and apply the same preprocessing order
//! upstream uses: invert dark images, upscale small ones. Tooltip parsing is
//! the vendored engine parser, so output shapes match the desktop app exactly.
//!
//! No new Rust dependencies — ImageMagick/tesseract are invoked as CLIs via
//! temp files, keeping memory use small (no full-image buffers in Rust).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::Serialize;

use hs_plan::calc::tooltip_parse::{self, TooltipParseResult};

const MAX_QUEUE: usize = 50;

/// One captured tooltip: OCR'd, parsed immediately, kept for review in the UI.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub id: u64,
    pub captured_at_ms: u64,
    /// "keybind" (shell script), "api" (direct POST), or "paste" (modal).
    pub source: String,
    pub ocr_lines: Vec<String>,
    pub parse: TooltipParseResult,
}

#[derive(Default)]
pub struct Queue {
    inner: Mutex<VecDeque<Capture>>,
    next_id: AtomicU64,
}

static QUEUE: LazyLock<Queue> = LazyLock::new(Queue::default);

pub fn queue() -> &'static Queue {
    &QUEUE
}

impl Queue {
    fn push(&self, c: Capture) {
        let mut q = self.inner.lock().unwrap();
        if q.len() >= MAX_QUEUE {
            q.pop_front();
        }
        q.push_back(c);
    }

    /// Drain the queue (the planner UI polls this).
    pub fn take_all(&self) -> Vec<Capture> {
        let mut q = self.inner.lock().unwrap();
        std::mem::take(&mut *q).into_iter().collect()
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut q = self.inner.lock().unwrap();
        let before = q.len();
        q.retain(|c| c.id != id);
        q.len() != before
    }
}

/// base64 (raw or `data:image/png;base64,` URL) → OCR + parse + enqueue.
pub fn capture_from_base64(image_base64: &str, source: &str) -> Result<Capture, String> {
    let bytes = decode_image_base64(image_base64)?;
    let ocr_lines = ocr_bytes(&bytes)?;
    if ocr_lines.is_empty() {
        return Err("OCR produced no text — image may not be a tooltip".to_string());
    }
    let parse = tooltip_parse::parse_tooltip_lines(ocr_lines.clone(), None);
    let capture = Capture {
        id: QUEUE.next_id.fetch_add(1, Ordering::Relaxed),
        captured_at_ms: now_ms(),
        source: source.to_string(),
        ocr_lines,
        parse,
    };
    QUEUE.push(capture.clone());
    Ok(capture)
}

/// OCR only (no parse, no queue) — mirrors upstream's `ocr_tooltip_lines`.
pub fn ocr_from_base64(image_base64: &str) -> Result<Vec<String>, String> {
    let bytes = decode_image_base64(image_base64)?;
    ocr_bytes(&bytes)
}

fn decode_image_base64(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    let s = s.strip_prefix("data:image/png;base64,").unwrap_or(s);
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64 decode: {e}"))
}

/// Decode + preprocess (invert dark images, upscale small ones, matching
/// upstream's preprocess order) with ImageMagick when present, then OCR with
/// tesseract (psm 6: single uniform block — tooltip layout).
pub fn ocr_bytes(bytes: &[u8]) -> Result<Vec<String>, String> {
    let stamp = now_ms();
    let dir = std::env::temp_dir();
    let input = dir.join(format!("hs-ocr-{stamp}-in.png"));
    let prepped = dir.join(format!("hs-ocr-{stamp}-out.png"));
    std::fs::write(&input, bytes).map_err(|e| format!("temp write: {e}"))?;

    let ocr_input: PathBuf = match preprocess(&input, &prepped) {
        Ok(true) => prepped.clone(),
        _ => input.clone(),
    };

    let output = Command::new("tesseract")
        .arg(&ocr_input)
        .arg("stdout")
        .arg("--psm")
        .arg("6")
        .args(tessdata_args())
        .output()
        .map_err(|e| format!("tesseract spawn (is it installed?): {e}"))?;

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&prepped);

    if !output.status.success() {
        return Err(format!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .map(|l| l.trim().replace('\u{00A0}', " "))
        .filter(|l| !l.is_empty())
        .collect())
}

/// ImageMagick pass: grayscale → invert (game tooltips are light-on-dark) →
/// 2x upscale (upstream upscales below 1200px width; tesseract reads scaled
/// game text better). Returns false when magick/convert is missing — tesseract
/// handles dark backgrounds acceptably on its own.
fn preprocess(input: &PathBuf, output: &PathBuf) -> Result<bool, String> {
    for prog in ["magick", "convert"] {
        let status = Command::new(prog)
            .arg(input)
            .args(["-colorspace", "Gray", "-negate", "-resize", "200%"])
            .arg(output)
            .output();
        match status {
            Ok(out) if out.status.success() => {
                return Ok(output.exists());
            }
            Ok(_) => continue, // wrong args for this binary; try the other
            Err(_) => continue, // not installed
        }
    }
    Ok(false)
}

/// Point tesseract at our bundled English model when the system one is
/// missing (Arch ships tesseract-data piecemeal).
fn tessdata_args() -> Vec<&'static str> {
    static ARGS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".local/share/hs-plan/tessdata"));
        match dir {
            Some(d) if d.join("eng.traineddata").is_file() => {
                let s = d.to_string_lossy().into_owned();
                // Leak is intentional: process-lifetime static strings.
                let leaked: &'static str = Box::leak(s.into_boxed_str());
                vec!["--tessdata-dir", leaked]
            }
            _ => Vec::new(),
        }
    });
    ARGS.clone()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

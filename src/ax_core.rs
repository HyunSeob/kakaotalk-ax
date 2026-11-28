//! Shared KakaoTalk AX core: 15-tool trait, row classification, snapshot hashing,
//! pagination, search, batch read, and idempotent send engine.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::error::{KaxError, Result};
use crate::macos_ax_logic::{CallBudget, MAX_MEMBER_LIMIT, MAX_ROOM_SCROLL_LIMIT};
use crate::state::{
    default_send_state_root, default_ui_root, ensure_migrated_if_default, load_private_json,
    save_private_json, send_fingerprint, send_lock, sha256_hex, ui_session,
    JOURNAL_SCHEMA_VERSION,
};
use crate::validation::{
    validate_integer, validate_operation_id, validate_output_path, validate_query_text,
    validate_read_title, validate_send_text, validate_snapshot_id_for_read,
    validate_snapshot_id_required, validate_title, validate_wait_seconds,
};

/// Unified 15-tool + build backend interface implemented by both macOS AX and Windows Win32.
pub trait KakaoBackend: Send + Sync {
    fn build(&self, kind: &str) -> Result<Value>;
    fn windows(&self) -> Result<Value>;
    /// `scroll=true` scrolls the room list (macOS) to collect up to `limit` rooms; `folder`
    /// reads one room-list folder (macOS) and switches back afterwards.
    fn rooms(&self, query: &str, scroll: bool, limit: i64, folder: Option<&str>) -> Result<Value>;
    /// The room-list folders with their unread counts (macOS).
    fn folders(&self) -> Result<Value>;
    fn search_rooms(&self, query: &str) -> Result<Value>;
    fn open_room(&self, window_title: &str, require_self: bool) -> Result<Value>;
    fn close_room(&self, window_title: &str) -> Result<Value>;
    /// Members of an open group room, read from its member list (macOS).
    fn members(&self, window_title: &str, limit: i64) -> Result<Value>;
    fn read_messages(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value>;
    /// Reads `limit` rows starting `offset` rows after the oldest loaded row (macOS), so a room
    /// loaded beyond the newest-rows limit can be read from its start. `snapshot_id` is the
    /// token a previous page returned; it changes when older rows were loaded meanwhile.
    fn read_messages_from_oldest(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value> {
        let _ = (window_title, limit, offset, snapshot_id);
        Err(KaxError::validation("from=oldest is supported on macOS only"))
    }
    fn read_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        snapshot_id: &str,
        output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value>;
    fn read_many(
        &self,
        window_titles: &[String],
        limit: i64,
        open_if_needed: bool,
        close_after: bool,
        max_workers: i64,
    ) -> Result<Value>;
    fn load_more(&self, window_title: &str, pages: i64) -> Result<Value>;
    /// Loads older rows until the history start shows or `max_seconds` pass, without reading
    /// the transcript; `progress` gets `{"progress": rows, "message": …}` now and then (macOS).
    fn load_to_start(
        &self,
        window_title: &str,
        max_seconds: i64,
        progress: &mut dyn FnMut(Value),
    ) -> Result<Value> {
        let _ = (window_title, max_seconds, progress);
        Err(KaxError::validation("to_start is supported on macOS only"))
    }
    #[allow(clippy::too_many_arguments)]
    fn search_messages(
        &self,
        window_title: &str,
        query: &str,
        limit: i64,
        load_pages: i64,
        case_sensitive: bool,
        use_app_search: bool,
        max_steps: i64,
        context_rows: i64,
    ) -> Result<Value>;
    fn close_search(&self, window_title: &str) -> Result<Value>;
    fn send_message(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value>;
    fn send_result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value>;
}

/// A room-list folder name as shown on its tab: non-empty, at most 200 bytes, no newlines.
fn validate_folder_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 200 || name.contains(['\n', '\r']) {
        return Err(KaxError::validation("folder must be a folder name shown in the room list"));
    }
    Ok(())
}

/// Stable 24-hex snapshot digest matching Python `snapshot_digest(raw)`.
pub fn snapshot_digest(raw: &Value) -> String {
    let mut map = Map::new();
    for k in [
        "exposedRowCount",
        "processId",
        "rowStart",
        "rows",
        "rowsTruncated",
        "windowTitle",
    ] {
        if let Some(v) = raw.get(k) {
            map.insert(k.to_string(), v.clone());
        }
    }
    // Layout verdicts come from optional geometry reads that may fail on one read and succeed
    // on the next; they must not change the snapshot (or older snapshot ids).
    if let Some(Value::Array(rows)) = map.get_mut("rows") {
        for row in rows.iter_mut() {
            if let Some(o) = row.as_object_mut() {
                o.remove("ownSide");
                o.remove("centered");
            }
        }
    }
    legacy_snapshot_id(&Value::Object(map))
}

// Python json.dumps(..., ensure_ascii=False) separates items with ", " and keys
// from values with ": ". These bytes are part of the public snapshot token.
pub(crate) fn legacy_snapshot_id(value: &Value) -> String {
    use serde::Serialize;
    use serde_json::ser::{Formatter, Serializer};
    use std::io;

    struct LegacySeparators;
    impl Formatter for LegacySeparators {
        fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
            if !first {
                w.write_all(b", ")?;
            }
            Ok(())
        }
        fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
            self.begin_array_value(w, first)
        }
        fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
            w.write_all(b": ")
        }
    }
    let mut bytes = Vec::new();
    value.serialize(&mut Serializer::with_formatter(&mut bytes, LegacySeparators))
        .expect("serializing a JSON value into a vector cannot fail");
    sha256_hex(&bytes)[..24].to_owned()
}

/// Classify one macOS AX row into `text`, `attachment`, or `unclassified`.
/// Never fabricates sender or grouped timestamps.
pub fn ax_row(row: &Value) -> Value {
    // Compiled once: search classifies up to 5000 rows per pass.
    static TIME_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static DATE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let time_re = TIME_RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:(?:오전|오후|AM|PM)\s*)?\d{1,2}:\d{2}(?:\s*(?:AM|PM))?$").unwrap()
    });
    let date_re =
        DATE_RE.get_or_init(|| Regex::new(r"^\d{4}\.\s*\d{1,2}\.\s*\d{1,2}\.?$").unwrap());

    let parts = row
        .get("parts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut bodies: Vec<String> = Vec::new();
    let mut times: Vec<(&Value, String)> = Vec::new();
    let mut links: Vec<String> = Vec::new();

    for part in &parts {
        let role = part.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "AXTextArea" {
            if let Some(val) = part.get("AXValue").and_then(|v| v.as_str()) {
                if !val.is_empty() {
                    bodies.push(val.to_string());
                }
            }
        } else if role == "AXStaticText" {
            if let Some(val) = part.get("AXValue").and_then(|v| v.as_str()) {
                for line in val.lines() {
                    let trimmed = line.trim();
                    if time_re.is_match(trimmed) {
                        times.push((part, trimmed.to_string()));
                    }
                }
            }
        } else if role == "AXLink" {
            let link = part
                .get("AXURL")
                .or_else(|| part.get("AXValue"))
                .or_else(|| part.get("AXTitle"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if let Some(l) = link {
                links.push(l.to_string());
            }
        }
    }

    let mut dates: Vec<String> = Vec::new();
    for (part, _) in &times {
        if let Some(help) = part.get("AXHelp").and_then(|v| v.as_str()) {
            if date_re.is_match(help) {
                dates.push(help.to_string());
            }
        }
    }
    let unique_dates: HashSet<&String> = dates.iter().collect();

    let attachments: Vec<Value> = if bodies.is_empty() {
        parts
            .iter()
            .filter(|p| p.get("role").and_then(|v| v.as_str()) == Some("AXImage"))
            .map(|p| {
                let desc = p
                    .get("AXDescription")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                let title = p
                    .get("AXTitle")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                json!({
                    "type": "image",
                    "textSource": "accessibility_metadata",
                    "description": desc,
                    "title": title,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let kind = if !bodies.is_empty() {
        "text"
    } else if !attachments.is_empty() {
        "attachment"
    } else {
        "unclassified"
    };

    let text_val = if bodies.is_empty() {
        Value::Null
    } else {
        Value::String(bodies.join("\n"))
    };
    let time_val = if times.len() == 1 {
        Value::String(times[0].1.clone())
    } else {
        Value::Null
    };
    let date_val = if unique_dates.len() == 1 {
        Value::String(dates[0].clone())
    } else {
        Value::Null
    };

    let mut item = json!({
        "rowIndex": row.get("rowIndex").cloned().unwrap_or(json!(0)),
        "kind": kind,
        "attachments": attachments,
        "links": links,
        "text": text_val,
        "sender": Value::Null,
        "timeText": time_val,
        "dateText": date_val,
        "axParts": parts,
    });
    if let Some(obj) = item.as_object_mut() {
        crate::macos_rows::enrich_row(row, obj);
    }
    item
}

/// Low-level driver for macOS AX operations (implemented by live macOS AX or test fixtures).
pub trait AxDriver: Send + Sync {
    fn build(&self, kind: &str) -> Result<()> {
        if !matches!(kind, "send" | "read" | "control") {
            return Err(KaxError::validation("Unknown native bridge kind"));
        }
        Ok(())
    }

    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value>;

    fn send_with_identity(
        &self,
        titles: &[String],
        text: &str,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Result<Value> {
        let _ = (require_self, window_identity);
        self.call("send", titles, Some(text))
    }

    fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value>;

    /// Rows from `start` (or the last `last` rows) to the end of the transcript, with part
    /// `frame`s, for send confirmation. `None`: not supported, confirm with full reads.
    fn read_tail(&self, window_title: &str, start: Option<usize>, last: usize) -> Option<Result<Value>> {
        let _ = (window_title, start, last);
        None
    }

    /// Transcript rows `start..start + count` counted from the oldest loaded row, with sender
    /// layout facts. `None`: not supported.
    fn read_range(&self, window_title: &str, start: usize, count: usize) -> Option<Result<Value>> {
        let _ = (window_title, start, count);
        None
    }

    /// The first `rows` transcript rows (the oldest loaded), to recognise the history start
    /// while loading older rows. `None`: not supported.
    fn read_head(&self, window_title: &str, rows: usize) -> Option<Result<Value>> {
        let _ = (window_title, rows);
        None
    }

    fn capture_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        expected_parts: &[Value],
        expected_count: usize,
        ocr: bool,
    ) -> Result<Value>;
}

/// Platform-agnostic send engine driver (implemented by macOS `AxEngine` and Windows `WindowsAx`).
pub trait SendDriver {
    fn source(&self) -> &'static str;
    fn confirmation_source(&self) -> &'static str;
    fn unknown_reason(&self) -> &'static str;
    fn ui_root(&self) -> &Path;
    fn state_root(&self) -> &Path;

    fn text_hash(&self, text: &str) -> String {
        sha256_hex(text)
    }

    /// The message body as the transcript shows it after sending (macOS cuts long ones).
    fn displayed_text(&self, text: &str) -> String {
        text.to_string()
    }

    fn body_hashes(&self, read_result: &Value) -> Vec<String> {
        read_result
            .get("items")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|row| row.get("text").and_then(|t| t.as_str()))
                    .map(|t| self.text_hash(t))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn prepare(&self, window_title: &str, require_self: bool) -> Result<(Value, Map<String, Value>)>;
    fn build_for_send(&self) -> Result<()> {
        Ok(())
    }
    fn dispatch(&self, record: &Map<String, Value>, text: &str) -> Result<Value>;
    fn read_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value>;
    /// Only the end of the transcript (see [`AxDriver::read_tail`]); `None` keeps the full-read
    /// confirmation.
    fn read_tail_for_confirmation(
        &self,
        record: &Map<String, Value>,
        start: Option<usize>,
    ) -> Option<Result<Value>> {
        let _ = (record, start);
        None
    }
    fn inspect_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value>;
}

/// KakaoTalk Mac shows at most this many UTF-16 code units of a message in the transcript.
pub const MACOS_SHOWN_TEXT_UNITS: usize = 500;

/// Characters that never start a new composed character (they extend the previous one):
/// combining marks, variation selectors, emoji skin tones, ZWJ, keycap and tag characters.
fn extends_previous(c: char) -> bool {
    matches!(u32::from(c),
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0x200D
        | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF)
}

/// A message body as the macOS transcript shows it: a text over [`MACOS_SHOWN_TEXT_UNITS`]
/// UTF-16 code units (an emoji counts two) is cut there, but a composed character that starts
/// before the limit is kept whole (`👍🏻` starting at unit 499 shows to unit 503), then `…`
/// follows. Measured on 26.10.0 with Korean and emoji texts.
pub fn macos_displayed_text(text: &str) -> String {
    if text.encode_utf16().count() <= MACOS_SHOWN_TEXT_UNITS {
        return text.to_string();
    }
    let mut shown = String::new();
    let mut units = 0;
    let mut after_zwj = false;
    for c in text.chars() {
        let starts_new = !extends_previous(c) && !after_zwj;
        if starts_new && units >= MACOS_SHOWN_TEXT_UNITS {
            break;
        }
        shown.push(c);
        units += c.len_utf16();
        after_zwj = c == '\u{200D}';
    }
    shown.push('…');
    shown
}

/// Journal `confirmMode` of sends confirmed from the transcript end (older records have none
/// and keep the full-read confirmation).
const CONFIRM_MODE_TAIL: &str = "tail_v1";

/// One transcript row as send confirmation sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct TailRow {
    /// Order key for matching the baseline: `t:<text hash>` or `a` (attachment).
    pub sig: String,
    pub text_hash: Option<String>,
    /// Laid out as the user's own message (right aligned, no avatar); `None`: no geometry.
    pub own_side: Option<bool>,
    /// A lone image without description or title: how a single emoji renders.
    pub bare_image: bool,
}

fn part_frame(part: &Value) -> Option<[f64; 4]> {
    let a = part.get("frame")?.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (slot, v) in out.iter_mut().zip(a) {
        *slot = v.as_f64()?;
    }
    Some(out)
}

/// Right edge gap of the user's own bubbles is 12–18pt; others' bubbles start after the
/// avatar column (60pt) and an avatar button sits within the first 50pt.
const OWN_SIDE_MAX_RIGHT_GAP: f64 = 24.0;
const OTHERS_BUBBLE_MAX_LEFT: f64 = 64.0;
const AVATAR_MAX_LEFT: f64 = 50.0;
/// A lone emoji renders as an 86pt square image (emoticons/stickers are larger, e.g. 122pt).
const EMOJI_IMAGE_SIZE: f64 = 86.0;
const EMOJI_IMAGE_TOLERANCE: f64 = 4.0;

fn own_side(parts: &[Value]) -> Option<bool> {
    let role = |p: &Value| p.get("role").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let row = parts.iter().find(|p| role(p) == "AXRow").and_then(part_frame)?;
    let msg = parts
        .iter()
        .find(|p| matches!(role(p).as_str(), "AXTextArea" | "AXImage"))
        .and_then(part_frame)?;
    let avatar = parts
        .iter()
        .filter(|p| role(p) == "AXButton")
        .filter_map(part_frame)
        .any(|b| b[0] - row[0] < AVATAR_MAX_LEFT);
    let right_gap = (row[0] + row[2]) - (msg[0] + msg[2]);
    let left = msg[0] - row[0];
    Some(!avatar && right_gap <= OWN_SIDE_MAX_RIGHT_GAP && left > OTHERS_BUBBLE_MAX_LEFT)
}

/// Message rows of a tail read. Rows without message parts (the spacer under the last
/// message, inserted messages go above it) and unclassified rows are dropped.
pub fn tail_rows(raw: &Value, hash: impl Fn(&str) -> String) -> Vec<TailRow> {
    let rows = raw.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let parts = row.get("parts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let content: Vec<&Value> = parts
            .iter()
            .filter(|p| !matches!(p.get("role").and_then(|v| v.as_str()), Some("AXRow" | "AXCell")))
            .collect();
        if content.is_empty() {
            continue;
        }
        let parsed = ax_row(row);
        let text_hash = parsed.get("text").and_then(|v| v.as_str()).map(&hash);
        let images: Vec<&&Value> = content
            .iter()
            .filter(|p| p.get("role").and_then(|v| v.as_str()) == Some("AXImage"))
            .collect();
        // Attachments are matched by kind only (a loading preview changes its parts);
        // unclassified rows (date separators, notices) are left out of the order entirely so
        // one appearing or disappearing does not break the match.
        let sig = match (&text_hash, parsed.get("kind").and_then(|v| v.as_str())) {
            (Some(h), _) => format!("t:{h}"),
            (None, Some("attachment")) => "a".to_string(),
            _ => continue,
        };
        let blank = |p: &Value, k: &str| p.get(k).and_then(|v| v.as_str()).map_or(true, str::is_empty);
        let emoji_sized = part_frame(images.first().copied().copied().unwrap_or(&Value::Null))
            .is_some_and(|f| {
                (f[2] - EMOJI_IMAGE_SIZE).abs() <= EMOJI_IMAGE_TOLERANCE
                    && (f[3] - EMOJI_IMAGE_SIZE).abs() <= EMOJI_IMAGE_TOLERANCE
            });
        let bare_image = text_hash.is_none()
            && images.len() == 1
            && emoji_sized
            && blank(images[0], "AXDescription")
            && blank(images[0], "AXTitle")
            && !content
                .iter()
                .any(|p| p.get("role").and_then(|v| v.as_str()) == Some("AXButton"));
        out.push(TailRow { sig, text_hash, own_side: own_side(&parts), bare_image });
    }
    out
}

/// Whether `text` is one emoji (with skin tone, variation selector, ZWJ sequence, keycap or
/// flag), which KakaoTalk renders as a large image instead of text.
pub fn is_single_emoji(text: &str) -> bool {
    fn pictographic(c: u32) -> bool {
        matches!(c,
            0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x2199 | 0x21A9..=0x21AA
            | 0x231A..=0x231B | 0x2328 | 0x23CF | 0x23E9..=0x23F3 | 0x23F8..=0x23FA | 0x24C2
            | 0x25AA..=0x25AB | 0x25B6 | 0x25C0 | 0x25FB..=0x25FE | 0x2600..=0x27BF
            | 0x2934..=0x2935 | 0x2B05..=0x2B07 | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x3030
            | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1F1E5 | 0x1F200..=0x1F3FA | 0x1F400..=0x1FAFF)
    }
    // Measured: ©, ™ and ↔ alone render as text, ☀ (U+2600) and 1️⃣ as an image; symbols
    // below U+2300 count only with the emoji selector FE0F.
    const TEXT_STYLE_SYMBOLS_END: u32 = 0x2300;
    let chars: Vec<u32> = text.chars().map(u32::from).collect();
    if chars.is_empty() || chars.len() > 16 {
        return false;
    }
    // Two regional indicators: one flag.
    if chars.len() == 2 && chars.iter().all(|c| (0x1F1E6..=0x1F1FF).contains(c)) {
        return true;
    }
    // Keycap: [0-9#*] FE0F? 20E3
    if matches!(chars[0], 0x23 | 0x2A | 0x30..=0x39) {
        return matches!(chars[1..], [0x20E3] | [0xFE0F, 0x20E3]);
    }
    // Pictographs joined by ZWJ, each followed only by modifiers or selectors.
    let mut expect_base = true;
    let mut last_base = 0u32;
    let mut selected = false;
    for &c in &chars {
        if expect_base {
            if !pictographic(c) {
                return false;
            }
            expect_base = false;
            last_base = c;
            selected = false;
        } else if c == 0x200D {
            if last_base < TEXT_STYLE_SYMBOLS_END && !selected {
                return false;
            }
            expect_base = true;
        } else if c == 0xFE0F {
            selected = true;
        } else if !matches!(c, 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F) {
            return false;
        }
    }
    !expect_base && (last_base >= TEXT_STYLE_SYMBOLS_END || selected)
}

/// How a transcript end read after sending compares with the baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum TailVerdict {
    /// The sent text appears once among the new rows.
    Text,
    /// A single emoji: one new own-side bare image and no matching text.
    EmojiImage,
    /// Not established (yet).
    Pending,
}

/// Compares the rows read from the baseline start to the end with the baseline.
///
/// The baseline rows must still lead the transcript in the same order (nothing removed or
/// rewritten), the row count must have grown, and among the new rows the sent text must occur
/// exactly once. If other new messages also contain text, the sent one must be laid out as the
/// user's own message. A single emoji may instead render as one new own-side bare image.
pub fn tail_verdict(
    baseline_sigs: &[String],
    baseline_count: usize,
    after_count: usize,
    after: &[TailRow],
    text_hash: &str,
    single_emoji: bool,
) -> TailVerdict {
    if after_count <= baseline_count || after.len() < baseline_sigs.len() {
        return TailVerdict::Pending;
    }
    if after.iter().zip(baseline_sigs).any(|(row, sig)| row.sig != *sig) {
        return TailVerdict::Pending;
    }
    let new_rows = &after[baseline_sigs.len()..];
    let matches: Vec<&TailRow> = new_rows
        .iter()
        .filter(|r| r.text_hash.as_deref() == Some(text_hash))
        .collect();
    let other_texts = new_rows
        .iter()
        .filter(|r| r.text_hash.is_some() && r.text_hash.as_deref() != Some(text_hash))
        .count();
    // Someone else's row with the same text is never ours; with other new texts around, ours
    // must be positively on the own side.
    let side_ok = match matches.first().map(|m| m.own_side) {
        Some(Some(false)) => false,
        Some(Some(true)) => true,
        _ => other_texts == 0,
    };
    if matches.len() == 1 && side_ok {
        return TailVerdict::Text;
    }
    if single_emoji && matches.is_empty() {
        let bare: Vec<&TailRow> = new_rows.iter().filter(|r| r.bare_image).collect();
        if bare.len() == 1 && bare[0].own_side == Some(true) {
            return TailVerdict::EmojiImage;
        }
    }
    TailVerdict::Pending
}

/// Sets `elapsedMs` to the wall time of the whole call.
fn with_elapsed(mut out: Value, started: Instant) -> Value {
    if let Some(obj) = out.as_object_mut() {
        let ms = (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0;
        obj.insert("elapsedMs".to_string(), json!(ms));
    }
    out
}

pub struct SendEngine<'a, S: SendDriver> {
    driver: &'a S,
}

impl<'a, S: SendDriver> SendEngine<'a, S> {
    pub fn new(driver: &'a S) -> Self {
        Self { driver }
    }

    pub fn record_path(&self, operation_id: &str) -> Result<PathBuf> {
        validate_operation_id(operation_id)?;
        Ok(self
            .driver
            .state_root()
            .join(format!("{}.json", sha256_hex(operation_id))))
    }

    fn output(&self, record: &Map<String, Value>, extra: Map<String, Value>) -> Value {
        let mut out = Map::new();
        out.insert(
            "status".to_string(),
            record.get("phase").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "operationId".to_string(),
            record.get("operationId").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "windowTitle".to_string(),
            record.get("windowTitle").cloned().unwrap_or(Value::Null),
        );
        out.insert("source".to_string(), json!(self.driver.source()));
        out.insert(
            "confirmationSource".to_string(),
            json!(self.driver.confirmation_source()),
        );
        out.insert("databaseCalls".to_string(), json!(0));
        out.insert("retrySafe".to_string(), json!(false));
        out.insert(
            "reason".to_string(),
            record.get("reason").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "native".to_string(),
            record.get("native").cloned().unwrap_or(Value::Null),
        );
        for (k, v) in extra {
            out.insert(k, v);
        }
        Value::Object(out)
    }

    fn confirm(
        &self,
        path: &Path,
        record: &mut Map<String, Value>,
        wait_seconds: f64,
    ) -> Result<Value> {
        let until = Instant::now() + Duration::from_secs_f64(wait_seconds);
        let strings = |key: &str| -> Vec<String> {
            record
                .get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(ToString::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let baseline_tail = strings("baselineTailHashes");
        let baseline_sigs = strings("baselineSigs");
        let text_hash = record
            .get("textHash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let baseline_count = record
            .get("baselineTextCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tail_mode =
            record.get("confirmMode").and_then(|v| v.as_str()) == Some(CONFIRM_MODE_TAIL);
        let baseline_rows = record
            .get("baselineRowCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let baseline_start = record
            .get("baselineRowStart")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let single_emoji = record.get("singleEmoji") == Some(&Value::Bool(true));

        let mut expected = baseline_tail;
        expected.push(text_hash.clone());

        loop {
            let reason = if tail_mode {
                match self
                    .driver
                    .read_tail_for_confirmation(record, Some(baseline_start))
                {
                    Some(Ok(after)) if after.get("status").and_then(|v| v.as_str()) == Some("ok") => {
                        let after_count = after
                            .get("exposedRowCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize;
                        let rows = tail_rows(&after, |t| self.driver.text_hash(t));
                        match tail_verdict(
                            &baseline_sigs,
                            baseline_rows,
                            after_count,
                            &rows,
                            &text_hash,
                            single_emoji,
                        ) {
                            TailVerdict::Text => Some("new_text_occurrence_and_empty_composer"),
                            TailVerdict::EmojiImage => Some("new_emoji_image_row_and_empty_composer"),
                            TailVerdict::Pending => None,
                        }
                    }
                    _ => None,
                }
            } else if let Ok(after) = self.driver.read_for_confirmation(record) {
                let hashes = self.driver.body_hashes(&after);
                let after_ok = after.get("status").and_then(|v| v.as_str()) == Some("ok");
                let after_count = after
                    .get("textMessageCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let tail_matches = hashes.len() >= expected.len()
                    && hashes[hashes.len() - expected.len()..] == expected[..];
                (after_ok && after_count == baseline_count + 1 && tail_matches)
                    .then_some("new_text_occurrence_and_empty_composer")
            } else {
                None
            };

            if let Some(reason) = reason {
                if let Ok(inspected) = self.driver.inspect_for_confirmation(record) {
                    if inspected.get("status").and_then(|v| v.as_str()) == Some("ready") {
                        record.insert("phase".to_string(), json!("observed_in_ax"));
                        record.insert("reason".to_string(), json!(reason));
                        save_private_json(path, &Value::Object(record.clone()))?;
                        return Ok(self.output(record, Map::new()));
                    }
                }
            }

            let now = Instant::now();
            if now >= until {
                record.insert("phase".to_string(), json!("outcome_unknown"));
                record.insert("reason".to_string(), json!(self.driver.unknown_reason()));
                save_private_json(path, &Value::Object(record.clone()))?;
                return Ok(self.output(record, Map::new()));
            }
            let remaining = until.saturating_duration_since(now);
            std::thread::sleep(remaining.min(Duration::from_millis(150)));
        }
    }

    /// Sends once per `operation_id` and confirms in AX; `elapsedMs` is the whole call.
    pub fn send(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        let started = Instant::now();
        let out = self.send_inner(window_title, text, operation_id, wait_seconds, require_self)?;
        Ok(with_elapsed(out, started))
    }

    fn send_inner(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_wait_seconds(wait_seconds)?;
        validate_send_text(text)?;
        let path = self.record_path(operation_id)?;
        let fingerprint = send_fingerprint(window_title, text, require_self);

        let _ui = ui_session(self.driver.ui_root())?;
        let _send = send_lock(self.driver.state_root())?;
        ensure_migrated_if_default(self.driver.state_root())?;
        crate::state::check_migration_blocked(self.driver.state_root(), operation_id)?;

        match load_private_json(&path)? {
            None => {}
            Some(Value::Object(mut existing)) => {
                let stored_fp = existing
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if stored_fp != fingerprint {
                    return Err(KaxError::validation(
                        "operation_id is already bound to another target or message",
                    ));
                }
                let phase = existing
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut extra = Map::new();
                extra.insert("deduplicated".to_string(), json!(true));
                extra.insert("nativeSendCalls".to_string(), json!(0));
                if matches!(phase, "observed_in_ax" | "blocked") {
                    return Ok(self.output(&existing, extra));
                }
                let mut confirmed = self.confirm(&path, &mut existing, wait_seconds)?;
                if let Some(obj) = confirmed.as_object_mut() {
                    obj.insert("deduplicated".to_string(), json!(true));
                    obj.insert("nativeSendCalls".to_string(), json!(0));
                }
                return Ok(confirmed);
            }
            Some(_) => {
                return Err(KaxError::validation(
                    "Corrupt send journal record; inspect journal before retrying",
                ));
            }
        }

        let (opened, target) = self.driver.prepare(window_title, require_self)?;
        if opened.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = opened.as_object().cloned().unwrap_or_default();
            out.insert("operationId".to_string(), json!(operation_id));
            out.insert("retrySafe".to_string(), json!(false));
            return Ok(Value::Object(out));
        }

        let mut probe_record = target.clone();
        probe_record.insert("windowTitle".to_string(), json!(window_title));
        let mut baseline_fields = Map::new();
        let tail = self
            .driver
            .read_tail_for_confirmation(&probe_record, None)
            .transpose()?
            // A transcript exposing no AXRows cannot be read from the end: full reads.
            .filter(|t| t.get("reason").and_then(|v| v.as_str()) != Some("tail_rows_unsupported"));
        let (before, tail_mode) = match tail {
            Some(t) => (t, true),
            None => (self.driver.read_for_confirmation(&probe_record)?, false),
        };
        if before.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = before.as_object().cloned().unwrap_or_default();
            out.insert("operationId".to_string(), json!(operation_id));
            out.insert("retrySafe".to_string(), json!(false));
            return Ok(Value::Object(out));
        }
        if tail_mode {
            // Transcript end read: confirm against these rows only.
            let rows = tail_rows(&before, |t| self.driver.text_hash(t));
            let sigs: Vec<String> = rows.into_iter().map(|r| r.sig).collect();
            baseline_fields.insert("confirmMode".to_string(), json!(CONFIRM_MODE_TAIL));
            baseline_fields.insert("baselineSigs".to_string(), json!(sigs));
            baseline_fields.insert(
                "baselineRowCount".to_string(),
                before.get("exposedRowCount").cloned().unwrap_or(json!(0)),
            );
            baseline_fields.insert(
                "baselineRowStart".to_string(),
                before.get("rowStart").cloned().unwrap_or(json!(0)),
            );
            baseline_fields.insert("singleEmoji".to_string(), json!(is_single_emoji(text)));
        } else {
            let all_hashes = self.driver.body_hashes(&before);
            let start = all_hashes.len().saturating_sub(64);
            baseline_fields.insert(
                "baselineTailHashes".to_string(),
                json!(all_hashes[start..].to_vec()),
            );
            baseline_fields.insert(
                "baselineTextCount".to_string(),
                json!(before.get("textMessageCount").and_then(|v| v.as_u64()).unwrap_or(0)),
            );
        }

        self.driver.build_for_send()?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let mut record = Map::new();
        record.insert("schemaVersion".to_string(), json!(JOURNAL_SCHEMA_VERSION));
        record.insert("operationId".to_string(), json!(operation_id));
        record.insert("windowTitle".to_string(), json!(window_title));
        record.insert("fingerprint".to_string(), json!(fingerprint));
        record.insert(
            "textHash".to_string(),
            json!(self.driver.text_hash(&self.driver.displayed_text(text))),
        );
        for (k, v) in baseline_fields {
            record.insert(k, v);
        }
        record.insert("createdAt".to_string(), json!(created_at));
        record.insert("phase".to_string(), json!("dispatching"));
        for (k, v) in target {
            record.insert(k, v);
        }
        save_private_json(&path, &Value::Object(record.clone()))?;

        let native = match self.driver.dispatch(&record, text) {
            Ok(v) => v,
            Err(err) => json!({
                "status": "bridge_result_unknown",
                "errorType": err.error_type(),
                "dispatchAttempted": Value::Null,
            }),
        };
        record.insert("native".to_string(), native.clone());

        if native.get("dispatchAttempted") == Some(&Value::Bool(false)) {
            let reason = native
                .get("reason")
                .or_else(|| native.get("status"))
                .cloned()
                .unwrap_or(Value::Null);
            record.insert("phase".to_string(), json!("blocked"));
            record.insert("reason".to_string(), reason);
            save_private_json(&path, &Value::Object(record.clone()))?;
            let mut extra = Map::new();
            extra.insert("nativeSendCalls".to_string(), json!(1));
            return Ok(self.output(&record, extra));
        }

        let mut confirmed = self.confirm(&path, &mut record, wait_seconds)?;
        if let Some(obj) = confirmed.as_object_mut() {
            obj.insert("nativeSendCalls".to_string(), json!(1));
        }
        Ok(confirmed)
    }

    pub fn result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        let started = Instant::now();
        let out = self.result_inner(operation_id, wait_seconds)?;
        Ok(with_elapsed(out, started))
    }

    fn result_inner(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        validate_wait_seconds(wait_seconds)?;
        let path = self.record_path(operation_id)?;

        let _ui = ui_session(self.driver.ui_root())?;
        let _send = send_lock(self.driver.state_root())?;
        ensure_migrated_if_default(self.driver.state_root())?;
        crate::state::check_migration_blocked(self.driver.state_root(), operation_id)?;

        let mut record = match load_private_json(&path)? {
            None => return Err(KaxError::validation("Unknown AX operation_id")),
            Some(Value::Object(obj)) => obj,
            Some(_) => {
                return Err(KaxError::validation(
                    "Corrupt send journal record; inspect journal before retrying",
                ));
            }
        };
        let phase = record.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(phase, "observed_in_ax" | "blocked") {
            return Ok(self.output(&record, Map::new()));
        }
        self.confirm(&path, &mut record, wait_seconds)
    }
}

thread_local! {
    /// Set while `send_message` / `send_result` run on this thread.
    static SENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks this thread as sending until dropped.
struct SendingGuard {
    outer: bool,
}

impl SendingGuard {
    fn begin() -> Self {
        Self { outer: SENDING.with(|s| s.replace(true)) }
    }
}

impl Drop for SendingGuard {
    fn drop(&mut self) {
        SENDING.with(|s| s.set(self.outer));
    }
}

/// Wait before retrying a transient window lookup failure.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(300);

/// Shown with `accessibility_permission_required`.
pub const ACCESSIBILITY_PERMISSION_HINT: &str = "시스템 설정 > 개인정보 보호 및 보안 > 손쉬운 사용(일부 macOS에서는 '기기 제어 및 데이터 접근')에서 executablePath의 kax 또는 이를 실행한 앱(Claude 등)을 켜세요. 켠 뒤에도 같으면 그 앱을 다시 시작하세요. 이미 떠 있는 MCP 프로세스에는 권한 변경이 바로 반영되지 않을 수 있습니다.";

fn blocked_reason(v: &Value) -> Option<&str> {
    (v.get("status").and_then(|s| s.as_str()) == Some("blocked"))
        .then(|| v.get("reason").and_then(|r| r.as_str()))
        .flatten()
}

/// Runs `call`, retrying once after a short wait when it failed to find the main window
/// (`main_window_not_unique`, if `retry_main`) or the chat window (if `retry_window`,
/// (`target_room_window_not_open`) without any UI action; adds the permission hint.
fn with_recovery(
    retry_main: bool,
    retry_window: bool,
    call: impl Fn() -> Result<Value>,
) -> Result<Value> {
    if SENDING.with(|s| s.get()) {
        // Sending (open, baseline and confirmation reads) keeps its own timing and results.
        return call();
    }
    let mut out = call()?;
    let transient = |v: &Value| {
        let untouched = v.get("uiActions").and_then(|u| u.as_u64()).unwrap_or(0) == 0;
        untouched
            && match blocked_reason(v) {
                Some("main_window_not_unique") => retry_main,
                Some("target_room_window_not_open") => retry_window,
                _ => false,
            }
    };
    if transient(&out) {
        let first = blocked_reason(&out).unwrap_or("").to_string();
        std::thread::sleep(TRANSIENT_RETRY_DELAY);
        out = call()?;
        if let Some(obj) = out.as_object_mut() {
            obj.insert("retriedAfter".to_string(), json!(first));
        }
    }
    if blocked_reason(&out) == Some("accessibility_permission_required") {
        if let Some(obj) = out.as_object_mut() {
            obj.insert("hint".to_string(), json!(ACCESSIBILITY_PERMISSION_HINT));
            obj.insert(
                "executablePath".to_string(),
                json!(std::env::current_exe().ok().map(|p| p.display().to_string())),
            );
        }
    }
    Ok(out)
}

/// High-level macOS AX engine wrapping an `AxDriver`.
pub struct AxEngine<D: AxDriver + 'static> {
    pub driver: Arc<D>,
    pub ui_root: PathBuf,
    pub state_root: PathBuf,
}

impl<D: AxDriver + 'static> AxEngine<D> {
    pub fn new(driver: Arc<D>) -> Self {
        Self {
            driver,
            ui_root: default_ui_root(),
            state_root: default_send_state_root(),
        }
    }

    pub fn with_roots(driver: Arc<D>, ui_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            driver,
            ui_root,
            state_root,
        }
    }

    /// A driver call with the engine's recovery: one retry of a transient lookup failure that
    /// changed nothing (the main or chat window briefly not found), and a hint on the
    /// accessibility permission error.
    fn driver_call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        // `inspect` belongs to sending, which keeps its own behavior.
        let reads = matches!(action, "read" | "read_all" | "windows");
        with_recovery(reads, reads && action != "windows", || self.driver.call(action, titles, text))
    }

    fn driver_control(&self, action: &str, args: &Map<String, Value>) -> Result<Value> {
        let retry_window = matches!(action, "load_older" | "members");
        with_recovery(true, retry_window, || self.driver.control(action, args))
    }

    fn control_locked(&self, action: &str, args: Map<String, Value>) -> Result<Value> {
        let _ui = ui_session(&self.ui_root)?;
        self.driver_control(action, &args)
    }

    fn call_checked(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        if matches!(action, "send" | "raise") {
            let _ui = ui_session(&self.ui_root)?;
            self.driver.call(action, titles, text)
        } else {
            self.driver_call(action, titles, text)
        }
    }

    fn send_checked(
        &self,
        titles: &[String],
        text: &str,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Result<Value> {
        let _ui = ui_session(&self.ui_root)?;
        self.driver
            .send_with_identity(titles, text, require_self, window_identity)
    }
}

struct AxSendAdapter<'a, D: AxDriver + 'static> {
    engine: &'a AxEngine<D>,
}

impl<'a, D: AxDriver + 'static> SendDriver for AxSendAdapter<'a, D> {
    fn source(&self) -> &'static str {
        "macos_accessibility"
    }

    fn confirmation_source(&self) -> &'static str {
        "macos_accessibility"
    }

    fn unknown_reason(&self) -> &'static str {
        "ax_confirmation_not_established"
    }

    fn ui_root(&self) -> &Path {
        &self.engine.ui_root
    }

    fn state_root(&self) -> &Path {
        &self.engine.state_root
    }

    fn prepare(&self, window_title: &str, require_self: bool) -> Result<(Value, Map<String, Value>)> {
        let opened = self.engine.open_room(window_title, require_self)?;
        let mut target = Map::new();
        if opened.get("status").and_then(|v| v.as_str()) == Some("ok") {
            let actual = opened
                .get("windowTitle")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !actual.is_empty() && actual != window_title {
                return Ok((
                    json!({
                        "status": "blocked",
                        "source": "macos_accessibility",
                        "reason": "exact_window_title_required",
                        "databaseCalls": 0,
                    }),
                    Map::new(),
                ));
            }
            if require_self {
                target.insert("requireSelf".to_string(), json!(true));
            }
            if let Some(identity) = opened
                .get("windowIdentity")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                target.insert("windowIdentity".to_string(), json!(identity));
            }
        }
        Ok((opened, target))
    }

    fn build_for_send(&self) -> Result<()> {
        self.engine.driver.build("send")
    }

    fn displayed_text(&self, text: &str) -> String {
        macos_displayed_text(text)
    }

    fn dispatch(&self, record: &Map<String, Value>, text: &str) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let require_self = record.get("requireSelf") == Some(&Value::Bool(true));
        let window_identity = record.get("windowIdentity").and_then(|v| v.as_str());
        self.engine
            .send_checked(&[title], text, require_self, window_identity)
    }

    fn read_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.engine.read_messages(title, 500, 0, "")
    }

    fn read_tail_for_confirmation(
        &self,
        record: &Map<String, Value>,
        start: Option<usize>,
    ) -> Option<Result<Value>> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let _budget = CallBudget::begin();
        self.engine
            .driver
            .read_tail(title, start, crate::macos_ax_logic::SEND_TAIL_ROWS)
    }

    fn inspect_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        self.engine.call_checked("inspect", &[title], None)
    }
}

impl<D: AxDriver + 'static> KakaoBackend for AxEngine<D> {
    fn build(&self, kind: &str) -> Result<Value> {
        let started = Instant::now();
        self.driver.build(kind)?;
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
        Ok(json!({
            "status": "built",
            "kind": kind,
            "elapsedMs": elapsed,
        }))
    }

    fn windows(&self) -> Result<Value> {
        self.call_checked("windows", &[], None)
    }

    fn rooms(&self, query: &str, scroll: bool, limit: i64, folder: Option<&str>) -> Result<Value> {
        validate_query_text(query, true)?;
        validate_integer(limit, "limit", 1, MAX_ROOM_SCROLL_LIMIT)?;
        if let Some(name) = folder {
            validate_folder_name(name)?;
        }
        let _budget = CallBudget::begin();
        let mut args = Map::new();
        if scroll {
            args.insert("scroll".to_string(), json!(true));
            args.insert("limit".to_string(), json!(limit));
        }
        if let Some(name) = folder {
            args.insert("folder".to_string(), json!(name));
        }
        let result = self.control_locked("rooms", args)?;
        if result.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(result);
        }
        let items = result
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let needle = caseless::default_case_fold_str(query);
        let filtered: Vec<Value> = items
            .iter()
            .filter(|item| {
                item.get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| caseless::default_case_fold_str(n).contains(&needle))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let mut out = result.as_object().cloned().unwrap_or_default();
        out.insert("query".to_string(), json!(query));
        out.insert("total".to_string(), json!(filtered.len()));
        out.insert("totalExposed".to_string(), json!(items.len()));
        out.insert("items".to_string(), Value::Array(filtered));
        Ok(Value::Object(out))
    }

    fn folders(&self) -> Result<Value> {
        self.control_locked("folders", Map::new())
    }

    fn search_rooms(&self, query: &str) -> Result<Value> {
        validate_query_text(query, true)?;
        let mut args = Map::new();
        args.insert("query".to_string(), json!(query));
        self.control_locked("search_rooms", args)
    }

    fn open_room(&self, window_title: &str, require_self: bool) -> Result<Value> {
        validate_title(window_title)?;
        let _ui = ui_session(&self.ui_root)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        args.insert("requireSelf".to_string(), json!(require_self));
        let result = self.driver_control("open", &args)?;
        if result.get("reason").and_then(|v| v.as_str()) != Some("room_not_in_exposed_list") {
            return Ok(result);
        }
        let previous = self.driver_control("rooms", &Map::new())?;
        if previous.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(previous);
        }
        let prev_query = previous
            .get("searchQuery")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Whether the room search bar was open before (older drivers do not say: treat a typed
        // query as open, an empty one as closed).
        let prev_open = previous
            .get("searchOpen")
            .and_then(|v| v.as_bool())
            .unwrap_or(!prev_query.is_empty());
        // Search by the prefix-free room name: the app's room search matches the name, not the
        // "오픈채팅, " / "팀채팅, " type label shown in the list.
        let search = self.search_rooms(crate::macos_ax_logic::room_base_name(window_title))?;
        let open_res = if search.get("status").and_then(|v| v.as_str()) == Some("ok") {
            let first = self.driver_control("open", &args)?;
            if first.get("reason").and_then(|v| v.as_str()) == Some("room_not_in_exposed_list") {
                self.open_room_in_scrolled_results(window_title, &args, first)?
            } else {
                first
            }
        } else {
            search
        };
        let restored = self.restore_room_search(&prev_query, prev_open).unwrap_or_else(|e| {
            json!({"status": "failed", "errorType": e.error_type()})
        });
        let mut out = open_res.as_object().cloned().unwrap_or_default();
        // The room exists only with another type than the one the request named.
        if out.get("reason").and_then(|v| v.as_str()) == Some("room_not_in_exposed_list")
            && out.get("roomTypeMismatch") == Some(&Value::Bool(true))
        {
            out.insert("reason".to_string(), json!("room_type_mismatch"));
        }
        out.insert("roomSearchUsed".to_string(), json!(true));
        // Restored means the same query and the same open/closed bar as before.
        let restored_ok = restored.get("status").and_then(|v| v.as_str()) == Some("ok")
            && restored.get("searchQuery").and_then(|v| v.as_str()).unwrap_or("") == prev_query
            && restored.get("searchOpen").and_then(|v| v.as_bool()).unwrap_or(prev_open) == prev_open;
        out.insert("roomSearchRestored".to_string(), json!(restored_ok));
        out.insert("roomSearchWasOpen".to_string(), json!(prev_open));
        Ok(Value::Object(out))
    }

    fn close_room(&self, window_title: &str) -> Result<Value> {
        validate_title(window_title)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        self.control_locked("close", args)
    }

    fn members(&self, window_title: &str, limit: i64) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(limit, "limit", 1, MAX_MEMBER_LIMIT)?;
        let _budget = CallBudget::begin();
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        args.insert("limit".to_string(), json!(limit));
        self.control_locked("members", args)
    }

    fn read_messages(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value> {
        validate_read_title(window_title)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(offset, "offset", 0, 1000)?;
        validate_snapshot_id_for_read(snapshot_id, offset)?;
        let _budget = CallBudget::begin();

        let raw = self.call_checked("read", &[window_title.to_string()], None)?;
        Ok(read_page_from_raw(raw, window_title, limit, offset, snapshot_id))
    }

    fn read_messages_from_oldest(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value> {
        validate_read_title(window_title)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(offset, "offset", 0, 10_000_000)?;
        let _budget = CallBudget::begin();
        let unsupported = || {
            json!({
                "status": "blocked",
                "source": "macos_accessibility",
                "reason": "read_from_oldest_unsupported",
                "databaseCalls": 0,
            })
        };
        // The oldest rows anchor the page positions: loading older rows shifts every position.
        let head = match self.driver.read_head(window_title, HISTORY_HEAD_ROWS) {
            Some(r) => r?,
            None => return Ok(unsupported()),
        };
        if head.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(head);
        }
        let head_rows: Vec<Value> = head.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let head_total = head.get("exposedRowCount").and_then(|v| v.as_u64());
        let anchor = oldest_anchor(&head_rows, head_total);
        if !snapshot_id.is_empty() && snapshot_id != anchor {
            return Ok(json!({
                "status": "snapshot_changed",
                "source": "macos_accessibility",
                "reason": "Older rows were loaded; restart at offset 0",
                "items": [],
                "databaseCalls": 0,
                "snapshotId": anchor,
            }));
        }
        let offset_u = offset as usize;
        let limit_u = limit as usize;
        // Rows just above the page give its date and the sender of a follow-up first row.
        let context = offset_u.min(READ_FROM_OLDEST_CONTEXT_ROWS);
        let start = offset_u - context;
        let page = match self.driver.read_range(window_title, start, context + limit_u) {
            Some(r) => r?,
            None => return Ok(unsupported()),
        };
        if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(page);
        }
        let total = page.get("exposedRowCount").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut rows: Vec<Value> = page.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let is_separator = |r: &Value| {
            let item = ax_row(r);
            item.get("kind").and_then(|v| v.as_str()) == Some("date_separator")
        };
        // The page's first row itself, or a context row above it, may already be a separator.
        let page_has_date = rows
            .iter()
            .take_while(|r| r.get("rowIndex").and_then(|v| v.as_u64()).unwrap_or(0) <= offset as u64)
            .any(is_separator);
        if !page_has_date && start > 0 {
            // Find the separator further up for the page's date: small chunks upward, bounded.
            let mut upper = start;
            let floor = start.saturating_sub(READ_FROM_OLDEST_DATE_SCAN_ROWS);
            while upper > floor {
                let from = upper.saturating_sub(READ_FROM_OLDEST_CONTEXT_ROWS).max(floor);
                let Some(Ok(scan)) = self.driver.read_range(window_title, from, upper - from) else { break };
                if scan.get("status").and_then(|v| v.as_str()) != Some("ok") {
                    break;
                }
                let scanned: Vec<Value> = scan.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if let Some(sep) = scanned.iter().rev().find(|r| is_separator(r)) {
                    rows.insert(0, sep.clone());
                    break;
                }
                upper = from;
            }
        }
        // Older rows loaded while the page was read shift every position.
        if let Some(Ok(again)) = self.driver.read_head(window_title, HISTORY_HEAD_ROWS) {
            let again_rows: Vec<Value> = again.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let again_total = again.get("exposedRowCount").and_then(|v| v.as_u64());
            if again.get("status").and_then(|v| v.as_str()) == Some("ok") && oldest_anchor(&again_rows, again_total) != anchor {
                return Ok(json!({
                    "status": "snapshot_changed",
                    "source": "macos_accessibility",
                    "reason": "Older rows were loaded while reading; restart at offset 0",
                    "items": [],
                    "databaseCalls": 0,
                    "snapshotId": oldest_anchor(&again_rows, again_total),
                }));
            }
        }
        let mut items: Vec<Value> = rows.iter().map(ax_row).collect();
        crate::macos_rows::annotate_sequence(&rows, &mut items);
        let last_index = total.saturating_sub(1);
        let page_items: Vec<Value> = items
            .into_iter()
            .filter(|i| i.get("rowIndex").and_then(|v| v.as_u64()).is_some_and(|n| n >= offset as u64))
            // The empty spacer under the newest message is not a message row.
            .filter(|i| {
                let last = i.get("rowIndex").and_then(|v| v.as_u64()) == Some(last_index);
                let empty = i
                    .get("axParts")
                    .and_then(|v| v.as_array())
                    .is_some_and(|p| p.iter().all(|x| matches!(x.get("role").and_then(|r| r.as_str()), Some("AXRow" | "AXCell"))));
                !(last && empty)
            })
            .collect();
        let read_rows = rows
            .iter()
            .filter(|r| r.get("rowIndex").and_then(|v| v.as_u64()).is_some_and(|n| n >= offset as u64))
            .count();
        let end = offset_u + read_rows;
        let count = |kind: &str| {
            page_items.iter().filter(|i| i.get("kind").and_then(|v| v.as_str()) == Some(kind)).count()
        };
        let (text_count, unclassified, attachments) = (count("text"), count("unclassified"), count("attachment"));
        let history = crate::macos_rows::history_start(&head_rows, 0);
        let (complete, reason, start_row, hint) = match history {
            crate::macos_rows::HistoryStart::Confirmed(r, i) => (true, json!(r), json!(i), Value::Null),
            crate::macos_rows::HistoryStart::InviteAtTop(i) => (false, Value::Null, json!(i), json!("group_invite_at_top")),
            crate::macos_rows::HistoryStart::Unknown => (false, Value::Null, Value::Null, Value::Null),
        };
        Ok(json!({
            "status": "ok",
            "source": "macos_accessibility",
            "windowTitle": window_title,
            "from": "oldest",
            "items": page_items,
            "total": total,
            "limit": limit,
            "offset": offset,
            "hasMore": (end as u64) < total,
            "nextOffset": if (end as u64) < total { json!(end) } else { Value::Null },
            "snapshotId": anchor,
            "textMessageCount": text_count,
            "unclassifiedRowCount": unclassified,
            "attachmentRowCount": attachments,
            "coverage": {
                "scope": "rows_from_oldest_loaded",
                "exposedRowCount": total,
                "exposedOldestDate": crate::macos_rows::head_oldest_date(&head_rows),
                "historyComplete": complete,
                "historyCompleteReason": reason,
                "historyStartRowIndex": start_row,
                "historyStartHint": hint,
                "consistency": "best_effort",
            },
            "databaseCalls": 0,
            "uiActions": 0,
            "elapsedMs": page.get("elapsedMs").cloned().unwrap_or(json!(0)),
        }))
    }

    fn read_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        snapshot_id: &str,
        output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(row_index, "row_index", 0, 1_000_000)?;
        validate_snapshot_id_required(snapshot_id)?;
        validate_output_path(output_path)?;

        let _ui = ui_session(&self.ui_root)?;
        let raw = self.call_checked("read", &[window_title.to_string()], None)?;
        if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(raw);
        }
        if snapshot_digest(&raw) != snapshot_id {
            return Ok(json!({
                "status": "snapshot_changed",
                "reason": "Read the attachment row again",
                "databaseCalls": 0,
            }));
        }
        let rows = raw
            .get("rows")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let selected: Vec<&Value> = rows
            .iter()
            .filter(|r| r.get("rowIndex").and_then(|v| v.as_i64()) == Some(row_index))
            .collect();
        if selected.len() != 1
            || ax_row(selected[0]).get("kind").and_then(|v| v.as_str()) != Some("attachment")
        {
            return Ok(json!({
                "status": "blocked",
                "reason": "attachment_row_required",
                "databaseCalls": 0,
            }));
        }
        let expected_parts = selected[0]
            .get("parts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let expected_count = raw
            .get("exposedRowCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(rows.len() as u64) as usize;

        let mut value = self.driver.capture_attachment(
            window_title,
            row_index,
            &expected_parts,
            expected_count,
            ocr,
        )?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| KaxError::runtime("RuntimeError", "Invalid attachment reader result"))?;
        let encoded = obj.remove("imageBase64");

        if obj.get("status").and_then(|v| v.as_str()) == Some("ok") {
            let b64 = encoded
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| KaxError::runtime("RuntimeError", "Attachment reader returned no image"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| KaxError::runtime("RuntimeError", e.to_string()))?;
            let final_path = write_private_png(&bytes, output_path)?;
            obj.insert(
                "imagePath".to_string(),
                Value::String(final_path.display().to_string()),
            );
        }
        obj.insert("snapshotId".to_string(), json!(snapshot_id));
        Ok(value)
    }

    fn read_many(
        &self,
        window_titles: &[String],
        limit: i64,
        open_if_needed: bool,
        close_after: bool,
        max_workers: i64,
    ) -> Result<Value> {
        if window_titles.is_empty() || window_titles.len() > 20 {
            return Err(KaxError::validation(
                "window_titles must contain 1..20 exact titles",
            ));
        }
        let mut seen = HashSet::new();
        for name in window_titles {
            validate_title(name)?;
            if !seen.insert(name.as_str()) {
                return Err(KaxError::validation(
                    "Duplicate window_titles are not allowed",
                ));
            }
        }
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(max_workers, "max_workers", 1, 4)?;

        let started = Instant::now();
        let mut prepared: HashMap<String, Value> = HashMap::new();
        // Actual window title per requested name: a room-list name such as "오픈채팅, A" opens a
        // window titled "A", and reads/closes must use the latter.
        let mut actual_titles: HashMap<String, String> = HashMap::new();
        let mut opened: Vec<String> = Vec::new();
        // Windows that a failed open left behind: unusable, so always closed again.
        let mut opened_unusable: Vec<String> = Vec::new();
        {
            let _ui = ui_session(&self.ui_root)?;
            if open_if_needed {
                for name in window_titles {
                    let res = match self.open_room(name, false) {
                        Ok(v) => v,
                        Err(e) => json!({"status": "failed", "errorType": e.error_type()}),
                    };
                    let actual = res
                        .get("windowTitle")
                        .and_then(|v| v.as_str())
                        .filter(|t| !t.is_empty())
                        .unwrap_or(name)
                        .to_string();
                    if res.get("status").and_then(|v| v.as_str()) != Some("ok") {
                        // A failed open can still leave a new window behind (e.g. one the driver
                        // could not use); track it by its real title so it is closed again.
                        if res.get("windowOpened").and_then(|v| v.as_bool()) == Some(true) {
                            opened.push(actual.clone());
                            opened_unusable.push(actual);
                        }
                        prepared.insert(name.clone(), res);
                        continue;
                    }
                    if res.get("alreadyOpen").and_then(|v| v.as_bool()) != Some(true) {
                        opened.push(actual.clone());
                    }
                    actual_titles.insert(name.clone(), actual);
                }
            }
        }

        // Parallel reads bounded by `max_workers` (up to 4).
        let worker_count = (max_workers as usize).min(window_titles.len());
        let items: Vec<Value> = if worker_count <= 1 {
            window_titles
                .iter()
                .map(|name| read_one_for_batch(self, name, limit, &prepared, &actual_titles))
                .collect()
        } else {
            let mut results: Vec<Option<Value>> = vec![None; window_titles.len()];
            let chunk_size = window_titles.len().div_ceil(worker_count);
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for (chunk_idx, chunk) in window_titles.chunks(chunk_size).enumerate() {
                    let prepared_ref = &prepared;
                    let titles_ref = &actual_titles;
                    handles.push(scope.spawn(move || {
                        let base = chunk_idx * chunk_size;
                        chunk
                            .iter()
                            .enumerate()
                            .map(|(i, name)| {
                                (base + i, read_one_for_batch(self, name, limit, prepared_ref, titles_ref))
                            })
                            .collect::<Vec<_>>()
                    }));
                }
                for h in handles {
                    if let Ok(part) = h.join() {
                        for (idx, val) in part {
                            results[idx] = Some(val);
                        }
                    }
                }
            });
            results
                .into_iter()
                .map(|o| o.unwrap_or_else(|| json!({"status": "failed"})))
                .collect()
        };

        let mut cleanup = Vec::new();
        for name in opened.iter().rev() {
            if close_after || opened_unusable.contains(name) {
                match self.close_room(name) {
                    Ok(v) => cleanup.push(v),
                    Err(e) => cleanup.push(json!({
                        "status": "failed",
                        "windowTitle": name,
                        "errorType": e.error_type(),
                    })),
                }
            }
        }

        let succeeded = items
            .iter()
            .filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("ok"))
            .count();
        let cleanup_ok = cleanup
            .iter()
            .all(|c| c.get("status").and_then(|v| v.as_str()) == Some("ok"));
        let status = if succeeded == items.len() && cleanup_ok {
            "ok"
        } else {
            "partial"
        };
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

        Ok(json!({
            "status": status,
            "source": "macos_accessibility",
            "items": items,
            "total": items.len(),
            "succeeded": succeeded,
            "windowsOpened": opened,
            "cleanup": cleanup,
            "databaseCalls": 0,
            "parallel": max_workers > 1 && window_titles.len() > 1,
            "atomicAcrossRooms": false,
            "elapsedMs": elapsed,
        }))
    }

    fn load_more(&self, window_title: &str, pages: i64) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(pages, "pages", 1, 10)?;
        let _budget = CallBudget::begin();
        let _ui = ui_session(&self.ui_root)?;
        self.load_history(window_title, pages, true)
    }

    fn load_to_start(
        &self,
        window_title: &str,
        max_seconds: i64,
        progress: &mut dyn FnMut(Value),
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(max_seconds, "max_seconds", MIN_TO_START_SECONDS, MAX_TO_START_SECONDS)?;
        // This call may run for minutes: its own budget instead of the per-call default.
        let _budget = CallBudget::begin_with(Duration::from_secs(max_seconds as u64 + 30));
        let _ui = ui_session(&self.ui_root)?;
        load_to_start_by_steps(
            "macos_accessibility",
            window_title,
            Duration::from_secs(max_seconds as u64),
            || {
                let mut args = Map::new();
                args.insert("windowTitle".to_string(), json!(window_title));
                self.driver_control("load_older", &args)
            },
            |expected| self.history_start_in_view(window_title, expected),
            progress,
        )
        .map(|mut out| {
            // The top rows may start partway through a day: find the topmost separator below.
            if let Some(obj) = out.as_object_mut() {
                if let Some(date) = self.loaded_oldest_date(window_title) {
                    obj.insert("exposedOldestDate".to_string(), json!(date));
                }
            }
            out
        })
    }

    fn search_messages(
        &self,
        window_title: &str,
        query: &str,
        limit: i64,
        load_pages: i64,
        case_sensitive: bool,
        use_app_search: bool,
        max_steps: i64,
        context_rows: i64,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_query_text(query, false)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(load_pages, "load_pages", 0, 10)?;
        validate_integer(max_steps, "max_steps", 1, 50)?;
        validate_integer(context_rows, "context_rows", 0, 10)?;

        let _budget = CallBudget::begin();
        let _ui = ui_session(&self.ui_root)?;
        // Open first so loading, reading and app search use the window's confirmed title (a
        // room-list name such as "오픈채팅, A" opens a window titled "A").
        let opened = self.open_room(window_title, false)?;
        if opened.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(opened);
        }
        let requested_title = window_title;
        let window_title = opened
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(requested_title)
            .to_string();
        let window_title = window_title.as_str();
        // One AX read and one parse per search pass; pages are cut from the parsed snapshot.
        let parsed_cache: std::cell::RefCell<Option<ParsedSnapshot>> = std::cell::RefCell::new(None);
        let result = search_messages_impl(
            "macos_accessibility",
            window_title,
            query,
            limit as usize,
            load_pages,
            case_sensitive,
            use_app_search,
            max_steps as usize,
            context_rows as usize,
            |row| row.get("text").and_then(|v| v.as_str()).is_some(),
            || Ok(opened.clone()),
            |p| self.load_history(window_title, p, false),
            |offset, snap| {
                if offset == 0 || parsed_cache.borrow().is_none() {
                    let raw = self.call_checked("read_all", &[window_title.to_string()], None)?;
                    *parsed_cache.borrow_mut() = Some(ParsedSnapshot::parse(raw));
                }
                let parsed = parsed_cache.borrow();
                Ok(page_from_snapshot(parsed.as_ref().unwrap(), window_title, 500, offset, snap))
            },
            |action| {
                let mut args = Map::new();
                args.insert("windowTitle".to_string(), json!(window_title));
                args.insert("query".to_string(), json!(query));
                self.driver_control(action, &args)
            },
        )?;
        let mut out = result.as_object().cloned().unwrap_or_default();
        if window_title != requested_title {
            out.insert("windowTitle".to_string(), json!(window_title));
            out.insert("requestedTitle".to_string(), json!(requested_title));
        }
        Ok(Value::Object(out))
    }

    fn close_search(&self, window_title: &str) -> Result<Value> {
        validate_title(window_title)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        self.control_locked("search_close", args)
    }

    fn send_message(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        let _sending = SendingGuard::begin();
        let adapter = AxSendAdapter { engine: self };
        SendEngine::new(&adapter).send(window_title, text, operation_id, wait_seconds, require_self)
    }

    fn send_result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        let _sending = SendingGuard::begin();
        let adapter = AxSendAdapter { engine: self };
        SendEngine::new(&adapter).result(operation_id, wait_seconds)
    }
}

fn read_one_for_batch<D: AxDriver + 'static>(
    engine: &AxEngine<D>,
    name: &str,
    limit: i64,
    prepared: &HashMap<String, Value>,
    actual_titles: &HashMap<String, String>,
) -> Value {
    if let Some(err_val) = prepared.get(name) {
        let mut out = err_val.as_object().cloned().unwrap_or_default();
        out.insert("windowTitle".to_string(), json!(name));
        return Value::Object(out);
    }
    let title = actual_titles.get(name).map(String::as_str).unwrap_or(name);
    let begin = Instant::now();
    let val = match engine.read_messages(title, limit, 0, "") {
        Ok(v) => v,
        Err(e) => json!({"status": "failed", "errorType": e.error_type()}),
    };
    let elapsed = (begin.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    let mut out = val.as_object().cloned().unwrap_or_default();
    out.insert("windowTitle".to_string(), json!(title));
    if title != name {
        out.insert("requestedTitle".to_string(), json!(name));
    }
    out.insert("callElapsedMs".to_string(), json!(elapsed));
    Value::Object(out)
}

/// Build one `read_messages` page from a raw driver `read` result (the snapshot).
fn read_page_from_raw(
    raw: Value,
    window_title: &str,
    limit: i64,
    offset: i64,
    snapshot_id: &str,
) -> Value {
    page_from_snapshot(&ParsedSnapshot::parse(raw), window_title, limit, offset, snapshot_id)
}

/// One raw driver read, hashed and classified once so several pages can be cut from it
/// (`search_messages` pages through up to 5000 rows of a single snapshot).
struct ParsedSnapshot {
    raw: Value,
    snapshot: String,
    items: Vec<Value>,
    text_count: usize,
    unclassified_count: usize,
    attachment_count: usize,
}

impl ParsedSnapshot {
    fn parse(raw: Value) -> Self {
        if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Self {
                raw,
                snapshot: String::new(),
                items: Vec::new(),
                text_count: 0,
                unclassified_count: 0,
                attachment_count: 0,
            };
        }
        let snapshot = snapshot_digest(&raw);
        let rows: Vec<Value> = raw.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut items: Vec<Value> = rows.iter().map(ax_row).collect();
        crate::macos_rows::annotate_sequence(&rows, &mut items);
        let count = |kind: &str| {
            items
                .iter()
                .filter(|i| i.get("kind").and_then(|v| v.as_str()) == Some(kind))
                .count()
        };
        let (text_count, unclassified_count, attachment_count) =
            (count("text"), count("unclassified"), count("attachment"));
        Self {
            raw,
            snapshot,
            items,
            text_count,
            unclassified_count,
            attachment_count,
        }
    }
}

fn page_from_snapshot(
    parsed: &ParsedSnapshot,
    window_title: &str,
    limit: i64,
    offset: i64,
    snapshot_id: &str,
) -> Value {
    let raw = &parsed.raw;
    if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return raw.clone();
    }
    let snapshot = parsed.snapshot.clone();
    if !snapshot_id.is_empty() && snapshot_id != snapshot {
        return json!({
            "status": "snapshot_changed",
            "source": "macos_accessibility",
            "reason": "AX rows changed; restart at offset 0",
            "items": [],
            "databaseCalls": 0,
            "uiActions": 0,
            "snapshotId": snapshot,
        });
    }

    let items = &parsed.items;
    let total = items.len();
    let limit_u = limit as usize;
    let offset_u = offset as usize;
    let end = total.saturating_sub(offset_u);
    let start = end.saturating_sub(limit_u);
    let page_items = items[start..end].to_vec();

    let (text_count, unclassified_count, attachment_count) =
        (parsed.text_count, parsed.unclassified_count, parsed.attachment_count);

    let next_offset = if start > 0 {
        json!(offset_u + (end - start))
    } else {
        Value::Null
    };
    let dates: Vec<&str> = items.iter().filter_map(|i| i.get("date").and_then(|v| v.as_str())).collect();
    let oldest_date = dates.first().copied();
    let newest_date = dates.last().copied();
    let truncated = raw.get("rowsTruncated").and_then(|v| v.as_bool()).unwrap_or(false);
    let row_start = raw.get("rowStart").and_then(|v| v.as_u64()).unwrap_or(0);
    let history = if truncated {
        crate::macos_rows::HistoryStart::Unknown
    } else {
        let raw_rows = raw.get("rows").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or(&[]);
        crate::macos_rows::history_start(raw_rows, row_start)
    };
    let (history_complete, history_reason, history_row, history_hint) = match history {
        crate::macos_rows::HistoryStart::Confirmed(reason, row) => (true, json!(reason), json!(row), Value::Null),
        // Only load_more, which can see that nothing older loads, counts an invitation.
        crate::macos_rows::HistoryStart::InviteAtTop(row) => (false, Value::Null, json!(row), json!("group_invite_at_top")),
        crate::macos_rows::HistoryStart::Unknown => (false, Value::Null, Value::Null, Value::Null),
    };

    json!({
        "status": "ok",
        "source": "macos_accessibility",
        "windowTitle": window_title,
        "items": page_items,
        "total": total,
        "limit": limit,
        "offset": offset,
        "hasMore": start > 0,
        "nextOffset": next_offset,
        "snapshotId": snapshot,
        "textMessageCount": text_count,
        "unclassifiedRowCount": unclassified_count,
        "attachmentRowCount": attachment_count,
        "coverage": {
            "scope": "currently_exposed_ax_rows",
            "historyComplete": history_complete,
            "historyCompleteReason": history_reason,
            "historyStartRowIndex": history_row,
            "historyStartHint": history_hint,
            "consistency": "best_effort",
            "exposedRowCount": raw.get("exposedRowCount").cloned().unwrap_or(json!(total)),
            "rowsTruncated": raw.get("rowsTruncated").cloned().unwrap_or(json!(false)),
            // Oldest date within the rows read (the newest 1000 at most).
            "oldestDate": oldest_date,
            "newestDate": newest_date,
            // Oldest day loaded in the transcript, also beyond the row limit.
            "exposedOldestDate": if truncated {
                raw.get("exposedOldestDate").cloned().unwrap_or(Value::Null)
            } else {
                json!(oldest_date)
            },
            // The oldest date whose separator was read: rows from before this day were dropped
            // by the row cap (and the undated rows above it may be incomplete).
            "truncatedBefore": if truncated { json!(oldest_date) } else { Value::Null },
            "rowStart": raw.get("rowStart").cloned().unwrap_or(json!(0)),
            "rowLimit": raw.get("rowLimit").cloned().unwrap_or(json!(1000)),
            "scrollPerformed": false,
            "attributeReadMode": raw.get("attributeReadMode").cloned().unwrap_or(json!("individual")),
            "optionalAttributeFailures": raw.get("optionalAttributeFailures").cloned().unwrap_or(json!({})),
        },
        "databaseCalls": 0,
        "uiActions": 0,
        "elapsedMs": raw.get("elapsedMs").cloned().unwrap_or(json!(0)),
        "warning": if truncated { json!(ROWS_TRUNCATED_WARNING) } else { Value::Null },
    })
}

const ROWS_TRUNCATED_WARNING: &str =
    "rows_truncated: only the newest rows up to the row limit were read; older rows were dropped";

/// The compact `load_more` result (MCP default): what was loaded and how to read it, without
/// the transcript rows (`after.items`) and the per-step reports.
pub fn load_more_summary(full: &Value) -> Value {
    // Failures keep every detail (actions, hints, Windows input conflicts); Windows results,
    // which need a clipboard read per page, keep their rows.
    if full.get("status").and_then(|v| v.as_str()) != Some("ok")
        || full.get("source").and_then(|v| v.as_str()) != Some("macos_accessibility")
    {
        return full.clone();
    }
    let after = full.get("after").cloned().unwrap_or(Value::Null);
    let coverage = after.get("coverage").cloned().unwrap_or(Value::Null);
    let pick = |v: &Value, keys: &[&str]| -> Map<String, Value> {
        keys.iter()
            .filter_map(|k| v.get(*k).map(|x| ((*k).to_string(), x.clone())))
            .collect()
    };
    let mut out = pick(
        full,
        &[
            "status", "reason", "source", "windowTitle", "pagesAttempted", "contentChanged",
            "exposedRowsBefore", "exposedRowsAfter", "loadStopReason", "endOfHistory",
            "historyComplete", "historyCompleteReason", "historyStartRowIndex", "databaseCalls",
        ],
    );
    let steps: Vec<Value> = full
        .get("steps")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|s| Value::Object(pick(s, &["status", "loadStatus", "exposedRowsAfter"])))
                .collect()
        })
        .unwrap_or_default();
    out.insert("steps".to_string(), Value::Array(steps));
    let mut read = pick(&after, &["status", "reason", "snapshotId", "total", "textMessageCount", "attachmentRowCount"]);
    read.extend(pick(
        &coverage,
        &[
            "exposedRowCount", "rowsTruncated", "rowStart", "rowLimit", "oldestDate", "newestDate",
            "truncatedBefore", "exposedOldestDate", "historyComplete", "historyCompleteReason", "historyStartRowIndex",
            "historyStartHint",
        ],
    ));
    if let Some(w) = after.get("warning").filter(|w| !w.is_null()) {
        out.insert("warning".to_string(), w.clone());
    }
    out.insert("snapshotId".to_string(), after.get("snapshotId").cloned().unwrap_or(Value::Null));
    out.insert("after".to_string(), Value::Object(read));
    out.insert("itemsIncluded".to_string(), json!(false));
    out.insert(
        "next".to_string(),
        json!("Read the rows with kakaotalk_ax_read(window_title, limit, offset, snapshot_id)."),
    );
    Value::Object(out)
}

impl<D: AxDriver + 'static> AxEngine<D> {
    /// The room search may list more rooms than fit on screen (a short member name of an unnamed
    /// group chat matches many rooms), and `open` only sees the visible rows. Reads the whole
    /// result list by scrolling it and, when exactly one room has the requested name, opens that
    /// row (`rowIndex`). Otherwise `first` (the visible-rows result) keeps its reason, replaced
    /// by why the scrolled list could not pick a room when it was read.
    fn open_room_in_scrolled_results(
        &self,
        window_title: &str,
        args: &Map<String, Value>,
        first: Value,
    ) -> Result<Value> {
        let mut list_args = Map::new();
        list_args.insert("scroll".to_string(), json!(true));
        list_args.insert("limit".to_string(), json!(MAX_ROOM_SCROLL_LIMIT));
        list_args.insert("namesOnly".to_string(), json!(true));
        let listed = self.driver_control("rooms", &list_args)?;
        let mut out = first.as_object().cloned().unwrap_or_default();
        let mut scroll = Map::new();
        scroll.insert("status".to_string(), listed.get("status").cloned().unwrap_or(Value::Null));
        for key in ["reason", "reportedRowCount", "itemsCollected", "stopReason", "scrollRestored"] {
            if let Some(v) = listed.get(key) {
                scroll.insert(key.to_string(), v.clone());
            }
        }
        if listed.get("status").and_then(|v| v.as_str()) != Some("ok") {
            out.insert("roomSearchScroll".to_string(), Value::Object(scroll));
            return Ok(Value::Object(out));
        }
        let items = listed.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let rows: Vec<(&str, Option<i64>)> = items
            .iter()
            .map(|item| {
                (
                    item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    item.get("rowIndex").and_then(|v| v.as_i64()),
                )
            })
            .collect();
        let complete = listed.get("historyComplete") == Some(&Value::Bool(true));
        match crate::macos_ax_logic::pick_room_in_results(window_title, &rows, complete) {
            Ok((index, other_type_listed)) => {
                let mut open_args = args.clone();
                open_args.insert("rowIndex".to_string(), json!(index));
                // Rows of the other type may be off screen once the row is revealed; the driver
                // must still refuse an open window titled with the bare name.
                if other_type_listed {
                    open_args.insert("otherTypeListed".to_string(), json!(true));
                }
                let opened = self.driver_control("open", &open_args)?;
                let mut out = opened.as_object().cloned().unwrap_or_default();
                scroll.insert("rowIndex".to_string(), json!(index));
                out.insert("roomSearchScroll".to_string(), Value::Object(scroll));
                Ok(Value::Object(out))
            }
            Err((reason, type_mismatch)) => {
                out.insert("status".to_string(), json!("blocked"));
                out.insert("reason".to_string(), json!(reason));
                if reason == "room_not_in_exposed_list" {
                    out.insert("roomTypeMismatch".to_string(), json!(type_mismatch));
                } else {
                    out.remove("roomTypeMismatch");
                }
                out.insert("roomSearchScroll".to_string(), Value::Object(scroll));
                Ok(Value::Object(out))
            }
        }
    }

    /// Puts the room search back the way `open_room` found it: the same query, and the bar open
    /// or closed as before (an empty query with a closed bar closes the bar).
    fn restore_room_search(&self, query: &str, was_open: bool) -> Result<Value> {
        validate_query_text(query, true)?;
        let mut args = Map::new();
        args.insert("query".to_string(), json!(query));
        if was_open {
            args.insert("keepOpen".to_string(), json!(true));
        }
        self.control_locked("search_rooms", args)
    }

    /// Request older history `pages` times, judging progress from each step's `AXRows` counts.
    /// With `final_read`, the transcript is read once at the end (the `load_more` tool output);
    /// `search_messages` passes `false` because it reads the transcript itself right after.
    fn load_history(&self, window_title: &str, pages: i64, final_read: bool) -> Result<Value> {
        load_more_by_step_counts(
            "macos_accessibility",
            window_title,
            pages,
            final_read,
            || self.read_messages(window_title, 500, 0, ""),
            || {
                let mut args = Map::new();
                args.insert("windowTitle".to_string(), json!(window_title));
                self.driver_control("load_older", &args)
            },
            |expected| self.history_start_in_view(window_title, expected),
        )
    }

    /// The topmost loaded date separator's date, scanning the first rows down in chunks.
    fn loaded_oldest_date(&self, window_title: &str) -> Option<String> {
        let mut start = 0usize;
        while start < crate::macos_ax_logic::OLDEST_DATE_SCAN_ROWS {
            let raw = self.driver.read_range(window_title, start, 10)?.ok()?;
            if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
                return None;
            }
            let rows: Vec<Value> = raw.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if rows.is_empty() {
                return None;
            }
            if let Some(date) = crate::macos_rows::head_oldest_date(&rows) {
                return Some(date);
            }
            start += rows.len();
        }
        None
    }

    /// Whether the oldest loaded rows show where the history begins (a few rows). When the
    /// transcript's row count differs from `expected` (rows arrived after the step counted
    /// them), the verdict is unknown.
    fn history_start_in_view(&self, window_title: &str, expected: Option<u64>) -> HeadCheck {
        match self.driver.read_head(window_title, HISTORY_HEAD_ROWS) {
            Some(Ok(raw)) if raw.get("status").and_then(|v| v.as_str()) == Some("ok") => {
                let count = raw.get("exposedRowCount").and_then(|v| v.as_u64());
                let rows: Vec<Value> = raw.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let oldest_date = crate::macos_rows::head_oldest_date(&rows);
                let top = rows.first().map(|r| oldest_anchor(std::slice::from_ref(r), count));
                if expected.is_some() && count != expected {
                    return HeadCheck {
                        start: crate::macos_rows::HistoryStart::Unknown,
                        rows: count,
                        status: "row_count_changed",
                        oldest_date,
                        top,
                    };
                }
                HeadCheck { start: crate::macos_rows::history_start(&rows, 0), rows: count, status: "ok", oldest_date, top }
            }
            Some(Ok(raw)) => HeadCheck {
                start: crate::macos_rows::HistoryStart::Unknown,
                rows: None,
                status: if raw.get("reason").is_some() { "blocked" } else { "unavailable" },
                oldest_date: None,
                top: None,
            },
            _ => HeadCheck {
                start: crate::macos_rows::HistoryStart::Unknown,
                rows: None,
                status: "unavailable",
                oldest_date: None,
                top: None,
            },
        }
    }
}

/// How [`load_more_impl`] decides whether a history step exposed new rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadProbe {
    /// Re-read the whole transcript after every step and compare `snapshotId` (Windows: the
    /// clipboard read is cheap, and its steps report no row counts).
    FullRead,
    /// Trust the step's own `exposedRowsBefore`/`exposedRowsAfter` row counts (macOS: a full AX
    /// read costs seconds, the step's `AXRows` count costs microseconds). The transcript is read
    /// at most once, after the last step, and only when `final_read` is set.
    StepCounts { final_read: bool },
}

/// Shared `load_more` implementation used by both macOS and Windows backends.
pub fn load_more_impl<FRead, FStep>(
    source: &str,
    window_title: &str,
    pages: i64,
    probe: LoadProbe,
    mut read_fn: FRead,
    mut step_fn: FStep,
) -> Result<Value>
where
    FRead: FnMut() -> Result<Value>,
    FStep: FnMut() -> Result<Value>,
{
    if let LoadProbe::StepCounts { final_read } = probe {
        return load_more_by_step_counts(
            source,
            window_title,
            pages,
            final_read,
            read_fn,
            step_fn,
            |_| HeadCheck {
                start: crate::macos_rows::HistoryStart::Unknown,
                rows: None,
                status: "unsupported",
                oldest_date: None,
                top: None,
            },
        );
    }
    let before = read_fn()?;
    if before.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Ok(before);
    }
    let mut steps = Vec::new();
    let mut previous = before.clone();
    let mut after = before.clone();
    for _ in 0..pages {
        let step = step_fn()?;
        steps.push(step.clone());
        if step.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = step.as_object().cloned().unwrap_or_default();
            out.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(out));
        }
        after = read_fn()?;
        if after.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = after.as_object().cloned().unwrap_or_default();
            out.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(out));
        }
        if after.get("snapshotId") == previous.get("snapshotId") {
            break;
        }
        previous = after.clone();
    }
    let before_exposed = before
        .get("coverage")
        .and_then(|c| c.get("exposedRowCount"))
        .cloned()
        .unwrap_or(json!(0));
    let after_exposed = after
        .get("coverage")
        .and_then(|c| c.get("exposedRowCount"))
        .cloned()
        .unwrap_or(json!(0));
    Ok(json!({
        "status": "ok",
        "source": source,
        "windowTitle": window_title,
        "steps": steps,
        "pagesAttempted": steps.len(),
        "contentChanged": before.get("snapshotId") != after.get("snapshotId"),
        "exposedRowsBefore": before_exposed,
        "exposedRowsAfter": after_exposed,
        "endOfHistory": Value::Null,
        "historyComplete": false,
        "after": after,
        "databaseCalls": 0,
    }))
}

/// What a look at the oldest loaded rows found during `load_more`.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadCheck {
    pub start: crate::macos_rows::HistoryStart,
    /// The transcript's row count at the look, if read.
    pub rows: Option<u64>,
    /// `ok`, `row_count_changed`, `blocked`, `unavailable` or `unsupported`.
    pub status: &'static str,
    /// The topmost date separator's date among the rows looked at.
    pub oldest_date: Option<String>,
    /// A digest of the oldest loaded row: it changes when older rows load (new messages at the
    /// bottom leave it alone).
    pub top: Option<String>,
}

impl HeadCheck {
    fn label(&self) -> &'static str {
        match self.start {
            crate::macos_rows::HistoryStart::Confirmed(..) => "start_notice",
            crate::macos_rows::HistoryStart::InviteAtTop(_) => "invite_at_top",
            crate::macos_rows::HistoryStart::Unknown => self.status,
        }
    }
}

/// `load_more(to_start)` time limits in seconds.
pub const MIN_TO_START_SECONDS: i64 = 10;
pub const MAX_TO_START_SECONDS: i64 = 1800;
pub const DEFAULT_TO_START_SECONDS: i64 = 300;
/// Consecutive steps without new rows (each waiting about 1.5 s plus this pause) before
/// `load_to_start` gives up with `no_new_rows`; the app sometimes pauses loading.
const TO_START_IDLE_STEPS: u32 = 6;
/// A row-count jump at least this large is a load, not new messages at the bottom.
const TO_START_LOAD_MIN_ROWS: u64 = 5;
const TO_START_IDLE_PAUSE: Duration = Duration::from_millis(1500);
/// How often `load_to_start` reports progress.
const TO_START_PROGRESS_EVERY: Duration = Duration::from_secs(3);

/// Loads older rows until the history start is in view, using only the steps' row counts and a
/// look at the first rows after each step (no transcript read). Stops at a start notice, at a
/// group invitation that a second, longer attempt confirms, after several steps in a row load
/// nothing (`no_new_rows`), or when `limit` passes (`time_limit`).
pub fn load_to_start_by_steps<FStep, FHead>(
    source: &str,
    window_title: &str,
    limit: Duration,
    mut step_fn: FStep,
    mut head_fn: FHead,
    progress: &mut dyn FnMut(Value),
) -> Result<Value>
where
    FStep: FnMut() -> Result<Value>,
    FHead: FnMut(Option<u64>) -> HeadCheck,
{
    use crate::macos_rows::HistoryStart;
    let started = Instant::now();
    let count = |step: &Value, key: &str| step.get(key).and_then(|v| v.as_u64());
    let mut history: Option<(&'static str, i64)> = None;
    let mut stop = "time_limit";
    let mut steps = 0u64;
    let mut idle = 0u32;
    let mut invite_rechecked = false;
    let first = head_fn(None);
    let mut first_before = first.rows;
    let mut last_after = first.rows;
    let mut oldest_date = first.oldest_date.clone();
    let mut last_top = first.top.clone();
    let mut last_count = first.rows;
    let mut last_report = Instant::now();
    let mut reported_rows = first.rows.unwrap_or(0);
    if let HistoryStart::Confirmed(reason, row) = first.start {
        history = Some((reason, row));
        stop = "start_of_history";
    }
    while history.is_none() {
        if started.elapsed() >= limit {
            stop = "time_limit";
            break;
        }
        if crate::macos_ax_logic::call_budget_exhausted() {
            stop = "tool_call_time_limit";
            break;
        }
        let step = match step_fn() {
            Ok(v) => v,
            // Keep what was loaded so far.
            Err(err) => json!({"status": "bridge_error", "errorType": err.error_type()}),
        };
        steps += 1;
        if step.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = step.as_object().cloned().unwrap_or_default();
            out.insert("mode".to_string(), json!("to_start"));
            out.insert("pagesAttempted".to_string(), json!(steps));
            out.insert("exposedRowsBefore".to_string(), json!(first_before));
            out.insert("exposedRowsAfter".to_string(), json!(last_after));
            out.insert("elapsedMs".to_string(), json!(started.elapsed().as_millis() as u64));
            out.insert("historyComplete".to_string(), json!(false));
            out.insert("exposedOldestDate".to_string(), json!(oldest_date));
            return Ok(Value::Object(out));
        }
        let (before, after) = (count(&step, "exposedRowsBefore"), count(&step, "exposedRowsAfter"));
        if first_before.is_none() {
            first_before = before;
        }
        let arrived_between = matches!((last_after, before), (Some(p), Some(b)) if b > p);
        last_after = after.or(last_after);
        let check = head_fn(after);
        if check.oldest_date.is_some() {
            oldest_date = check.oldest_date.clone();
        }
        if let HistoryStart::Confirmed(reason, row) = check.start {
            history = Some((reason, row));
            stop = "start_of_history";
            break;
        }
        // Older rows loaded when the oldest row changed, or when the count jumped by more than
        // a few new messages at the bottom could explain (a load brings dozens of rows).
        let jumped = matches!((before, after), (Some(b), Some(a)) if a >= b + TO_START_LOAD_MIN_ROWS)
            || matches!((last_count, before), (Some(p), Some(b)) if b >= p + TO_START_LOAD_MIN_ROWS);
        let grew = match (&last_top, &check.top) {
            (Some(prev), Some(now)) => prev != now || jumped,
            _ => matches!((before, after), (Some(b), Some(a)) if a > b) || arrived_between,
        };
        last_count = after.or(last_count);
        if check.top.is_some() {
            last_top = check.top.clone();
        }
        if grew {
            idle = 0;
            invite_rechecked = false;
        } else {
            if let HistoryStart::InviteAtTop(row) = check.start {
                if invite_rechecked {
                    history = Some(("group_invite_at_top", row));
                    stop = "start_of_history";
                    break;
                }
                invite_rechecked = true;
            } else {
                idle += 1;
                if idle >= TO_START_IDLE_STEPS {
                    stop = "no_new_rows";
                    break;
                }
            }
            std::thread::sleep(TO_START_IDLE_PAUSE.min(limit.saturating_sub(started.elapsed())));
        }
        // Progress only goes up (MCP): report when more rows are loaded.
        if last_report.elapsed() >= TO_START_PROGRESS_EVERY && last_after.unwrap_or(0) > reported_rows {
            last_report = Instant::now();
            reported_rows = last_after.unwrap_or(0);
            progress(json!({
                "progress": last_after.unwrap_or(0),
                "message": format!(
                    "{} rows loaded, oldest {}, {} s",
                    last_after.unwrap_or(0),
                    oldest_date.as_deref().unwrap_or("unknown"),
                    started.elapsed().as_secs()
                ),
            }));
        }
    }
    let mut out = Map::new();
    out.insert("status".to_string(), json!("ok"));
    out.insert("source".to_string(), json!(source));
    out.insert("windowTitle".to_string(), json!(window_title));
    out.insert("mode".to_string(), json!("to_start"));
    out.insert("pagesAttempted".to_string(), json!(steps));
    out.insert("exposedRowsBefore".to_string(), json!(first_before));
    out.insert("exposedRowsAfter".to_string(), json!(last_after));
    out.insert(
        "contentChanged".to_string(),
        match (first_before, last_after) {
            (Some(b), Some(a)) => json!(a > b),
            _ => Value::Null,
        },
    );
    out.insert("loadStopReason".to_string(), json!(stop));
    out.insert("exposedOldestDate".to_string(), json!(oldest_date));
    out.insert("elapsedMs".to_string(), json!(started.elapsed().as_millis() as u64));
    out.insert("maxSeconds".to_string(), json!(limit.as_secs()));
    match history {
        Some((reason, row)) => {
            out.insert("historyComplete".to_string(), json!(true));
            out.insert("endOfHistory".to_string(), json!(true));
            out.insert("historyCompleteReason".to_string(), json!(reason));
            out.insert("historyStartRowIndex".to_string(), json!(row));
        }
        None => {
            out.insert("historyComplete".to_string(), json!(false));
            out.insert("endOfHistory".to_string(), Value::Null);
        }
    }
    // No transcript read here (reading the newest 1000 rows takes tens of seconds).
    out.insert("snapshotId".to_string(), Value::Null);
    out.insert("itemsIncluded".to_string(), json!(false));
    out.insert(
        "next".to_string(),
        json!("Read the rows with kakaotalk_ax_read(window_title, limit, offset, from=\"oldest\") for the oldest first, or from the newest as before."),
    );
    out.insert("databaseCalls".to_string(), json!(0));
    Ok(Value::Object(out))
}

/// Rows read above a `from=oldest` page for its date and sender context.
const READ_FROM_OLDEST_CONTEXT_ROWS: usize = 30;
/// How far above that a `from=oldest` page looks for its date separator (in chunks, stopping at
/// the first one; a busy day can run hundreds of rows).
const READ_FROM_OLDEST_DATE_SCAN_ROWS: usize = 1000;

/// The `from=oldest` page token: a digest of the oldest loaded row (layout verdicts, unread
/// counts and the unread part of time labels left out), which changes when older rows are
/// loaded and every position shifts.
fn oldest_anchor(head_rows: &[Value], total: Option<u64>) -> String {
    let _ = total;
    // Only the oldest non-empty row: it changes when older rows load, while new messages (and
    // the spacer under them) never reach it, even in a room of a few rows.
    let rows: Vec<Value> = head_rows
        .iter()
        .filter(|r| {
            r.get("parts").and_then(|v| v.as_array()).is_some_and(|p| {
                p.iter().any(|x| !matches!(x.get("role").and_then(|v| v.as_str()), Some("AXRow" | "AXCell")))
            })
        })
        .take(1)
        .map(|r| {
            let mut r = r.clone();
            if let Some(o) = r.as_object_mut() {
                o.remove("ownSide");
                o.remove("centered");
                // Unread counts drop as members read: not a change of position.
                if let Some(Value::Array(parts)) = o.get_mut("parts") {
                    parts.retain(|p| {
                        let v = p.get("AXValue").and_then(|v| v.as_str()).unwrap_or("");
                        !(p.get("role").and_then(|x| x.as_str()) == Some("AXStaticText")
                            && crate::macos_rows::label_unread(v).is_some())
                    });
                    for p in parts.iter_mut() {
                        if let Some(v) = p.get("AXValue").and_then(|v| v.as_str()) {
                            if let Some(t) = crate::macos_rows::label_time(v) {
                                let t = t.to_string();
                                p["AXValue"] = json!(t);
                            }
                        }
                    }
                }
            }
            r
        })
        .collect();
    legacy_snapshot_id(&json!({"from": "oldest", "rows": rows}))
}

/// Rows read from the top to recognise the history start after a load step.
const HISTORY_HEAD_ROWS: usize = 6;
/// Extra wait before the second attempt when an invitation is on top and nothing more loaded
/// (with the step's own 1.5 s wait, the second attempt waits about 3 s).
const INVITE_RECHECK_DELAY: Duration = Duration::from_millis(1500);

/// `load_more` by the steps' own row counts. `head_fn` tells whether the oldest loaded rows show
/// the history start: a start-only notice stops at once (also before the first step), and a
/// group invitation on top stops once a second, longer attempt still loads nothing.
#[allow(clippy::too_many_arguments)]
pub fn load_more_by_step_counts<FRead, FStep, FHead>(
    source: &str,
    window_title: &str,
    pages: i64,
    final_read: bool,
    mut read_fn: FRead,
    mut step_fn: FStep,
    mut head_fn: FHead,
) -> Result<Value>
where
    FRead: FnMut() -> Result<Value>,
    FStep: FnMut() -> Result<Value>,
    FHead: FnMut(Option<u64>) -> HeadCheck,
{
    use crate::macos_rows::HistoryStart;
    let count = |step: &Value, key: &str| step.get(key).and_then(|v| v.as_u64());
    let mut steps = Vec::new();
    let mut first_before: Option<u64> = None;
    let mut last_after: Option<u64> = None;
    let mut load_stop = "page_limit";
    let mut history: Option<(&'static str, i64)> = None;
    let mut invite_rechecked = false;
    let mut remaining = pages;
    let first = head_fn(None);
    if let HistoryStart::Confirmed(reason, row) = first.start {
        history = Some((reason, row));
        load_stop = "start_of_history";
        remaining = 0;
        first_before = first.rows;
        last_after = first.rows;
    }
    while remaining > 0 {
        remaining -= 1;
        if crate::macos_ax_logic::call_budget_exhausted() {
            load_stop = "tool_call_time_limit";
            break;
        }
        let step = step_fn()?;
        steps.push(step.clone());
        if step.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = step.as_object().cloned().unwrap_or_default();
            out.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(out));
        }
        let (before, after) = (count(&step, "exposedRowsBefore"), count(&step, "exposedRowsAfter"));
        if first_before.is_none() {
            first_before = before;
        }
        // Rows that arrived between two steps also count as loading.
        let arrived_between = matches!((last_after, before), (Some(p), Some(b)) if b > p);
        last_after = after.or(last_after);
        let check = head_fn(after);
        if let Some(Value::Object(o)) = steps.last_mut() {
            o.insert("historyStartCheck".to_string(), json!(check.label()));
        }
        let start = check.start;
        if let HistoryStart::Confirmed(reason, row) = start {
            history = Some((reason, row));
            load_stop = "start_of_history";
            break;
        }
        // No new rows alone is not an end-of-history claim: the app may simply not have loaded
        // anything this time.
        if let (Some(b), Some(a)) = (before, after) {
            if a > b || arrived_between {
                // Older rows came in: an invitation seen before was a later one.
                invite_rechecked = false;
            } else {
                if let HistoryStart::InviteAtTop(row) = start {
                    if invite_rechecked {
                        history = Some(("group_invite_at_top", row));
                        load_stop = "start_of_history";
                        break;
                    }
                    // A second, longer attempt within the requested pages; without one left
                    // the invitation is not taken as the start.
                    if remaining > 0 {
                        invite_rechecked = true;
                        std::thread::sleep(INVITE_RECHECK_DELAY);
                        continue;
                    }
                }
                load_stop = "no_new_rows";
                break;
            }
        }
    }
    let mut out = Map::new();
    out.insert("status".to_string(), json!("ok"));
    out.insert("source".to_string(), json!(source));
    out.insert("windowTitle".to_string(), json!(window_title));
    out.insert("pagesAttempted".to_string(), json!(steps.len()));
    out.insert("steps".to_string(), Value::Array(steps.clone()));
    out.insert("loadProbe".to_string(), json!("step_row_counts"));
    out.insert("loadStopReason".to_string(), json!(load_stop));
    let mut exposed_after = last_after.map(Value::from);
    if final_read {
        let after = read_fn()?;
        if after.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut err = after.as_object().cloned().unwrap_or_default();
            err.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(err));
        }
        if let Some(n) = after.get("coverage").and_then(|c| c.get("exposedRowCount")) {
            exposed_after = Some(n.clone());
        }
        out.insert("after".to_string(), after);
    }
    let changed = match (first_before, exposed_after.as_ref().and_then(|v| v.as_u64())) {
        (Some(b), Some(a)) => json!(a > b),
        _ => Value::Null,
    };
    out.insert("contentChanged".to_string(), changed);
    out.insert("exposedRowsBefore".to_string(), json!(first_before));
    out.insert("exposedRowsAfter".to_string(), exposed_after.unwrap_or(Value::Null));
    match history {
        Some((reason, row)) => {
            out.insert("endOfHistory".to_string(), json!(true));
            out.insert("historyComplete".to_string(), json!(true));
            out.insert("historyCompleteReason".to_string(), json!(reason));
            out.insert("historyStartRowIndex".to_string(), json!(row));
        }
        None => {
            out.insert("endOfHistory".to_string(), Value::Null);
            out.insert("historyComplete".to_string(), json!(false));
        }
    }
    out.insert("databaseCalls".to_string(), json!(0));
    Ok(Value::Object(out))
}

/// Shared `search_messages` implementation used by both macOS and Windows backends.
#[allow(clippy::too_many_arguments)]
pub fn search_messages_impl<FSearchable, FOpen, FLoad, FRead, FControl>(
    source: &str,
    window_title: &str,
    query: &str,
    limit: usize,
    load_pages: i64,
    case_sensitive: bool,
    use_app_search: bool,
    max_steps: usize,
    context_rows: usize,
    searchable: FSearchable,
    mut open_fn: FOpen,
    mut load_fn: FLoad,
    mut read_fn: FRead,
    mut control_fn: FControl,
) -> Result<Value>
where
    FSearchable: Fn(&Value) -> bool,
    FOpen: FnMut() -> Result<Value>,
    FLoad: FnMut(i64) -> Result<Value>,
    FRead: FnMut(i64, &str) -> Result<Value>,
    FControl: FnMut(&str) -> Result<Value>,
{
    let opened = open_fn()?;
    if opened.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Ok(opened);
    }
    let mut history_load = Value::Null;
    if load_pages > 0 {
        let history = load_fn(load_pages)?;
        if history.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(history);
        }
        // Summary of the history steps (row counts per step), without any transcript copy.
        let mut summary = history.as_object().cloned().unwrap_or_default();
        summary.remove("after");
        history_load = Value::Object(summary);
    }

    let mut visits: Vec<Value> = Vec::new();
    let mut matches: Vec<Value> = Vec::new();
    let mut seen_snapshots: HashSet<String> = HashSet::new();
    let mut searched_rows: usize = 0;
    let mut stop_reason = if use_app_search {
        "step_limit".to_string()
    } else {
        "exposed_rows_only".to_string()
    };
    let mut verified = true;
    let mut coverage = json!({"historyComplete": false});
    let mut snapshot: Value = Value::Null;
    let needle = if case_sensitive {
        query.to_string()
    } else {
        caseless::default_case_fold_str(query)
    };

    let total_steps = if use_app_search { max_steps } else { 1 };
    for step in 0..total_steps {
        let native = if use_app_search {
            Some(control_fn(if step == 0 {
                "search_query"
            } else {
                "search_previous"
            })?)
        } else {
            None
        };

        if let Some(ref nat) = native {
            if nat.get("searchOutcome").and_then(|v| v.as_str()) == Some("navigation_exhausted") {
                visits.push(nat.clone());
                stop_reason = "navigation_exhausted".to_string();
                break;
            }
            visits.push(nat.clone());
            verified = verified && nat.get("searchVerified") == Some(&Value::Bool(true));
            let st = nat.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if !matches!(st, "ok" | "partial") {
                stop_reason = nat
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search_navigation_failed")
                    .to_string();
                verified = false;
                break;
            }
        }

        let mut page = read_fn(0, "")?;
        if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
            stop_reason = page
                .get("reason")
                .or_else(|| page.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            verified = false;
            break;
        }

        let mut rows = page
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let snap_str = page
            .get("snapshotId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        snapshot = Value::String(snap_str.clone());
        if let Some(cov) = page.get("coverage") {
            coverage = cov.clone();
        }

        while page.get("hasMore") == Some(&Value::Bool(true)) {
            let next_off = page
                .get("nextOffset")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            page = read_fn(next_off, &snap_str)?;
            if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
                break;
            }
            let mut older = page
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            older.extend(rows);
            rows = older;
        }
        if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
            stop_reason = page
                .get("reason")
                .or_else(|| page.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            verified = false;
            break;
        }

        if seen_snapshots.insert(snap_str.clone()) {
            searched_rows += rows.len();
            for (idx, row) in rows.iter().enumerate() {
                if !searchable(row) {
                    continue;
                }
                let Some(body) = row.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let matched = if case_sensitive {
                    body.contains(&needle)
                } else {
                    caseless::default_case_fold_str(body).contains(&needle)
                };
                if !matched {
                    continue;
                }
                let before_start = idx.saturating_sub(context_rows);
                let after_end = (idx + 1 + context_rows).min(rows.len());
                let mut item = row.as_object().cloned().unwrap_or_default();
                item.insert("snapshotId".to_string(), Value::String(snap_str.clone()));
                item.insert("searchVisit".to_string(), json!(step));
                item.insert(
                    "contextBefore".to_string(),
                    Value::Array(rows[before_start..idx].to_vec()),
                );
                item.insert(
                    "contextAfter".to_string(),
                    Value::Array(rows[idx + 1..after_end].to_vec()),
                );
                matches.push(Value::Object(item));
            }
        }

        if let Some(ref nat) = native {
            if !verified {
                stop_reason = nat
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search_result_not_observed")
                    .to_string();
                break;
            }
        }
        if matches.len() >= limit {
            stop_reason = "result_limit".to_string();
            break;
        }
        if let Some(ref nat) = native {
            if nat
                .get("searchState")
                .and_then(|s| s.get("previousEnabled"))
                == Some(&Value::Bool(false))
            {
                stop_reason = "navigation_exhausted".to_string();
                break;
            }
        }
    }

    let mut cov_map = coverage.as_object().cloned().unwrap_or_default();
    cov_map.insert("historyComplete".to_string(), json!(false));
    cov_map.insert("snapshotsVisited".to_string(), json!(seen_snapshots.len()));
    // Exposed rows older than the row cap were not searched: say so instead of implying coverage.
    let row_start = cov_map.get("rowStart").and_then(|v| v.as_u64()).unwrap_or(0);
    let unsearched = if row_start > 0 {
        json!({"start": 0, "end": row_start, "count": row_start})
    } else {
        Value::Null
    };

    let total_matches = matches.len();
    let truncated = total_matches > limit;
    let items_slice: Vec<Value> = matches.into_iter().take(limit).collect();

    Ok(json!({
        "status": if verified { "ok" } else { "partial" },
        "source": source,
        "windowTitle": window_title,
        "query": query,
        "items": items_slice,
        "total": total_matches,
        "truncated": truncated,
        "coverage": Value::Object(cov_map),
        "searchedRows": searched_rows,
        "unsearchedRowRange": unsearched,
        "historyRequested": load_pages > 0,
        "historyLoad": history_load,
        "searchScope": if use_app_search { "visited_app_search_snapshots" } else { "currently_exposed_bodies" },
        "snapshotId": snapshot,
        "databaseCalls": 0,
        "appSearchUsed": use_app_search,
        "searchVerified": if use_app_search { Value::Bool(verified) } else { Value::Null },
        "stopReason": stop_reason,
        "searchVisits": visits,
        "duplicatesPossible": seen_snapshots.len() > 1,
        "nativeSearch": visits.first().cloned().unwrap_or(Value::Null),
    }))
}

/// Write a PNG to either the requested exclusive absolute path (`0600`) or a private temp file.
pub fn write_private_png(bytes: &[u8], output_path: Option<&str>) -> Result<PathBuf> {
    if let Some(path_str) = output_path {
        let path = PathBuf::from(path_str);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path)?;
        if let Err(err) = file.write_all(bytes) {
            let _ = std::fs::remove_file(&path);
            return Err(err.into());
        }
        Ok(path)
    } else {
        let mut temp = tempfile::Builder::new()
            .prefix("kax-attachment-")
            .suffix(".png")
            .tempfile()?;
        temp.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
        }
        let (_, path) = temp
            .keep()
            .map_err(|e| KaxError::runtime("OSError", e.to_string()))?;
        Ok(path)
    }
}

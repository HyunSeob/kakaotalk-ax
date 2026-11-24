//! KakaoTalk Windows Unicode clipboard records and the visible chat-room list parser.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::win32_image::Frame;

#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

fn message_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?is)^\[(?P<sender>.+?)\] \[(?P<time>(?:(?:오전|오후|AM|PM) )?\d{1,2}:\d{2}(?: (?:AM|PM))?)\](?: (?P<body>[\s\S]*))?$",
        )
        .unwrap()
    })
}

fn date_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:[-─━]+ *)?(?:\d{4}년 \d{1,2}월 \d{1,2}일(?: [월화수목금토일]요일)?|(?:(?:Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday),? )?(?:January|February|March|April|May|June|July|August|September|October|November|December) \d{1,2}, \d{4}|\d{4}[./-] *\d{1,2}[./-] *\d{1,2}\.?)(?: *[-─━]+)?$",
        )
        .unwrap()
    })
}

fn system_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:님이 (?:들어왔습니다|나갔습니다|초대했습니다)\.|삭제된 메시지입니다\.|This message was deleted\.)$")
            .unwrap()
    })
}

fn photo_count_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^사진 \d+장$").unwrap())
}

fn attachment_type(body: &str) -> Option<&'static str> {
    match body {
        "사진" | "Photo" | "Image" => Some("image"),
        "동영상" | "Video" => Some("video"),
        "이모티콘" | "Emoticon" => Some("emoticon"),
        "음성메시지" | "Voice Message" => Some("audio"),
        "파일" | "File" => Some("file"),
        _ if body.starts_with("파일: ") || body.starts_with("File: ") => Some("file"),
        _ if photo_count_regex().is_match(body) => Some("image"),
        _ => None,
    }
}

pub fn transcript_rows(text: &str) -> Vec<Value> {
    let separator = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut records: Vec<&str> = text.split(separator).collect();
    if records.last() == Some(&"") {
        records.pop();
    }
    let msg_re = message_regex();
    let date_re = date_regex();
    let sys_re = system_regex();

    let mut date: Option<String> = None;
    let mut rows: Vec<Value> = Vec::new();

    for line in records {
        let matched = msg_re.captures(line);
        let is_date = date_re.is_match(line);
        let is_system = sys_re.is_match(line);

        if matched.is_none()
            && !is_date
            && !is_system
            && separator == "\n"
            && rows
                .last()
                .and_then(|r| r.get("sender"))
                .and_then(|s| s.as_str())
                .is_some()
        {
            if let Some(last) = rows.last_mut().and_then(|r| r.as_object_mut()) {
                let prev = last
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                last.insert("text".to_string(), Value::String(format!("{prev}\n{line}")));
            }
            continue;
        }
        if is_date {
            date = Some(line.to_string());
        }
        let (kind, sender, time_text, body) = if let Some(caps) = matched {
            let s = caps.name("sender").map(|m| m.as_str().to_string());
            let t = caps.name("time").map(|m| m.as_str().to_string());
            let b = caps
                .name("body")
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            ("text", s, t, b)
        } else {
            (
                if is_date { "date" } else { "unclassified" },
                None,
                None,
                line.to_string(),
            )
        };
        let row_index = rows.len();
        rows.push(json!({
            "rowIndex": row_index,
            "kind": kind,
            "sender": sender,
            "timeText": time_text,
            "dateText": date,
            "text": body,
            "attachments": [],
            "links": [],
            "axParts": [],
        }));
    }

    for row in &mut rows {
        let is_text = row.get("kind").and_then(|v| v.as_str()) == Some("text");
        if !is_text {
            continue;
        }
        let body = row
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(att_type) = attachment_type(&body) {
            if let Some(obj) = row.as_object_mut() {
                obj.insert("kind".to_string(), json!("attachment"));
                obj.insert(
                    "attachments".to_string(),
                    json!([{
                        "type": att_type,
                        "description": body,
                        "title": Value::Null,
                        "textSource": "clipboard_marker",
                    }]),
                );
            }
        }
    }
    rows
}

pub fn badge_present(frame: &Frame, top: usize, bottom: usize, dpi: u32) -> bool {
    let scale = dpi as f64 / 96.0;
    let left = frame.width.saturating_sub((57.0 * scale).round() as usize);
    let right = frame
        .width
        .saturating_sub((13.0 * scale).round() as usize)
        .max(left);
    let mut occupied = 0usize;
    for y in top..bottom.min(frame.height) {
        for x in left..right.min(frame.width) {
            let (b, g, r) = frame.pixel(x, y);
            if r >= 205 && (r as i32) >= (g as i32) + 65 && (r as i32) >= (b as i32) + 55 {
                occupied += 1;
            }
        }
    }
    occupied >= (14.0 * scale * scale).round() as usize
}

pub fn self_badge_present(frame: &Frame, top: usize, bottom: usize, dpi: u32) -> bool {
    let scale = dpi as f64 / 96.0;
    let left = (79.0 * scale).round() as usize;
    let right = frame.width.min((96.0 * scale).round() as usize);
    let upper = top.max(top + (18.0 * scale).round() as usize);
    let lower = bottom.min(top + (35.0 * scale).round() as usize).min(frame.height);
    if right <= left || lower <= upper {
        return false;
    }
    let mut slate = 0usize;
    for y in upper..lower {
        for x in left..right {
            let (b, g, r) = frame.pixel(x, y);
            let mx = r.max(g).max(b);
            let mn = r.min(g).min(b);
            if (55..=125).contains(&r)
                && (60..=130).contains(&g)
                && (65..=140).contains(&b)
                && b >= r
                && (mx - mn) <= 28
            {
                slate += 1;
            }
        }
    }
    if slate < (80.0 * scale * scale).round() as usize {
        return false;
    }
    let core_left = (83.0 * scale).round() as usize;
    let core_right = frame.width.min((92.0 * scale).round() as usize);
    let core_top = top.max(top + (22.0 * scale).round() as usize);
    let core_bottom = bottom
        .min(top + (30.0 * scale).round() as usize)
        .min(frame.height);
    let mut glyph = 0usize;
    for y in core_top..core_bottom {
        for x in core_left..core_right {
            let (b, g, r) = frame.pixel(x, y);
            if b >= 230 && g >= 230 && r >= 230 {
                glyph += 1;
            }
        }
    }
    glyph >= ((6.0 * scale * scale).round() as usize).max(4)
}

#[derive(Debug, Clone)]
pub struct RoomItem {
    pub row_index: usize,
    pub name: String,
    pub preview: Option<String>,
    pub unread: bool,
    pub is_self: Option<bool>,
    pub point: (i32, i32),
}

impl RoomItem {
    pub fn to_public_json(&self) -> Value {
        json!({
            "rowIndex": self.row_index,
            "name": self.name,
            "preview": self.preview,
            "unread": self.unread,
            "isSelf": self.is_self,
            "textExact": false,
            "source": "windows_ocr",
        })
    }
}

pub fn room_items<F>(frame: &Frame, mut recognize: F, dpi: u32) -> Vec<RoomItem>
where
    F: FnMut(&Frame) -> Vec<OcrLine>,
{
    let scale = dpi as f64 / 96.0;
    let expected_height = (70.0 * scale).round() as usize;
    let anchor_left = (17.0 * scale).round() as usize;
    let anchor_right = (63.0 * scale).round() as usize;
    let text_left = (78.0 * scale).round() as usize;
    if frame.width <= text_left + (45.0 * scale).round() as usize {
        return Vec::new();
    }

    let bg_x = (5.0 * scale).round() as usize;
    let min_count = ((5.0 * scale).round() as usize).max(4);
    let mut sampled = Vec::with_capacity(frame.height);
    for y in 0..frame.height {
        let bg = frame.pixel(bg_x.min(frame.width - 1), y);
        let mut count = 0usize;
        let mut x = anchor_left;
        while x < anchor_right.min(frame.width) {
            let p = frame.pixel(x, y);
            let diff = (p.0 as i32 - bg.0 as i32)
                .abs()
                .max((p.1 as i32 - bg.1 as i32).abs())
                .max((p.2 as i32 - bg.2 as i32).abs());
            if diff > 18 {
                count += 1;
            }
            x += 2;
        }
        sampled.push(count >= min_count);
    }

    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut y = 0usize;
    while y < frame.height {
        if !sampled[y] {
            y += 1;
            continue;
        }
        let start = y;
        while y < frame.height && sampled[y] {
            y += 1;
        }
        if let Some(last) = runs.last_mut() {
            if ((start - last.1) as f64) < 6.0 * scale {
                last.1 = y;
                continue;
            }
        }
        runs.push((start, y));
    }

    let avatars: Vec<(usize, usize)> = runs
        .into_iter()
        .filter(|(a, b)| ((b - a) as f64) >= 23.0 * scale)
        .collect();
    if avatars.is_empty() {
        return Vec::new();
    }

    let first_mid = (avatars[0].0 + avatars[0].1) / 2;
    let origin = first_mid.saturating_sub(expected_height / 2);

    let mut items = Vec::new();
    for (index, (a, b)) in avatars.into_iter().enumerate() {
        let midpoint = (a + b) / 2;
        let top = if index == 0 {
            origin
        } else {
            midpoint.saturating_sub(expected_height / 2)
        };
        let bottom = frame
            .height
            .min(midpoint + expected_height / 2)
            .min(top + expected_height);
        let right = frame
            .width
            .saturating_sub((14.0 * scale).round() as usize)
            .max(text_left + 1)
            .min(frame.width);
        let is_self = self_badge_present(frame, top, bottom, dpi);
        let Ok(mut cropped) = frame.crop(text_left, top, right, bottom) else {
            continue;
        };
        if is_self {
            let bg_y = (top + (8.0 * scale).round() as usize).min(frame.height - 1);
            let bg = frame.pixel(bg_x.min(frame.width - 1), bg_y);
            cropped = cropped.fill(
                0,
                (17.0 * scale).round() as usize,
                (19.0 * scale).round() as usize,
                (36.0 * scale).round() as usize,
                bg,
            );
        }
        let raw_lines = recognize(&cropped.scale(2));
        let text_right = right
            .saturating_sub((55.0 * scale).round() as usize)
            .max(text_left + (50.0 * scale).round() as usize) as f64;
        let mut lines: Vec<OcrLine> = raw_lines
            .into_iter()
            .map(|l| OcrLine {
                text: l.text,
                x: l.x / 2.0 + text_left as f64,
                y: l.y / 2.0,
                width: l.width / 2.0,
                height: l.height / 2.0,
            })
            .filter(|l| l.x < text_right)
            .collect();
        lines.sort_by(|a, b| {
            a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x))
        });
        if lines.is_empty() || lines[0].y > 30.0 * scale {
            continue;
        }
        let mut name = lines[0].text.clone();
        if is_self
            && (name.starts_with("0 ") || name.starts_with("O ") || name.starts_with("ㅇ "))
        {
            let stripped = name
                .chars()
                .skip(2)
                .collect::<String>()
                .trim_start()
                .to_string();
            if !stripped.is_empty() {
                name = stripped;
            }
        }
        let preview_lines: Vec<&str> = lines[1..]
            .iter()
            .filter(|l| l.y >= lines[0].y + 10.0 * scale)
            .map(|l| l.text.as_str())
            .collect();
        let preview = if preview_lines.is_empty() {
            None
        } else {
            Some(preview_lines.join("\n"))
        };
        items.push(RoomItem {
            row_index: index,
            name,
            preview,
            unread: badge_present(frame, top, bottom, dpi),
            is_self: if is_self { Some(true) } else { None },
            point: ((46.0 * scale).round() as i32, midpoint as i32),
        });
    }
    items
}

pub fn blank_chat(frame: &Frame) -> bool {
    if frame.width < 40 || frame.height < 40 {
        return false;
    }
    let color = frame.pixel(4, 4);
    let left = 4;
    let right = frame.width - 18;
    for y in 4..(frame.height - 4) {
        for x in left..right {
            if frame.pixel(x, y) != color {
                return false;
            }
        }
    }
    true
}

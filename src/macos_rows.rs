//! Message-row facts of the macOS transcript (KakaoTalk Mac 26.10.0): sender, own messages,
//! time and date, unread counts, files, system notices, edited and cut messages.
//!
//! Row shapes measured on real rooms:
//! - someone else's first message of a group: `AXButton("프로필")`, the name `AXStaticText`,
//!   the body, and the time (`"오후 10:48"` or `"오후 10:48, 안읽음 5"`) on the group's last row;
//! - the user's own messages and others' follow-up messages: no profile and no name;
//! - a date separator: one `AXButton` described `"2026년 11월 23일 월요일"`;
//! - a system notice: one centered `AXTextArea` ("…님이 들어왔습니다." and the like);
//! - a file: `AXGroup("파일")`, the file name, `"유효기간: ~2026. 12. 8."`, `"용량: 516.5KB"`;
//! - an edited message: a separate `AXStaticText("수정됨")` after the body;
//! - a long message: the first 500 characters followed by `…`.
//!
//! The rows only show a name on someone else's first message, so whether a name-less row is
//! the user's own needs context: it is when it starts a group (after a row that ended a group
//! with its time, a date separator or a notice), which the driver confirms from the bubble's
//! position (right aligned), and follow-up rows continue the previous row's sender.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Map, Value};

const PROFILE_LABEL: &str = "프로필";
const FILE_LABEL: &str = "파일";
const EDITED_LABEL: &str = "수정됨";
const UNREAD_PREFIX: &str = "안읽음 ";
/// A cut message: 500 UTF-16 code units and an ellipsis; an emoji that starts before unit 500
/// is kept whole, so the kept part can run a few units longer (see
/// [`crate::ax_core::macos_displayed_text`]).
const CUT_TEXT_UNITS: usize = 500;
const CUT_TEXT_MAX_OVERRUN: usize = 32;

fn time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:(?:오전|오후|AM|PM)\s*)?\d{1,2}:\d{2}(?:\s*(?:AM|PM))?$").unwrap()
    })
}

fn separator_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{4})년 (\d{1,2})월 (\d{1,2})일 \S+요일$").unwrap())
}

fn dotted_date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d{4})\.\s*(\d{1,2})\.\s*(\d{1,2})\.?").unwrap())
}

fn role(part: &Value) -> &str {
    part.get("role").and_then(|v| v.as_str()).unwrap_or("")
}

fn value(part: &Value) -> &str {
    part.get("AXValue").and_then(|v| v.as_str()).unwrap_or("")
}

fn description(part: &Value) -> &str {
    part.get("AXDescription").and_then(|v| v.as_str()).unwrap_or("")
}

/// `"오후 10:48"` from a time label, also when the unread count follows (`"오후 10:48, 안읽음 5"`).
pub fn label_time(label: &str) -> Option<&str> {
    let first = label.split(", ").next().unwrap_or("").trim();
    time_re().is_match(first).then_some(first)
}

/// `5` from `"안읽음 5"` or `"오후 10:48, 안읽음 5"`.
pub fn label_unread(label: &str) -> Option<u64> {
    label
        .split(", ")
        .find_map(|piece| piece.trim().strip_prefix(UNREAD_PREFIX))
        .and_then(|n| n.trim().parse().ok())
}

/// `"2026-11-23"` from a date separator label `"2026년 11월 23일 월요일"`.
pub fn separator_date(label: &str) -> Option<String> {
    let c = separator_re().captures(label)?;
    iso_date(&c[1], &c[2], &c[3])
}

fn iso_date(y: &str, m: &str, d: &str) -> Option<String> {
    let (m, d): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| format!("{y}-{m:02}-{d:02}"))
}

/// Kinds of centered system notices, matched on their fixed wording.
const SYSTEM_PATTERNS: &[(&str, &str, Option<&str>)] = &[
    // (systemType, phrase, target suffix: the text before it names the person)
    ("notice", "공지가 등록되었습니다", None),
    ("deleted", "삭제되었습니다", None),
    ("hidden", "가렸습니다", None),
    ("kick", "님을 내보냈습니다", Some("님을 내보냈습니다")),
    ("leave", "님이 나갔습니다", Some("님이 나갔습니다")),
    ("leave", "나갔습니다", None),
    ("join", "님이 들어왔습니다", Some("님이 들어왔습니다")),
    ("invite", "초대했습니다", None),
    ("role", "님이 부방장이 되었습니다", Some("님이 부방장이 되었습니다")),
    ("role", "님이 방장이 되었습니다", Some("님이 방장이 되었습니다")),
    ("role", "되었습니다", None),
];

/// `(systemType, target)` when `text` reads like a system notice.
pub fn system_notice(text: &str) -> Option<(&'static str, Option<String>)> {
    let line = text.trim();
    for (kind, phrase, suffix) in SYSTEM_PATTERNS {
        if line.contains(phrase) {
            let target = suffix.and_then(|s| {
                let mut head = line.split(s).next().unwrap_or("").trim();
                // "방장이 A님을 내보냈습니다": the actor is not the target.
                for actor in ["방장이 ", "부방장이 ", "관리자가 ", "팀장이 "] {
                    if let Some(rest) = head.strip_prefix(actor) {
                        head = rest.trim();
                    }
                }
                (!head.is_empty() && head.len() <= 200).then(|| head.to_string())
            });
            return Some((kind, target));
        }
    }
    None
}

/// The user's own bubbles end 12–18pt from the transcript's right edge; others' start after
/// the 60pt avatar column.
pub fn own_side_layout(table_x: f64, table_w: f64, x: f64, w: f64) -> bool {
    let right_gap = (table_x + table_w) - (x + w);
    let left = x - table_x;
    right_gap <= 24.0 && left > 64.0
}

/// A system notice sits in the middle of the transcript.
pub fn centered_layout(table_x: f64, table_w: f64, x: f64, w: f64) -> bool {
    ((x + w / 2.0) - (table_x + table_w / 2.0)).abs() <= 12.0
}

/// What the driver must measure for a row to tell its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutNeed {
    None,
    /// Is the message bubble right aligned (the user's own)?
    OwnSide,
    /// Is the lone text centered (a system notice)?
    Centered,
    /// A notice-worded row where a group may start: centered (a notice) or, if not, right
    /// aligned (the user's own) — both from the same frame.
    Both,
}

/// The structural facts of one row that the sender rule uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowShape {
    pub empty: bool,
    pub separator: bool,
    pub profile: bool,
    pub has_time: bool,
    /// A lone text body without profile or time whose wording is a system notice.
    pub notice_wording: bool,
}

pub fn row_shape(parts: &[Value]) -> RowShape {
    let content: Vec<&Value> = parts
        .iter()
        .filter(|p| !matches!(role(p), "AXRow" | "AXCell"))
        .collect();
    let profile = content
        .iter()
        .any(|p| role(p) == "AXButton" && description(p) == PROFILE_LABEL);
    let has_time = content
        .iter()
        .any(|p| role(p) == "AXStaticText" && value(p).lines().any(|l| label_time(l).is_some()));
    let separator = content.len() == 1
        && role(content[0]) == "AXButton"
        && separator_date(description(content[0])).is_some();
    let notice_wording = !profile
        && !has_time
        && content.len() == 1
        && role(content[0]) == "AXTextArea"
        && system_notice(value(content[0])).is_some();
    RowShape { empty: content.is_empty(), separator, profile, has_time, notice_wording }
}

/// Running context of the sender rule while walking rows in order.
#[derive(Debug, Clone, Default)]
pub struct SenderContext {
    /// Kind of the last non-empty row: no row yet, a message, or a separator/notice.
    last: Option<LastRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastRow {
    Message { ended_group: bool },
    Break,
}

impl SenderContext {
    /// What to measure for the next row (call before [`SenderContext::push`]).
    pub fn need(&self, shape: &RowShape) -> LayoutNeed {
        if shape.empty || shape.separator {
            return LayoutNeed::None;
        }
        let starts = self.starts_group(shape);
        match (shape.notice_wording, starts) {
            (true, true) => LayoutNeed::Both,
            (true, false) => LayoutNeed::Centered,
            (false, true) => LayoutNeed::OwnSide,
            (false, false) => LayoutNeed::None,
        }
    }

    /// Whether a name-less message row here would start a group: after a row that ended its
    /// group with a time, a separator, a notice, or at the start of what was read. A follow-up
    /// row (the previous message showed no time) continues its sender.
    pub fn starts_group(&self, shape: &RowShape) -> bool {
        !shape.empty
            && !shape.separator
            && !shape.profile
            && !matches!(self.last, Some(LastRow::Message { ended_group: false }))
    }

    /// Records a row; `centered` is the measured layout of a notice-worded row.
    pub fn push(&mut self, shape: &RowShape, centered: Option<bool>) {
        if shape.empty {
            return;
        }
        self.last = if shape.separator || (shape.notice_wording && centered == Some(true)) {
            Some(LastRow::Break)
        } else {
            Some(LastRow::Message { ended_group: shape.has_time })
        };
    }
}

/// Row-local facts added to a parsed row (`item`, from [`crate::ax_core::ax_row`]).
pub fn enrich_row(row: &Value, item: &mut Map<String, Value>) {
    let parts: Vec<Value> = row.get("parts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let shape = row_shape(&parts);
    let content: Vec<&Value> = parts
        .iter()
        .filter(|p| !matches!(role(p), "AXRow" | "AXCell"))
        .collect();

    // Sender: the name text right after the profile button.
    let mut sender = Value::Null;
    if let Some(i) = content
        .iter()
        .position(|p| role(p) == "AXButton" && description(p) == PROFILE_LABEL)
    {
        if let Some(next) = content.get(i + 1) {
            let name = value(next).trim();
            if role(next) == "AXStaticText"
                && !name.is_empty()
                && name != EDITED_LABEL
                && label_time(name).is_none()
                && label_unread(name).is_none()
            {
                sender = json!(name);
            }
        }
    }

    let statics: Vec<&str> = content
        .iter()
        .filter(|p| role(p) == "AXStaticText")
        .map(|p| value(p))
        .collect();
    // Same rule as `row_shape`: a time on any line of a static text.
    let times: Vec<&str> = statics.iter().flat_map(|s| s.lines()).filter_map(label_time).collect();
    if item.get("timeText").is_none_or(Value::is_null) && times.len() == 1 {
        item.insert("timeText".to_string(), json!(times[0]));
    }
    let unread = statics.iter().find_map(|s| label_unread(s));
    let edited = statics.iter().any(|s| s.trim() == EDITED_LABEL);
    let truncated = content.iter().any(|p| {
        role(p) == "AXTextArea" && {
            let v = value(p);
            v.strip_suffix('…').is_some_and(|kept| {
                let units = kept.encode_utf16().count();
                (CUT_TEXT_UNITS..CUT_TEXT_UNITS + CUT_TEXT_MAX_OVERRUN).contains(&units)
            })
        }
    });

    item.insert("sender".to_string(), sender);
    item.insert("unreadCount".to_string(), json!(unread));
    item.insert("edited".to_string(), json!(edited));
    item.insert("truncated".to_string(), json!(truncated));

    if shape.separator {
        let label = description(content[0]);
        item.insert("kind".to_string(), json!("date_separator"));
        item.insert("dateText".to_string(), json!(label));
        item.insert("date".to_string(), json!(separator_date(label)));
        return;
    }
    if shape.notice_wording && row.get("centered") == Some(&Value::Bool(true)) {
        let (kind, target) = system_notice(value(content[0])).unwrap_or(("other", None));
        item.insert("kind".to_string(), json!("system"));
        item.insert("systemType".to_string(), json!(kind));
        item.insert("target".to_string(), json!(target));
        return;
    }
    if let Some(i) = content
        .iter()
        .position(|p| role(p) == "AXGroup" && description(p) == FILE_LABEL)
    {
        let rest = &content[i + 1..];
        let name = rest
            .iter()
            .find(|p| role(p) == "AXStaticText")
            .map(|p| value(p).trim().to_string())
            .filter(|n| !n.is_empty() && !n.starts_with("유효기간") && !n.starts_with("용량"));
        let size = rest
            .iter()
            .find_map(|p| value(p).trim().strip_prefix("용량:").map(|s| s.trim().to_string()));
        let expires = rest.iter().find_map(|p| {
            let v = value(p).trim().strip_prefix("유효기간:")?;
            let c = dotted_date_re().captures(v)?;
            iso_date(&c[1], &c[2], &c[3])
        });
        item.insert("kind".to_string(), json!("file"));
        item.insert("file".to_string(), json!({"name": name, "size": size, "expiresAt": expires}));
    }
}

fn is_message(item: &Map<String, Value>) -> bool {
    !matches!(
        item.get("kind").and_then(|v| v.as_str()),
        Some("date_separator" | "system")
    ) && item.get("_empty") != Some(&Value::Bool(true))
}

/// Fills the facts that need neighbouring rows, in transcript order: `date` / `dateText` from
/// the latest date separator above, `isSelf`, `groupSender`, `senderSource` and
/// `groupTimeText`. `rows` are the raw driver rows matching `items` one to one.
pub fn annotate_sequence(rows: &[Value], items: &mut [Value]) {
    let mut date: Option<(String, Option<String>)> = None;
    let mut ctx = SenderContext::default();
    // Sender of the previous message row: (isSelf, groupSender, ended its group).
    let mut prev: Option<(Option<bool>, Value, bool)> = None;
    // Indices of the group being built, for groupTimeText.
    let mut group: Vec<usize> = Vec::new();

    let flush = |group: &mut Vec<usize>, items: &mut [Value], time: Value| {
        for &i in group.iter() {
            if let Some(o) = items[i].as_object_mut() {
                o.insert("groupTimeText".to_string(), time.clone());
            }
        }
        group.clear();
    };

    for (i, row) in rows.iter().enumerate() {
        let parts: Vec<Value> = row.get("parts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let shape = row_shape(&parts);
        let centered = row.get("centered").and_then(|v| v.as_bool());
        let own_side = row.get("ownSide").and_then(|v| v.as_bool());
        let starts_group = ctx.starts_group(&shape);
        ctx.push(&shape, centered);
        let Some(obj) = items[i].as_object_mut() else { continue };
        if shape.empty {
            obj.insert("_empty".to_string(), json!(true));
        }

        if shape.separator {
            let label = obj.get("dateText").and_then(|v| v.as_str()).unwrap_or("").to_string();
            date = Some((label, obj.get("date").and_then(|v| v.as_str()).map(ToString::to_string)));
        } else if let Some((label, iso)) = &date {
            if obj.get("dateText").is_none_or(Value::is_null) {
                obj.insert("dateText".to_string(), json!(label));
            }
            obj.insert("date".to_string(), json!(iso));
        } else {
            obj.entry("date").or_insert(Value::Null);
        }

        let message = is_message(obj);
        if !message {
            obj.insert("isSelf".to_string(), Value::Null);
            obj.insert("groupSender".to_string(), Value::Null);
            obj.insert("senderSource".to_string(), Value::Null);
            obj.insert("groupTimeText".to_string(), Value::Null);
            if !shape.empty {
                // A separator or notice ends any open group without a time.
                flush(&mut group, items, Value::Null);
                prev = None;
            }
            continue;
        }

        let sender = obj.get("sender").cloned().unwrap_or(Value::Null);
        let (is_self, group_sender, source): (Option<bool>, Value, Value) = if shape.profile {
            (Some(false), sender.clone(), json!("row"))
        } else if starts_group {
            match own_side {
                Some(true) => (Some(true), Value::Null, json!("self_layout")),
                // Not right aligned: someone else's, but no name is shown and the previous
                // group already ended, so who sent it is not known.
                Some(false) => (Some(false), Value::Null, json!("not_own_layout")),
                None => (None, Value::Null, Value::Null),
            }
        } else {
            match &prev {
                Some((s, g, _)) => (*s, g.clone(), json!("inherited")),
                None => (None, Value::Null, Value::Null),
            }
        };
        obj.insert("isSelf".to_string(), json!(is_self));
        obj.insert("groupSender".to_string(), group_sender.clone());
        obj.insert("senderSource".to_string(), source);

        // A new group starts at a profile row or a group-starting row; it ends at a timed row.
        if shape.profile || starts_group {
            flush(&mut group, items, Value::Null);
        }
        group.push(i);
        if shape.has_time {
            let t = items[i].get("timeText").cloned().unwrap_or(Value::Null);
            flush(&mut group, items, t);
        }
        prev = Some((is_self, group_sender, shape.has_time));
    }
    flush(&mut group, items, Value::Null);
    for item in items.iter_mut() {
        if let Some(o) = item.as_object_mut() {
            o.remove("_empty");
        }
    }
}

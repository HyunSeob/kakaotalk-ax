//! Send confirmation from the transcript end (macOS `read_tail`) instead of full reads.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{
    is_single_emoji, macos_displayed_text, tail_rows, tail_verdict, AxDriver, AxEngine, KakaoBackend, TailRow, TailVerdict,
};
use kakaotalk_ax::error::{KaxError, Result};
use kakaotalk_ax::macos_ax_logic::SEND_TAIL_MAX_ROWS;
use kakaotalk_ax::state::sha256_hex;

/// A transcript row of the fixture window (380pt wide, like the measured chat windows).
#[derive(Clone, Debug)]
enum Row {
    /// The user's own text bubble: right aligned, 18pt from the edge.
    Own(String),
    /// Someone else's text bubble: avatar button, bubble from 60pt.
    Other(String),
    /// A single emoji rendered as a bare 86pt image on the own side.
    OwnEmoji,
    /// The same bare image from someone else.
    OtherEmoji,
    /// A photo (image with a description).
    Photo,
    /// An emoticon/sticker: a bare own-side image, larger than an emoji.
    OwnSticker,
    /// A date separator or notice row (unclassified).
    Notice,
}

fn row_json(index: usize, row: Option<&Row>, frames: bool) -> Value {
    let y = index as f64 * 40.0;
    let mut parts = Vec::new();
    if frames {
        parts.push(json!({"role": "AXRow", "frame": [0.0, y, 380.0, 36.0]}));
    }
    let mut push = |mut part: Value, frame: [f64; 4]| {
        if frames {
            part["frame"] = json!(frame);
        }
        parts.push(part);
    };
    match row {
        None => {} // spacer row under the last message
        Some(Row::Own(t)) => {
            // Bubbles wrap at 280pt like the measured 380pt windows.
            let w = (8.0 * t.chars().count() as f64).min(280.0);
            push(json!({"role": "AXTextArea", "AXValue": t}), [362.0 - w, y, w, 16.0]);
            push(json!({"role": "AXStaticText", "AXValue": "오후 3:21"}), [300.0 - w, y, 53.0, 16.0]);
        }
        Some(Row::Other(t)) => {
            push(json!({"role": "AXButton", "AXDescription": "프로필"}), [10.0, y, 36.0, 36.0]);
            push(json!({"role": "AXTextArea", "AXValue": t}), [60.0, y, 120.0, 16.0]);
        }
        Some(Row::OwnEmoji) => push(json!({"role": "AXImage"}), [282.0, y, 86.0, 86.0]),
        Some(Row::OtherEmoji) => {
            push(json!({"role": "AXButton", "AXDescription": "프로필"}), [10.0, y, 36.0, 36.0]);
            push(json!({"role": "AXImage"}), [60.0, y, 86.0, 86.0]);
        }
        Some(Row::Photo) => push(json!({"role": "AXImage", "AXDescription": "사진"}), [156.0, y, 212.0, 196.0]),
        Some(Row::OwnSticker) => push(json!({"role": "AXImage"}), [246.0, y, 122.0, 122.0]),
        Some(Row::Notice) => push(json!({"role": "AXButton", "AXDescription": "2026년 11월 28일 금요일"}), [104.0, y, 171.0, 28.0]),
    }
    json!({"rowIndex": index, "parts": parts})
}

struct TailState {
    rows: Vec<Row>,
    /// Supports `read_tail` (false: an older driver, full reads only).
    tail: bool,
    /// Report part frames in tail reads.
    frames: bool,
    /// What a dispatch adds: "own", "own_emoji", "with_incoming", "nothing", "flood".
    mode: String,
    sends: usize,
    full_reads: usize,
    tail_reads: Vec<Option<usize>>,
    /// The transcript exposes no AXRows: tail reads report `tail_rows_unsupported`.
    no_axrows: bool,
}

struct TailDriver {
    state: Mutex<TailState>,
}

impl TailDriver {
    fn new(rows: Vec<Row>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TailState {
                rows,
                tail: true,
                frames: true,
                mode: "own".to_string(),
                sends: 0,
                full_reads: 0,
                tail_reads: Vec::new(),
                no_axrows: false,
            }),
        })
    }
}

/// All rows of the window, with the trailing spacer row.
fn window_rows(st: &TailState, frames: bool) -> Vec<Value> {
    let mut out: Vec<Value> = st.rows.iter().enumerate().map(|(i, r)| row_json(i, Some(r), frames)).collect();
    out.push(row_json(st.rows.len(), None, frames));
    out
}

impl AxDriver for TailDriver {
    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        let mut st = self.state.lock().unwrap();
        match action {
            "read" => {
                st.full_reads += 1;
                let rows = window_rows(&st, false);
                Ok(json!({
                    "status": "ok",
                    "windowTitle": titles[0],
                    "processId": 1,
                    "exposedRowCount": rows.len(),
                    "rows": rows,
                    "rowStart": 0,
                }))
            }
            "inspect" => Ok(json!({"status": "ready"})),
            "send" => {
                st.sends += 1;
                let body = text.unwrap_or("").to_string();
                match st.mode.as_str() {
                    "own" => st.rows.push(Row::Own(macos_displayed_text(&body))),
                    "own_emoji" => st.rows.push(Row::OwnEmoji),
                    "with_incoming" => {
                        st.rows.push(Row::Other("들어온 다른 메시지".to_string()));
                        st.rows.push(Row::Own(body));
                    }
                    "other_emoji" => st.rows.push(Row::OtherEmoji),
                    "own_sticker" => st.rows.push(Row::OwnSticker),
                    "other_same" => st.rows.push(Row::Other(body)),
                    "notice_and_own" => {
                        st.rows.push(Row::Notice);
                        st.rows.push(Row::Own(body));
                    }
                    "photo_loaded_and_own" => {
                        // A baseline photo finishes loading (its parts change), then ours.
                        if let Some(i) = st.rows.iter().position(|r| matches!(r, Row::Photo)) {
                            st.rows[i] = Row::OwnSticker;
                        }
                        st.rows.push(Row::Own(body));
                    }
                    "flood" => {
                        for i in 0..=SEND_TAIL_MAX_ROWS {
                            st.rows.push(Row::Other(format!("들어온 메시지 {i}")));
                        }
                        st.rows.push(Row::Own(body));
                    }
                    _ => {}
                }
                Ok(json!({"status": "dispatch_result_unknown", "dispatchAttempted": true}))
            }
            other => panic!("unexpected native action {other}"),
        }
    }

    fn control(&self, action: &str, _args: &Map<String, Value>) -> Result<Value> {
        assert_eq!(action, "open");
        Ok(json!({"status": "ok", "alreadyOpen": true, "windowTitle": "Fixture"}))
    }

    fn read_tail(&self, _title: &str, start: Option<usize>, last: usize) -> Option<Result<Value>> {
        let mut st = self.state.lock().unwrap();
        if !st.tail {
            return None;
        }
        st.tail_reads.push(start);
        if st.no_axrows {
            return Some(Ok(json!({"status": "blocked", "reason": "tail_rows_unsupported"})));
        }
        let rows = window_rows(&st, st.frames);
        let count = rows.len();
        let from = match start {
            Some(s) if s > count => {
                return Some(Ok(json!({"status": "blocked", "reason": "transcript_rows_shrank"})))
            }
            Some(s) => s,
            None => count - last.min(count),
        };
        if count - from > SEND_TAIL_MAX_ROWS {
            return Some(Ok(json!({"status": "blocked", "reason": "send_tail_rows_exceeded"})));
        }
        Some(Ok(json!({
            "status": "ok",
            "windowTitle": "Fixture",
            "exposedRowCount": count,
            "rowStart": from,
            "rows": rows[from..].to_vec(),
        })))
    }

    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        Err(KaxError::validation("not used"))
    }
}

fn engine(rows: Vec<Row>) -> (Arc<TailDriver>, AxEngine<TailDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let driver = TailDriver::new(rows);
    let engine = AxEngine::with_roots(Arc::clone(&driver), dir.path().join("ui"), dir.path().join("operations"));
    (driver, engine, dir)
}

fn history(n: usize) -> Vec<Row> {
    (0..n)
        .map(|i| if i % 3 == 0 { Row::Other(format!("이전 {i}")) } else { Row::Own(format!("내 이전 {i}")) })
        .collect()
}

#[test]
fn delivered_text_is_observed_from_the_transcript_end_without_full_reads() {
    let (driver, engine, _dir) = engine(history(69));
    let out = engine.send_message("Fixture", "새 메시지", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
    assert_eq!(out["reason"], "new_text_occurrence_and_empty_composer");
    assert!(out["elapsedMs"].as_f64().is_some());
    let st = driver.state.lock().unwrap();
    assert_eq!(st.full_reads, 0, "tail mode must not read the whole transcript");
    assert_eq!(st.sends, 1);
    // Baseline: the last rows; confirmation: from the baseline start (69 rows + spacer, 15 rows).
    assert_eq!(st.tail_reads[0], None);
    assert!(st.tail_reads[1..].iter().all(|s| *s == Some(70 - 15)));
}

#[test]
fn nothing_new_or_an_old_equal_body_is_not_observed_and_never_resent() {
    let mut rows = history(20);
    rows.push(Row::Own("같은 말".to_string()));
    let (driver, engine, _dir) = engine(rows);
    driver.state.lock().unwrap().mode = "nothing".to_string();
    let out = engine.send_message("Fixture", "같은 말", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
    assert_eq!(out["reason"], "ax_confirmation_not_established");

    let again = engine.send_message("Fixture", "같은 말", "op-1", 0.0, false).unwrap();
    assert_eq!(again["nativeSendCalls"], 0);
    assert_eq!(driver.state.lock().unwrap().sends, 1);

    // It shows up later: send_result observes it from the stored tail baseline.
    driver.state.lock().unwrap().rows.push(Row::Own("같은 말".to_string()));
    let late = engine.send_result("op-1", 0.0).unwrap();
    assert_eq!(late["status"], "observed_in_ax");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
    assert_eq!(driver.state.lock().unwrap().full_reads, 0);
}

#[test]
fn an_incoming_message_during_the_send_needs_the_sent_row_on_the_own_side() {
    let (driver, engine, _dir) = engine(history(30));
    driver.state.lock().unwrap().mode = "with_incoming".to_string();
    let out = engine.send_message("Fixture", "보낸 말", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");

    // Without geometry the sent row cannot be told apart: stay unknown.
    let (driver, engine, _dir) = engine_with(history(30), |st| {
        st.mode = "with_incoming".to_string();
        st.frames = false;
    });
    let out = engine.send_message("Fixture", "보낸 말", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

fn engine_with(
    rows: Vec<Row>,
    f: impl FnOnce(&mut TailState),
) -> (Arc<TailDriver>, AxEngine<TailDriver>, tempfile::TempDir) {
    let (driver, engine, dir) = engine(rows);
    f(&mut driver.state.lock().unwrap());
    (driver, engine, dir)
}

#[test]
fn a_single_emoji_rendered_as_an_image_is_observed_with_its_own_reason() {
    let (_driver, engine, _dir) = engine_with(history(10), |st| st.mode = "own_emoji".to_string());
    let out = engine.send_message("Fixture", "💕", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
    assert_eq!(out["reason"], "new_emoji_image_row_and_empty_composer");

    // Someone else's emoji image is not ours.
    let (_driver, engine, _dir) = engine_with(history(10), |st| st.mode = "other_emoji".to_string());
    let out = engine.send_message("Fixture", "💕", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");

    // A bare image after non-emoji text is not taken as the message.
    let (_driver, engine, _dir) = engine_with(history(10), |st| st.mode = "own_emoji".to_string());
    let out = engine.send_message("Fixture", "가💕", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
}

#[test]
fn a_flood_of_new_rows_beyond_the_tail_limit_stays_unknown() {
    let (driver, engine, _dir) = engine_with(history(10), |st| st.mode = "flood".to_string());
    let out = engine.send_message("Fixture", "보낸 말", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
    assert_eq!(driver.state.lock().unwrap().full_reads, 0);
}

#[test]
fn older_drivers_and_older_journal_records_keep_the_full_read_confirmation() {
    let (driver, engine, dir) = engine_with(history(10), |st| st.tail = false);
    let out = engine.send_message("Fixture", "새 메시지", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
    assert!(driver.state.lock().unwrap().full_reads >= 2);
    let path = dir.path().join("operations").join(format!("{}.json", sha256_hex("op-1")));
    let record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(record.get("confirmMode").is_none());
    assert!(record.get("baselineTailHashes").is_some());

    // A pending record written by the previous version (no confirmMode) on a driver that now
    // supports tail reads: send_result still confirms with full reads.
    let (driver, engine, dir) = engine_with(history(10), |st| {
        st.tail = false;
        st.mode = "nothing".to_string();
    });
    assert_eq!(
        engine.send_message("Fixture", "늦은 메시지", "op-2", 0.0, false).unwrap()["status"],
        "outcome_unknown"
    );
    {
        let mut st = driver.state.lock().unwrap();
        st.tail = true;
        st.rows.push(Row::Own("늦은 메시지".to_string()));
    }
    let late = engine.send_result("op-2", 0.0).unwrap();
    assert_eq!(late["status"], "observed_in_ax");
    let st = driver.state.lock().unwrap();
    assert!(st.tail_reads.is_empty());
    drop(st);
    drop(dir);
}

#[test]
fn the_tail_journal_stores_hashes_and_counts_but_no_bodies() {
    let (_driver, engine, dir) = engine(history(10));
    engine.send_message("Fixture", "고유한 본문 문장", "op-1", 0.0, false).unwrap();
    let path = dir.path().join("operations").join(format!("{}.json", sha256_hex("op-1")));
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("고유한 본문 문장"));
    assert!(!content.contains("내 이전"));
    let record: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(record["confirmMode"], "tail_v1");
    assert_eq!(record["baselineRowCount"], 11);
    assert_eq!(record["singleEmoji"], false);
}

fn row(sig: &str, text: Option<&str>, own: Option<bool>, bare: bool) -> TailRow {
    TailRow { sig: sig.to_string(), text_hash: text.map(ToString::to_string), own_side: own, bare_image: bare }
}

#[test]
fn verdict_requires_the_baseline_order_growth_and_one_occurrence() {
    let base = vec!["t:a".to_string(), "a".to_string()];
    let kept = |extra: Vec<TailRow>| {
        let mut v = vec![row("t:a", Some("a"), Some(true), false), row("a", None, Some(false), false)];
        v.extend(extra);
        v
    };
    let mine = || row("t:h", Some("h"), Some(true), false);
    assert_eq!(tail_verdict(&base, 3, 4, &kept(vec![mine()]), "h", false), TailVerdict::Text);
    // Row count did not grow.
    assert_eq!(tail_verdict(&base, 3, 3, &kept(vec![mine()]), "h", false), TailVerdict::Pending);
    // A baseline row changed or vanished.
    let mut changed = kept(vec![mine()]);
    changed[0].sig = "t:z".to_string();
    assert_eq!(tail_verdict(&base, 3, 4, &changed, "h", false), TailVerdict::Pending);
    // Twice: ambiguous.
    assert_eq!(tail_verdict(&base, 3, 5, &kept(vec![mine(), mine()]), "h", false), TailVerdict::Pending);
    // Other new text: the sent row must be on the own side.
    let other = || row("t:o", Some("o"), Some(false), false);
    assert_eq!(tail_verdict(&base, 3, 5, &kept(vec![other(), mine()]), "h", false), TailVerdict::Text);
    let unknown_side = row("t:h", Some("h"), None, false);
    assert_eq!(tail_verdict(&base, 3, 5, &kept(vec![other(), unknown_side]), "h", false), TailVerdict::Pending);
    // Emoji image only for a single-emoji text, only one, only own side.
    let own_img = || row("a", None, Some(true), true);
    assert_eq!(tail_verdict(&base, 3, 4, &kept(vec![own_img()]), "h", true), TailVerdict::EmojiImage);
    assert_eq!(tail_verdict(&base, 3, 4, &kept(vec![own_img()]), "h", false), TailVerdict::Pending);
    assert_eq!(tail_verdict(&base, 3, 5, &kept(vec![own_img(), own_img()]), "h", true), TailVerdict::Pending);
    let other_img = row("a", None, Some(false), true);
    assert_eq!(tail_verdict(&base, 3, 4, &kept(vec![other_img]), "h", true), TailVerdict::Pending);
}

#[test]
fn tail_rows_skip_the_spacer_and_read_sides_from_frames() {
    let rows: Vec<Value> = vec![
        row_json(0, Some(&Row::Other("안녕".into())), true),
        row_json(1, Some(&Row::Own("반가워".into())), true),
        row_json(2, Some(&Row::OwnEmoji), true),
        row_json(3, Some(&Row::Photo), true),
        row_json(4, None, true),
    ];
    let parsed = tail_rows(&json!({"rows": rows}), |t| format!("h({t})"));
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed[0].own_side, Some(false));
    assert_eq!(parsed[1], row("t:h(반가워)", Some("h(반가워)"), Some(true), false));
    assert_eq!(parsed[2].sig, "a");
    assert!(parsed[2].bare_image);
    assert_eq!(parsed[2].own_side, Some(true));
    assert!(!parsed[3].bare_image, "a described photo is not an emoji");
    let no_frames = tail_rows(&json!({"rows": [row_json(0, Some(&Row::Own("x".into())), false)]}), |t| t.to_string());
    assert_eq!(no_frames[0].own_side, None);
}

#[test]
fn single_emoji_detection() {
    for yes in ["💕", "👍🏻", "❤️", "👨‍👩‍👧", "🇰🇷", "1️⃣", "☀️", "☀", "❤️‍🔥", "©️"] {
        assert!(is_single_emoji(yes), "{yes}");
    }
    for no in ["", "💕💕", "가💕", "a", "1", "💕 ", "ㅋ", "👍🏻👍🏻", "©", "↔", "™"] {
        assert!(!is_single_emoji(no), "{no:?}");
    }
}

#[test]
fn someone_elses_identical_text_alone_is_not_our_message() {
    let (driver, engine, _dir) = engine_with(history(10), |st| st.mode = "other_same".to_string());
    let out = engine.send_message("Fixture", "네", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn a_sticker_sized_image_does_not_confirm_a_single_emoji() {
    let (_driver, engine, _dir) = engine_with(history(10), |st| st.mode = "own_sticker".to_string());
    let out = engine.send_message("Fixture", "👍", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "outcome_unknown");
}

#[test]
fn separators_and_attachments_that_change_do_not_break_the_match() {
    let (_driver, engine, _dir) = engine_with(history(10), |st| st.mode = "notice_and_own".to_string());
    let out = engine.send_message("Fixture", "날짜 바뀐 뒤", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");

    let mut rows = history(10);
    rows.insert(5, Row::Photo);
    rows.push(Row::Notice);
    let (_driver, engine, _dir) = engine_with(rows, |st| st.mode = "photo_loaded_and_own".to_string());
    let out = engine.send_message("Fixture", "사진 뒤", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
}

#[test]
fn a_short_transcript_and_an_empty_one_confirm_from_row_zero() {
    for n in [0, 3] {
        let (driver, engine, _dir) = engine(history(n));
        let out = engine.send_message("Fixture", "첫 메시지", "op-1", 0.0, false).unwrap();
        assert_eq!(out["status"], "observed_in_ax", "{n} rows");
        assert!(driver.state.lock().unwrap().tail_reads[1..].iter().all(|s| *s == Some(0)));
    }
}

#[test]
fn a_transcript_without_axrows_falls_back_to_full_reads() {
    let (driver, engine, _dir) = engine_with(history(10), |st| st.no_axrows = true);
    let out = engine.send_message("Fixture", "새 메시지", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
    let st = driver.state.lock().unwrap();
    assert_eq!(st.tail_reads.len(), 1);
    assert!(st.full_reads >= 2);
}

#[test]
fn a_long_text_is_confirmed_by_the_cut_form_the_transcript_shows() {
    let long: String = "가나다라마바사아자차카타파하 ".repeat(45).chars().take(620).collect();
    let shown = macos_displayed_text(&long);
    assert_eq!(shown.chars().count(), 501);
    assert!(shown.ends_with('…') && long.starts_with(shown.trim_end_matches('…')));
    assert_eq!(macos_displayed_text("짧은 말"), "짧은 말");
    let exact: String = "가".repeat(500);
    assert_eq!(macos_displayed_text(&exact), exact);
    // Emoji count two UTF-16 units and are never split: "가😀" is 3 units, so after 166 pairs
    // and "가" (499 units) the next emoji still starts before 500 and is kept (501).
    let emoji: String = "가😀".repeat(300);
    let shown = macos_displayed_text(&emoji);
    assert_eq!(shown.trim_end_matches('…').encode_utf16().count(), 501);
    // Measured: a skin-toned thumb starting at unit 499 shows whole, to unit 503.
    let thumb = "가".repeat(499) + "👍🏻" + &"나".repeat(20);
    assert_eq!(macos_displayed_text(&thumb), "가".repeat(499) + "👍🏻…");
    let family = "가".repeat(498) + "👨‍👩‍👧" + "나나";
    assert_eq!(macos_displayed_text(&family), "가".repeat(498) + "👨‍👩‍👧…");
    assert!(emoji.starts_with(shown.trim_end_matches('…')));

    let (_driver, engine, _dir) = engine(history(10));
    let out = engine.send_message("Fixture", &long, "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "observed_in_ax");
}

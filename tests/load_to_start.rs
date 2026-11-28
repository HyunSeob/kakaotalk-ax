//! `load_more(to_start)`, `exposedOldestDate` and `read(from="oldest")` (macOS).
//! Names and texts are fictional; the start wordings are the measured app texts.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{load_to_start_by_steps, AxDriver, AxEngine, HeadCheck, KakaoBackend};
use kakaotalk_ax::error::{KaxError, Result};
use kakaotalk_ax::macos_rows::{head_oldest_date, HistoryStart};

fn check(start: HistoryStart) -> HeadCheck {
    HeadCheck { start, rows: None, status: "ok", oldest_date: None, top: None }
}

fn top(digest: &str) -> HeadCheck {
    HeadCheck { start: HistoryStart::Unknown, rows: None, status: "ok", oldest_date: None, top: Some(digest.into()) }
}

fn run(limit: Duration, steps: Vec<(u64, u64)>, heads: Vec<HeadCheck>) -> (Value, usize) {
    let steps = RefCell::new(steps.into_iter());
    let heads = RefCell::new(heads.into_iter());
    let calls = RefCell::new(0usize);
    let out = load_to_start_by_steps(
        "macos_accessibility",
        "가상방",
        limit,
        || {
            *calls.borrow_mut() += 1;
            let (b, a) = steps.borrow_mut().next().expect("unexpected extra load step");
            Ok(json!({"status": "ok", "exposedRowsBefore": b, "exposedRowsAfter": a}))
        },
        |_| heads.borrow_mut().next().unwrap_or_else(|| check(HistoryStart::Unknown)),
        &mut |_| {},
    )
    .unwrap();
    let n = *calls.borrow();
    (out, n)
}

const LONG: Duration = Duration::from_secs(300);

#[test]
fn to_start_runs_steps_until_the_start_notice_without_a_page_limit() {
    let mut steps = Vec::new();
    let mut heads = vec![check(HistoryStart::Unknown)];
    for i in 0..30u64 {
        steps.push((100 + i * 50, 150 + i * 50));
        heads.push(check(HistoryStart::Unknown));
    }
    heads.pop();
    heads.push(HeadCheck {
        start: HistoryStart::Confirmed("room_creation_notice", 1),
        rows: Some(1600),
        status: "ok",
        oldest_date: Some("2026-11-20".into()), top: None,
    });
    let (out, n) = run(LONG, steps, heads);
    assert_eq!(n, 30, "more than the 10-page limit of a normal load_more");
    assert_eq!(out["mode"], "to_start");
    assert_eq!(out["loadStopReason"], "start_of_history");
    assert_eq!(out["historyComplete"], true);
    assert_eq!(out["endOfHistory"], true);
    assert_eq!(out["historyCompleteReason"], "room_creation_notice");
    assert_eq!(out["exposedRowsBefore"], 100);
    assert_eq!(out["exposedRowsAfter"], 1600);
    assert_eq!(out["exposedOldestDate"], "2026-11-20");
    assert_eq!(out["snapshotId"], Value::Null);
    assert!(out["elapsedMs"].is_u64());
    // A summary: no per-step list, no rows.
    assert!(out.get("steps").is_none());
    assert!(serde_json::to_string(&out).unwrap().len() < 1500);
}

#[test]
fn to_start_stops_at_once_when_the_start_is_already_in_view() {
    let (out, n) = run(LONG, vec![], vec![HeadCheck {
        start: HistoryStart::Confirmed("team_chat_start", 2),
        rows: Some(979),
        status: "ok",
        oldest_date: Some("2025-01-15".into()), top: None,
    }]);
    assert_eq!(n, 0);
    assert_eq!(out["historyCompleteReason"], "team_chat_start");
    assert_eq!(out["exposedRowsAfter"], 979);
}

#[test]
fn to_start_accepts_a_group_invitation_only_after_a_second_attempt() {
    let invite = || check(HistoryStart::InviteAtTop(1));
    let (out, n) = run(LONG, vec![(500, 900), (900, 900), (900, 900)], vec![check(HistoryStart::Unknown), check(HistoryStart::Unknown), invite(), invite()]);
    assert_eq!(n, 3);
    assert_eq!(out["historyCompleteReason"], "group_invite_at_top");
    assert_eq!(out["loadStopReason"], "start_of_history");
}

#[test]
fn to_start_gives_up_after_several_empty_steps_without_claiming_the_start() {
    let (out, n) = run(LONG, vec![(500, 900), (900, 900), (900, 900), (900, 900), (900, 900)], vec![]);
    assert_eq!(n, 5);
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["historyComplete"], false);
    assert_eq!(out["endOfHistory"], Value::Null);
    // A pause in loading followed by more rows keeps going.
    let (out, n) = run(LONG, vec![(500, 500), (500, 500), (500, 800), (800, 800), (800, 800), (800, 800), (800, 800)], vec![]);
    assert_eq!(n, 7);
    assert_eq!(out["exposedRowsAfter"], 800);
}

#[test]
fn to_start_stops_at_its_time_limit() {
    let (out, n) = run(Duration::ZERO, vec![], vec![]);
    assert_eq!(n, 0);
    assert_eq!(out["loadStopReason"], "time_limit");
    assert_eq!(out["historyComplete"], false);
}

#[test]
fn a_failed_step_is_reported_with_what_was_loaded() {
    let out = load_to_start_by_steps(
        "macos_accessibility",
        "가상방",
        LONG,
        || Ok(json!({"status": "blocked", "reason": "history_scroll_not_supported"})),
        |_| check(HistoryStart::Unknown),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(out["status"], "blocked");
    assert_eq!(out["mode"], "to_start");
    assert_eq!(out["pagesAttempted"], 1);
}

#[test]
fn the_oldest_loaded_date_is_the_topmost_separator() {
    let sep = |d: &str| json!({"rowIndex": 0, "parts": [{"role": "AXRow"}, {"role": "AXButton", "AXDescription": d}]});
    assert_eq!(head_oldest_date(&[sep("2021년 4월 28일 수요일")]).as_deref(), Some("2021-04-28"));
    let msg = json!({"rowIndex": 0, "parts": [{"role": "AXRow"}, {"role": "AXTextArea", "AXValue": "앞부분"}]});
    assert_eq!(head_oldest_date(&[msg, sep("2026년 6월 11일 목요일")]).as_deref(), Some("2026-06-11"));
    assert_eq!(head_oldest_date(&[]), None);
}

// ---- read(from="oldest") with a fixture driver -------------------------------------------

/// A long transcript: a separator every 40 rows, others' and own rows alternating in groups.
struct LongDriver {
    rows: Mutex<Vec<Value>>,
    ranges: Mutex<Vec<(usize, usize)>>,
}

fn transcript(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let mut parts = vec![json!({"role": "AXRow", "frame": [0.0, 0.0, 380.0, 36.0]})];
            if i % 40 == 0 {
                let day = 1 + i / 40;
                parts.push(json!({"role": "AXButton", "AXDescription": format!("2021년 5월 {day}일 수요일")}));
                return json!({"rowIndex": i, "parts": parts});
            }
            if i % 4 == 1 {
                parts.push(json!({"role": "AXButton", "AXDescription": "프로필"}));
                parts.push(json!({"role": "AXStaticText", "AXValue": "김가상"}));
            }
            parts.push(json!({"role": "AXTextArea", "AXValue": format!("메시지 {i}")}));
            if i % 4 == 0 || i % 4 == 2 {
                parts.push(json!({"role": "AXStaticText", "AXValue": "오후 3:00"}));
            }
            let mut r = json!({"rowIndex": i, "parts": parts});
            // A nameless row after a timed one starts the user's own group: right aligned.
            if i % 4 == 3 || i % 4 == 0 {
                r["ownSide"] = json!(true);
            }
            r
        })
        .collect()
}

impl AxDriver for LongDriver {
    fn call(&self, _action: &str, _titles: &[String], _text: Option<&str>) -> Result<Value> {
        Err(KaxError::validation("not used"))
    }
    fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
        Err(KaxError::validation("not used"))
    }
    fn read_range(&self, _title: &str, start: usize, count: usize) -> Option<Result<Value>> {
        self.ranges.lock().unwrap().push((start, count));
        let rows = self.rows.lock().unwrap();
        let end = (start + count).min(rows.len());
        Some(Ok(json!({
            "status": "ok",
            "rows": rows[start.min(end)..end].to_vec(),
            "exposedRowCount": rows.len(),
            "rowStart": start,
        })))
    }
    fn read_head(&self, title: &str, n: usize) -> Option<Result<Value>> {
        self.read_range(title, 0, n)
    }
    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        Err(KaxError::validation("not used"))
    }
}

fn long_engine(n: usize) -> (Arc<LongDriver>, AxEngine<LongDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let driver = Arc::new(LongDriver { rows: Mutex::new(transcript(n)), ranges: Mutex::new(Vec::new()) });
    let engine = AxEngine::with_roots(Arc::clone(&driver), dir.path().join("ui"), dir.path().join("ops"));
    (driver, engine, dir)
}

#[test]
fn reading_from_the_oldest_pages_through_rows_beyond_the_newest_limit() {
    let (_driver, engine, _dir) = long_engine(2500);
    let first = engine.read_messages_from_oldest("가상방", 500, 0, "").unwrap();
    assert_eq!(first["status"], "ok");
    assert_eq!(first["from"], "oldest");
    let items = first["items"].as_array().unwrap();
    assert_eq!(items.len(), 500);
    assert_eq!(items[0]["rowIndex"], 0);
    assert_eq!(items[0]["kind"], "date_separator");
    assert_eq!(first["total"], 2500);
    assert_eq!(first["nextOffset"], 500);
    assert_eq!(first["coverage"]["exposedOldestDate"], "2021-05-01");
    let snap = first["snapshotId"].as_str().unwrap().to_string();

    // Page through to the end with the same token.
    let mut offset = 500;
    let mut seen = 500;
    while let Some(next) = {
        let page = engine.read_messages_from_oldest("가상방", 500, offset, &snap).unwrap();
        assert_eq!(page["status"], "ok");
        let items = page["items"].as_array().unwrap();
        assert_eq!(items[0]["rowIndex"], offset);
        seen += items.len();
        page["nextOffset"].as_i64()
    } {
        offset = next;
    }
    assert_eq!(seen, 2500);
}

#[test]
fn an_oldest_page_gets_its_date_and_sender_context_from_rows_above_it() {
    let (driver, engine, _dir) = long_engine(2500);
    // Row 1210: 10 rows after the separator at 1200; a follow-up of the group starting at 1209.
    let page = engine.read_messages_from_oldest("가상방", 20, 1210, "").unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items[0]["rowIndex"], 1210);
    assert_eq!(items[0]["date"], "2021-05-31");
    assert!(items.iter().all(|i| !i["isSelf"].is_null() || i["kind"] == "date_separator"));
    // Context rows are read above the page, and no more than needed.
    let ranges = driver.ranges.lock().unwrap().clone();
    assert!(ranges.contains(&(1180, 50)), "{ranges:?}");

    // A page whose context has no separator scans further up for the date.
    let page = engine.read_messages_from_oldest("가상방", 5, 1239, "").unwrap();
    assert_eq!(page["items"][0]["date"], "2021-05-31");
}

#[test]
fn loading_older_rows_between_pages_changes_the_token() {
    let (driver, engine, _dir) = long_engine(1200);
    let first = engine.read_messages_from_oldest("가상방", 100, 0, "").unwrap();
    let snap = first["snapshotId"].as_str().unwrap().to_string();
    // Older rows arrive on top: every position shifts.
    {
        let mut rows = driver.rows.lock().unwrap();
        let mut older = transcript(40);
        older[0]["parts"][1]["AXDescription"] = json!("2021년 4월 30일 금요일");
        older.extend(rows.drain(..));
        *rows = older;
    }
    let page = engine.read_messages_from_oldest("가상방", 100, 100, &snap).unwrap();
    assert_eq!(page["status"], "snapshot_changed");
    // New messages at the bottom do not change it.
    let (driver, engine, _dir) = long_engine(1200);
    let snap = engine.read_messages_from_oldest("가상방", 100, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    driver.rows.lock().unwrap().extend(transcript(1205).split_off(1200));
    assert_eq!(engine.read_messages_from_oldest("가상방", 100, 100, &snap).unwrap()["status"], "ok");
}

#[test]
fn reading_from_the_oldest_validates_and_needs_driver_support() {
    let (_driver, engine, _dir) = long_engine(10);
    assert!(engine.read_messages_from_oldest("가상방", 501, 0, "").is_err());
    assert!(engine.read_messages_from_oldest("가상방", 10, -1, "").is_err());
    // Past the end: an empty page.
    let page = engine.read_messages_from_oldest("가상방", 10, 50, "").unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 0);
    assert_eq!(page["hasMore"], false);
}

#[test]
fn new_messages_at_the_bottom_are_not_taken_for_older_rows() {
    // Row counts grow (messages arriving) but the oldest row stays: nothing older loaded.
    let (out, n) = run(
        LONG,
        vec![(500, 501), (501, 502), (502, 503), (503, 504)],
        vec![top("a"), top("a"), top("a"), top("a"), top("a")],
    );
    assert_eq!(n, 4);
    assert_eq!(out["loadStopReason"], "no_new_rows");
    // The oldest row changes: older rows loaded, the idle count restarts.
    let (out, n) = run(
        LONG,
        vec![(500, 500), (500, 500), (500, 800), (800, 800), (800, 800), (800, 800), (800, 800)],
        vec![top("a"), top("a"), top("a"), top("b"), top("b"), top("b"), top("b"), top("b")],
    );
    assert_eq!(n, 7);
    assert_eq!(out["loadStopReason"], "no_new_rows");
}

#[test]
fn a_driver_error_mid_way_keeps_what_was_loaded() {
    let calls = RefCell::new(0);
    let out = load_to_start_by_steps(
        "macos_accessibility",
        "가상방",
        LONG,
        || {
            *calls.borrow_mut() += 1;
            if *calls.borrow() == 3 {
                return Err(KaxError::runtime("TimeoutError", "timed out"));
            }
            let n = *calls.borrow() as u64;
            Ok(json!({"status": "ok", "exposedRowsBefore": 100 * n, "exposedRowsAfter": 100 * (n + 1)}))
        },
        |_| check(HistoryStart::Unknown),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(out["status"], "bridge_error");
    assert_eq!(out["pagesAttempted"], 3);
    assert_eq!(out["exposedRowsAfter"], 300);
    assert_eq!(out["historyComplete"], false);
}

#[test]
fn an_oldest_page_starting_at_a_separator_needs_no_date_scan_and_drops_the_spacer() {
    let (driver, engine, _dir) = long_engine(2500);
    driver.ranges.lock().unwrap().clear();
    let page = engine.read_messages_from_oldest("가상방", 10, 1200, "").unwrap();
    assert_eq!(page["items"][0]["kind"], "date_separator");
    let ranges = driver.ranges.lock().unwrap().clone();
    // Head look, the page with its context, the head look again: no upward scan.
    assert_eq!(ranges.len(), 3, "{ranges:?}");

    // An empty spacer as the last row is not returned.
    let (driver, engine, _dir) = long_engine(100);
    driver.rows.lock().unwrap().push(json!({"rowIndex": 100, "parts": [{"role": "AXRow"}]}));
    let page = engine.read_messages_from_oldest("가상방", 500, 0, "").unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 100);
    assert_eq!(page["hasMore"], false);
}

#[test]
fn the_oldest_page_token_ignores_layout_verdicts_unread_counts_and_new_messages() {
    let (driver, engine, _dir) = long_engine(300);
    let snap = engine.read_messages_from_oldest("가상방", 50, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    {
        let mut rows = driver.rows.lock().unwrap();
        // A layout verdict missing this time, and an unread count dropping on an old row.
        rows[3].as_object_mut().unwrap().remove("ownSide");
        rows[2]["parts"].as_array_mut().unwrap().push(json!({"role": "AXStaticText", "AXValue": "안읽음 1"}));
    }
    let snap2 = engine.read_messages_from_oldest("가상방", 50, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    assert_eq!(snap, snap2);
    // A tiny room whose head reaches the newest rows: a new message keeps the token.
    let (driver, engine, _dir) = long_engine(4);
    let snap = engine.read_messages_from_oldest("가상방", 50, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    driver.rows.lock().unwrap().push(json!({"rowIndex": 4, "parts": [{"role": "AXRow"}]}));
    let snap2 = engine.read_messages_from_oldest("가상방", 50, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    assert_eq!(snap, snap2);
}

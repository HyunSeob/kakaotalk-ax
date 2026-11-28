//! `read(from="oldest")`: pages counted from the oldest loaded row, beyond the 1000-row limit.
//! Row shapes follow KakaoTalk Mac 26.10.0; all names and texts are fictional.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;

fn sep(label: &str) -> Value {
    json!({"parts": [{"role": "AXButton", "AXDescription": label}]})
}
fn text(v: &str) -> Value {
    json!({"parts": [{"role": "AXTextArea", "AXValue": v}]})
}
fn spacer() -> Value {
    json!({"parts": [{"role": "AXRow"}]})
}

struct Driver {
    rows: Mutex<Vec<Value>>,
    ranges: bool,
    /// Older rows that appear right after the first range read (loading meanwhile).
    load_during_read: Mutex<Option<Vec<Value>>>,
    /// Range reads answered `blocked` from this call number on.
    block_from: Mutex<Option<usize>>,
    calls: Mutex<usize>,
}

impl Driver {
    fn new(rows: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(rows),
            ranges: true,
            load_during_read: Mutex::new(None),
            block_from: Mutex::new(None),
            calls: Mutex::new(0),
        })
    }
}

impl AxDriver for Driver {
    fn call(&self, _action: &str, _titles: &[String], _text: Option<&str>) -> Result<Value> {
        unreachable!("from=oldest never reads the newest rows")
    }
    fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
        unreachable!()
    }
    fn read_range(&self, window_title: &str, start: usize, count: usize) -> Option<Result<Value>> {
        if !self.ranges {
            return None;
        }
        let call = {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            *c
        };
        if self.block_from.lock().unwrap().is_some_and(|n| call >= n) {
            return Some(Ok(json!({"status": "blocked", "reason": "transcript_changed_during_read"})));
        }
        let mut rows = self.rows.lock().unwrap();
        let out: Vec<Value> = rows
            .iter()
            .enumerate()
            .skip(start)
            .take(count)
            .map(|(i, r)| {
                let mut r = r.clone();
                r["rowIndex"] = json!(i);
                r
            })
            .collect();
        let total = rows.len();
        if let Some(older) = self.load_during_read.lock().unwrap().take() {
            let mut v = older;
            v.extend(rows.drain(..));
            *rows = v;
        }
        Some(Ok(json!({"status": "ok", "windowTitle": window_title, "rows": out,
                       "exposedRowCount": total, "rowStart": start, "elapsedMs": 1})))
    }
    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        unreachable!()
    }
}

fn engine(driver: &Arc<Driver>) -> (AxEngine<Driver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let e = AxEngine::with_roots(Arc::clone(driver), dir.path().join("ui"), dir.path().join("operations"));
    (e, dir)
}

/// 1 row from an earlier day, then Aug 1 (300 rows), Aug 2 (300 rows), and the spacer.
fn history() -> Vec<Value> {
    let mut rows = vec![text("앞날 끝")];
    rows.push(sep("2026년 8월 1일 토요일"));
    rows.extend((0..300).map(|i| text(&format!("첫날 {i}"))));
    rows.push(sep("2026년 8월 2일 일요일"));
    rows.extend((0..300).map(|i| text(&format!("둘째 날 {i}"))));
    rows.push(spacer());
    rows
}

#[test]
fn pages_run_from_the_oldest_row_with_dates_from_above_and_without_the_spacer() {
    let driver = Driver::new(history());
    let (e, _d) = engine(&driver);
    let first = e.read_messages_from_oldest("Fixture", 200, 0, "").unwrap();
    assert_eq!(first["status"], "ok");
    assert_eq!(first["from"], "oldest");
    assert_eq!(first["items"][0]["text"], "앞날 끝");
    assert_eq!(first["items"][0]["date"], Value::Null, "above the first separator: unknown");
    assert_eq!(first["items"][2]["date"], "2026-08-01");
    assert_eq!(first["nextOffset"], 200);
    assert_eq!(first["coverage"]["exposedOldestDate"], "2026-08-01");
    assert_eq!(first["coverage"]["historyComplete"], false);
    let token = first["snapshotId"].as_str().unwrap().to_string();

    // Page 2 starts mid-day: its separator is far above, found by the upward scan.
    let second = e.read_messages_from_oldest("Fixture", 50, 200, &token).unwrap();
    assert_eq!(second["items"][0]["rowIndex"], 200);
    assert_eq!(second["items"][0]["date"], "2026-08-01");
    assert_eq!(second["items"].as_array().unwrap().len(), 50);
    assert_eq!(second["coverage"]["dateSearch"], "above");
    assert_eq!(second["coverage"]["exposedOldestDate"], Value::Null, "first page only");

    // Last page: the spacer under the newest message is not returned.
    let last = e.read_messages_from_oldest("Fixture", 500, 500, &token).unwrap();
    let items = last["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["text"], "둘째 날 299");
    assert_eq!(items.last().unwrap()["date"], "2026-08-02");
    assert_eq!(last["hasMore"], false);
    assert_eq!(last["nextOffset"], Value::Null);
    assert_eq!(last["total"], 603, "the spacer is not counted");

    // A page ending just above the spacer has no empty next page.
    let edge = e.read_messages_from_oldest("Fixture", 103, 500, &token).unwrap();
    assert_eq!(edge["items"].as_array().unwrap().len(), 103);
    assert_eq!(edge["hasMore"], false);
    // At or past the end: an empty page without reading rows.
    let calls = *driver.calls.lock().unwrap();
    let past = e.read_messages_from_oldest("Fixture", 10, 700, &token).unwrap();
    assert_eq!(past["items"], json!([]));
    assert_eq!(past["hasMore"], false);
    assert_eq!(*driver.calls.lock().unwrap(), calls + 1, "only the head read");
}

#[test]
fn a_failed_recheck_never_confirms_the_page() {
    let driver = Driver::new(history());
    // Head read (1), page read (2), then the re-check (3) is blocked.
    *driver.block_from.lock().unwrap() = Some(3);
    let (e, _d) = engine(&driver);
    let out = e.read_messages_from_oldest("Fixture", 10, 0, "").unwrap();
    assert_eq!(out["status"], "blocked");
    assert_eq!(out["reason"], "transcript_changed_during_read");
}

#[test]
fn a_repeated_short_top_row_still_changes_the_token() {
    let mut rows = vec![text("네"), text("알겠어"), text("그럼 내일")];
    rows.extend(history());
    let driver = Driver::new(rows);
    let (e, _d) = engine(&driver);
    let token = e.read_messages_from_oldest("Fixture", 10, 0, "").unwrap()["snapshotId"].as_str().unwrap().to_string();
    // Older rows load and the new top row is again "네".
    {
        let mut r = driver.rows.lock().unwrap();
        for t in ["다른 말", "또 다른 말", "네"].iter().rev() {
            r.insert(0, text(t));
        }
        r.insert(0, text("네"));
    }
    let out = e.read_messages_from_oldest("Fixture", 10, 10, &token).unwrap();
    assert_eq!(out["status"], "snapshot_changed");
}

#[test]
fn new_messages_keep_positions_and_older_rows_change_the_token() {
    let driver = Driver::new(history());
    let (e, _d) = engine(&driver);
    let first = e.read_messages_from_oldest("Fixture", 10, 0, "").unwrap();
    let token = first["snapshotId"].as_str().unwrap().to_string();
    // A new message at the bottom: same positions, same token.
    {
        let mut rows = driver.rows.lock().unwrap();
        let n = rows.len();
        rows.insert(n - 1, text("새 메시지"));
    }
    let again = e.read_messages_from_oldest("Fixture", 10, 10, &token).unwrap();
    assert_eq!(again["status"], "ok");
    assert_eq!(again["snapshotId"], token);
    // Older rows loaded: every position shifts.
    driver.rows.lock().unwrap().insert(0, text("더 오래된 말"));
    let changed = e.read_messages_from_oldest("Fixture", 10, 20, &token).unwrap();
    assert_eq!(changed["status"], "snapshot_changed");
    assert_ne!(changed["snapshotId"], token);
}

#[test]
fn rows_loaded_while_a_page_is_read_fail_the_page() {
    let driver = Driver::new(history());
    *driver.load_during_read.lock().unwrap() = Some(vec![text("읽는 중 불러온 말")]);
    let (e, _d) = engine(&driver);
    let out = e.read_messages_from_oldest("Fixture", 10, 0, "").unwrap();
    assert_eq!(out["status"], "snapshot_changed");
}

#[test]
fn counts_use_the_same_kinds_as_read() {
    let driver = Driver::new(history());
    let (e, _d) = engine(&driver);
    let out = e.read_messages_from_oldest("Fixture", 5, 0, "").unwrap();
    // 앞날 끝, separator, 첫날 0..2: separators count as unclassified as in `read`.
    assert_eq!(out["textMessageCount"], 4);
    assert_eq!(out["unclassifiedRowCount"], 1);
}

#[test]
fn a_driver_without_range_reads_is_unsupported() {
    let driver = Arc::new(Driver {
        rows: Mutex::new(history()),
        ranges: false,
        load_during_read: Mutex::new(None),
        block_from: Mutex::new(None),
        calls: Mutex::new(0),
    });
    let (e, _d) = engine(&driver);
    let out = e.read_messages_from_oldest("Fixture", 10, 0, "").unwrap();
    assert_eq!(out["status"], "blocked");
    assert_eq!(out["reason"], "read_from_oldest_unsupported");
    assert!(e.read_messages_from_oldest("Fixture", 501, 0, "").is_err());
    assert!(e.read_messages_from_oldest("Fixture", 10, 10, "").is_err(), "a later page needs the token");
}

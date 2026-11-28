//! `exposedOldestDate`: the oldest loaded day beyond the newest-rows read limit.
//! Row shapes follow KakaoTalk Mac 26.10.0; all names and texts are fictional.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{load_more_summary, AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;
use kakaotalk_ax::macos_rows::head_oldest_date;

fn separator(i: usize, label: &str) -> Value {
    json!({"rowIndex": i, "parts": [{"role": "AXButton", "AXDescription": label}]})
}
fn body(i: usize, v: &str) -> Value {
    json!({"rowIndex": i, "parts": [{"role": "AXTextArea", "AXValue": v}]})
}

/// Returns `read` rows starting with a separator; `truncated` adds the driver's topmost date.
struct Driver {
    truncated: bool,
    exposed: Option<&'static str>,
    reads: Mutex<usize>,
}

impl AxDriver for Driver {
    fn call(&self, action: &str, titles: &[String], _text: Option<&str>) -> Result<Value> {
        assert!(matches!(action, "read" | "read_all"), "unexpected {action}");
        *self.reads.lock().unwrap() += 1;
        let mut out = json!({
            "status": "ok", "windowTitle": titles[0], "processId": 1,
            "rows": [body(0, "앞날 끝"), separator(1, "2026년 8월 1일 토요일"), body(2, "안녕")],
            "exposedRowCount": if self.truncated { 1500 } else { 3 },
            "rowStart": if self.truncated { 1497 } else { 0 },
            "rowsTruncated": self.truncated,
            "elapsedMs": 1,
        });
        if let Some(d) = self.exposed {
            out["exposedOldestDate"] = json!(d);
        }
        Ok(out)
    }
    fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
        unreachable!()
    }
    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        unreachable!()
    }
}

fn read(truncated: bool, exposed: Option<&'static str>) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let driver = Arc::new(Driver { truncated, exposed, reads: Mutex::new(0) });
    let engine = AxEngine::with_roots(driver, dir.path().join("ui"), dir.path().join("operations"));
    engine.read_messages("Fixture", 50, 0, "").unwrap()
}

#[test]
fn a_truncated_read_reports_the_drivers_topmost_date_apart_from_oldest_date() {
    let out = read(true, Some("2025-02-11"));
    assert_eq!(out["coverage"]["oldestDate"], "2026-08-01");
    assert_eq!(out["coverage"]["exposedOldestDate"], "2025-02-11");
    // No separator found in the first rows: unknown, never a guess from the rows read.
    let out = read(true, None);
    assert_eq!(out["coverage"]["exposedOldestDate"], Value::Null);
}

#[test]
fn an_untruncated_read_has_read_its_oldest_day() {
    let out = read(false, None);
    assert_eq!(out["coverage"]["exposedOldestDate"], "2026-08-01");
    assert_eq!(out["coverage"]["exposedOldestDate"], out["coverage"]["oldestDate"]);
}

#[test]
fn the_load_more_summary_keeps_exposed_oldest_date() {
    let full = json!({
        "status": "ok", "source": "macos_accessibility", "windowTitle": "가상방",
        "pagesAttempted": 1, "steps": [],
        "after": {"status": "ok", "snapshotId": "abc", "total": 3, "items": [],
                  "coverage": {"oldestDate": "2026-08-01", "exposedOldestDate": "2025-02-11"}},
    });
    let s = load_more_summary(&full);
    assert_eq!(s["after"]["exposedOldestDate"], "2025-02-11");
}

#[test]
fn the_topmost_separator_gives_the_oldest_loaded_day() {
    let rows = vec![body(0, "앞날 끝"), separator(1, "2025년 2월 11일 화요일"), separator(9, "2025년 2월 12일 수요일")];
    assert_eq!(head_oldest_date(&rows).as_deref(), Some("2025-02-11"));
    assert_eq!(head_oldest_date(&[body(0, "2025년 2월 11일 화요일")]), None, "a message body is not a separator");
    assert_eq!(head_oldest_date(&[separator(0, "가상 버튼")]), None);
}

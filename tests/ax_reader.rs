use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;

fn text_row(index: usize, text: &str, time: Option<&str>, date: Option<&str>) -> Value {
    let mut parts = Vec::new();
    if let Some(t) = time {
        let mut p = Map::new();
        p.insert("role".to_string(), json!("AXStaticText"));
        p.insert("AXValue".to_string(), json!(t));
        if let Some(d) = date {
            p.insert("AXHelp".to_string(), json!(d));
        }
        parts.push(Value::Object(p));
    }
    parts.push(json!({"role": "AXTextArea", "AXValue": text}));
    json!({"rowIndex": index, "parts": parts})
}

struct SnapshotDriver {
    calls: Mutex<Vec<(String, Vec<String>, Option<String>)>>,
    raw: Mutex<Value>,
}

impl SnapshotDriver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            raw: Mutex::new(json!({
                "status": "ok",
                "windowTitle": "Fixture",
                "processId": 123,
                "rows": [
                    text_row(0, "같은 말", Some("오전 11:09"), Some("2026. 11. 2.")),
                    text_row(1, "같은 말", None, None),
                    text_row(2, "오전 12:34", None, None),
                    text_row(3, "줄 하나\n줄 둘 🙂", None, None),
                    {"rowIndex": 4, "parts": [{"role": "AXImage", "AXDescription": "사진"}]},
                ],
                "exposedRowCount": 5,
                "rowStart": 0,
                "rowsTruncated": false,
                "elapsedMs": 80,
            })),
        })
    }
}

impl AxDriver for SnapshotDriver {
    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        self.calls.lock().unwrap().push((
            action.to_string(),
            titles.to_vec(),
            text.map(ToString::to_string),
        ));
        Ok(self.raw.lock().unwrap().clone())
    }

    fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
        Ok(json!({"status": "ok"}))
    }

    fn capture_attachment(
        &self,
        _window_title: &str,
        _row_index: i64,
        _expected_parts: &[Value],
        _expected_count: usize,
        _ocr: bool,
    ) -> Result<Value> {
        Ok(json!({"status": "ok"}))
    }
}

fn make_engine() -> (Arc<SnapshotDriver>, AxEngine<SnapshotDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let driver = SnapshotDriver::new();
    let engine = AxEngine::with_roots(
        Arc::clone(&driver),
        dir.path().join("ui"),
        dir.path().join("ops"),
    );
    (driver, engine, dir)
}

#[test]
fn python_snapshot_token_survives_upgrade() {
    let raw = json!({
        "processId": 123, "exposedRowCount": 1, "rowStart": 0, "rowsTruncated": false,
        "windowTitle": "테스트",
        "rows": [{"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "a, b: c 🙂"}]}],
    });
    assert_eq!(kakaotalk_ax::ax_core::snapshot_digest(&raw), "26b53285096eb8889a28c350");
}

#[test]
fn read_keeps_multiline_emoji_and_distinct_repeated_messages() {
    let (driver, engine, _dir) = make_engine();
    let result = engine.read_messages("Fixture", 50, 0, "").unwrap();
    let texts: Vec<Option<&str>> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            Some("같은 말"),
            Some("같은 말"),
            Some("오전 12:34"),
            Some("줄 하나\n줄 둘 🙂"),
            None,
        ]
    );
    assert_eq!(result["databaseCalls"], 0);
    assert_eq!(result["uiActions"], 0);
    assert_eq!(
        *driver.calls.lock().unwrap(),
        vec![("read".to_string(), vec!["Fixture".to_string()], None)]
    );
}

#[test]
fn missing_sender_or_grouped_time_is_not_invented() {
    let (_driver, engine, _dir) = make_engine();
    let result = engine.read_messages("Fixture", 50, 0, "").unwrap();
    let items = result["items"].as_array().unwrap();
    assert_eq!(items[0]["timeText"], "오전 11:09");
    assert_eq!(items[0]["dateText"], "2026. 11. 2.");
    assert!(items[1]["timeText"].is_null());
    assert!(items[1]["dateText"].is_null());
    assert!(items[2]["timeText"].is_null());
    assert!(items.iter().all(|i| i["sender"].is_null()));
}

#[test]
fn time_combined_with_unread_count_is_read_without_inventing_a_sender() {
    let (driver, engine, _dir) = make_engine();
    driver.raw.lock().unwrap()["rows"] =
        json!([text_row(0, "fixture", Some("1\n오후 2:48"), Some("2026. 11. 3."))]);
    let res = engine.read_messages("Fixture", 50, 0, "").unwrap();
    let item = &res["items"][0];
    assert_eq!(item["timeText"], "오후 2:48");
    assert_eq!(item["dateText"], "2026. 11. 3.");
    assert!(item["sender"].is_null());
}

#[test]
fn multiple_time_lines_are_not_assigned_an_arbitrary_time() {
    let (driver, engine, _dir) = make_engine();
    driver.raw.lock().unwrap()["rows"] =
        json!([text_row(0, "fixture", Some("오전 10:00\n오후 2:48"), None)]);
    let res = engine.read_messages("Fixture", 50, 0, "").unwrap();
    assert!(res["items"][0]["timeText"].is_null());
}

#[test]
fn unknown_attachment_rows_are_preserved_and_coverage_is_explicit() {
    let (driver, engine, _dir) = make_engine();
    let result = engine.read_messages("Fixture", 50, 0, "").unwrap();
    let last = result["items"].as_array().unwrap().last().unwrap();
    assert_eq!(last["kind"], "attachment");
    assert_eq!(last["axParts"][0]["AXDescription"], "사진");
    assert_eq!(result["unclassifiedRowCount"], 0);
    assert_eq!(result["attachmentRowCount"], 1);
    assert_eq!(result["coverage"]["historyComplete"], false);
    assert_eq!(result["coverage"]["scrollPerformed"], false);
    {
        let mut raw = driver.raw.lock().unwrap();
        raw["exposedRowCount"] = json!(1005);
        raw["rowStart"] = json!(5);
        raw["rowsTruncated"] = json!(true);
    }
    let res2 = engine.read_messages("Fixture", 50, 0, "").unwrap();
    assert_eq!(res2["coverage"]["rowsTruncated"], true);
}

#[test]
fn older_pages_are_disjoint_chronological_and_snapshot_checked() {
    let (_driver, engine, _dir) = make_engine();
    let first = engine.read_messages("Fixture", 2, 0, "").unwrap();
    let ids = |v: &Value| -> Vec<i64> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["rowIndex"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(ids(&first), vec![3, 4]);
    let snap = first["snapshotId"].as_str().unwrap();
    let second = engine
        .read_messages("Fixture", 2, first["nextOffset"].as_i64().unwrap(), snap)
        .unwrap();
    assert_eq!(ids(&second), vec![1, 2]);
    let third = engine
        .read_messages("Fixture", 2, second["nextOffset"].as_i64().unwrap(), snap)
        .unwrap();
    assert_eq!(ids(&third), vec![0]);
    assert_eq!(third["hasMore"], false);
    assert!(third["nextOffset"].is_null());
}

#[test]
fn changed_snapshot_never_returns_a_shifted_page() {
    let (driver, engine, _dir) = make_engine();
    let first = engine.read_messages("Fixture", 2, 0, "").unwrap();
    {
        let mut raw = driver.raw.lock().unwrap();
        raw["rows"]
            .as_array_mut()
            .unwrap()
            .push(text_row(5, "새 메시지", None, None));
        raw["exposedRowCount"] = json!(6);
    }
    let changed = engine
        .read_messages("Fixture", 2, 2, first["snapshotId"].as_str().unwrap())
        .unwrap();
    assert_eq!(changed["status"], "snapshot_changed");
    assert_eq!(changed["items"], json!([]));
}

#[test]
fn content_edits_also_invalidate_snapshot() {
    let (driver, engine, _dir) = make_engine();
    let first = engine.read_messages("Fixture", 2, 0, "").unwrap();
    {
        let mut raw = driver.raw.lock().unwrap();
        raw["rows"][0]["parts"][1]["AXValue"] = json!("수정됨");
    }
    let changed = engine
        .read_messages("Fixture", 2, 2, first["snapshotId"].as_str().unwrap())
        .unwrap();
    assert_eq!(changed["status"], "snapshot_changed");
}

#[test]
fn empty_transcript_is_a_partial_empty_snapshot() {
    let (driver, engine, _dir) = make_engine();
    {
        let mut raw = driver.raw.lock().unwrap();
        raw["rows"] = json!([]);
        raw["exposedRowCount"] = json!(0);
    }
    let result = engine.read_messages("Fixture", 50, 0, "").unwrap();
    assert_eq!(result["items"], json!([]));
    assert_eq!(result["total"], 0);
    assert_eq!(result["hasMore"], false);
    assert_eq!(result["coverage"]["historyComplete"], false);
}

#[test]
fn blocked_native_read_has_no_database_fallback() {
    let (driver, engine, _dir) = make_engine();
    *driver.raw.lock().unwrap() =
        json!({"status": "blocked", "reason": "target_room_window_ambiguous"});
    let result = engine.read_messages("Fixture", 50, 0, "").unwrap();
    assert_eq!(result["status"], "blocked");
    assert_eq!(result["reason"], "target_room_window_ambiguous");
    assert_eq!(driver.calls.lock().unwrap().len(), 1);
}

#[test]
fn invalid_pagination_or_title_never_reaches_native() {
    let (driver, engine, _dir) = make_engine();
    assert!(engine.read_messages("", 50, 0, "").is_err());
    assert!(engine.read_messages("   ", 50, 0, "").is_err());
    assert!(engine.read_messages("Fixture", 501, 0, "").is_err());
    assert!(engine.read_messages("Fixture", 50, -1, "").is_err());
    assert!(engine.read_messages("Fixture", 50, 1, "").is_err());
    assert!(engine
        .read_messages("Fixture", 50, 0, "not-a-snapshot")
        .is_err());
    assert!(driver.calls.lock().unwrap().is_empty());
}

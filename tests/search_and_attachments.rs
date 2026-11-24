use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{ax_row, AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;

struct SearchAttachDriver {
    rows: Mutex<Vec<Value>>,
    control_fn: Mutex<Box<dyn FnMut(&str, &Map<String, Value>, &mut Vec<Value>) -> Value + Send>>,
    controls: Mutex<Vec<(String, Map<String, Value>)>>,
    attachment_calls: Mutex<Vec<(i64, usize, bool)>>,
    attachment_response: Mutex<Value>,
}

impl SearchAttachDriver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(vec![
                json!({"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "old"}]}),
                json!({"rowIndex": 1, "parts": [{"role": "AXTextArea", "AXValue": "repeat"}]}),
                json!({"rowIndex": 4, "parts": [{"role": "AXImage", "AXDescription": "사진"}]}),
            ]),
            control_fn: Mutex::new(Box::new(|action, _, _| {
                if action == "open" {
                    json!({"status": "ok"})
                } else {
                    json!({"status": "ok", "searchVerified": true, "searchState": {"previousEnabled": false}})
                }
            })),
            controls: Mutex::new(Vec::new()),
            attachment_calls: Mutex::new(Vec::new()),
            attachment_response: Mutex::new(json!({
                "status": "ok",
                "ocrPerformed": false,
                "imageBase64": base64::engine::general_purpose::STANDARD.encode(b"fixture image"),
            })),
        })
    }
}

impl AxDriver for SearchAttachDriver {
    fn call(&self, _action: &str, _titles: &[String], _text: Option<&str>) -> Result<Value> {
        let rows = self.rows.lock().unwrap().clone();
        Ok(json!({
            "status": "ok",
            "windowTitle": "Fixture",
            "processId": 123,
            "rows": rows,
            "exposedRowCount": rows.len(),
            "rowStart": 0,
            "rowsTruncated": false,
            "elapsedMs": 10,
        }))
    }

    fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value> {
        self.controls
            .lock()
            .unwrap()
            .push((action.to_string(), args.clone()));
        let mut rows = self.rows.lock().unwrap();
        let mut f = self.control_fn.lock().unwrap();
        Ok(f(action, args, &mut rows))
    }

    fn capture_attachment(
        &self,
        _window_title: &str,
        row_index: i64,
        _expected_parts: &[Value],
        expected_count: usize,
        ocr: bool,
    ) -> Result<Value> {
        self.attachment_calls
            .lock()
            .unwrap()
            .push((row_index, expected_count, ocr));
        Ok(self.attachment_response.lock().unwrap().clone())
    }
}

fn setup() -> (Arc<SearchAttachDriver>, AxEngine<SearchAttachDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let driver = SearchAttachDriver::new();
    let engine = AxEngine::with_roots(
        Arc::clone(&driver),
        dir.path().join("ui"),
        dir.path().join("ops"),
    );
    (driver, engine, dir)
}

#[test]
fn visits_older_results_and_keeps_context() {
    let (driver, engine, _dir) = setup();
    *driver.control_fn.lock().unwrap() = Box::new(|action, _, rows| {
        if action == "open" {
            return json!({"status": "ok"});
        }
        *rows = if action == "search_query" {
            vec![
                json!({"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "context"}]}),
                json!({"rowIndex": 1, "parts": [{"role": "AXTextArea", "AXValue": "dress latest"}]}),
            ]
        } else {
            vec![
                json!({"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "dress older"}]}),
                json!({"rowIndex": 1, "parts": [{"role": "AXTextArea", "AXValue": "tail"}]}),
            ]
        };
        json!({
            "status": "ok",
            "searchVerified": true,
            "searchState": {"previousEnabled": action == "search_query"},
        })
    });
    let result = engine
        .search_messages("Fixture", "dress", 100, 0, false, true, 10, 2)
        .unwrap();
    let texts: Vec<&str> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["dress latest", "dress older"]);
    assert_eq!(result["items"][0]["contextBefore"][0]["text"], "context");
    assert_eq!(result["stopReason"], "navigation_exhausted");
    assert_eq!(result["duplicatesPossible"], true);
    assert_eq!(result["coverage"]["historyComplete"], false);
}

#[test]
fn attachment_context_is_preserved_without_searching_it_as_text() {
    let (driver, engine, _dir) = setup();
    *driver.rows.lock().unwrap() = vec![
        json!({"rowIndex": 0, "parts": [{"role": "AXImage"}]}),
        json!({"rowIndex": 1, "parts": [{"role": "AXTextArea", "AXValue": "Straße"}]}),
    ];
    let result = engine
        .search_messages("Fixture", "STRASSE", 100, 0, false, false, 10, 2)
        .unwrap();
    let ctx = &result["items"][0]["contextBefore"][0];
    assert_eq!(ctx["kind"], "attachment");
    assert!(ctx["text"].is_null());
}

#[test]
fn unverified_search_preserves_local_matches_but_is_partial() {
    let (driver, engine, _dir) = setup();
    *driver.control_fn.lock().unwrap() = Box::new(|action, _, _| {
        if action == "search_query" {
            json!({"status": "partial", "searchVerified": false, "reason": "search_result_not_observed"})
        } else {
            json!({"status": "ok"})
        }
    });
    let result = engine
        .search_messages("Fixture", "repeat", 100, 0, false, true, 10, 2)
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["searchVerified"], false);
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["searchVisits"].as_array().unwrap().len(), 1);
}

#[test]
fn legacy_ok_without_evidence_is_not_verified() {
    let (driver, engine, _dir) = setup();
    *driver.control_fn.lock().unwrap() = Box::new(|_, _, _| json!({"status": "ok"}));
    let result = engine
        .search_messages("Fixture", "repeat", 100, 0, false, true, 10, 2)
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["searchVerified"], false);
}

#[test]
fn failure_after_first_result_preserves_prior_items() {
    let (driver, engine, _dir) = setup();
    *driver.control_fn.lock().unwrap() = Box::new(|action, _, _| match action {
        "search_query" => json!({"status": "ok", "searchVerified": true, "searchState": {"previousEnabled": true}}),
        "search_previous" => json!({"status": "blocked", "reason": "search_query_changed"}),
        _ => json!({"status": "ok"}),
    });
    let result = engine
        .search_messages("Fixture", "repeat", 100, 0, false, true, 10, 2)
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["items"][0]["text"], "repeat");
    assert_eq!(result["stopReason"], "search_query_changed");
}

#[test]
fn step_budget_and_repeated_body_are_not_silently_deduplicated() {
    let (driver, engine, _dir) = setup();
    *driver.rows.lock().unwrap() = vec![
        json!({"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "dress"}]}),
        json!({"rowIndex": 1, "parts": [{"role": "AXTextArea", "AXValue": "dress"}]}),
    ];
    *driver.control_fn.lock().unwrap() = Box::new(|action, _, _| {
        if matches!(action, "search_query" | "search_previous") {
            json!({"status": "ok", "searchVerified": true, "searchState": {"previousEnabled": true}})
        } else {
            json!({"status": "ok"})
        }
    });
    let result = engine
        .search_messages("Fixture", "dress", 100, 0, false, true, 2, 2)
        .unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["searchVisits"].as_array().unwrap().len(), 2);
    assert_eq!(result["stopReason"], "step_limit");
}

#[test]
fn invalid_search_arguments_do_not_navigate() {
    use kakaotalk_ax::mcp::dispatch_method;
    let (driver, engine, _dir) = setup();
    assert!(engine.search_messages("Fixture", "dress", 100, 0, false, true, 0, 2).is_err());
    assert!(engine.search_messages("Fixture", "dress", 100, 0, false, true, 51, 2).is_err());
    assert!(engine.search_messages("Fixture", "dress", 100, 0, false, true, 10, -1).is_err());
    // Passing boolean values where integers are expected must fail before reaching native navigation.
    for key in ["limit", "load_pages", "max_steps", "context_rows"] {
        let mut args = serde_json::Map::new();
        args.insert("window_title".into(), json!("Fixture"));
        args.insert("query".into(), json!("dress"));
        args.insert(key.into(), json!(true));
        assert!(
            dispatch_method(&engine, "search_messages", &args).is_err(),
            "bool for {key} must be rejected"
        );
    }
    let mut att_args = serde_json::Map::new();
    att_args.insert("window_title".into(), json!("Fixture"));
    att_args.insert("row_index".into(), json!(true));
    att_args.insert("snapshot_id".into(), json!("a".repeat(24)));
    assert!(dispatch_method(&engine, "read_attachment", &att_args).is_err());
    assert!(driver.controls.lock().unwrap().is_empty());
}

#[test]
fn unnamed_image_is_preserved_without_making_up_body() {
    let result = ax_row(&json!({"rowIndex": 7, "parts": [{"role": "AXImage"}]}));
    assert_eq!(result["kind"], "attachment");
    assert!(result["text"].is_null());
    assert!(result["attachments"][0]["description"].is_null());
}

#[test]
fn decorative_image_does_not_reclassify_text_message() {
    let result = ax_row(&json!({
        "rowIndex": 7,
        "parts": [{"role": "AXImage"}, {"role": "AXTextArea", "AXValue": "hello"}],
    }));
    assert_eq!(result["kind"], "text");
    assert_eq!(result["attachments"], json!([]));
}

#[test]
fn snapshot_change_and_non_image_never_capture() {
    let (driver, engine, _dir) = setup();
    let snap = engine.read_messages("Fixture", 50, 0, "").unwrap()["snapshotId"]
        .as_str()
        .unwrap()
        .to_string();
    let res1 = engine
        .read_attachment("Fixture", 4, &"0".repeat(24), None, false)
        .unwrap();
    assert_eq!(res1["status"], "snapshot_changed");
    let res2 = engine
        .read_attachment("Fixture", 0, &snap, None, false)
        .unwrap();
    assert_eq!(res2["reason"], "attachment_row_required");
    assert!(driver.attachment_calls.lock().unwrap().is_empty());
}

#[test]
fn image_is_private_and_ocr_is_opt_in() {
    let (driver, engine, dir) = setup();
    let snap = engine.read_messages("Fixture", 50, 0, "").unwrap()["snapshotId"]
        .as_str()
        .unwrap()
        .to_string();
    let path = dir.path().join("crop.png");
    let path_str = path.to_str().unwrap();

    let result = engine
        .read_attachment("Fixture", 4, &snap, Some(path_str), false)
        .unwrap();
    assert_eq!(result["ocrPerformed"], false);
    assert!(result.get("text").is_none());
    assert!(result.get("imageBase64").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"fixture image");
    assert_eq!(*driver.attachment_calls.lock().unwrap(), vec![(4, 3, false)]);

    // Existing file rejected
    assert!(engine
        .read_attachment("Fixture", 4, &snap, Some(path_str), false)
        .is_err());

    // Temp file + ocr=true
    let res_ocr = engine
        .read_attachment("Fixture", 4, &snap, None, true)
        .unwrap();
    let tmp_path = std::path::PathBuf::from(res_ocr["imagePath"].as_str().unwrap());
    assert!(tmp_path.is_file());
    std::fs::remove_file(&tmp_path).unwrap();
    assert!(res_ocr.get("imageBase64").is_none());
    assert_eq!(driver.attachment_calls.lock().unwrap().last(), Some(&(4, 3, true)));
}

#[test]
fn invalid_snapshot_and_relative_export_are_rejected() {
    let (_driver, engine, _dir) = setup();
    let snap = engine.read_messages("Fixture", 50, 0, "").unwrap()["snapshotId"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(engine.read_attachment("Fixture", 4, "", None, false).is_err());
    assert!(engine
        .read_attachment("Fixture", 4, &snap, Some("relative.png"), false)
        .is_err());
}

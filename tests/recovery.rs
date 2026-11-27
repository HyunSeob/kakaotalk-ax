//! Engine recovery: one retry of transient window lookups, and the permission hint.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend, ACCESSIBILITY_PERMISSION_HINT};
use kakaotalk_ax::error::Result;

struct Scripted {
    /// Replies to successive `read` calls; the last one repeats.
    reads: Mutex<Vec<Value>>,
    calls: Mutex<Vec<String>>,
    /// `uiActions` reported by blocked controls.
    control_ui_actions: Mutex<u64>,
}

impl Scripted {
    fn new(reads: Vec<Value>) -> Arc<Self> {
        Arc::new(Self { reads: Mutex::new(reads), calls: Mutex::new(Vec::new()), control_ui_actions: Mutex::new(1) })
    }
    fn count(&self, action: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|a| *a == action).count()
    }
}

fn ok_read() -> Value {
    json!({"status": "ok", "windowTitle": "가상방", "processId": 1, "rows": [], "exposedRowCount": 0, "rowStart": 0})
}

impl AxDriver for Scripted {
    fn call(&self, action: &str, _titles: &[String], _text: Option<&str>) -> Result<Value> {
        self.calls.lock().unwrap().push(action.to_string());
        let mut reads = self.reads.lock().unwrap();
        Ok(if reads.len() > 1 { reads.remove(0) } else { reads[0].clone() })
    }
    fn control(&self, action: &str, _args: &Map<String, Value>) -> Result<Value> {
        self.calls.lock().unwrap().push(action.to_string());
        Ok(json!({"status": "blocked", "reason": "main_window_not_unique", "uiActions": *self.control_ui_actions.lock().unwrap()}))
    }
    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        Ok(json!({"status": "ok"}))
    }
}

fn engine(driver: &Arc<Scripted>) -> (AxEngine<Scripted>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let e = AxEngine::with_roots(Arc::clone(driver), dir.path().join("ui"), dir.path().join("ops"));
    (e, dir)
}

#[test]
fn a_transient_window_lookup_failure_is_retried_once() {
    for reason in ["main_window_not_unique", "target_room_window_not_open"] {
        let d = Scripted::new(vec![json!({"status": "blocked", "reason": reason, "uiActions": 0}), ok_read()]);
        let (e, _dir) = engine(&d);
        let out = e.read_messages("가상방", 10, 0, "").unwrap();
        assert_eq!(out["status"], "ok", "{reason}");
        assert_eq!(d.count("read"), 2);
    }
    // Still failing: one retry only, and the failure is reported.
    let d = Scripted::new(vec![json!({"status": "blocked", "reason": "target_room_window_not_open", "uiActions": 0})]);
    let (e, _dir) = engine(&d);
    let out = e.read_messages("가상방", 10, 0, "").unwrap();
    assert_eq!(out["reason"], "target_room_window_not_open");
    assert_eq!(out["retriedAfter"], "target_room_window_not_open");
    assert_eq!(d.count("read"), 2);
}

#[test]
fn failures_after_a_ui_action_or_of_other_kinds_are_not_retried() {
    let d = Scripted::new(vec![json!({"status": "blocked", "reason": "transcript_changed_during_read"})]);
    let (e, _dir) = engine(&d);
    e.read_messages("가상방", 10, 0, "").unwrap();
    assert_eq!(d.count("read"), 1);
    // A control that already acted is not repeated.
    let (e, _dir) = engine(&d);
    let out = e.rooms("", false, 300, None).unwrap();
    assert_eq!(out["reason"], "main_window_not_unique");
    assert_eq!(d.count("rooms"), 1);
}

#[test]
fn the_permission_error_carries_a_hint_and_the_executable_path() {
    let d = Scripted::new(vec![json!({"status": "blocked", "reason": "accessibility_permission_required"})]);
    let (e, _dir) = engine(&d);
    let out = e.read_messages("가상방", 10, 0, "").unwrap();
    assert_eq!(out["reason"], "accessibility_permission_required");
    assert_eq!(out["hint"], ACCESSIBILITY_PERMISSION_HINT);
    assert!(out["executablePath"].as_str().is_some_and(|p| !p.is_empty()));
    assert_eq!(d.count("read"), 1);
}

#[test]
fn sending_never_retries_its_reads() {
    // open_room fails transiently: send reports it without a second try.
    let d = Scripted::new(vec![json!({"status": "blocked", "reason": "main_window_not_unique", "uiActions": 0})]);
    // A blocked open that touched nothing would be retried outside sending.
    *d.control_ui_actions.lock().unwrap() = 0;
    let (e, _dir) = engine(&d);
    let out = e.send_message("가상방", "안녕", "op-1", 0.0, false).unwrap();
    assert_eq!(out["status"], "blocked");
    assert!(out.get("retriedAfter").is_none());
    assert_eq!(d.count("read") + d.count("open"), d.calls.lock().unwrap().len());
    assert_eq!(d.count("open"), 1, "open_room is not repeated while sending");
}

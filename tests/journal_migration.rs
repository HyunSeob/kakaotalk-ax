//! Tests for one-way migration of Python v1 send journals into Rust v2 journals.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;
use kakaotalk_ax::state::{
    load_private_json, migrate_legacy_journal, save_private_json, send_fingerprint, sha256_hex,
    JOURNAL_SCHEMA_VERSION,
};

struct MigrationFixtureDriver {
    rows: Mutex<Vec<String>>,
    sends: Mutex<usize>,
}

impl MigrationFixtureDriver {
    fn new(initial: Vec<&str>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(initial.into_iter().map(ToString::to_string).collect()),
            sends: Mutex::new(0),
        })
    }
}

impl AxDriver for MigrationFixtureDriver {
    fn call(&self, action: &str, titles: &[String], _text: Option<&str>) -> Result<Value> {
        match action {
            "read" => {
                let rows = self.rows.lock().unwrap();
                let rj: Vec<Value> = rows
                    .iter()
                    .enumerate()
                    .map(|(i, t)| json!({"rowIndex": i, "parts": [{"role": "AXTextArea", "AXValue": t}]}))
                    .collect();
                Ok(json!({
                    "status": "ok",
                    "windowTitle": titles[0],
                    "processId": 1,
                    "rows": rj,
                    "exposedRowCount": rows.len(),
                    "rowStart": 0,
                    "rowsTruncated": false,
                    "elapsedMs": 1,
                }))
            }
            "inspect" => Ok(json!({"status": "ready"})),
            "send" => {
                *self.sends.lock().unwrap() += 1;
                Ok(json!({"status": "dispatch_attempted", "dispatchAttempted": true}))
            }
            _ => Ok(json!({"status": "ok"})),
        }
    }

    fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
        Ok(json!({"status": "ok", "alreadyOpen": true}))
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

/// Simulate a Python v1 journal written by `kakaotalk_ax.py` before termination.
fn write_python_v1_record(
    dir: &std::path::Path,
    operation_id: &str,
    window_title: &str,
    text: &str,
    require_self: bool,
    baseline: &[&str],
    phase: &str,
) {
    // Python `json.dumps([window_title, text, require_self], ensure_ascii=False)` uses ", " separators.
    let py_json = format!(
        "[{}, {}, {}]",
        serde_json::to_string(window_title).unwrap(),
        serde_json::to_string(text).unwrap(),
        if require_self { "true" } else { "false" }
    );
    let fp = sha256_hex(&py_json);
    let record = json!({
        "operationId": operation_id,
        "windowTitle": window_title,
        "fingerprint": fp,
        "textHash": sha256_hex(text),
        "baselineTailHashes": baseline.iter().map(|s| sha256_hex(s)).collect::<Vec<_>>(),
        "baselineTextCount": baseline.len(),
        "createdAt": 1732400000.5,
        "phase": phase,
    });
    let path = dir.join(format!("{}.json", sha256_hex(operation_id)));
    save_private_json(&path, &record).unwrap();
}

#[test]
fn fingerprint_matches_python_json_dumps_format() {
    // Python: hashlib.sha256(json.dumps(["나와의 채팅", "안녕\n세계", True], ensure_ascii=False).encode()).hexdigest()
    let expected = sha256_hex(r#"["나와의 채팅", "안녕\n세계", true]"#);
    assert_eq!(send_fingerprint("나와의 채팅", "안녕\n세계", true), expected);
}

#[test]
fn python_dispatching_journal_migrates_and_confirms_without_resending() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let target = dir.path().join("rust-ops");

    // Python wrote intent ("dispatching") and crashed after message was actually delivered.
    write_python_v1_record(
        &legacy,
        "py-op-001",
        "나와의 채팅",
        "이전 테스트 메시지",
        true,
        &["기존1", "기존2"],
        "dispatching",
    );

    let report = migrate_legacy_journal(&legacy, &target).unwrap();
    assert_eq!(report.status, "ok");
    assert_eq!(report.migrated, 1);
    assert_eq!(report.invalid_files, 0);

    // Verify upgraded schemaVersion=2
    let migrated_path = target.join(format!("{}.json", sha256_hex("py-op-001")));
    let stored = load_private_json(&migrated_path).unwrap().unwrap();
    assert_eq!(stored["schemaVersion"], JOURNAL_SCHEMA_VERSION);
    assert_eq!(stored["migratedFrom"], "python_v1");

    // Now Rust sees the delivered message in chat -> confirms WITHOUT sending again!
    let driver = MigrationFixtureDriver::new(vec!["기존1", "기존2", "이전 테스트 메시지"]);
    let engine = AxEngine::with_roots(Arc::clone(&driver), dir.path().join("ui"), target.clone());

    let res = engine
        .send_message("나와의 채팅", "이전 테스트 메시지", "py-op-001", 0.0, true)
        .unwrap();
    assert_eq!(res["status"], "observed_in_ax");
    assert_eq!(res["deduplicated"], true);
    assert_eq!(res["nativeSendCalls"], 0);
    assert_eq!(*driver.sends.lock().unwrap(), 0);

    // Reusing the migrated operation_id with different body or target MUST fail with ValueError
    let err_body = engine
        .send_message("나와의 채팅", "다른 본문", "py-op-001", 0.0, true)
        .unwrap_err();
    assert_eq!(err_body.error_type(), "ValueError");

    let err_target = engine
        .send_message("다른 방", "이전 테스트 메시지", "py-op-001", 0.0, true)
        .unwrap_err();
    assert_eq!(err_target.error_type(), "ValueError");

    assert_eq!(*driver.sends.lock().unwrap(), 0);
}

#[test]
fn python_unknown_journal_migrates_and_send_result_observes_late_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let target = dir.path().join("rust-ops");

    write_python_v1_record(
        &legacy,
        "py-op-002",
        "Fixture",
        "지연 확인",
        false,
        &["old"],
        "outcome_unknown",
    );
    migrate_legacy_journal(&legacy, &target).unwrap();

    let driver = MigrationFixtureDriver::new(vec!["old"]);
    let engine = AxEngine::with_roots(Arc::clone(&driver), dir.path().join("ui"), target);

    // Still not delivered -> remains outcome_unknown, zero sends
    assert_eq!(
        engine.send_result("py-op-002", 0.0).unwrap()["status"],
        "outcome_unknown"
    );
    assert_eq!(*driver.sends.lock().unwrap(), 0);

    // Late delivery appears -> send_result confirms observed_in_ax, zero sends
    driver.rows.lock().unwrap().push("지연 확인".to_string());
    assert_eq!(
        engine.send_result("py-op-002", 0.0).unwrap()["status"],
        "observed_in_ax"
    );
    assert_eq!(*driver.sends.lock().unwrap(), 0);
}

#[test]
fn in_place_windows_style_migration_upgrades_once_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("ax-send-operations");

    write_python_v1_record(
        &shared,
        "win-op-1",
        "내 프로필",
        "윈도우 기록",
        true,
        &["hello"],
        "observed_in_ax",
    );

    let first = migrate_legacy_journal(&shared, &shared).unwrap();
    assert_eq!(first.migrated, 1);
    assert_eq!(first.upgraded_in_place, 1);
    assert_eq!(first.skipped_existing, 0);

    let second = migrate_legacy_journal(&shared, &shared).unwrap();
    assert_eq!(second.migrated, 0);
    assert_eq!(second.upgraded_in_place, 0);
    assert_eq!(second.skipped_existing, 1);
}

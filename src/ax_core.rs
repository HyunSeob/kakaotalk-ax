//! Shared KakaoTalk AX core: 13-tool trait, row classification, snapshot hashing,
//! pagination, search, batch read, and idempotent send engine.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::error::{KaxError, Result};
use crate::state::{
    default_send_state_root, default_ui_root, ensure_migrated_if_default, load_private_json,
    save_private_json, send_fingerprint, send_lock, sha256_hex, ui_session,
    JOURNAL_SCHEMA_VERSION,
};
use crate::validation::{
    validate_integer, validate_operation_id, validate_output_path, validate_query_text,
    validate_read_title, validate_send_text, validate_snapshot_id_for_read,
    validate_snapshot_id_required, validate_title, validate_wait_seconds,
};

/// Unified 13-tool + build backend interface implemented by both macOS AX and Windows Win32.
pub trait KakaoBackend: Send + Sync {
    fn build(&self, kind: &str) -> Result<Value>;
    fn windows(&self) -> Result<Value>;
    fn rooms(&self, query: &str) -> Result<Value>;
    fn search_rooms(&self, query: &str) -> Result<Value>;
    fn open_room(&self, window_title: &str, require_self: bool) -> Result<Value>;
    fn close_room(&self, window_title: &str) -> Result<Value>;
    fn read_messages(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value>;
    fn read_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        snapshot_id: &str,
        output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value>;
    fn read_many(
        &self,
        window_titles: &[String],
        limit: i64,
        open_if_needed: bool,
        close_after: bool,
        max_workers: i64,
    ) -> Result<Value>;
    fn load_more(&self, window_title: &str, pages: i64) -> Result<Value>;
    #[allow(clippy::too_many_arguments)]
    fn search_messages(
        &self,
        window_title: &str,
        query: &str,
        limit: i64,
        load_pages: i64,
        case_sensitive: bool,
        use_app_search: bool,
        max_steps: i64,
        context_rows: i64,
    ) -> Result<Value>;
    fn close_search(&self, window_title: &str) -> Result<Value>;
    fn send_message(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value>;
    fn send_result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value>;
}

/// Stable 24-hex snapshot digest matching Python `snapshot_digest(raw)`.
pub fn snapshot_digest(raw: &Value) -> String {
    let mut map = Map::new();
    for k in [
        "exposedRowCount",
        "processId",
        "rowStart",
        "rows",
        "rowsTruncated",
        "windowTitle",
    ] {
        if let Some(v) = raw.get(k) {
            map.insert(k.to_string(), v.clone());
        }
    }
    legacy_snapshot_id(&Value::Object(map))
}

// Python json.dumps(..., ensure_ascii=False) separates items with ", " and keys
// from values with ": ". These bytes are part of the public snapshot token.
pub(crate) fn legacy_snapshot_id(value: &Value) -> String {
    use serde::Serialize;
    use serde_json::ser::{Formatter, Serializer};
    use std::io;

    struct LegacySeparators;
    impl Formatter for LegacySeparators {
        fn begin_array_value<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
            if !first {
                w.write_all(b", ")?;
            }
            Ok(())
        }
        fn begin_object_key<W: ?Sized + Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
            self.begin_array_value(w, first)
        }
        fn begin_object_value<W: ?Sized + Write>(&mut self, w: &mut W) -> io::Result<()> {
            w.write_all(b": ")
        }
    }
    let mut bytes = Vec::new();
    value.serialize(&mut Serializer::with_formatter(&mut bytes, LegacySeparators))
        .expect("serializing a JSON value into a vector cannot fail");
    sha256_hex(&bytes)[..24].to_owned()
}

/// Classify one macOS AX row into `text`, `attachment`, or `unclassified`.
/// Never fabricates sender or grouped timestamps.
pub fn ax_row(row: &Value) -> Value {
    let time_re = Regex::new(
        r"(?i)^(?:(?:오전|오후|AM|PM)\s*)?\d{1,2}:\d{2}(?:\s*(?:AM|PM))?$",
    )
    .unwrap();
    let date_re = Regex::new(r"^\d{4}\.\s*\d{1,2}\.\s*\d{1,2}\.?$").unwrap();

    let parts = row
        .get("parts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut bodies: Vec<String> = Vec::new();
    let mut times: Vec<(&Value, String)> = Vec::new();
    let mut links: Vec<String> = Vec::new();

    for part in &parts {
        let role = part.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "AXTextArea" {
            if let Some(val) = part.get("AXValue").and_then(|v| v.as_str()) {
                if !val.is_empty() {
                    bodies.push(val.to_string());
                }
            }
        } else if role == "AXStaticText" {
            if let Some(val) = part.get("AXValue").and_then(|v| v.as_str()) {
                for line in val.lines() {
                    let trimmed = line.trim();
                    if time_re.is_match(trimmed) {
                        times.push((part, trimmed.to_string()));
                    }
                }
            }
        } else if role == "AXLink" {
            let link = part
                .get("AXURL")
                .or_else(|| part.get("AXValue"))
                .or_else(|| part.get("AXTitle"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if let Some(l) = link {
                links.push(l.to_string());
            }
        }
    }

    let mut dates: Vec<String> = Vec::new();
    for (part, _) in &times {
        if let Some(help) = part.get("AXHelp").and_then(|v| v.as_str()) {
            if date_re.is_match(help) {
                dates.push(help.to_string());
            }
        }
    }
    let unique_dates: HashSet<&String> = dates.iter().collect();

    let attachments: Vec<Value> = if bodies.is_empty() {
        parts
            .iter()
            .filter(|p| p.get("role").and_then(|v| v.as_str()) == Some("AXImage"))
            .map(|p| {
                let desc = p
                    .get("AXDescription")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                let title = p
                    .get("AXTitle")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                json!({
                    "type": "image",
                    "textSource": "accessibility_metadata",
                    "description": desc,
                    "title": title,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let kind = if !bodies.is_empty() {
        "text"
    } else if !attachments.is_empty() {
        "attachment"
    } else {
        "unclassified"
    };

    let text_val = if bodies.is_empty() {
        Value::Null
    } else {
        Value::String(bodies.join("\n"))
    };
    let time_val = if times.len() == 1 {
        Value::String(times[0].1.clone())
    } else {
        Value::Null
    };
    let date_val = if unique_dates.len() == 1 {
        Value::String(dates[0].clone())
    } else {
        Value::Null
    };

    json!({
        "rowIndex": row.get("rowIndex").cloned().unwrap_or(json!(0)),
        "kind": kind,
        "attachments": attachments,
        "links": links,
        "text": text_val,
        "sender": Value::Null,
        "timeText": time_val,
        "dateText": date_val,
        "axParts": parts,
    })
}

/// Low-level driver for macOS AX operations (implemented by live macOS AX or test fixtures).
pub trait AxDriver: Send + Sync {
    fn build(&self, kind: &str) -> Result<()> {
        if !matches!(kind, "send" | "read" | "control") {
            return Err(KaxError::validation("Unknown native bridge kind"));
        }
        Ok(())
    }

    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value>;

    fn send_with_identity(
        &self,
        titles: &[String],
        text: &str,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Result<Value> {
        let _ = (require_self, window_identity);
        self.call("send", titles, Some(text))
    }

    fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value>;

    fn capture_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        expected_parts: &[Value],
        expected_count: usize,
        ocr: bool,
    ) -> Result<Value>;
}

/// Platform-agnostic send engine driver (implemented by macOS `AxEngine` and Windows `WindowsAx`).
pub trait SendDriver {
    fn source(&self) -> &'static str;
    fn confirmation_source(&self) -> &'static str;
    fn unknown_reason(&self) -> &'static str;
    fn ui_root(&self) -> &Path;
    fn state_root(&self) -> &Path;

    fn text_hash(&self, text: &str) -> String {
        sha256_hex(text)
    }

    fn body_hashes(&self, read_result: &Value) -> Vec<String> {
        read_result
            .get("items")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|row| row.get("text").and_then(|t| t.as_str()))
                    .map(|t| self.text_hash(t))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn prepare(&self, window_title: &str, require_self: bool) -> Result<(Value, Map<String, Value>)>;
    fn build_for_send(&self) -> Result<()> {
        Ok(())
    }
    fn dispatch(&self, record: &Map<String, Value>, text: &str) -> Result<Value>;
    fn read_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value>;
    fn inspect_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value>;
}

pub struct SendEngine<'a, S: SendDriver> {
    driver: &'a S,
}

impl<'a, S: SendDriver> SendEngine<'a, S> {
    pub fn new(driver: &'a S) -> Self {
        Self { driver }
    }

    pub fn record_path(&self, operation_id: &str) -> Result<PathBuf> {
        validate_operation_id(operation_id)?;
        Ok(self
            .driver
            .state_root()
            .join(format!("{}.json", sha256_hex(operation_id))))
    }

    fn output(&self, record: &Map<String, Value>, extra: Map<String, Value>) -> Value {
        let mut out = Map::new();
        out.insert(
            "status".to_string(),
            record.get("phase").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "operationId".to_string(),
            record.get("operationId").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "windowTitle".to_string(),
            record.get("windowTitle").cloned().unwrap_or(Value::Null),
        );
        out.insert("source".to_string(), json!(self.driver.source()));
        out.insert(
            "confirmationSource".to_string(),
            json!(self.driver.confirmation_source()),
        );
        out.insert("databaseCalls".to_string(), json!(0));
        out.insert("retrySafe".to_string(), json!(false));
        out.insert(
            "reason".to_string(),
            record.get("reason").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "native".to_string(),
            record.get("native").cloned().unwrap_or(Value::Null),
        );
        for (k, v) in extra {
            out.insert(k, v);
        }
        Value::Object(out)
    }

    fn confirm(
        &self,
        path: &Path,
        record: &mut Map<String, Value>,
        wait_seconds: f64,
    ) -> Result<Value> {
        let until = Instant::now() + Duration::from_secs_f64(wait_seconds);
        let baseline_tail: Vec<String> = record
            .get("baselineTailHashes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let text_hash = record
            .get("textHash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let baseline_count = record
            .get("baselineTextCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let mut expected = baseline_tail;
        expected.push(text_hash);

        loop {
            if let Ok(after) = self.driver.read_for_confirmation(record) {
                let hashes = self.driver.body_hashes(&after);
                let after_ok = after.get("status").and_then(|v| v.as_str()) == Some("ok");
                let after_count = after
                    .get("textMessageCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let tail_matches = hashes.len() >= expected.len()
                    && hashes[hashes.len() - expected.len()..] == expected[..];

                if after_ok && after_count == baseline_count + 1 && tail_matches {
                    if let Ok(inspected) = self.driver.inspect_for_confirmation(record) {
                        if inspected.get("status").and_then(|v| v.as_str()) == Some("ready") {
                            record.insert("phase".to_string(), json!("observed_in_ax"));
                            record.insert(
                                "reason".to_string(),
                                json!("new_text_occurrence_and_empty_composer"),
                            );
                            save_private_json(path, &Value::Object(record.clone()))?;
                            return Ok(self.output(record, Map::new()));
                        }
                    }
                }
            }

            let now = Instant::now();
            if now >= until {
                record.insert("phase".to_string(), json!("outcome_unknown"));
                record.insert("reason".to_string(), json!(self.driver.unknown_reason()));
                save_private_json(path, &Value::Object(record.clone()))?;
                return Ok(self.output(record, Map::new()));
            }
            let remaining = until.saturating_duration_since(now);
            std::thread::sleep(remaining.min(Duration::from_millis(150)));
        }
    }

    pub fn send(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_wait_seconds(wait_seconds)?;
        validate_send_text(text)?;
        let path = self.record_path(operation_id)?;
        let fingerprint = send_fingerprint(window_title, text, require_self);

        let _ui = ui_session(self.driver.ui_root())?;
        let _send = send_lock(self.driver.state_root())?;
        ensure_migrated_if_default(self.driver.state_root())?;
        crate::state::check_migration_blocked(self.driver.state_root(), operation_id)?;

        match load_private_json(&path)? {
            None => {}
            Some(Value::Object(mut existing)) => {
                let stored_fp = existing
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if stored_fp != fingerprint {
                    return Err(KaxError::validation(
                        "operation_id is already bound to another target or message",
                    ));
                }
                let phase = existing
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut extra = Map::new();
                extra.insert("deduplicated".to_string(), json!(true));
                extra.insert("nativeSendCalls".to_string(), json!(0));
                if matches!(phase, "observed_in_ax" | "blocked") {
                    return Ok(self.output(&existing, extra));
                }
                let mut confirmed = self.confirm(&path, &mut existing, wait_seconds)?;
                if let Some(obj) = confirmed.as_object_mut() {
                    obj.insert("deduplicated".to_string(), json!(true));
                    obj.insert("nativeSendCalls".to_string(), json!(0));
                }
                return Ok(confirmed);
            }
            Some(_) => {
                return Err(KaxError::validation(
                    "Corrupt send journal record; inspect journal before retrying",
                ));
            }
        }

        let (opened, target) = self.driver.prepare(window_title, require_self)?;
        if opened.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = opened.as_object().cloned().unwrap_or_default();
            out.insert("operationId".to_string(), json!(operation_id));
            out.insert("retrySafe".to_string(), json!(false));
            return Ok(Value::Object(out));
        }

        let mut probe_record = target.clone();
        probe_record.insert("windowTitle".to_string(), json!(window_title));
        let before = self.driver.read_for_confirmation(&probe_record)?;
        if before.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = before.as_object().cloned().unwrap_or_default();
            out.insert("operationId".to_string(), json!(operation_id));
            out.insert("retrySafe".to_string(), json!(false));
            return Ok(Value::Object(out));
        }

        let all_hashes = self.driver.body_hashes(&before);
        let start = all_hashes.len().saturating_sub(64);
        let baseline = all_hashes[start..].to_vec();
        let baseline_count = before
            .get("textMessageCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        self.driver.build_for_send()?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let mut record = Map::new();
        record.insert("schemaVersion".to_string(), json!(JOURNAL_SCHEMA_VERSION));
        record.insert("operationId".to_string(), json!(operation_id));
        record.insert("windowTitle".to_string(), json!(window_title));
        record.insert("fingerprint".to_string(), json!(fingerprint));
        record.insert("textHash".to_string(), json!(self.driver.text_hash(text)));
        record.insert("baselineTailHashes".to_string(), json!(baseline));
        record.insert("baselineTextCount".to_string(), json!(baseline_count));
        record.insert("createdAt".to_string(), json!(created_at));
        record.insert("phase".to_string(), json!("dispatching"));
        for (k, v) in target {
            record.insert(k, v);
        }
        save_private_json(&path, &Value::Object(record.clone()))?;

        let native = match self.driver.dispatch(&record, text) {
            Ok(v) => v,
            Err(err) => json!({
                "status": "bridge_result_unknown",
                "errorType": err.error_type(),
                "dispatchAttempted": Value::Null,
            }),
        };
        record.insert("native".to_string(), native.clone());

        if native.get("dispatchAttempted") == Some(&Value::Bool(false)) {
            let reason = native
                .get("reason")
                .or_else(|| native.get("status"))
                .cloned()
                .unwrap_or(Value::Null);
            record.insert("phase".to_string(), json!("blocked"));
            record.insert("reason".to_string(), reason);
            save_private_json(&path, &Value::Object(record.clone()))?;
            let mut extra = Map::new();
            extra.insert("nativeSendCalls".to_string(), json!(1));
            return Ok(self.output(&record, extra));
        }

        let mut confirmed = self.confirm(&path, &mut record, wait_seconds)?;
        if let Some(obj) = confirmed.as_object_mut() {
            obj.insert("nativeSendCalls".to_string(), json!(1));
        }
        Ok(confirmed)
    }

    pub fn result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        validate_wait_seconds(wait_seconds)?;
        let path = self.record_path(operation_id)?;

        let _ui = ui_session(self.driver.ui_root())?;
        let _send = send_lock(self.driver.state_root())?;
        ensure_migrated_if_default(self.driver.state_root())?;
        crate::state::check_migration_blocked(self.driver.state_root(), operation_id)?;

        let mut record = match load_private_json(&path)? {
            None => return Err(KaxError::validation("Unknown AX operation_id")),
            Some(Value::Object(obj)) => obj,
            Some(_) => {
                return Err(KaxError::validation(
                    "Corrupt send journal record; inspect journal before retrying",
                ));
            }
        };
        let phase = record.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(phase, "observed_in_ax" | "blocked") {
            return Ok(self.output(&record, Map::new()));
        }
        self.confirm(&path, &mut record, wait_seconds)
    }
}

/// High-level macOS AX engine wrapping an `AxDriver`.
pub struct AxEngine<D: AxDriver + 'static> {
    pub driver: Arc<D>,
    pub ui_root: PathBuf,
    pub state_root: PathBuf,
}

impl<D: AxDriver + 'static> AxEngine<D> {
    pub fn new(driver: Arc<D>) -> Self {
        Self {
            driver,
            ui_root: default_ui_root(),
            state_root: default_send_state_root(),
        }
    }

    pub fn with_roots(driver: Arc<D>, ui_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            driver,
            ui_root,
            state_root,
        }
    }

    fn control_locked(&self, action: &str, args: Map<String, Value>) -> Result<Value> {
        let _ui = ui_session(&self.ui_root)?;
        self.driver.control(action, &args)
    }

    fn call_checked(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        if matches!(action, "send" | "raise") {
            let _ui = ui_session(&self.ui_root)?;
            self.driver.call(action, titles, text)
        } else {
            self.driver.call(action, titles, text)
        }
    }

    fn send_checked(
        &self,
        titles: &[String],
        text: &str,
        require_self: bool,
        window_identity: Option<&str>,
    ) -> Result<Value> {
        let _ui = ui_session(&self.ui_root)?;
        self.driver
            .send_with_identity(titles, text, require_self, window_identity)
    }
}

struct AxSendAdapter<'a, D: AxDriver + 'static> {
    engine: &'a AxEngine<D>,
}

impl<'a, D: AxDriver + 'static> SendDriver for AxSendAdapter<'a, D> {
    fn source(&self) -> &'static str {
        "macos_accessibility"
    }

    fn confirmation_source(&self) -> &'static str {
        "macos_accessibility"
    }

    fn unknown_reason(&self) -> &'static str {
        "ax_confirmation_not_established"
    }

    fn ui_root(&self) -> &Path {
        &self.engine.ui_root
    }

    fn state_root(&self) -> &Path {
        &self.engine.state_root
    }

    fn prepare(&self, window_title: &str, require_self: bool) -> Result<(Value, Map<String, Value>)> {
        let opened = self.engine.open_room(window_title, require_self)?;
        let mut target = Map::new();
        if opened.get("status").and_then(|v| v.as_str()) == Some("ok") {
            let actual = opened
                .get("windowTitle")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !actual.is_empty() && actual != window_title {
                return Ok((
                    json!({
                        "status": "blocked",
                        "source": "macos_accessibility",
                        "reason": "exact_window_title_required",
                        "databaseCalls": 0,
                    }),
                    Map::new(),
                ));
            }
            if require_self {
                target.insert("requireSelf".to_string(), json!(true));
            }
            if let Some(identity) = opened
                .get("windowIdentity")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                target.insert("windowIdentity".to_string(), json!(identity));
            }
        }
        Ok((opened, target))
    }

    fn build_for_send(&self) -> Result<()> {
        self.engine.driver.build("send")
    }

    fn dispatch(&self, record: &Map<String, Value>, text: &str) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let require_self = record.get("requireSelf") == Some(&Value::Bool(true));
        let window_identity = record.get("windowIdentity").and_then(|v| v.as_str());
        self.engine
            .send_checked(&[title], text, require_self, window_identity)
    }

    fn read_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.engine.read_messages(title, 500, 0, "")
    }

    fn inspect_for_confirmation(&self, record: &Map<String, Value>) -> Result<Value> {
        let title = record
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        self.engine.call_checked("inspect", &[title], None)
    }
}

impl<D: AxDriver + 'static> KakaoBackend for AxEngine<D> {
    fn build(&self, kind: &str) -> Result<Value> {
        let started = Instant::now();
        self.driver.build(kind)?;
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
        Ok(json!({
            "status": "built",
            "kind": kind,
            "elapsedMs": elapsed,
        }))
    }

    fn windows(&self) -> Result<Value> {
        self.call_checked("windows", &[], None)
    }

    fn rooms(&self, query: &str) -> Result<Value> {
        validate_query_text(query, true)?;
        let result = self.control_locked("rooms", Map::new())?;
        if result.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(result);
        }
        let items = result
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let needle = caseless::default_case_fold_str(query);
        let filtered: Vec<Value> = items
            .iter()
            .filter(|item| {
                item.get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| caseless::default_case_fold_str(n).contains(&needle))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let mut out = result.as_object().cloned().unwrap_or_default();
        out.insert("query".to_string(), json!(query));
        out.insert("total".to_string(), json!(filtered.len()));
        out.insert("totalExposed".to_string(), json!(items.len()));
        out.insert("items".to_string(), Value::Array(filtered));
        Ok(Value::Object(out))
    }

    fn search_rooms(&self, query: &str) -> Result<Value> {
        validate_query_text(query, true)?;
        let mut args = Map::new();
        args.insert("query".to_string(), json!(query));
        self.control_locked("search_rooms", args)
    }

    fn open_room(&self, window_title: &str, require_self: bool) -> Result<Value> {
        validate_title(window_title)?;
        let _ui = ui_session(&self.ui_root)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        args.insert("requireSelf".to_string(), json!(require_self));
        let result = self.driver.control("open", &args)?;
        if result.get("reason").and_then(|v| v.as_str()) != Some("room_not_in_exposed_list") {
            return Ok(result);
        }
        let previous = self.driver.control("rooms", &Map::new())?;
        if previous.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(previous);
        }
        let prev_query = previous
            .get("searchQuery")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let search = self.search_rooms(window_title)?;
        let open_res = if search.get("status").and_then(|v| v.as_str()) == Some("ok") {
            self.driver.control("open", &args)?
        } else {
            search
        };
        let restored = self.search_rooms(&prev_query).unwrap_or_else(|e| {
            json!({"status": "failed", "errorType": e.error_type()})
        });
        let mut out = open_res.as_object().cloned().unwrap_or_default();
        out.insert("roomSearchUsed".to_string(), json!(true));
        out.insert(
            "roomSearchRestored".to_string(),
            json!(restored.get("status").and_then(|v| v.as_str()) == Some("ok")),
        );
        Ok(Value::Object(out))
    }

    fn close_room(&self, window_title: &str) -> Result<Value> {
        validate_title(window_title)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        self.control_locked("close", args)
    }

    fn read_messages(
        &self,
        window_title: &str,
        limit: i64,
        offset: i64,
        snapshot_id: &str,
    ) -> Result<Value> {
        validate_read_title(window_title)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(offset, "offset", 0, 1000)?;
        validate_snapshot_id_for_read(snapshot_id, offset)?;

        let raw = self.call_checked("read", &[window_title.to_string()], None)?;
        if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(raw);
        }
        let snapshot = snapshot_digest(&raw);
        if !snapshot_id.is_empty() && snapshot_id != snapshot {
            return Ok(json!({
                "status": "snapshot_changed",
                "source": "macos_accessibility",
                "reason": "AX rows changed; restart at offset 0",
                "items": [],
                "databaseCalls": 0,
                "uiActions": 0,
                "snapshotId": snapshot,
            }));
        }

        let rows = raw
            .get("rows")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let items: Vec<Value> = rows.iter().map(ax_row).collect();
        let total = items.len();
        let limit_u = limit as usize;
        let offset_u = offset as usize;
        let end = total.saturating_sub(offset_u);
        let start = end.saturating_sub(limit_u);
        let page_items = items[start..end].to_vec();

        let text_count = items
            .iter()
            .filter(|i| i.get("kind").and_then(|v| v.as_str()) == Some("text"))
            .count();
        let unclassified_count = items
            .iter()
            .filter(|i| i.get("kind").and_then(|v| v.as_str()) == Some("unclassified"))
            .count();
        let attachment_count = items
            .iter()
            .filter(|i| i.get("kind").and_then(|v| v.as_str()) == Some("attachment"))
            .count();

        let next_offset = if start > 0 {
            json!(offset_u + (end - start))
        } else {
            Value::Null
        };

        Ok(json!({
            "status": "ok",
            "source": "macos_accessibility",
            "windowTitle": window_title,
            "items": page_items,
            "total": total,
            "limit": limit,
            "offset": offset,
            "hasMore": start > 0,
            "nextOffset": next_offset,
            "snapshotId": snapshot,
            "textMessageCount": text_count,
            "unclassifiedRowCount": unclassified_count,
            "attachmentRowCount": attachment_count,
            "coverage": {
                "scope": "currently_exposed_ax_rows",
                "historyComplete": false,
                "consistency": "best_effort",
                "exposedRowCount": raw.get("exposedRowCount").cloned().unwrap_or(json!(total)),
                "rowsTruncated": raw.get("rowsTruncated").cloned().unwrap_or(json!(false)),
                "rowStart": raw.get("rowStart").cloned().unwrap_or(json!(0)),
                "rowLimit": 1000,
                "scrollPerformed": false,
                "attributeReadMode": raw.get("attributeReadMode").cloned().unwrap_or(json!("individual")),
                "optionalAttributeFailures": raw.get("optionalAttributeFailures").cloned().unwrap_or(json!({})),
            },
            "databaseCalls": 0,
            "uiActions": 0,
            "elapsedMs": raw.get("elapsedMs").cloned().unwrap_or(json!(0)),
        }))
    }

    fn read_attachment(
        &self,
        window_title: &str,
        row_index: i64,
        snapshot_id: &str,
        output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(row_index, "row_index", 0, 1_000_000)?;
        validate_snapshot_id_required(snapshot_id)?;
        validate_output_path(output_path)?;

        let _ui = ui_session(&self.ui_root)?;
        let raw = self.call_checked("read", &[window_title.to_string()], None)?;
        if raw.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(raw);
        }
        if snapshot_digest(&raw) != snapshot_id {
            return Ok(json!({
                "status": "snapshot_changed",
                "reason": "Read the attachment row again",
                "databaseCalls": 0,
            }));
        }
        let rows = raw
            .get("rows")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let selected: Vec<&Value> = rows
            .iter()
            .filter(|r| r.get("rowIndex").and_then(|v| v.as_i64()) == Some(row_index))
            .collect();
        if selected.len() != 1
            || ax_row(selected[0]).get("kind").and_then(|v| v.as_str()) != Some("attachment")
        {
            return Ok(json!({
                "status": "blocked",
                "reason": "attachment_row_required",
                "databaseCalls": 0,
            }));
        }
        let expected_parts = selected[0]
            .get("parts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let expected_count = raw
            .get("exposedRowCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(rows.len() as u64) as usize;

        let mut value = self.driver.capture_attachment(
            window_title,
            row_index,
            &expected_parts,
            expected_count,
            ocr,
        )?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| KaxError::runtime("RuntimeError", "Invalid attachment reader result"))?;
        let encoded = obj.remove("imageBase64");

        if obj.get("status").and_then(|v| v.as_str()) == Some("ok") {
            let b64 = encoded
                .as_ref()
                .and_then(|v| v.as_str())
                .ok_or_else(|| KaxError::runtime("RuntimeError", "Attachment reader returned no image"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| KaxError::runtime("RuntimeError", e.to_string()))?;
            let final_path = write_private_png(&bytes, output_path)?;
            obj.insert(
                "imagePath".to_string(),
                Value::String(final_path.display().to_string()),
            );
        }
        obj.insert("snapshotId".to_string(), json!(snapshot_id));
        Ok(value)
    }

    fn read_many(
        &self,
        window_titles: &[String],
        limit: i64,
        open_if_needed: bool,
        close_after: bool,
        max_workers: i64,
    ) -> Result<Value> {
        if window_titles.is_empty() || window_titles.len() > 20 {
            return Err(KaxError::validation(
                "window_titles must contain 1..20 exact titles",
            ));
        }
        let mut seen = HashSet::new();
        for name in window_titles {
            validate_title(name)?;
            if !seen.insert(name.as_str()) {
                return Err(KaxError::validation(
                    "Duplicate window_titles are not allowed",
                ));
            }
        }
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(max_workers, "max_workers", 1, 4)?;

        let started = Instant::now();
        let mut prepared: HashMap<String, Value> = HashMap::new();
        let mut opened: Vec<String> = Vec::new();
        {
            let _ui = ui_session(&self.ui_root)?;
            if open_if_needed {
                for name in window_titles {
                    let res = match self.open_room(name, false) {
                        Ok(v) => v,
                        Err(e) => json!({"status": "failed", "errorType": e.error_type()}),
                    };
                    if res.get("status").and_then(|v| v.as_str()) != Some("ok") {
                        prepared.insert(name.clone(), res);
                    } else if res.get("alreadyOpen").and_then(|v| v.as_bool()) != Some(true) {
                        opened.push(name.clone());
                    }
                }
            }
        }

        // Parallel reads bounded by `max_workers` (up to 4).
        let worker_count = (max_workers as usize).min(window_titles.len());
        let items: Vec<Value> = if worker_count <= 1 {
            window_titles
                .iter()
                .map(|name| read_one_for_batch(self, name, limit, &prepared))
                .collect()
        } else {
            let mut results: Vec<Option<Value>> = vec![None; window_titles.len()];
            let chunk_size = window_titles.len().div_ceil(worker_count);
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for (chunk_idx, chunk) in window_titles.chunks(chunk_size).enumerate() {
                    let prepared_ref = &prepared;
                    handles.push(scope.spawn(move || {
                        let base = chunk_idx * chunk_size;
                        chunk
                            .iter()
                            .enumerate()
                            .map(|(i, name)| {
                                (base + i, read_one_for_batch(self, name, limit, prepared_ref))
                            })
                            .collect::<Vec<_>>()
                    }));
                }
                for h in handles {
                    if let Ok(part) = h.join() {
                        for (idx, val) in part {
                            results[idx] = Some(val);
                        }
                    }
                }
            });
            results
                .into_iter()
                .map(|o| o.unwrap_or_else(|| json!({"status": "failed"})))
                .collect()
        };

        let mut cleanup = Vec::new();
        if close_after {
            for name in opened.iter().rev() {
                match self.close_room(name) {
                    Ok(v) => cleanup.push(v),
                    Err(e) => cleanup.push(json!({
                        "status": "failed",
                        "windowTitle": name,
                        "errorType": e.error_type(),
                    })),
                }
            }
        }

        let succeeded = items
            .iter()
            .filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("ok"))
            .count();
        let cleanup_ok = cleanup
            .iter()
            .all(|c| c.get("status").and_then(|v| v.as_str()) == Some("ok"));
        let status = if succeeded == items.len() && cleanup_ok {
            "ok"
        } else {
            "partial"
        };
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

        Ok(json!({
            "status": status,
            "source": "macos_accessibility",
            "items": items,
            "total": items.len(),
            "succeeded": succeeded,
            "windowsOpened": opened,
            "cleanup": cleanup,
            "databaseCalls": 0,
            "parallel": max_workers > 1 && window_titles.len() > 1,
            "atomicAcrossRooms": false,
            "elapsedMs": elapsed,
        }))
    }

    fn load_more(&self, window_title: &str, pages: i64) -> Result<Value> {
        validate_title(window_title)?;
        validate_integer(pages, "pages", 1, 10)?;
        let _ui = ui_session(&self.ui_root)?;
        load_more_impl(
            "macos_accessibility",
            window_title,
            pages,
            || self.read_messages(window_title, 500, 0, ""),
            || {
                let mut args = Map::new();
                args.insert("windowTitle".to_string(), json!(window_title));
                self.driver.control("load_older", &args)
            },
        )
    }

    fn search_messages(
        &self,
        window_title: &str,
        query: &str,
        limit: i64,
        load_pages: i64,
        case_sensitive: bool,
        use_app_search: bool,
        max_steps: i64,
        context_rows: i64,
    ) -> Result<Value> {
        validate_title(window_title)?;
        validate_query_text(query, false)?;
        validate_integer(limit, "limit", 1, 500)?;
        validate_integer(load_pages, "load_pages", 0, 10)?;
        validate_integer(max_steps, "max_steps", 1, 50)?;
        validate_integer(context_rows, "context_rows", 0, 10)?;

        let _ui = ui_session(&self.ui_root)?;
        search_messages_impl(
            "macos_accessibility",
            window_title,
            query,
            limit as usize,
            load_pages,
            case_sensitive,
            use_app_search,
            max_steps as usize,
            context_rows as usize,
            |row| row.get("text").and_then(|v| v.as_str()).is_some(),
            || self.open_room(window_title, false),
            |p| self.load_more(window_title, p),
            |offset, snap| self.read_messages(window_title, 500, offset, snap),
            |action| {
                let mut args = Map::new();
                args.insert("windowTitle".to_string(), json!(window_title));
                args.insert("query".to_string(), json!(query));
                self.driver.control(action, &args)
            },
        )
    }

    fn close_search(&self, window_title: &str) -> Result<Value> {
        validate_title(window_title)?;
        let mut args = Map::new();
        args.insert("windowTitle".to_string(), json!(window_title));
        self.control_locked("search_close", args)
    }

    fn send_message(
        &self,
        window_title: &str,
        text: &str,
        operation_id: &str,
        wait_seconds: f64,
        require_self: bool,
    ) -> Result<Value> {
        let adapter = AxSendAdapter { engine: self };
        SendEngine::new(&adapter).send(window_title, text, operation_id, wait_seconds, require_self)
    }

    fn send_result(&self, operation_id: &str, wait_seconds: f64) -> Result<Value> {
        let adapter = AxSendAdapter { engine: self };
        SendEngine::new(&adapter).result(operation_id, wait_seconds)
    }
}

fn read_one_for_batch<D: AxDriver + 'static>(
    engine: &AxEngine<D>,
    name: &str,
    limit: i64,
    prepared: &HashMap<String, Value>,
) -> Value {
    if let Some(err_val) = prepared.get(name) {
        let mut out = err_val.as_object().cloned().unwrap_or_default();
        out.insert("windowTitle".to_string(), json!(name));
        return Value::Object(out);
    }
    let begin = Instant::now();
    let val = match engine.read_messages(name, limit, 0, "") {
        Ok(v) => v,
        Err(e) => json!({"status": "failed", "errorType": e.error_type()}),
    };
    let elapsed = (begin.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    let mut out = val.as_object().cloned().unwrap_or_default();
    out.insert("windowTitle".to_string(), json!(name));
    out.insert("callElapsedMs".to_string(), json!(elapsed));
    Value::Object(out)
}

/// Shared `load_more` implementation used by both macOS and Windows backends.
pub fn load_more_impl<FRead, FStep>(
    source: &str,
    window_title: &str,
    pages: i64,
    mut read_fn: FRead,
    mut step_fn: FStep,
) -> Result<Value>
where
    FRead: FnMut() -> Result<Value>,
    FStep: FnMut() -> Result<Value>,
{
    let before = read_fn()?;
    if before.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Ok(before);
    }
    let mut steps = Vec::new();
    let mut previous = before.clone();
    let mut after = before.clone();
    for _ in 0..pages {
        let step = step_fn()?;
        steps.push(step.clone());
        if step.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = step.as_object().cloned().unwrap_or_default();
            out.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(out));
        }
        after = read_fn()?;
        if after.get("status").and_then(|v| v.as_str()) != Some("ok") {
            let mut out = after.as_object().cloned().unwrap_or_default();
            out.insert("steps".to_string(), Value::Array(steps));
            return Ok(Value::Object(out));
        }
        if after.get("snapshotId") == previous.get("snapshotId") {
            break;
        }
        previous = after.clone();
    }
    let before_exposed = before
        .get("coverage")
        .and_then(|c| c.get("exposedRowCount"))
        .cloned()
        .unwrap_or(json!(0));
    let after_exposed = after
        .get("coverage")
        .and_then(|c| c.get("exposedRowCount"))
        .cloned()
        .unwrap_or(json!(0));
    Ok(json!({
        "status": "ok",
        "source": source,
        "windowTitle": window_title,
        "steps": steps,
        "pagesAttempted": steps.len(),
        "contentChanged": before.get("snapshotId") != after.get("snapshotId"),
        "exposedRowsBefore": before_exposed,
        "exposedRowsAfter": after_exposed,
        "endOfHistory": Value::Null,
        "historyComplete": false,
        "after": after,
        "databaseCalls": 0,
    }))
}

/// Shared `search_messages` implementation used by both macOS and Windows backends.
#[allow(clippy::too_many_arguments)]
pub fn search_messages_impl<FSearchable, FOpen, FLoad, FRead, FControl>(
    source: &str,
    window_title: &str,
    query: &str,
    limit: usize,
    load_pages: i64,
    case_sensitive: bool,
    use_app_search: bool,
    max_steps: usize,
    context_rows: usize,
    searchable: FSearchable,
    mut open_fn: FOpen,
    mut load_fn: FLoad,
    mut read_fn: FRead,
    mut control_fn: FControl,
) -> Result<Value>
where
    FSearchable: Fn(&Value) -> bool,
    FOpen: FnMut() -> Result<Value>,
    FLoad: FnMut(i64) -> Result<Value>,
    FRead: FnMut(i64, &str) -> Result<Value>,
    FControl: FnMut(&str) -> Result<Value>,
{
    let opened = open_fn()?;
    if opened.get("status").and_then(|v| v.as_str()) != Some("ok") {
        return Ok(opened);
    }
    if load_pages > 0 {
        let history = load_fn(load_pages)?;
        if history.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Ok(history);
        }
    }

    let mut visits: Vec<Value> = Vec::new();
    let mut matches: Vec<Value> = Vec::new();
    let mut seen_snapshots: HashSet<String> = HashSet::new();
    let mut searched_rows: usize = 0;
    let mut stop_reason = if use_app_search {
        "step_limit".to_string()
    } else {
        "exposed_rows_only".to_string()
    };
    let mut verified = true;
    let mut coverage = json!({"historyComplete": false});
    let mut snapshot: Value = Value::Null;
    let needle = if case_sensitive {
        query.to_string()
    } else {
        caseless::default_case_fold_str(query)
    };

    let total_steps = if use_app_search { max_steps } else { 1 };
    for step in 0..total_steps {
        let native = if use_app_search {
            Some(control_fn(if step == 0 {
                "search_query"
            } else {
                "search_previous"
            })?)
        } else {
            None
        };

        if let Some(ref nat) = native {
            if nat.get("searchOutcome").and_then(|v| v.as_str()) == Some("navigation_exhausted") {
                visits.push(nat.clone());
                stop_reason = "navigation_exhausted".to_string();
                break;
            }
            visits.push(nat.clone());
            verified = verified && nat.get("searchVerified") == Some(&Value::Bool(true));
            let st = nat.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if !matches!(st, "ok" | "partial") {
                stop_reason = nat
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search_navigation_failed")
                    .to_string();
                verified = false;
                break;
            }
        }

        let mut page = read_fn(0, "")?;
        if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
            stop_reason = page
                .get("reason")
                .or_else(|| page.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            verified = false;
            break;
        }

        let mut rows = page
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let snap_str = page
            .get("snapshotId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        snapshot = Value::String(snap_str.clone());
        if let Some(cov) = page.get("coverage") {
            coverage = cov.clone();
        }

        while page.get("hasMore") == Some(&Value::Bool(true)) {
            let next_off = page
                .get("nextOffset")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            page = read_fn(next_off, &snap_str)?;
            if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
                break;
            }
            let mut older = page
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            older.extend(rows);
            rows = older;
        }
        if page.get("status").and_then(|v| v.as_str()) != Some("ok") {
            stop_reason = page
                .get("reason")
                .or_else(|| page.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            verified = false;
            break;
        }

        if seen_snapshots.insert(snap_str.clone()) {
            searched_rows += rows.len();
            for (idx, row) in rows.iter().enumerate() {
                if !searchable(row) {
                    continue;
                }
                let Some(body) = row.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let matched = if case_sensitive {
                    body.contains(&needle)
                } else {
                    caseless::default_case_fold_str(body).contains(&needle)
                };
                if !matched {
                    continue;
                }
                let before_start = idx.saturating_sub(context_rows);
                let after_end = (idx + 1 + context_rows).min(rows.len());
                let mut item = row.as_object().cloned().unwrap_or_default();
                item.insert("snapshotId".to_string(), Value::String(snap_str.clone()));
                item.insert("searchVisit".to_string(), json!(step));
                item.insert(
                    "contextBefore".to_string(),
                    Value::Array(rows[before_start..idx].to_vec()),
                );
                item.insert(
                    "contextAfter".to_string(),
                    Value::Array(rows[idx + 1..after_end].to_vec()),
                );
                matches.push(Value::Object(item));
            }
        }

        if let Some(ref nat) = native {
            if !verified {
                stop_reason = nat
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("search_result_not_observed")
                    .to_string();
                break;
            }
        }
        if matches.len() >= limit {
            stop_reason = "result_limit".to_string();
            break;
        }
        if let Some(ref nat) = native {
            if nat
                .get("searchState")
                .and_then(|s| s.get("previousEnabled"))
                == Some(&Value::Bool(false))
            {
                stop_reason = "navigation_exhausted".to_string();
                break;
            }
        }
    }

    let mut cov_map = coverage.as_object().cloned().unwrap_or_default();
    cov_map.insert("historyComplete".to_string(), json!(false));
    cov_map.insert("snapshotsVisited".to_string(), json!(seen_snapshots.len()));

    let total_matches = matches.len();
    let truncated = total_matches > limit;
    let items_slice: Vec<Value> = matches.into_iter().take(limit).collect();

    Ok(json!({
        "status": if verified { "ok" } else { "partial" },
        "source": source,
        "windowTitle": window_title,
        "query": query,
        "items": items_slice,
        "total": total_matches,
        "truncated": truncated,
        "coverage": Value::Object(cov_map),
        "searchedRows": searched_rows,
        "historyRequested": load_pages > 0,
        "searchScope": if use_app_search { "visited_app_search_snapshots" } else { "currently_exposed_bodies" },
        "snapshotId": snapshot,
        "databaseCalls": 0,
        "appSearchUsed": use_app_search,
        "searchVerified": if use_app_search { Value::Bool(verified) } else { Value::Null },
        "stopReason": stop_reason,
        "searchVisits": visits,
        "duplicatesPossible": seen_snapshots.len() > 1,
        "nativeSearch": visits.first().cloned().unwrap_or(Value::Null),
    }))
}

/// Write a PNG to either the requested exclusive absolute path (`0600`) or a private temp file.
pub fn write_private_png(bytes: &[u8], output_path: Option<&str>) -> Result<PathBuf> {
    if let Some(path_str) = output_path {
        let path = PathBuf::from(path_str);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path)?;
        if let Err(err) = file.write_all(bytes) {
            let _ = std::fs::remove_file(&path);
            return Err(err.into());
        }
        Ok(path)
    } else {
        let mut temp = tempfile::Builder::new()
            .prefix("kax-attachment-")
            .suffix(".png")
            .tempfile()?;
        temp.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
        }
        let (_, path) = temp
            .keep()
            .map_err(|e| KaxError::runtime("OSError", e.to_string()))?;
        Ok(path)
    }
}

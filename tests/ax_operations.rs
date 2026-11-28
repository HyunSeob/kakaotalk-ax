use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::{KaxError, Result};
use kakaotalk_ax::state::{sha256_hex, ui_session, ProcessLock};

struct FixtureState {
    opened: HashSet<String>,
    known: HashSet<String>,
    rows: HashMap<String, Vec<String>>,
    controls: Vec<(String, Map<String, Value>)>,
    sends: usize,
    mode: String,
    draft: String,
    history: Vec<String>,
    fail_close: bool,
    reads: usize,
    row_start: usize,
}

struct FixtureDriver {
    state: Mutex<FixtureState>,
    barrier: Mutex<Option<Arc<Barrier>>>,
}

impl FixtureDriver {
    fn new() -> Arc<Self> {
        let mut opened = HashSet::new();
        opened.insert("Fixture".to_string());
        let mut known = HashSet::new();
        known.insert("Fixture".to_string());
        known.insert("Other".to_string());
        let mut rows = HashMap::new();
        rows.insert(
            "Fixture".to_string(),
            vec!["old".to_string(), "repeat".to_string()],
        );
        rows.insert("Other".to_string(), vec!["other".to_string()]);
        Arc::new(Self {
            state: Mutex::new(FixtureState {
                opened,
                known,
                rows,
                controls: Vec::new(),
                sends: 0,
                mode: "delivered".to_string(),
                draft: String::new(),
                history: Vec::new(),
                fail_close: false,
                reads: 0,
                row_start: 0,
            }),
            barrier: Mutex::new(None),
        })
    }
}

impl AxDriver for FixtureDriver {
    fn call(&self, action: &str, titles: &[String], text: Option<&str>) -> Result<Value> {
        if action == "read" || action == "read_all" {
            let b = self.barrier.lock().unwrap().clone();
            if let Some(bar) = b {
                bar.wait();
            }
            let mut st = self.state.lock().unwrap();
            st.reads += 1;
            let wt = &titles[0];
            if !st.opened.contains(wt) {
                return Ok(json!({"status": "blocked", "reason": "target_room_window_not_open"}));
            }
            let bodies = st.rows.get(wt).cloned().unwrap_or_default();
            let rows_json: Vec<Value> = bodies
                .iter()
                .enumerate()
                .map(|(i, t)| json!({"rowIndex": i, "parts": [{"role": "AXTextArea", "AXValue": t}]}))
                .collect();
            return Ok(json!({
                "status": "ok",
                "windowTitle": wt,
                "processId": 1,
                "rows": rows_json,
                "exposedRowCount": bodies.len(),
                "rowStart": st.row_start,
                "rowsTruncated": st.row_start > 0,
                "elapsedMs": 1,
            }));
        }
        let mut st = self.state.lock().unwrap();
        if action == "inspect" {
            return Ok(json!({"status": if st.draft.is_empty() { "ready" } else { "blocked" }}));
        }
        if action == "send" {
            st.sends += 1;
            if !st.draft.is_empty() {
                return Ok(json!({
                    "status": "blocked",
                    "dispatchAttempted": false,
                    "reason": "existing_draft_preserved",
                }));
            }
            let wt = titles[0].clone();
            let body = text.unwrap_or("").to_string();
            if matches!(st.mode.as_str(), "delivered" | "lost_reply" | "extra_message") {
                let extra = st.mode == "extra_message";
                let entry = st.rows.entry(wt).or_default();
                if extra {
                    entry.push("unrelated incoming text".to_string());
                }
                entry.push(body);
            }
            if matches!(st.mode.as_str(), "lost_reply" | "not_delivered") {
                return Err(KaxError::runtime("TimeoutError", "timed out"));
            }
            return Ok(json!({
                "status": "dispatch_result_unknown",
                "dispatchAttempted": true,
                "axResult": -25200,
            }));
        }
        panic!("Unexpected native action: {action}");
    }

    fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value> {
        let mut st = self.state.lock().unwrap();
        st.controls.push((action.to_string(), args.clone()));
        let name = args
            .get("windowTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if action == "rooms" {
            let mut names: Vec<String> = st.known.iter().cloned().collect();
            names.sort();
            let items: Vec<Value> = names
                .into_iter()
                .map(|n| json!({"name": n, "isSelf": n == "Fixture"}))
                .collect();
            return Ok(json!({
                "status": "ok",
                "items": items,
                "searchQuery": "",
                "scope": "currently_exposed_room_list",
            }));
        }
        if action == "search_rooms" {
            return Ok(json!({"status": "ok", "items": []}));
        }
        if action == "open" {
            // Like the macOS driver: a room-list name with a type label ("오픈채팅, X") opens
            // the window titled without it and reports that title.
            let title = kakaotalk_ax::macos_ax_logic::room_base_name(&name).to_string();
            if !st.known.contains(&title) {
                return Ok(json!({"status": "blocked", "reason": "room_not_in_exposed_list"}));
            }
            let existed = !st.opened.insert(title.clone());
            return Ok(json!({"status": "ok", "alreadyOpen": existed, "windowTitle": title}));
        }
        if action == "close" {
            if st.fail_close {
                return Err(KaxError::runtime("TimeoutError", "close timed out"));
            }
            if !st.draft.is_empty() && name == "Fixture" {
                return Ok(json!({"status": "blocked", "reason": "existing_draft_preserved"}));
            }
            st.opened.remove(&name);
            return Ok(json!({"status": "ok"}));
        }
        if action == "load_older" {
            // Mirrors the macOS driver, which reports the transcript's AXRows count around a step.
            let before = st.rows.get(&name).map_or(0, |r| r.len());
            if !st.history.is_empty() {
                let older = st.history.remove(0);
                st.rows.entry(name.clone()).or_default().insert(0, older);
            }
            let after = st.rows.get(&name).map_or(0, |r| r.len());
            return Ok(json!({"status": "ok", "exposedRowsBefore": before, "exposedRowsAfter": after}));
        }
        if matches!(action, "search_query" | "search_previous") {
            return Ok(json!({
                "status": "ok",
                "searchVerified": true,
                "searchState": {"previousEnabled": false},
            }));
        }
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

fn setup() -> (Arc<FixtureDriver>, AxEngine<FixtureDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let driver = FixtureDriver::new();
    let engine = AxEngine::with_roots(
        Arc::clone(&driver),
        dir.path().join("ui"),
        dir.path().join("operations"),
    );
    (driver, engine, dir)
}

#[test]
fn ui_lock_can_be_nested_by_operations() {
    let (_driver, engine, dir) = setup();
    let ui_root = dir.path().join("ui");
    let _g1 = ui_session(&ui_root).unwrap();
    let _g2 = ui_session(&ui_root).unwrap();
    assert_eq!(engine.open_room("Other", false).unwrap()["status"], "ok");
}

#[test]
#[ignore]
fn hold_ui_lock_child_helper() {
    use std::io::{BufRead, Write};
    let root = std::path::PathBuf::from(std::env::var("KAX_TEST_UI_LOCK_ROOT").unwrap());
    let _guard = ui_session(&root).expect("child acquire ui_session");
    println!("LOCKED");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

#[test]
fn process_lock_rejects_concurrent_holder() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ui");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hold_ui_lock_child_helper", "--ignored", "--nocapture"])
        .env("KAX_TEST_UI_LOCK_ROOT", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child lock holder");

    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    loop {
        ready.clear();
        let n = reader.read_line(&mut ready).unwrap();
        assert!(n > 0, "child exited before acquiring lock");
        if ready.trim() == "LOCKED" {
            break;
        }
    }

    // While child process holds the UI lock, another process cannot acquire it.
    let err = ProcessLock::acquire(&root, "Another KakaoTalk AX operation is in progress")
        .err()
        .expect("cross-process lock must fail while child holds it");
    assert!(err.message().contains("in progress"));

    // Release child and confirm lock becomes available again.
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(b"release\n");
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());

    let _reacquired =
        ProcessLock::acquire(&root, "Another KakaoTalk AX operation is in progress").unwrap();
}

#[test]
fn batch_opens_missing_rooms_and_reads_actually_in_parallel() {
    let (driver, engine, _dir) = setup();
    *driver.barrier.lock().unwrap() = Some(Arc::new(Barrier::new(2)));
    let result = engine
        .read_many(
            &["Fixture".to_string(), "Other".to_string()],
            50,
            true,
            false,
            2,
        )
        .unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["windowsOpened"], json!(["Other"]));
    assert_eq!(result["succeeded"], 2);
    let titles: Vec<&str> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["windowTitle"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Fixture", "Other"]);
    assert_eq!(result["atomicAcrossRooms"], false);
    assert_eq!(result["parallel"], true);
}

#[test]
fn batch_preserves_existing_windows_and_reports_missing_room_individually() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .read_many(
            &[
                "Fixture".to_string(),
                "Other".to_string(),
                "Missing".to_string(),
            ],
            50,
            true,
            true,
            3,
        )
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["succeeded"], 2);
    let opened = &driver.state.lock().unwrap().opened;
    assert_eq!(opened.len(), 1);
    assert!(opened.contains("Fixture"));
    assert_eq!(result["items"][2]["status"], "blocked");
}

#[test]
fn batch_reads_and_closes_the_window_a_type_labelled_name_opened() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .read_many(&["오픈채팅, Other".to_string()], 50, true, true, 1)
        .unwrap();
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["items"][0]["status"], "ok");
    assert_eq!(result["items"][0]["windowTitle"], "Other");
    assert_eq!(result["items"][0]["requestedTitle"], "오픈채팅, Other");
    assert_eq!(result["windowsOpened"], json!(["Other"]));
    assert!(!driver.state.lock().unwrap().opened.contains("Other"), "window left open");
    let closes: Vec<Value> = driver
        .state
        .lock()
        .unwrap()
        .controls
        .iter()
        .filter(|(a, _)| a == "close")
        .map(|(_, args)| args["windowTitle"].clone())
        .collect();
    assert_eq!(closes, vec![json!("Other")]);
}

#[test]
fn search_uses_the_window_title_a_type_labelled_name_opened() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .search_messages("오픈채팅, Other", "other", 100, 0, false, false, 10, 2)
        .unwrap();
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["windowTitle"], "Other");
    assert_eq!(result["requestedTitle"], "오픈채팅, Other");
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    // Exactly one open, made with the requested name.
    let opens: Vec<Value> = driver
        .state
        .lock()
        .unwrap()
        .controls
        .iter()
        .filter(|(a, _)| a == "open")
        .map(|(_, args)| args["windowTitle"].clone())
        .collect();
    assert_eq!(opens, vec![json!("오픈채팅, Other")]);
}

#[test]
fn batch_does_not_open_when_disabled() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .read_many(&["Other".to_string()], 50, false, false, 3)
        .unwrap();
    assert_eq!(result["items"][0]["reason"], "target_room_window_not_open");
    assert!(driver.state.lock().unwrap().controls.is_empty());
}

#[test]
fn batch_cleanup_does_not_discard_a_draft() {
    let (driver, engine, _dir) = setup();
    {
        let mut st = driver.state.lock().unwrap();
        st.opened.clear();
        st.draft = "keep this".to_string();
    }
    let result = engine
        .read_many(&["Fixture".to_string()], 50, true, true, 3)
        .unwrap();
    assert_eq!(result["status"], "partial");
    let st = driver.state.lock().unwrap();
    assert!(st.opened.contains("Fixture"));
    assert_eq!(st.draft, "keep this");
}

#[test]
fn cleanup_failure_keeps_the_already_read_results() {
    let (driver, engine, _dir) = setup();
    driver.state.lock().unwrap().fail_close = true;
    let result = engine
        .read_many(&["Other".to_string()], 50, true, true, 3)
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert_eq!(result["succeeded"], 1);
    assert_eq!(result["cleanup"][0]["errorType"], "TimeoutError");
}

#[test]
fn literal_search_reads_all_exposed_pages_without_regex_or_wildcards() {
    let (driver, engine, _dir) = setup();
    let mut rows: Vec<String> = (0..501).map(|i| format!("row {i}")).collect();
    rows.extend(["A%_B".to_string(), "a%_b".to_string(), "AxxB".to_string()]);
    driver.state.lock().unwrap().rows.insert("Fixture".to_string(), rows);
    let result = engine
        .search_messages("Fixture", "%_", 100, 0, false, false, 10, 2)
        .unwrap();
    let texts: Vec<&str> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["A%_B", "a%_b"]);
    assert_eq!(result["searchedRows"], 504);
    assert_eq!(result["coverage"]["historyComplete"], false);
    assert!(!driver
        .state
        .lock()
        .unwrap()
        .controls
        .iter()
        .any(|(a, _)| a == "search_query"));
}

#[test]
fn app_search_is_inside_the_helper() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .search_messages("Fixture", "REPEAT", 100, 0, false, true, 10, 2)
        .unwrap();
    assert_eq!(result["total"], 1);
    assert!(driver.state.lock().unwrap().controls.iter().any(|(a, args)| {
        a == "search_query"
            && args.get("windowTitle") == Some(&json!("Fixture"))
            && args.get("query") == Some(&json!("REPEAT"))
    }));
}

#[test]
fn history_is_bounded_and_no_change_is_not_an_end_of_history_claim() {
    let (driver, engine, _dir) = setup();
    let result = engine.load_more("Fixture", 5).unwrap();
    assert_eq!(result["pagesAttempted"], 1);
    assert_eq!(result["contentChanged"], false);
    assert!(result["endOfHistory"].is_null());
    assert_eq!(result["historyComplete"], false);

    driver.state.lock().unwrap().history = vec!["older one".to_string(), "older two".to_string()];
    let result2 = engine.load_more("Fixture", 2).unwrap();
    assert_eq!(result2["pagesAttempted"], 2);
    assert_eq!(result2["contentChanged"], true);
}

#[test]
fn history_steps_use_row_counts_instead_of_rereading_the_transcript() {
    let (driver, engine, _dir) = setup();
    driver.state.lock().unwrap().history =
        (0..12).map(|i| format!("older {i}")).collect();

    // load_more: ten steps, one transcript read at the end.
    let result = engine.load_more("Fixture", 10).unwrap();
    assert_eq!(result["pagesAttempted"], 10);
    assert_eq!(result["loadStopReason"], "page_limit");
    assert_eq!(result["exposedRowsBefore"], 2);
    assert_eq!(result["exposedRowsAfter"], 12);
    assert_eq!(result["contentChanged"], true);
    assert_eq!(driver.state.lock().unwrap().reads, 1);

    // Stops early once a step exposes no new rows (2 left, then nothing).
    let result = engine.load_more("Fixture", 10).unwrap();
    assert_eq!(result["pagesAttempted"], 3);
    assert_eq!(result["loadStopReason"], "no_new_rows");
    assert!(result["endOfHistory"].is_null());
    assert_eq!(driver.state.lock().unwrap().reads, 2);

    // search_messages loads without any extra read; only the search's own read remains.
    driver.state.lock().unwrap().history = vec!["older game".to_string()];
    let before = driver.state.lock().unwrap().reads;
    let found = engine
        .search_messages("Fixture", "game", 100, 10, false, false, 10, 0)
        .unwrap();
    assert_eq!(found["total"], 1);
    assert_eq!(driver.state.lock().unwrap().reads - before, 1);
    assert_eq!(found["historyLoad"]["loadStopReason"], "no_new_rows");
    assert_eq!(found["historyLoad"]["exposedRowsAfter"], 15);
    assert!(found["historyLoad"].get("after").is_none());
}

#[test]
fn search_reports_rows_beyond_the_row_cap_as_unsearched() {
    let (driver, engine, _dir) = setup();
    let full = engine.search_messages("Fixture", "old", 100, 0, false, false, 10, 0).unwrap();
    assert!(full["unsearchedRowRange"].is_null());
    assert!(full["historyLoad"].is_null());

    driver.state.lock().unwrap().row_start = 292;
    let cut = engine.search_messages("Fixture", "old", 100, 0, false, false, 10, 0).unwrap();
    assert_eq!(cut["unsearchedRowRange"], json!({"start": 0, "end": 292, "count": 292}));
}

#[test]
fn invalid_requests_do_not_reach_navigation() {
    let (driver, engine, _dir) = setup();
    assert!(engine.open_room(" ", false).is_err());
    assert!(engine
        .read_many(&["Fixture".to_string(), "Fixture".to_string()], 50, true, false, 3)
        .is_err());
    assert!(engine
        .read_many(&["Fixture".to_string()], 50, true, false, 5)
        .is_err());
    assert!(engine.load_more("Fixture", 11).is_err());
    assert!(engine
        .search_messages("Fixture", "", 100, 0, false, true, 10, 2)
        .is_err());
    assert!(engine.rooms("", true, 0).is_err());
    assert!(engine.rooms("", true, 1001).is_err());
    assert!(engine.members("Fixture", 0).is_err());
    assert!(engine.members("Fixture", 3001).is_err());
    assert!(engine.members(" ", 10).is_err());
    assert!(driver.state.lock().unwrap().controls.is_empty());
}

#[test]
fn members_forwards_the_exact_title_and_limit_to_the_driver() {
    let (driver, engine, _dir) = setup();
    engine.members("Fixture", 25).unwrap();
    let controls = driver.state.lock().unwrap().controls.clone();
    assert_eq!(controls.len(), 1);
    assert_eq!(controls[0].0, "members");
    assert_eq!(controls[0].1.get("windowTitle"), Some(&json!("Fixture")));
    assert_eq!(controls[0].1.get("limit"), Some(&json!(25)));
}

#[test]
fn rooms_forwards_scroll_only_when_requested_and_still_filters_by_query() {
    let (driver, engine, _dir) = setup();
    engine.rooms("", false, 300).unwrap();
    let out = engine.rooms("fix", true, 50).unwrap();
    assert_eq!(out["status"], "ok");
    assert_eq!(out["total"], 1);
    assert_eq!(out["items"][0]["name"], "Fixture");
    let controls = driver.state.lock().unwrap().controls.clone();
    assert_eq!(controls.len(), 2);
    assert_eq!(controls[0].0, "rooms");
    assert!(controls[0].1.is_empty(), "plain rooms keeps the old request");
    assert_eq!(controls[1].1.get("scroll"), Some(&json!(true)));
    assert_eq!(controls[1].1.get("limit"), Some(&json!(50)));
}

#[test]
fn ax_error_after_delivery_is_observed_without_retry_or_database() {
    let (driver, engine, _dir) = setup();
    let result = engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(result["status"], "observed_in_ax");
    assert_eq!(result["native"]["axResult"], -25200);
    assert_eq!(driver.state.lock().unwrap().sends, 1);

    let second = engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(second["status"], "observed_in_ax");
    assert_eq!(second["deduplicated"], true);
    assert_eq!(second["nativeSendCalls"], 0);
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn lost_reply_after_delivery_can_be_observed() {
    let (driver, engine, _dir) = setup();
    driver.state.lock().unwrap().mode = "lost_reply".to_string();
    let res = engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(res["status"], "observed_in_ax");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn uncertainty_is_persisted_and_late_observation_never_resends() {
    let (driver, engine, dir) = setup();
    driver.state.lock().unwrap().mode = "not_delivered".to_string();
    assert_eq!(
        engine
            .send_message("Fixture", "new body", "intent", 0.0, false)
            .unwrap()["status"],
        "outcome_unknown"
    );
    let engine2 = AxEngine::with_roots(
        Arc::clone(&driver),
        dir.path().join("ui"),
        dir.path().join("operations"),
    );
    let second = engine2
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(second["nativeSendCalls"], 0);
    driver
        .state
        .lock()
        .unwrap()
        .rows
        .get_mut("Fixture")
        .unwrap()
        .push("new body".to_string());
    assert_eq!(engine.send_result("intent", 0.0).unwrap()["status"], "observed_in_ax");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn preexisting_equal_body_does_not_confirm_an_unsent_message() {
    let (driver, engine, _dir) = setup();
    {
        let mut st = driver.state.lock().unwrap();
        st.rows.get_mut("Fixture").unwrap().push("new body".to_string());
        st.mode = "not_delivered".to_string();
    }
    assert_eq!(
        engine
            .send_message("Fixture", "new body", "intent", 0.0, false)
            .unwrap()["status"],
        "outcome_unknown"
    );
}

#[test]
fn extra_incoming_message_makes_confirmation_ambiguous() {
    let (driver, engine, _dir) = setup();
    driver.state.lock().unwrap().mode = "extra_message".to_string();
    assert_eq!(
        engine
            .send_message("Fixture", "new body", "intent", 0.0, false)
            .unwrap()["status"],
        "outcome_unknown"
    );
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn existing_draft_is_preserved_and_blocked_intent_is_not_retried() {
    let (driver, engine, _dir) = setup();
    driver.state.lock().unwrap().draft = "keep this".to_string();
    let result = engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(result["reason"], "existing_draft_preserved");
    assert_eq!(driver.state.lock().unwrap().draft, "keep this");

    let second = engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    assert_eq!(second["nativeSendCalls"], 0);
    assert_eq!(driver.state.lock().unwrap().sends, 1);
    assert_eq!(
        driver.state.lock().unwrap().rows["Fixture"],
        vec!["old", "repeat"]
    );
}

#[test]
fn intent_cannot_be_reused_for_a_different_payload() {
    let (driver, engine, _dir) = setup();
    engine
        .send_message("Fixture", "new body", "intent", 0.0, false)
        .unwrap();
    let err = engine
        .send_message("Fixture", "different", "intent", 0.0, false)
        .err()
        .expect("reusing operation_id with different body must fail");
    assert_eq!(err.error_type(), "ValueError");
    assert_eq!(driver.state.lock().unwrap().sends, 1);
}

#[test]
fn journal_is_private_and_does_not_store_bodies() {
    let (_driver, engine, dir) = setup();
    engine
        .send_message("Fixture", "fixture private payload", "intent", 0.0, false)
        .unwrap();
    let path = dir
        .path()
        .join("operations")
        .join(format!("{}.json", sha256_hex("intent")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("fixture private payload"));
}

#[test]
fn corrupt_or_non_object_current_journal_blocks_without_dispatching() {
    let (driver, engine, dir) = setup();
    let ops_dir = dir.path().join("operations");
    std::fs::create_dir_all(&ops_dir).unwrap();

    // Case 1: Valid JSON array `[]` (non-object) must NOT be treated as a new send.
    let array_path = ops_dir.join(format!("{}.json", sha256_hex("corrupt-array")));
    std::fs::write(&array_path, "[]").unwrap();
    let err = engine
        .send_message("Fixture", "should not send", "corrupt-array", 0.0, false)
        .unwrap_err();
    assert_eq!(err.error_type(), "ValueError");
    assert_eq!(driver.state.lock().unwrap().sends, 0);

    let res_err = engine.send_result("corrupt-array", 0.0).unwrap_err();
    assert_eq!(res_err.error_type(), "ValueError");
    assert!(res_err.message().contains("Corrupt send journal"));

    // Case 2: Truncated/malformed JSON must also block with 0 sends.
    let broken_path = ops_dir.join(format!("{}.json", sha256_hex("corrupt-json")));
    std::fs::write(&broken_path, "{not-json").unwrap();
    assert!(engine
        .send_message("Fixture", "should not send", "corrupt-json", 0.0, false)
        .is_err());
    assert_eq!(driver.state.lock().unwrap().sends, 0);
}

#[test]
fn require_self_propagates_verified_window_identity_and_blocks_if_changed_at_dispatch() {
    struct IdentityCheckingDriver {
        opened_identity: String,
        current_identity: Mutex<String>,
        sends: Mutex<usize>,
    }

    impl AxDriver for IdentityCheckingDriver {
        fn call(&self, action: &str, titles: &[String], _text: Option<&str>) -> Result<Value> {
            match action {
                "read" => Ok(json!({
                    "status": "ok",
                    "windowTitle": titles[0],
                    "processId": 1,
                    "rows": [],
                    "exposedRowCount": 0,
                    "rowStart": 0,
                    "rowsTruncated": false,
                    "elapsedMs": 1,
                })),
                "inspect" => Ok(json!({"status": "ready"})),
                _ => Ok(json!({"status": "ok"})),
            }
        }

        fn send_with_identity(
            &self,
            _titles: &[String],
            _text: &str,
            require_self: bool,
            window_identity: Option<&str>,
        ) -> Result<Value> {
            assert!(require_self, "requireSelf must propagate to dispatch");
            let current = self.current_identity.lock().unwrap().clone();
            if window_identity != Some(current.as_str()) {
                return Ok(json!({
                    "status": "blocked",
                    "reason": "target_room_window_changed",
                    "dispatchAttempted": false,
                }));
            }
            *self.sends.lock().unwrap() += 1;
            Ok(json!({
                "status": "dispatch_attempted",
                "dispatchAttempted": true,
            }))
        }

        fn control(&self, action: &str, args: &Map<String, Value>) -> Result<Value> {
            assert_eq!(action, "open");
            assert_eq!(args.get("requireSelf"), Some(&Value::Bool(true)));
            Ok(json!({
                "status": "ok",
                "windowTitle": args["windowTitle"],
                "alreadyOpen": true,
                "isSelf": true,
                "windowIdentity": self.opened_identity,
            }))
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

    let dir = tempfile::tempdir().unwrap();
    let driver = Arc::new(IdentityCheckingDriver {
        opened_identity: "macos-self-ax:1:42".to_string(),
        current_identity: Mutex::new("macos-self-ax:1:99".to_string()),
        sends: Mutex::new(0),
    });
    let engine = AxEngine::with_roots(
        Arc::clone(&driver),
        dir.path().join("ui"),
        dir.path().join("ops"),
    );

    let blocked = engine
        .send_message("이현섭", "테스트", "self-changed-1", 0.0, true)
        .unwrap();
    assert_eq!(blocked["status"], "blocked");
    assert_eq!(blocked["reason"], "target_room_window_changed");
    assert_eq!(*driver.sends.lock().unwrap(), 0);
}

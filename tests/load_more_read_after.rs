//! `load_more(read_after=false, until_date)` and the measured "did it load" rule.
//! Row shapes follow KakaoTalk Mac 26.10.0; all names and texts are fictional.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use kakaotalk_ax::ax_core::{load_more_summary, AxDriver, AxEngine, KakaoBackend};
use kakaotalk_ax::error::Result;

#[derive(Clone)]
enum Row {
    Sep(&'static str),
    Text(String),
}

fn row_json(i: usize, r: &Row) -> Value {
    match r {
        Row::Sep(label) => json!({"rowIndex": i, "parts": [{"role": "AXButton", "AXDescription": label}]}),
        Row::Text(t) => json!({"rowIndex": i, "parts": [{"role": "AXTextArea", "AXValue": t}]}),
    }
}

/// What one load step does to the transcript.
#[derive(Clone)]
enum Step {
    /// Older rows arrive at the top.
    Older(Vec<Row>),
    /// Nothing loads, but `n` new messages arrive at the bottom.
    NewAtBottom(usize),
    /// Nothing changes.
    Nothing,
}

struct State {
    rows: Vec<Row>,
    script: Vec<Step>,
    full_reads: usize,
    steps: usize,
    range_reads: usize,
}

struct Driver {
    st: Mutex<State>,
    ranges: bool,
}

impl Driver {
    fn new(rows: Vec<Row>, script: Vec<Step>) -> Arc<Self> {
        Arc::new(Self { st: Mutex::new(State { rows, script, full_reads: 0, steps: 0, range_reads: 0 }), ranges: true })
    }
}

impl AxDriver for Driver {
    fn call(&self, action: &str, titles: &[String], _text: Option<&str>) -> Result<Value> {
        assert!(matches!(action, "read" | "read_all"), "unexpected {action}");
        let mut st = self.st.lock().unwrap();
        st.full_reads += 1;
        let rows: Vec<Value> = st.rows.iter().enumerate().map(|(i, r)| row_json(i, r)).collect();
        Ok(json!({"status": "ok", "windowTitle": titles[0], "processId": 1, "rows": rows,
                  "exposedRowCount": st.rows.len(), "rowStart": 0, "rowsTruncated": false, "elapsedMs": 1}))
    }
    fn control(&self, action: &str, _args: &Map<String, Value>) -> Result<Value> {
        assert_eq!(action, "load_older");
        let mut st = self.st.lock().unwrap();
        st.steps += 1;
        let before = st.rows.len();
        let step = if st.script.is_empty() { Step::Nothing } else { st.script.remove(0) };
        match step {
            Step::Older(rows) => {
                let mut v = rows;
                v.extend(st.rows.drain(..));
                st.rows = v;
            }
            Step::NewAtBottom(n) => {
                for i in 0..n {
                    st.rows.push(Row::Text(format!("새 메시지 {i}")));
                }
            }
            Step::Nothing => {}
        }
        Ok(json!({"status": "ok", "loadStatus": "loaded", "exposedRowsBefore": before, "exposedRowsAfter": st.rows.len()}))
    }
    fn read_range(&self, window_title: &str, start: usize, count: usize) -> Option<Result<Value>> {
        if !self.ranges {
            return None;
        }
        let mut st = self.st.lock().unwrap();
        st.range_reads += 1;
        let rows: Vec<Value> = st.rows.iter().enumerate().skip(start).take(count).map(|(i, r)| row_json(i, r)).collect();
        Some(Ok(json!({"status": "ok", "windowTitle": window_title, "rows": rows,
                       "exposedRowCount": st.rows.len(), "rowStart": start})))
    }
    fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
        unreachable!()
    }
}

fn texts(prefix: &str, n: usize) -> Vec<Row> {
    (0..n).map(|i| Row::Text(format!("{prefix} {i}"))).collect()
}

fn day(label: &'static str, prefix: &str, n: usize) -> Vec<Row> {
    let mut v = vec![Row::Sep(label)];
    v.extend(texts(prefix, n));
    v
}

fn engine(driver: &Arc<Driver>) -> (AxEngine<Driver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let e = AxEngine::with_roots(Arc::clone(driver), dir.path().join("ui"), dir.path().join("operations"));
    (e, dir)
}

#[test]
fn read_after_false_skips_the_newest_rows_read_and_reports_the_oldest_loaded_day() {
    let driver = Driver::new(
        day("2026년 8월 3일 월요일", "셋째 날", 20),
        vec![Step::Older(day("2026년 8월 2일 일요일", "둘째 날", 30)), Step::Older(day("2026년 8월 1일 토요일", "첫날", 30))],
    );
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 2, false, None).unwrap();
    assert_eq!(driver.st.lock().unwrap().full_reads, 0, "no transcript read");
    assert_eq!(out["loadStopReason"], "page_limit");
    assert_eq!(out["exposedRowsBefore"], 21);
    assert_eq!(out["exposedRowsAfter"], 83);
    assert_eq!(out["exposedOldestDate"], "2026-08-01");
    assert_eq!(out["historyComplete"], false);
    assert!(out.get("after").is_none());
    let s = load_more_summary(&out);
    assert_eq!(s["snapshotId"], Value::Null);
    assert_eq!(s["exposedOldestDate"], "2026-08-01");
    assert_eq!(s["readAfter"], false);
    assert_eq!(s["loadProbe"], "first_rows");
    assert!(s.get("after").is_none());
    assert!(s["next"].as_str().unwrap().contains("calling again"));
}

#[test]
fn the_default_still_reads_the_newest_rows_after_loading() {
    let driver = Driver::new(day("2026년 8월 3일 월요일", "셋째 날", 20), vec![Step::Older(day("2026년 8월 2일 일요일", "둘째 날", 30))]);
    let (e, _d) = engine(&driver);
    let out = e.load_more("Fixture", 1).unwrap();
    assert_eq!(driver.st.lock().unwrap().full_reads, 1);
    assert_eq!(out["after"]["coverage"]["exposedOldestDate"], "2026-08-02");
    let s = load_more_summary(&out);
    assert_eq!(s["exposedOldestDate"], "2026-08-02");
    assert!(s["snapshotId"].is_string());
}

#[test]
fn until_date_stops_once_a_separator_on_or_before_it_shows_at_the_top() {
    let driver = Driver::new(
        day("2026년 8월 5일 수요일", "다섯째 날", 10),
        vec![
            Step::Older(day("2026년 8월 4일 화요일", "넷째 날", 30)),
            Step::Older(day("2026년 8월 2일 일요일", "둘째 날", 30)),
            Step::Older(day("2026년 8월 1일 토요일", "첫날", 30)),
        ],
    );
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 10, false, Some("2026-08-03")).unwrap();
    assert_eq!(out["loadStopReason"], "until_date");
    assert_eq!(out["untilDate"], "2026-08-03");
    assert_eq!(out["pagesAttempted"], 2, "Aug 2 at the top covers Aug 3");
    assert_eq!(out["historyComplete"], false);
    assert_eq!(out["endOfHistory"], Value::Null);

    // Already loaded: no step at all.
    let steps_before = driver.st.lock().unwrap().steps;
    let again = e.load_more_with("Fixture", 10, false, Some("2026-08-03")).unwrap();
    assert_eq!(again["loadStopReason"], "until_date");
    assert_eq!(again["pagesAttempted"], 0);
    assert_eq!(again["exposedRowsBefore"], again["exposedRowsAfter"]);
    assert_eq!(driver.st.lock().unwrap().steps, steps_before);
}

#[test]
fn a_top_without_a_separator_keeps_loading_until_the_pages_run_out() {
    // The top rows start partway through a day: no separator to compare yet.
    let driver = Driver::new(texts("오늘", 10), vec![Step::Older(texts("어제 끝", 20)), Step::Older(texts("그제 끝", 20))]);
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 2, false, Some("2026-08-03")).unwrap();
    assert_eq!(out["loadStopReason"], "page_limit");
    assert_eq!(out["exposedOldestDate"], Value::Null);
}

#[test]
fn new_messages_at_the_bottom_are_not_a_load_and_a_jump_is() {
    // A few new messages at the bottom while nothing older loads: no_new_rows (not a load).
    let driver = Driver::new(day("2026년 8월 3일 월요일", "셋째 날", 10), vec![Step::NewAtBottom(4)]);
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 10, false, None).unwrap();
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["pagesAttempted"], 1);
    assert_eq!(out["loadProbe"], "first_rows");
    assert_eq!(out["historyComplete"], false);
    assert_eq!(out["endOfHistory"], Value::Null);
    // A jump of five or more rows with the oldest row unchanged still counts as a load.
    let driver = Driver::new(day("2026년 8월 3일 월요일", "셋째 날", 10), vec![Step::NewAtBottom(6), Step::Nothing]);
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 10, false, None).unwrap();
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["pagesAttempted"], 2);
    // The default load_more judges the same way.
    let driver = Driver::new(day("2026년 8월 3일 월요일", "셋째 날", 10), vec![Step::NewAtBottom(1)]);
    let (e, _d) = engine(&driver);
    let out = e.load_more("Fixture", 3).unwrap();
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["pagesAttempted"], 1);
}

#[test]
fn until_date_finds_a_separator_below_the_first_rows() {
    // The load ends partway through Aug 2: its separator sits 30 rows down, past the first six.
    let mut older = texts("1일 끝", 30);
    older.extend(day("2026년 8월 2일 일요일", "둘째 날", 30));
    let driver = Driver::new(day("2026년 8월 5일 수요일", "다섯째 날", 10), vec![Step::Older(older), Step::Older(texts("더 앞", 30))]);
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 10, false, Some("2026-08-03")).unwrap();
    assert_eq!(out["loadStopReason"], "until_date");
    assert_eq!(out["pagesAttempted"], 1);
    assert_eq!(out["exposedOldestDate"], "2026-08-02");
}

#[test]
fn without_range_reads_progress_falls_back_to_row_counts_and_says_so() {
    let driver = Arc::new(Driver {
        st: Mutex::new(State {
            rows: day("2026년 8월 3일 월요일", "셋째 날", 10),
            script: vec![Step::Older(texts("앞", 5)), Step::NewAtBottom(1), Step::Nothing],
            full_reads: 0,
            steps: 0,
            range_reads: 0,
        }),
        ranges: false,
    });
    let (e, _d) = engine(&driver);
    let out = e.load_more_with("Fixture", 10, true, None).unwrap();
    // Any growth counts without the first rows (one new message too).
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["pagesAttempted"], 3);
    assert_eq!(out["loadProbe"], "step_row_counts");
}

#[test]
fn until_date_must_be_a_date() {
    let driver = Driver::new(texts("오늘", 3), vec![]);
    let (e, _d) = engine(&driver);
    for bad in ["2026-8-1", "2026/08/01", "2026-13-01", "어제", ""] {
        assert!(e.load_more_with("Fixture", 1, false, Some(bad)).is_err(), "{bad}");
    }
}

//! Recognising where a room's history begins (KakaoTalk Mac 26.10.0, Korean app).
//! Names and texts are fictional; the start wordings are the measured app texts.

use std::cell::RefCell;

use serde_json::{json, Value};

use kakaotalk_ax::ax_core::{ax_row, load_more_by_step_counts};
use kakaotalk_ax::macos_rows::{history_start, HistoryStart};

fn row(i: usize, parts: Vec<Value>) -> Value {
    ax_row(&json!({"rowIndex": i, "parts": parts}))
}
fn separator(i: usize) -> Value {
    row(i, vec![json!({"role": "AXButton", "AXDescription": "2026년 11월 20일 금요일"})])
}
fn lone(i: usize, text: &str) -> Value {
    row(i, vec![json!({"role": "AXTextArea", "AXValue": text})])
}
fn message(i: usize) -> Value {
    row(i, vec![
        json!({"role": "AXButton", "AXDescription": "프로필"}),
        json!({"role": "AXStaticText", "AXValue": "김가상"}),
        json!({"role": "AXTextArea", "AXValue": "안녕하세요"}),
    ])
}

const OPEN_CHAT_JOIN: &str = "가상인님이 들어왔습니다.\n\n타인, 기관 등의 사칭에 유의해 주세요. 금전 또는 개인정보를 요구 받을 경우 신고해 주시기 바랍니다.\n운영정책을 위반한 메시지로 신고 접수 시 카카오톡 이용에 제한이 있을 수 있습니다. 운영정책 보기\n\n불법촬영물 식별 및 게재제한 안내";

#[test]
fn an_open_chat_join_notice_under_the_top_separator_is_the_start() {
    let items = vec![separator(0), lone(1, OPEN_CHAT_JOIN), lone(2, "나가상님이 들어왔습니다.")];
    assert_eq!(history_start(&items, 0), HistoryStart::Confirmed("room_creation_notice", 1));
    // Either wording alone is enough.
    let items = vec![separator(0), lone(1, "가상인님이 들어왔습니다.\n\n불법촬영물 식별 및 게재제한 안내")];
    assert_eq!(history_start(&items, 0), HistoryStart::Confirmed("room_creation_notice", 1));
}

#[test]
fn a_team_chat_start_line_after_the_invitation_is_the_start() {
    // Team chats begin with the invitation and then the start line; the start line decides.
    let items = vec![
        separator(0),
        lone(1, "가상인님이 나가상님, 다가상님을 초대했습니다."),
        lone(2, "가상인님이 방장이 되어 팀채팅을 시작했어요!"),
    ];
    assert_eq!(history_start(&items, 0), HistoryStart::Confirmed("team_chat_start", 2));
    let items = vec![separator(0), lone(1, "가상인님이 방장이 되어 팀채팅을 시작했어요!")];
    assert_eq!(history_start(&items, 0), HistoryStart::Confirmed("team_chat_start", 1));
}

#[test]
fn an_invitation_on_top_is_only_a_hint_and_anything_else_is_unknown() {
    let items = vec![separator(0), lone(1, "가상인님이 나가상님과 다가상님을 초대했습니다."), message(2)];
    assert_eq!(history_start(&items, 0), HistoryStart::InviteAtTop(1));
    // A normal message on top (older history not kept on this Mac): unknown.
    assert_eq!(history_start(&[separator(0), message(1)], 0), HistoryStart::Unknown);
    // The notice must sit right under the topmost separator.
    assert_eq!(history_start(&[separator(0), message(1), lone(2, OPEN_CHAT_JOIN)], 0), HistoryStart::Unknown);
    // No separator on top.
    assert_eq!(history_start(&[lone(0, OPEN_CHAT_JOIN)], 0), HistoryStart::Unknown);
    // Someone quoting the wording in a message (profile row) is not the notice.
    let quoted = row(1, vec![
        json!({"role": "AXButton", "AXDescription": "프로필"}),
        json!({"role": "AXStaticText", "AXValue": "김가상"}),
        json!({"role": "AXTextArea", "AXValue": "타인, 기관 등의 사칭에 유의해 주세요 라고 떴어요"}),
    ]);
    assert_eq!(history_start(&[separator(0), quoted], 0), HistoryStart::Unknown);
    assert_eq!(history_start(&[], 0), HistoryStart::Unknown);
}

#[test]
fn rows_above_the_read_range_mean_the_start_is_not_known() {
    let items = vec![separator(102), lone(103, OPEN_CHAT_JOIN)];
    assert_eq!(history_start(&items, 102), HistoryStart::Unknown);
}

#[test]
fn the_team_chat_start_line_is_a_system_notice_when_centered() {
    let mut raw = json!({"rowIndex": 2, "parts": [{"role": "AXTextArea", "AXValue": "가상인님이 방장이 되어 팀채팅을 시작했어요!"}]});
    raw["centered"] = json!(true);
    let item = ax_row(&raw);
    assert_eq!(item["kind"], "system");
    assert_eq!(item["systemType"], "start");
}

/// Runs the macOS load loop with scripted step row counts and head verdicts.
fn run(pages: i64, steps: Vec<(u64, u64)>, heads: Vec<HistoryStart>) -> (Value, usize, usize) {
    let steps = RefCell::new(steps.into_iter());
    let heads = RefCell::new(heads.into_iter());
    let step_calls = RefCell::new(0usize);
    let head_calls = RefCell::new(0usize);
    let out = load_more_by_step_counts(
        "macos_accessibility",
        "가상방",
        pages,
        false,
        || Ok(json!({"status": "ok"})),
        || {
            *step_calls.borrow_mut() += 1;
            let (b, a) = steps.borrow_mut().next().expect("unexpected extra load step");
            Ok(json!({"status": "ok", "exposedRowsBefore": b, "exposedRowsAfter": a}))
        },
        || {
            *head_calls.borrow_mut() += 1;
            heads.borrow_mut().next().unwrap_or(HistoryStart::Unknown)
        },
    )
    .unwrap();
    let (s, h) = (*step_calls.borrow(), *head_calls.borrow());
    (out, s, h)
}

#[test]
fn load_more_stops_at_a_start_notice_without_asking_for_more() {
    // Already in view: no load step at all.
    let (out, steps, _) = run(10, vec![], vec![HistoryStart::Confirmed("room_creation_notice", 1)]);
    assert_eq!(steps, 0);
    assert_eq!(out["loadStopReason"], "start_of_history");
    assert_eq!(out["historyComplete"], true);
    assert_eq!(out["endOfHistory"], true);
    assert_eq!(out["historyCompleteReason"], "room_creation_notice");
    assert_eq!(out["historyStartRowIndex"], 1);
    // Appears after the second step: stops there, before the step that would load nothing.
    let (out, steps, _) = run(
        10,
        vec![(55, 300), (300, 520)],
        vec![HistoryStart::Unknown, HistoryStart::Unknown, HistoryStart::Confirmed("team_chat_start", 2)],
    );
    assert_eq!(steps, 2);
    assert_eq!(out["loadStopReason"], "start_of_history");
    assert_eq!(out["historyCompleteReason"], "team_chat_start");
}

#[test]
fn an_invitation_on_top_counts_only_after_a_second_attempt_loads_nothing() {
    let invite = || HistoryStart::InviteAtTop(1);
    let (out, steps, _) = run(10, vec![(41, 41), (41, 41)], vec![HistoryStart::Unknown, invite(), invite()]);
    assert_eq!(steps, 2);
    assert_eq!(out["historyComplete"], true);
    assert_eq!(out["historyCompleteReason"], "group_invite_at_top");
    assert_eq!(out["loadStopReason"], "start_of_history");
    // The second attempt still loads older rows: it was a later invitation, keep going.
    let (out, steps, _) = run(
        3,
        vec![(41, 41), (41, 300), (300, 300)],
        vec![HistoryStart::Unknown, invite(), HistoryStart::Unknown, HistoryStart::Unknown],
    );
    assert_eq!(steps, 3);
    assert_eq!(out["historyComplete"], false);
    assert_eq!(out["loadStopReason"], "no_new_rows");
}

#[test]
fn nothing_new_without_a_start_wording_stays_unknown() {
    let (out, steps, _) = run(10, vec![(59, 327), (327, 327)], vec![]);
    assert_eq!(steps, 2);
    assert_eq!(out["loadStopReason"], "no_new_rows");
    assert_eq!(out["historyComplete"], false);
    assert_eq!(out["endOfHistory"], Value::Null);
    assert!(out.get("historyCompleteReason").is_none());
}

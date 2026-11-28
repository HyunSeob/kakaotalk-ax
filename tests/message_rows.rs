//! Message-row facts (sender, own messages, time/date, files, notices) from macOS AX rows.
//! Row shapes follow KakaoTalk Mac 26.10.0; all names and texts are fictional.

use serde_json::{json, Value};

use kakaotalk_ax::ax_core::ax_row;
use kakaotalk_ax::macos_rows::{
    annotate_sequence, label_time, label_unread, row_shape, separator_date, system_notice, LayoutNeed,
    SenderContext,
};

fn text(v: &str) -> Value {
    json!({"role": "AXStaticText", "AXValue": v})
}
fn body(v: &str) -> Value {
    json!({"role": "AXTextArea", "AXValue": v})
}
fn profile() -> Value {
    json!({"role": "AXButton", "AXDescription": "프로필"})
}
fn row(i: usize, parts: Vec<Value>) -> Value {
    json!({"rowIndex": i, "parts": parts})
}
fn with(mut r: Value, key: &str, v: bool) -> Value {
    r[key] = json!(v);
    r
}
fn separator(i: usize, label: &str) -> Value {
    row(i, vec![json!({"role": "AXButton", "AXDescription": label})])
}

fn parse(rows: &[Value]) -> Vec<Value> {
    let mut items: Vec<Value> = rows.iter().map(ax_row).collect();
    annotate_sequence(rows, &mut items);
    items
}

#[test]
fn labels_parse_time_unread_dates_and_notices() {
    assert_eq!(label_time("오후 10:48, 안읽음 5"), Some("오후 10:48"));
    assert_eq!(label_time("오전 9:05"), Some("오전 9:05"));
    assert_eq!(label_time("안읽음 5"), None);
    assert_eq!(label_unread("오후 10:48, 안읽음 5"), Some(5));
    assert_eq!(label_unread("안읽음 12"), Some(12));
    assert_eq!(label_unread("오후 10:48"), None);
    assert_eq!(separator_date("2026년 11월 23일 월요일").as_deref(), Some("2026-11-23"));
    assert_eq!(separator_date("2026년 8월 1일 토요일").as_deref(), Some("2026-08-01"));
    assert_eq!(separator_date("가상 버튼"), None);

    assert_eq!(system_notice("가상인님이 들어왔습니다."), Some(("join", Some("가상인".to_string()))));
    assert_eq!(system_notice("가상인님이 나갔습니다."), Some(("leave", Some("가상인".to_string()))));
    assert_eq!(system_notice("방장이 가상인님을 내보냈습니다."), Some(("kick", Some("가상인".to_string()))));
    assert_eq!(system_notice("가상인님이 부방장이 되었습니다."), Some(("role", Some("가상인".to_string()))));
    assert_eq!(system_notice("관리자가 메시지를 가렸습니다."), Some(("hidden", None)));
    assert_eq!(system_notice("메시지가 삭제되었습니다."), Some(("deleted", None)));
    assert_eq!(system_notice("공지가 등록되었습니다"), Some(("notice", None)));
    assert_eq!(system_notice("오늘 수업 자료입니다"), None);
}

#[test]
fn a_profile_row_gives_sender_time_and_unread() {
    let item = ax_row(&row(0, vec![profile(), text("김가상"), body("https://example.com/v"), text("오후 10:48, 안읽음 5")]));
    assert_eq!(item["sender"], "김가상");
    assert_eq!(item["timeText"], "오후 10:48");
    assert_eq!(item["unreadCount"], 5);
    assert_eq!(item["kind"], "text");
    assert_eq!(item["text"], "https://example.com/v");
    assert_eq!(item["edited"], false);
    assert!(item["axParts"].is_array(), "existing fields stay");
}

#[test]
fn an_edited_label_is_neither_the_name_nor_the_body() {
    let item = ax_row(&row(0, vec![profile(), text("김가상"), body("고친 말"), text("수정됨"), text("오후 1:02")]));
    assert_eq!(item["sender"], "김가상");
    assert_eq!(item["text"], "고친 말");
    assert_eq!(item["edited"], true);
    // A follow-up row whose only static text besides the time is the edited label.
    let item = ax_row(&row(1, vec![body("또 고침"), text("수정됨"), text("오후 1:03")]));
    assert_eq!(item["sender"], Value::Null);
    assert_eq!(item["edited"], true);
}

#[test]
fn a_file_row_becomes_kind_file_with_name_size_and_expiry() {
    let item = ax_row(&row(0, vec![
        profile(),
        text("김가상"),
        json!({"role": "AXGroup", "AXDescription": "파일"}),
        text("가상 자료집 03.pdf"),
        text("유효기간: ~2026. 12. 8."),
        text("용량: 516.5KB"),
        json!({"role": "AXButton", "AXDescription": "저장"}),
        json!({"role": "AXButton", "AXDescription": "다른 이름으로 저장"}),
        text("안읽음 5"),
    ]));
    assert_eq!(item["kind"], "file");
    assert_eq!(item["file"], json!({"name": "가상 자료집 03.pdf", "size": "516.5KB", "expiresAt": "2026-12-08"}));
    assert_eq!(item["sender"], "김가상");
    assert_eq!(item["unreadCount"], 5);
    assert_eq!(item["timeText"], Value::Null);
}

#[test]
fn a_cut_long_message_is_marked_truncated() {
    let cut: String = "가".repeat(500) + "…";
    assert_eq!(ax_row(&row(0, vec![body(&cut), text("오후 2:00")]))["truncated"], true);
    // Emoji take two UTF-16 units and one starting before unit 500 is kept whole (to 503).
    let emoji_cut: String = "가".repeat(499) + "👍🏻…";
    assert_eq!(ax_row(&row(0, vec![body(&emoji_cut)]))["truncated"], true);
    let short_cut: String = "가".repeat(498) + "…";
    assert_eq!(ax_row(&row(0, vec![body(&short_cut)]))["truncated"], false);
    assert_eq!(ax_row(&row(0, vec![body("짧은 말…"), text("오후 2:00")]))["truncated"], false);
}

#[test]
fn notices_need_centered_layout_and_separators_carry_the_date() {
    let rows = vec![
        separator(0, "2026년 8월 1일 토요일"),
        with(row(1, vec![body("가상인님이 들어왔습니다.")]), "centered", true),
        // Same wording in a follow-up bubble (not centered) is an ordinary message.
        row(2, vec![profile(), text("김가상"), body("집에 왔어요")]),
        with(row(3, vec![body("저 방금 집에 들어왔습니다")]), "centered", false),
    ];
    let items = parse(&rows);
    assert_eq!(items[0]["kind"], "date_separator");
    assert_eq!(items[0]["date"], "2026-08-01");
    assert_eq!(items[1]["kind"], "system");
    assert_eq!(items[1]["systemType"], "join");
    assert_eq!(items[1]["target"], "가상인");
    assert_eq!(items[1]["date"], "2026-08-01");
    assert_eq!(items[3]["kind"], "text");
    assert_eq!(items[3]["groupSender"], "김가상");
    assert_eq!(items[3]["senderSource"], "inherited");
}

/// The 2026-08-01 example: a friend, then the user (no name), then the friend again.
#[test]
fn own_messages_and_follow_ups_get_sender_facts_and_group_times() {
    let rows = vec![
        separator(0, "2026년 8월 1일 토요일"),
        row(1, vec![profile(), text("김가상"), body("19일로 가자")]),
        row(2, vec![body("어때")]),
        row(3, vec![body("다들"), text("오후 3:10")]),
        // Starts a group without a name: the driver measured it right aligned.
        with(row(4, vec![body("8이나 9일 어떠누")]), "ownSide", true),
        row(5, vec![body("나는 둘 다 돼"), text("오후 3:11")]),
        row(6, vec![profile(), text("김가상"), body("가능은 하긴 하지"), text("오후 3:12, 안읽음 1")]),
        // Someone's photo right after the friend's timed row: not right aligned.
        with(row(7, vec![json!({"role": "AXImage", "AXDescription": "사진"}), text("오후 3:12")]), "ownSide", false),
        row(8, vec![]),
    ];
    let items = parse(&rows);
    let facts = |i: usize| {
        (
            items[i]["isSelf"].clone(),
            items[i]["groupSender"].clone(),
            items[i]["senderSource"].clone(),
            items[i]["groupTimeText"].clone(),
        )
    };
    assert_eq!(facts(1), (json!(false), json!("김가상"), json!("row"), json!("오후 3:10")));
    assert_eq!(facts(2), (json!(false), json!("김가상"), json!("inherited"), json!("오후 3:10")));
    assert_eq!(facts(3), (json!(false), json!("김가상"), json!("inherited"), json!("오후 3:10")));
    assert_eq!(facts(4), (json!(true), Value::Null, json!("self_layout"), json!("오후 3:11")));
    assert_eq!(facts(5), (json!(true), Value::Null, json!("inherited"), json!("오후 3:11")));
    assert_eq!(facts(6), (json!(false), json!("김가상"), json!("row"), json!("오후 3:12")));
    // Not right aligned but nameless after a finished group: someone else, sender unknown.
    assert_eq!(facts(7), (json!(false), Value::Null, json!("not_own_layout"), json!("오후 3:12")));
    assert_eq!(items[2]["sender"], Value::Null, "sender stays what the row shows");
    assert_eq!(items[2]["timeText"], Value::Null);
    for i in 1..8 {
        assert_eq!(items[i]["date"], "2026-08-01");
        assert_eq!(items[i]["dateText"], "2026년 8월 1일 토요일");
    }
}

#[test]
fn without_layout_or_context_the_sender_stays_unknown() {
    // A name-less first row with no measured layout, and a group never closed by a time.
    let rows = vec![row(0, vec![body("앞부분이 잘린 묶음")]), row(1, vec![body("끝")])];
    let items = parse(&rows);
    assert_eq!(items[0]["isSelf"], Value::Null);
    assert_eq!(items[0]["senderSource"], Value::Null);
    assert_eq!(items[1]["isSelf"], Value::Null);
    assert_eq!(items[0]["date"], Value::Null);
    assert_eq!(items[0]["groupTimeText"], Value::Null);
}

#[test]
fn the_driver_measures_only_group_starts_and_notice_candidates() {
    let mut ctx = SenderContext::default();
    let shape = |parts: Vec<Value>| row_shape(&parts);
    let first = shape(vec![body("첫 줄")]);
    assert_eq!(ctx.need(&first), LayoutNeed::OwnSide);
    ctx.push(&first, None);
    let follow = shape(vec![body("이어서"), text("오후 1:00")]);
    assert_eq!(ctx.need(&follow), LayoutNeed::None, "a follow-up continues its sender");
    ctx.push(&follow, None);
    let after_time = shape(vec![body("새 묶음")]);
    assert_eq!(ctx.need(&after_time), LayoutNeed::OwnSide);
    let named = shape(vec![profile(), text("김가상"), body("안녕")]);
    assert_eq!(ctx.need(&named), LayoutNeed::None);
    let notice = shape(vec![body("가상인님이 나갔습니다.")]);
    // Where a group may start, the same frame tells a notice from the user's own bubble.
    assert_eq!(ctx.need(&notice), LayoutNeed::Both);
    let mut follow_ctx = SenderContext::default();
    follow_ctx.push(&shape(vec![profile(), text("김가상"), body("안녕")]), None);
    assert_eq!(follow_ctx.need(&notice), LayoutNeed::Centered);
    let sep = shape(vec![json!({"role": "AXButton", "AXDescription": "2026년 8월 1일 토요일"})]);
    assert_eq!(ctx.need(&sep), LayoutNeed::None);
    ctx.push(&sep, None);
    assert_eq!(ctx.need(&shape(vec![body("날짜 뒤")])), LayoutNeed::OwnSide);
}

#[test]
fn an_own_group_starting_with_notice_wording_is_still_the_users() {
    let rows = vec![
        row(0, vec![profile(), text("김가상"), body("확인 부탁해요"), text("오후 4:00")]),
        // "…되었습니다" reads like a notice, but the bubble is right aligned, not centered.
        with(with(row(1, vec![body("저는 준비가 완료되었습니다")]), "centered", false), "ownSide", true),
        row(2, vec![body("바로 갈게요"), text("오후 4:01")]),
    ];
    let items = parse(&rows);
    assert_eq!(items[1]["kind"], "text");
    assert_eq!((items[1]["isSelf"].clone(), items[1]["senderSource"].clone()), (json!(true), json!("self_layout")));
    assert_eq!((items[2]["isSelf"].clone(), items[2]["senderSource"].clone()), (json!(true), json!("inherited")));
    assert_eq!(items[1]["groupTimeText"], "오후 4:01");
}

#[test]
fn layout_verdicts_do_not_change_the_snapshot_id() {
    use kakaotalk_ax::ax_core::snapshot_digest;
    let raw = |own: Option<bool>| {
        let mut r = row(0, vec![body("안녕")]);
        if let Some(v) = own {
            r["ownSide"] = json!(v);
        }
        json!({"status": "ok", "windowTitle": "가상방", "processId": 1, "rows": [r], "exposedRowCount": 1, "rowStart": 0})
    };
    assert_eq!(snapshot_digest(&raw(None)), snapshot_digest(&raw(Some(true))));
    assert_eq!(snapshot_digest(&raw(Some(false))), snapshot_digest(&raw(Some(true))));
}

#[test]
fn a_display_name_that_reads_like_metadata_is_only_the_name() {
    // Display names are user-controlled: "수정됨", "12:30" or "안읽음 3" are names here.
    let edited = ax_row(&row(0, vec![profile(), text("수정됨"), body("안녕")]));
    assert_eq!(edited["sender"], "수정됨");
    assert_eq!(edited["edited"], false);
    let unread = ax_row(&row(0, vec![profile(), text("안읽음 3"), body("안녕")]));
    assert_eq!(unread["sender"], "안읽음 3");
    assert_eq!(unread["unreadCount"], Value::Null);
    let timed = ax_row(&row(0, vec![profile(), text("12:30"), body("안녕"), text("오후 1:02")]));
    assert_eq!(timed["sender"], "12:30");
    assert_eq!(timed["timeText"], "오후 1:02");

    // A time-like name does not end the group: the next name-less row still inherits it.
    let rows = vec![
        row(0, vec![profile(), text("12:30"), body("먼저 갈게")]),
        row(1, vec![body("이따 봐"), text("오후 1:02")]),
    ];
    assert_eq!(row_shape(rows[0]["parts"].as_array().unwrap()).has_time, false);
    let items = parse(&rows);
    assert_eq!(items[0]["timeText"], Value::Null);
    assert_eq!(items[1]["groupSender"], "12:30");
    assert_eq!(items[1]["senderSource"], "inherited");
    assert_eq!(items[0]["groupTimeText"], "오후 1:02");
}

#[test]
fn a_file_name_that_reads_like_metadata_is_only_the_file_name() {
    let file_row = |name: &str, tail: Vec<Value>| {
        let mut parts = vec![
            json!({"role": "AXGroup", "AXDescription": "파일"}),
            text(name),
            text("유효기간: ~2026. 12. 8."),
            text("용량: 1KB"),
        ];
        parts.extend(tail);
        row(1, parts)
    };
    let edited = ax_row(&file_row("수정됨", vec![]));
    assert_eq!(edited["file"]["name"], "수정됨");
    assert_eq!(edited["edited"], false);
    let unread = ax_row(&file_row("안읽음 3", vec![]));
    assert_eq!(unread["unreadCount"], Value::Null);
    let timed = ax_row(&file_row("12:30", vec![text("오후 1:02")]));
    assert_eq!(timed["timeText"], "오후 1:02");

    // A time-like file name mid-group does not end the group.
    let rows = vec![
        row(0, vec![profile(), text("김가상"), body("자료 보낼게")]),
        file_row("12:30", vec![]),
        row(2, vec![body("받았어?"), text("오후 1:05")]),
    ];
    let items = parse(&rows);
    assert_eq!(items[1]["timeText"], Value::Null);
    assert_eq!(items[2]["groupSender"], "김가상");
    assert_eq!(items[0]["groupTimeText"], "오후 1:05");
}

mod snapshot_counts {
    use super::*;
    use kakaotalk_ax::ax_core::{AxDriver, AxEngine, KakaoBackend};
    use kakaotalk_ax::error::Result;
    use serde_json::Map;
    use std::sync::Arc;

    struct NoticeDriver;

    impl AxDriver for NoticeDriver {
        fn call(&self, action: &str, titles: &[String], _text: Option<&str>) -> Result<Value> {
            assert!(matches!(action, "read" | "read_all"), "unexpected {action}");
            Ok(json!({
                "status": "ok",
                "windowTitle": titles[0],
                "processId": 1,
                "rows": [
                    {"rowIndex": 0, "parts": [{"role": "AXTextArea", "AXValue": "안녕"}]},
                    {"rowIndex": 1, "centered": true,
                     "parts": [{"role": "AXTextArea", "AXValue": "김가상님이 들어왔습니다."}]},
                    {"rowIndex": 2, "parts": [{"role": "AXButton", "AXDescription": "2026년 8월 1일 토요일"}]},
                    {"rowIndex": 3, "parts": [
                        {"role": "AXGroup", "AXDescription": "파일"},
                        {"role": "AXStaticText", "AXValue": "가상 자료.pdf"},
                        {"role": "AXStaticText", "AXValue": "용량: 1KB"}]},
                ],
                "exposedRowCount": 4,
                "rowStart": 0,
                "rowsTruncated": false,
                "elapsedMs": 1,
            }))
        }
        fn control(&self, _action: &str, _args: &Map<String, Value>) -> Result<Value> {
            unreachable!()
        }
        fn capture_attachment(&self, _: &str, _: i64, _: &[Value], _: usize, _: bool) -> Result<Value> {
            unreachable!()
        }
    }

    #[test]
    fn counts_keep_their_meaning_from_before_row_facts() {
        // Send journals written by older versions compare `textMessageCount` across reads, so
        // a notice row still counts as text and a separator as unclassified.
        let dir = tempfile::tempdir().unwrap();
        let engine =
            AxEngine::with_roots(Arc::new(NoticeDriver), dir.path().join("ui"), dir.path().join("operations"));
        let out = engine.read_messages("Fixture", 50, 0, "").unwrap();
        assert_eq!(out["items"][1]["kind"], "system");
        assert_eq!(out["items"][2]["kind"], "date_separator");
        assert_eq!(out["items"][3]["kind"], "file");
        assert_eq!(out["textMessageCount"], 2);
        assert_eq!(out["unclassifiedRowCount"], 2);
        assert_eq!(out["attachmentRowCount"], 0);
    }
}

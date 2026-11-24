use std::collections::HashSet;

use kakaotalk_ax::win32_attachment::{
    candidate_rows, photo_boxes, scrollbar_edge, selected_ranges, selection_overlay,
};
use kakaotalk_ax::win32_image::Frame;
use kakaotalk_ax::win32_rows::{
    badge_present, blank_chat, room_items, self_badge_present, transcript_rows, OcrLine,
};

fn make_frame(width: usize, height: usize, rects: &[(usize, usize, usize, usize, (u8, u8, u8))]) -> Frame {
    let mut data = Vec::with_capacity(width * height * 4);
    for _ in 0..(width * height) {
        data.extend_from_slice(&[220, 205, 185, 0]);
    }
    for &(left, top, right, bottom, (b, g, r)) in rects {
        for y in top..bottom {
            for x in left..right {
                let i = (y * width + x) * 4;
                data[i] = b;
                data[i + 1] = g;
                data[i + 2] = r;
                data[i + 3] = 0;
            }
        }
    }
    Frame::new(width, height, data)
}

#[test]
fn keeps_utf16_content_and_record_separators_distinct() {
    let body = "기록 🧪 \n[이름] [오후 1:02] 본문 내부\n  ";
    let raw = format!(
        "2026년 11월 22일 일요일\r\n[작성] [오전 9:01] {body}\r\n[작성] [오전 9:01] 반복\r\n[작성] [오전 9:01] 반복\r\n"
    );
    let rows = transcript_rows(&raw);
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["date", "text", "text", "text"]);
    assert_eq!(rows[1]["sender"], "작성");
    assert_eq!(rows[1]["timeText"], "오전 9:01");
    assert_eq!(rows[1]["dateText"], "2026년 11월 22일 일요일");
    assert_eq!(rows[1]["text"], body);
    assert_eq!(rows[2]["text"], "반복");
    assert_eq!(rows[3]["text"], "반복");
    let indices: Vec<u64> = rows.iter().map(|r| r["rowIndex"].as_u64().unwrap()).collect();
    assert_eq!(indices, vec![0, 1, 2, 3]);
}

#[test]
fn attachments_and_system_rows_keep_their_original_text() {
    let rows = transcript_rows(
        "2026년 11월 22일 일요일\r\n누군가님이 들어왔습니다.\r\n\
         [A] [오후 1:20] 사진\r\n[A] [오후 1:20] 파일: 드래프트.pdf\r\n\
         [A] [오후 1:21] 이모티콘\r\n2026년 11월 23일 월요일\r\n[A] [오전 1:00] 원문\r\n",
    );
    assert_eq!(rows[1]["kind"], "unclassified");
    assert_eq!(rows[1]["text"], "누군가님이 들어왔습니다.");
    let att_types: Vec<&str> = rows[2..5]
        .iter()
        .map(|r| r["attachments"][0]["type"].as_str().unwrap())
        .collect();
    assert_eq!(att_types, vec!["image", "file", "emoticon"]);
    assert_eq!(rows[3]["text"], "파일: 드래프트.pdf");
    assert_eq!(rows.last().unwrap()["dateText"], "2026년 11월 23일 월요일");
    assert!(transcript_rows("").is_empty());
}

#[test]
fn photo_marker_with_matching_context_disambiguates_duplicate_photos() {
    let raw = "[A] [오후 1:00] First\r\n[A] [오후 1:01] 사진\r\n\
               [A] [오후 1:01] 사진\r\n[A] [오후 1:02] Last\r\n";
    let rows = transcript_rows(raw);
    assert_eq!(candidate_rows(&rows, "사진", false), HashSet::from([1, 2]));
    assert_eq!(
        candidate_rows(&rows, "[A] [오후 1:00] First\r\n[A] [오후 1:01] 사진\r\n", true),
        HashSet::from([1])
    );
    assert_eq!(
        candidate_rows(&rows, "[A] [오후 1:01] 사진\r\n[A] [오후 1:02] Last\r\n", false),
        HashSet::from([2])
    );
}

#[test]
fn room_ocr_runs_on_row_crops_and_unread_is_pixel_derived() {
    let picture = make_frame(
        325,
        140,
        &[
            (20, 9, 69, 61, (0, 0, 0)),
            (20, 79, 69, 131, (0, 0, 0)),
            (275, 42, 290, 57, (100, 120, 244)),
        ],
    );
    let mut recognized = Vec::new();
    let items = room_items(
        &picture,
        |row| {
            recognized.push((row.width, row.height));
            vec![
                OcrLine { text: "방 이름".into(), x: 4.0, y: 35.0, width: 94.0, height: 20.0 },
                OcrLine { text: "미리 보기".into(), x: 4.0, y: 77.0, width: 94.0, height: 20.0 },
            ]
        },
        96,
    );
    assert_eq!(recognized.len(), 2);
    assert!(recognized.iter().all(|&(w, h)| w > 400 && h >= 135));
    assert_eq!(items.iter().map(|i| i.row_index).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(items.iter().map(|i| i.unread).collect::<Vec<_>>(), vec![true, false]);
    assert_eq!(items[0].preview.as_deref(), Some("미리 보기"));
    assert_eq!(items[0].to_public_json()["textExact"], false);
    assert_eq!(items[0].to_public_json()["source"], "windows_ocr");
    assert!(!badge_present(&picture, 70, 140, 96));
}

#[test]
fn photo_box_excludes_timestamp_and_small_bubbles() {
    let picture = make_frame(
        380,
        419,
        &[
            (160, 100, 352, 232, (20, 110, 200)),
            (108, 222, 154, 229, (10, 10, 10)),
            (220, 44, 347, 78, (4, 222, 250)),
        ],
    );
    assert_eq!(photo_boxes(&picture, 96), vec![(160, 100, 352, 232)]);
    let png = picture.crop(160, 100, 352, 232).unwrap().png().unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(!blank_chat(&picture));
    assert!(blank_chat(&make_frame(380, 419, &[])));
}

#[test]
fn select_all_bands_bind_photo_to_chronological_row() {
    let clean = make_frame(380, 280, &[(160, 100, 352, 232, (20, 110, 200))]);
    let marked = make_frame(
        380,
        280,
        &[
            (10, 6, 360, 44, (152, 140, 127)),
            (151, 55, 360, 94, (152, 140, 127)),
            (105, 97, 359, 236, (152, 140, 127)),
            (160, 100, 352, 232, (13, 75, 136)),
        ],
    );
    assert_eq!(selected_ranges(&clean, &marked), vec![(6, 44), (55, 94), (97, 236)]);
    assert!(selection_overlay(&clean).is_empty());
    assert_eq!(selection_overlay(&marked), vec![(6, 44), (55, 94), (97, 236)]);
    assert_eq!(photo_boxes(&clean, 96), vec![(160, 100, 352, 232)]);
}

#[test]
fn scrollbar_anchor_requires_thumb_at_an_exclusive_edge() {
    let thumb = (179, 165, 149);
    for (start, end, expected) in [
        (2, 176, Some("top")),
        (67, 241, Some("bottom")),
        (25, 199, None),
        (2, 241, None),
    ] {
        let pic = make_frame(378, 243, &[(368, start, 374, end, thumb)]);
        assert_eq!(scrollbar_edge(&pic, 96), expected);
    }
}

#[test]
fn self_badge_detected_and_masked_before_ocr_without_false_positives() {
    let picture = make_frame(
        325,
        140,
        &[
            (20, 9, 60, 61, (0, 0, 0)),
            (80, 19, 95, 33, (92, 82, 74)),
            (84, 23, 86, 29, (255, 255, 255)),
            (89, 23, 91, 29, (255, 255, 255)),
            (20, 79, 60, 131, (0, 0, 0)),
            (82, 91, 93, 102, (15, 15, 15)),
        ],
    );
    assert!(self_badge_present(&picture, 0, 70, 96));
    assert!(!self_badge_present(&picture, 70, 140, 96));

    let mut crops = Vec::new();
    let items = room_items(
        &picture,
        |scaled| {
            crops.push(scaled.clone());
            if crops.len() == 1 {
                vec![
                    OcrLine { text: "0 내 프로필".into(), x: 42.0, y: 35.0, width: 90.0, height: 20.0 },
                    OcrLine { text: "테스트 미리보기".into(), x: 4.0, y: 78.0, width: 120.0, height: 20.0 },
                ]
            } else {
                vec![
                    OcrLine { text: "0 일반방".into(), x: 4.0, y: 35.0, width: 90.0, height: 20.0 },
                    OcrLine { text: "일반 미리보기".into(), x: 4.0, y: 78.0, width: 120.0, height: 20.0 },
                ]
            }
        },
        96,
    );
    assert_eq!(crops[0].pixel(16, 48), (220, 205, 185));
    assert_eq!(items[0].name, "내 프로필");
    assert_eq!(items[0].is_self, Some(true));
    assert_eq!(items[0].preview.as_deref(), Some("테스트 미리보기"));
    assert_eq!(items[1].name, "0 일반방");
    assert_eq!(items[1].is_self, None);
}

import unittest

from kakaotalk_win32_attachment import candidate_rows, photo_boxes, scrollbar_edge, selected_ranges, selection_overlay
from kakaotalk_win32_image import Frame
from kakaotalk_win32_ocr import Line
from kakaotalk_win32_rows import badge_present, blank_chat, room_items, transcript_rows


class ClipboardRowsTests(unittest.TestCase):
    def test_keeps_utf16_content_and_record_separators_distinct(self):
        body = "기록 🧪 \n[이름] [오후 1:02] 본문 내부\n  "
        raw = "2026년 11월 22일 일요일\r\n[작성] [오전 9:01] " + body + "\r\n"
        raw += "[작성] [오전 9:01] 반복\r\n[작성] [오전 9:01] 반복\r\n"
        rows = transcript_rows(raw)
        self.assertEqual([r["kind"] for r in rows], ["date", "text", "text", "text"])
        self.assertEqual(rows[1]["sender"], "작성")
        self.assertEqual(rows[1]["timeText"], "오전 9:01")
        self.assertEqual(rows[1]["dateText"], "2026년 11월 22일 일요일")
        self.assertEqual(rows[1]["text"], body)
        self.assertEqual([r["text"] for r in rows[-2:]], ["반복", "반복"])
        self.assertEqual([r["rowIndex"] for r in rows], [0, 1, 2, 3])

    def test_attachments_and_system_rows_keep_their_original_text(self):
        rows = transcript_rows(
            "2026년 11월 22일 일요일\r\n누군가님이 들어왔습니다.\r\n"
            "[A] [오후 1:20] 사진\r\n[A] [오후 1:20] 파일: 드래프트.pdf\r\n"
            "[A] [오후 1:21] 이모티콘\r\n2026년 11월 23일 월요일\r\n[A] [오전 1:00] 원문\r\n")
        self.assertEqual(rows[1]["kind"], "unclassified")
        self.assertEqual(rows[1]["text"], "누군가님이 들어왔습니다.")
        self.assertEqual([r["attachments"][0]["type"] for r in rows[2:5]], ["image", "file", "emoticon"])
        self.assertEqual(rows[3]["text"], "파일: 드래프트.pdf")
        self.assertEqual(rows[-1]["dateText"], "2026년 11월 23일 월요일")
        self.assertEqual(transcript_rows(""), [])

    def test_photo_marker_with_matching_context_disambiguates_duplicate_photos(self):
        raw = "[A] [오후 1:00] First\r\n[A] [오후 1:01] 사진\r\n"
        raw += "[A] [오후 1:01] 사진\r\n[A] [오후 1:02] Last\r\n"
        rows = transcript_rows(raw)
        self.assertEqual(candidate_rows(rows, "사진"), {1, 2})
        self.assertEqual(candidate_rows(rows, "[A] [오후 1:00] First\r\n[A] [오후 1:01] 사진\r\n", last=True), {1})
        self.assertEqual(candidate_rows(rows, "[A] [오후 1:01] 사진\r\n[A] [오후 1:02] Last\r\n"), {2})


def frame(width, height, rectangles=()):
    data = bytearray(bytes((220, 205, 185, 0)) * (width * height))
    for left, top, right, bottom, color in rectangles:
        for y in range(top, bottom):
            data[(y*width+left)*4:(y*width+right)*4] = bytes((*color, 0))*(right-left)
    return Frame(width, height, bytes(data))


class CapturedRowsTests(unittest.TestCase):
    def test_room_ocr_runs_on_row_crops_and_unread_is_pixel_derived(self):
        picture = frame(325, 140, ((20, 9, 69, 61, (0, 0, 0)),
                                   (20, 79, 69, 131, (0, 0, 0)),
                                   (275, 42, 290, 57, (100, 120, 244))))
        recognized = []

        def recognize(row):
            recognized.append((row.width, row.height))
            return [Line("방 이름", 4, 35, 94, 20), Line("미리 보기", 4, 77, 94, 20)]

        items = room_items(picture, recognize)
        self.assertEqual(len(recognized), 2)
        self.assertTrue(all(w > 400 and h >= 135 for w, h in recognized))
        self.assertEqual([item["rowIndex"] for item in items], [0, 1])
        self.assertEqual([item["unread"] for item in items], [True, False])
        self.assertEqual(items[0]["preview"], "미리 보기")
        self.assertFalse(items[0]["textExact"])
        self.assertEqual(items[0]["source"], "windows_ocr")
        self.assertFalse(badge_present(picture, 70, 140, 96))

    def test_photo_box_excludes_timestamp_and_small_bubbles(self):
        picture = frame(380, 419, ((160, 100, 352, 232, (20, 110, 200)),
                                   (108, 222, 154, 229, (10, 10, 10)),
                                   (220, 44, 347, 78, (4, 222, 250))))
        self.assertEqual(photo_boxes(picture, 96), [(160, 100, 352, 232)])
        self.assertTrue(picture.crop(160, 100, 352, 232).png().startswith(b"\x89PNG\r\n\x1a\n"))
        self.assertFalse(blank_chat(picture))
        self.assertTrue(blank_chat(frame(380, 419)))

    def test_select_all_bands_bind_photo_to_chronological_row(self):
        clean = frame(380, 280, ((160, 100, 352, 232, (20, 110, 200)),))
        marked = frame(380, 280, ((10, 6, 360, 44, (152, 140, 127)),
                                  (151, 55, 360, 94, (152, 140, 127)),
                                  (105, 97, 359, 236, (152, 140, 127)),
                                  (160, 100, 352, 232, (13, 75, 136))))
        self.assertEqual(selected_ranges(clean, marked), [(6, 44), (55, 94), (97, 236)])
        self.assertEqual(selection_overlay(clean), [])
        self.assertEqual(selection_overlay(marked), [(6, 44), (55, 94), (97, 236)])
        self.assertEqual(photo_boxes(clean, 96), [(160, 100, 352, 232)])

    def test_scrollbar_anchor_requires_thumb_at_an_exclusive_edge(self):
        thumb = (179, 165, 149)
        for start, end, edge in ((2, 176, "top"), (67, 241, "bottom"), (25, 199, None), (2, 241, None)):
            pic = frame(378, 243, ((368, start, 374, end, thumb),))
            self.assertEqual(scrollbar_edge(pic, 96), edge)

    def test_self_badge_detected_and_masked_before_ocr_without_false_positives(self):
        from kakaotalk_win32_rows import self_badge_present
        # Row 0: self-chat circular slate-grey badge (BGR 92, 82, 74) with white '나' glyph strokes inside.
        # Row 1: normal chat row with dark text strokes at the same x-offset.
        picture = frame(325, 140, ((20, 9, 60, 61, (0, 0, 0)),
                                   (80, 19, 95, 33, (92, 82, 74)),
                                   (84, 23, 86, 29, (255, 255, 255)),
                                   (89, 23, 91, 29, (255, 255, 255)),
                                   (20, 79, 60, 131, (0, 0, 0)),
                                   (82, 91, 93, 102, (15, 15, 15))))
        self.assertTrue(self_badge_present(picture, 0, 70, 96))
        self.assertFalse(self_badge_present(picture, 70, 140, 96))
        crops = []

        def recognize(scaled):
            crops.append(scaled)
            if len(crops) == 1:
                return [Line("0 내 프로필", 42, 35, 90, 20), Line("테스트 미리보기", 4, 78, 120, 20)]
            return [Line("0 일반방", 4, 35, 90, 20), Line("일반 미리보기", 4, 78, 120, 20)]

        items = room_items(picture, recognize)
        # Badge pixels in row 0 crop (scaled 2x: x=4..34, y=38..66) must be masked to background (220, 205, 185).
        self.assertEqual(crops[0].pixel(16, 48), (220, 205, 185))
        self.assertEqual(items[0]["name"], "내 프로필")
        self.assertTrue(items[0]["isSelf"])
        self.assertEqual(items[0]["preview"], "테스트 미리보기")
        # Normal row starting with literal "0 " keeps its title and isSelf=None.
        self.assertEqual(items[1]["name"], "0 일반방")
        self.assertIsNone(items[1]["isSelf"])


if __name__ == "__main__":
    unittest.main()

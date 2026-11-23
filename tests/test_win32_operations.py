from contextlib import nullcontext
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from kakaotalk_win32 import WindowsAX
from kakaotalk_win32_api import Blocked, Window, WM_CLOSE
from kakaotalk_win32_ocr import Line
from kakaotalk_win32_rows import transcript_rows
from test_win32_rows import frame


NAME = "테스트 방 🧪"
OTHER = "다른 방"
INITIAL = "2026년 11월 22일 일요일\r\n[A] [오후 1:00] 기존 원문\r\n"


class Desktop:
    def __init__(self):
        self.user = self
        self.root_windows = {10: Window(10, "EVA_Window_Dblclk", "카카오톡", 0)}
        self.nodes = {10: [Window(100, "EVA_ChildWindow", "OnlineMainView_1", 132),
                           Window(101, "EVA_ChildWindow_Dblclk", "LockModeView_1", 183)]}
        self.hidden = {101}
        self.disabled = set()
        self.owners = {}
        self.contents = {}
        self.transcripts = {}
        self.clicked = []
        self.closed = []
        self.delivered = True
        self.inactive = False
        self.input = 0
        self.foreground = 10
        self.add_chat(20, NAME)
        self.add_chat(30, OTHER)

    def add_chat(self, hwnd, name):
        self.root_windows[hwnd] = Window(hwnd, "EVA_Window_Dblclk", name, 0)
        self.nodes[hwnd] = [Window(hwnd*10, "RICHEDIT50W", "", 1006),
                            Window(hwnd*10+1, "EVA_VH_ListControl_Dblclk", "", 100),
                            Window(hwnd*10+2, "Edit", "", 100)]
        self.contents[hwnd*10] = ""
        self.transcripts[hwnd] = INITIAL

    def handles(self):
        return self.root_windows.keys()

    def window(self, hwnd):
        return self.root_windows[hwnd]

    def class_name(self, hwnd):
        return self.window(hwnd).cls

    def title(self, hwnd):
        return self.window(hwnd).name

    def pid(self, hwnd):
        return 42

    def process(self, pid):
        return "kakaotalk.exe", 123

    def children(self, hwnd):
        return self.nodes.get(hwnd, [])

    def control_text(self, hwnd):
        return self.contents.get(hwnd, "")

    def set_text(self, hwnd, text):
        self.contents[hwnd] = text

    replace_edit = set_text

    def dpi_aware(self):
        return nullcontext()

    def GetParent(self, hwnd):
        return hwnd // 10

    def IsWindowVisible(self, hwnd):
        return hwnd not in self.hidden

    def IsWindowEnabled(self, hwnd):
        return hwnd not in self.disabled

    def IsWindow(self, hwnd):
        return hwnd in self.root_windows or any(
            child.hwnd == hwnd for parent in self.root_windows for child in self.nodes.get(parent, ())
        )

    def IsIconic(self, hwnd):
        return False

    def GetWindow(self, hwnd, flag):
        return self.owners.get(hwnd)

    def GetDpiForWindow(self, hwnd):
        return 96

    def GetForegroundWindow(self):
        return self.foreground

    def idle(self):
        if self.inactive:
            raise Blocked("user_input_active")
        return self.input

    def last_input(self):
        return self.input

    def rect(self, hwnd, client=False):
        return (14, 532, 379, 593) if hwnd >= 200 else (0, 0, 380, 640)

    def point(self, hwnd, x, y, *, to_screen=False):
        return x, y

    def click(self, hwnd, x, y):
        self.clicked.append(hwnd)
        if self.delivered:
            self.transcripts[hwnd] += "[A] [오후 1:02] " + self.contents[hwnd*10] + "\r\n"
            self.contents[hwnd*10] = ""

    def post(self, hwnd, message):
        assert message == WM_CLOSE
        self.closed.append(hwnd)
        del self.root_windows[hwnd]


class Win32OperationTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        guard = patch("kakaotalk_ax.UI_ROOT", self.root / "ui")
        guard.start()
        self.addCleanup(guard.stop)
        self.api = Desktop()
        self.native = WindowsAX(api=self.api, state_root=self.root / "journal")
        stub = patch.object(self.native, "snapshot", side_effect=self.snapshot)
        self.snap_mock = stub.start()
        self.addCleanup(stub.stop)
        guard = Mock(click=lambda hwnd, x, y: self.api.click(hwnd, x, y))
        fixture = patch("kakaotalk_win32.foreground", return_value=nullcontext(guard))
        fixture.start()
        self.addCleanup(fixture.stop)
        button = patch.object(self.native, "send_button_ready", return_value=True)
        self.send_button = button.start()
        self.addCleanup(button.stop)

    def snapshot(self, chat, guard=None):
        from kakaotalk_state import digest
        self.api.idle()
        text = self.api.transcripts[chat.hwnd]
        return digest(text)[:24], transcript_rows(text), 0

    def test_open_window_enumeration_flags_homonyms_without_reading(self):
        self.api.add_chat(40, NAME)
        chats = self.native.windows()["items"]
        self.assertEqual([c["readable"] for c in chats if c["windowTitle"] == NAME], [False, False])
        self.snap_mock.assert_not_called()
        self.assertEqual(self.native.open_room(NAME)["reason"], "target_room_window_ambiguous")

    def test_send_target_is_exact_and_must_already_be_open(self):
        for wrong in (NAME + " ", NAME[:-1], NAME.casefold().replace("방", "방 ")):
            self.assertEqual(self.native.send_message(wrong, "본문", "exact")["reason"],
                             "target_room_window_not_open")
        self.assertEqual(self.api.clicked, [])
        self.assertEqual(list((self.root / "journal").glob("*.json")), [])

    def test_duplicate_locked_login_and_owned_modal_are_blocked(self):
        self.api.hidden.remove(101)
        self.assertEqual(self.native.send_message(NAME, "본문", "lock")["reason"], "lock_mode_active")
        self.api.hidden.add(101)
        online = self.api.nodes[10].pop(0)
        self.assertEqual(self.native.send_message(NAME, "본문", "login")["reason"], "not_logged_in")
        self.api.nodes[10].append(online)
        self.api.root_windows[40] = Window(40, "#32770", "알림", 0)
        self.api.owners[40] = 20
        self.assertEqual(self.native.send_message(NAME, "본문", "modal")["reason"], "modal_dialog_present")
        del self.api.root_windows[40]
        self.api.add_chat(41, NAME)
        self.assertEqual(self.native.send_message(NAME, "본문", "duplicate")["reason"],
                         "target_room_window_ambiguous")
        self.assertEqual(self.api.clicked, [])
        self.snap_mock.assert_not_called()

    def test_draft_one_character_is_preserved_without_copy_or_click(self):
        self.api.contents[200] = " "
        self.assertEqual(self.native.close_room(NAME)["reason"], "existing_draft_preserved")
        self.assertEqual(self.native.send_message(NAME, "새 본문", "draft")["reason"], "existing_draft_preserved")
        self.assertEqual(self.api.contents[200], " ")
        self.assertEqual(self.api.clicked, [])
        self.assertEqual(self.api.closed, [])
        self.snap_mock.assert_not_called()

    def test_require_self_does_not_bypass_evidence(self):
        with patch.object(self.native, "verify_self", side_effect=Blocked("self_chat_unverified")) as verify:
            self.assertEqual(self.native.send_message(NAME, "새 본문", "not-self", require_self=True)["reason"],
                             "self_chat_unverified")
            verify.assert_called_once()
        self.assertEqual(self.api.clicked, [])

    def test_verify_self_reports_hidden_main_window_before_foreground_switch(self):
        self.api.foreground = 99
        self.api.hidden.add(10)
        self.assertEqual(self.native.open_room(NAME, require_self=True)["reason"], "main_window_not_visible")

    def test_send_journal_is_written_before_dispatch_and_never_resends(self):
        body = "한글 🧪\n둘째 줄"
        real_click = self.api.click
        engine = self.native.send_engine()

        def click(hwnd, x, y):
            record = engine.load(engine.path("once"))
            self.assertEqual(record["phase"], "dispatching")
            self.assertNotIn(body, engine.path("once").read_text(encoding="utf-8"))
            real_click(hwnd, x, y)

        with patch.object(self.api, "click", side_effect=click):
            sent = self.native.send_message(NAME, body, "once", wait_seconds=0)
            self.assertEqual(sent["status"], "observed_in_ax")
            self.assertEqual(sent["confirmationSource"], "windows_clipboard")
            self.assertEqual(self.native.send_message(NAME, body, "once")["nativeSendCalls"], 0)
            self.assertEqual(self.native.send_result("once")["status"], "observed_in_ax")
        self.assertEqual(self.api.clicked, [20])

    def test_unknown_send_never_reopens_or_retries_after_handle_replacement(self):
        self.api.delivered = False
        initial = self.native.send_message(NAME, "기존 원문", "unsure", wait_seconds=0)
        self.assertEqual(initial["status"], "outcome_unknown")
        self.api.post(20, WM_CLOSE)
        self.api.add_chat(22, NAME)
        self.api.transcripts[22] += "[A] [오후 1:02] 기존 원문\r\n"
        self.assertEqual(self.native.send_result("unsure")["status"], "outcome_unknown")
        self.assertEqual(self.native.send_message(NAME, "기존 원문", "unsure", 0)["nativeSendCalls"], 0)
        self.assertEqual(self.api.clicked, [20])

    def test_disabled_button_does_not_dispatch_and_only_helper_draft_is_cleared(self):
        self.send_button.return_value = False
        with patch("kakaotalk_win32.until", side_effect=lambda predicate, *args: predicate()):
            result = self.native.send_message(NAME, "도움말", "disabled", wait_seconds=0)
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["reason"], "send_button_not_enabled")
        self.assertFalse(result["native"]["dispatchAttempted"])
        self.assertEqual(self.api.clicked, [])
        self.assertEqual(self.api.contents[200], "")

    def test_new_input_blocks_read_and_send_before_dispatch(self):
        self.api.inactive = True
        self.assertEqual(self.native.read_messages(NAME)["reason"], "user_input_active")
        self.assertEqual(self.native.send_message(NAME, "본문", "conflict")["reason"], "user_input_active")
        self.assertEqual(self.api.clicked, [])

    def test_snapshot_change_detected_even_when_row_count_remains_equal(self):
        snapshot = self.native.read_messages(NAME, limit=1)["snapshotId"]
        self.api.transcripts[20] = INITIAL.replace("기존 원문", "수정 문자")
        page = self.native.read_messages(NAME, limit=1, offset=1, snapshot_id=snapshot)
        self.assertEqual(page["status"], "snapshot_changed")
        self.assertEqual(page["items"], [])
        self.assertNotEqual(page["snapshotId"], snapshot)

    def test_serial_batch_only_closes_own_new_window(self):
        self.api.post(30, WM_CLOSE)
        self.api.closed.clear()
        original_open = self.native.open_room

        def open_room(name):
            if name == OTHER:
                self.api.add_chat(31, OTHER)
                return {"status": "ok", "alreadyOpen": False}
            return original_open(name)
        with patch.object(self.native, "open_room", side_effect=open_room):
            result = self.native.read_many([NAME, OTHER], close_after=True, max_workers=4)
        self.assertEqual(result["status"], "ok")
        self.assertFalse(result["parallel"])
        self.assertEqual(result["windowsOpened"], [OTHER])
        self.assertEqual(self.api.closed, [31])

    def test_attachment_rechecks_snapshot_and_ocr_is_opt_in(self):
        self.api.transcripts[20] += "[A] [오후 1:01] 사진\r\n"
        page = self.native.read_messages(NAME)
        snapshot = page["snapshotId"]
        row = page["items"][-1]["rowIndex"]
        image = frame(100, 60)
        recognize = Mock(return_value=[Line("인식", 0, 0, 6, 10)])
        self.native._ocr = Mock(session=Mock(return_value=nullcontext(recognize)))
        with patch("kakaotalk_win32.foreground", return_value=nullcontext(Mock())), \
                patch("kakaotalk_win32.PhotoLocator") as locator:
            locator.return_value.locate.return_value = (image, True)
            self.assertEqual(self.native.read_attachment(NAME, row, "0" * 24)["status"], "snapshot_changed")
            locator.assert_not_called()
            output = self.root / "fixture.png"
            plain = self.native.read_attachment(NAME, row, snapshot, str(output))
            self.assertNotIn("ocr", plain)
            self.assertEqual(output.read_bytes(), image.png())
            recognize.assert_not_called()
            with self.assertRaisesRegex(ValueError, "already exists"):
                self.native.read_attachment(NAME, row, snapshot, str(output))
            result = self.native.read_attachment(NAME, row, snapshot, str(self.root / "ocr.png"), ocr=True)
            self.assertEqual(result["ocr"], "인식")
            self.assertTrue(result["scrollRestored"])

    def test_snapshot_arms_list_separates_ctrl_a_ctrl_c_and_falls_back_to_physical_gutter_click(self):
        chat = self.native.exact_chat(self.native.app(), NAME)
        self.api.foreground = chat.hwnd
        real_snapshot = WindowsAX.snapshot.__get__(self.native, WindowsAX)
        armed = False
        arm_calls = []
        key_batches = []
        gutter_clicks = []
        self.api.arm_list = lambda hwnd: arm_calls.append(hwnd)
        self.api.clear_selection = Mock()
        non_blank = frame(380, 419, ((40, 40, 180, 90, (4, 222, 250)),))
        self.api.capture = Mock(return_value=non_blank)

        def guard_keys(child, *keys, control=False):
            key_batches.append((child, keys, control))

        def guard_click(hwnd, x, y):
            nonlocal armed
            gutter_clicks.append((hwnd, x, y))
            armed = True

        def copy_chat(pid, perform, guard, check_target):
            perform()
            check_target()
            if not armed:
                raise Blocked("clipboard_copy_not_observed")
            return INITIAL

        guard = Mock(keys=guard_keys, click=guard_click)
        self.native._clipboard = Mock(copy_chat=copy_chat)
        with patch("kakaotalk_win32.time.sleep"):
            _, rows, _ = real_snapshot(chat, guard)
        self.assertEqual(len(rows), 2)
        self.assertEqual(arm_calls, [chat.history, chat.history])
        self.assertEqual(gutter_clicks, [(chat.history, 4, 411)])
        # Ctrl+A (65) and Ctrl+C (67) are sent in separate batches rather than a 0ms burst.
        self.assertEqual(key_batches, [
            (chat.history, (65,), True),
            (chat.history, (67,), True),
            (chat.history, (65,), True),
            (chat.history, (67,), True),
        ])
        self.api.clear_selection.assert_called()

    def test_minimized_chat_composer_empty_restores_for_placeholder_and_preserves_real_draft(self):
        minimized = {20}
        restores = []
        self.api.IsIconic = lambda hwnd: hwnd in minimized
        self.api.contents[200] = "메시지 입력"
        # Idle frame: disabled grey send button at (w-54, h-18..26) and grey prompt ink (150, 150, 150) inside composer.
        idle_frame = frame(380, 640, (
            (0, 520, 380, 640, (255, 255, 255)),
            (310, 605, 370, 630, (240, 240, 240)),
            (22, 538, 120, 550, (150, 150, 150)),
        ))
        # Armed frame: user literally typed "메시지 입력", so send button turns Kakao yellow (BGR 0, 229, 254).
        armed_frame = frame(380, 640, (
            (0, 520, 380, 640, (255, 255, 255)),
            (310, 605, 370, 630, (0, 229, 254)),
            (22, 538, 120, 550, (20, 20, 20)),
        ))
        active_capture = [idle_frame]
        self.api.capture = lambda hwnd: frame(160, 28) if hwnd in minimized else active_capture[0]

        from contextlib import contextmanager

        @contextmanager
        def restore_guard(api, hwnd):
            was_min = hwnd in minimized
            if was_min:
                restores.append(hwnd)
                minimized.remove(hwnd)
            try:
                yield Mock()
            finally:
                if was_min and api.IsWindow(hwnd):
                    minimized.add(hwnd)

        with patch("kakaotalk_win32.foreground", side_effect=restore_guard):
            chat = self.native.exact_chat(self.native.app(), NAME)
            self.assertTrue(self.native.composer_empty(chat))
            self.assertIn(20, minimized)
            # Armed yellow send button with "메시지 입력" text is preserved as a user draft!
            active_capture[0] = armed_frame
            self.assertFalse(self.native.composer_empty(chat))
            self.assertEqual(self.native.close_room(NAME)["reason"], "existing_draft_preserved")
            # Normal non-placeholder draft returns False immediately without restoring at all.
            restores.clear()
            self.api.contents[200] = "작성 중인 초안"
            self.assertEqual(self.native.send_message(NAME, "본문", "min-draft")["reason"],
                             "existing_draft_preserved")
            self.assertEqual(restores, [])
            # Idle placeholder on minimized window closes cleanly with only a single restore cycle.
            self.api.contents[200] = "메시지 입력"
            active_capture[0] = idle_frame
            closed = self.native.close_room(NAME)
            self.assertEqual(closed["status"], "ok")
            self.assertEqual(restores, [20])

    def test_open_room_accepts_self_aliases_only_with_require_self_while_send_stays_exact(self):
        self.api.add_chat(50, "내 프로필")
        self_chat = self.native.exact_chat(self.native.app(), "내 프로필")

        def fake_verify_self(app, name, expected=None):
            if (name is not None and self_chat.name != name) or (expected and self_chat.identity != expected.identity):
                raise Blocked("self_chat_unverified")
            self.native._self_identity = self_chat.identity
            return self_chat, True

        with patch.object(self.native, "verify_self", side_effect=fake_verify_self):
            for alias in ("나", "나와의 채팅", "Chat with myself"):
                opened = self.native.open_room(alias, require_self=True)
                self.assertEqual(opened["status"], "ok")
                self.assertEqual(opened["windowTitle"], "내 프로필")
                self.assertTrue(opened["isSelf"])
            # Non-alias wrong name still blocks with self_chat_unverified.
            self.assertEqual(self.native.open_room(OTHER, require_self=True)["reason"], "self_chat_unverified")
            # send_message NEVER accepts aliases — exact GetWindowTextW match is mandatory.
            self.assertEqual(
                self.native.send_message("나", "본문", "alias-blocked", require_self=True)["reason"],
                "target_room_window_not_open",
            )
            # If an open friend chat is literally titled "나", open_room("나", require_self=True) blocks safely.
            self.api.add_chat(60, "나")
            self.assertEqual(self.native.open_room("나", require_self=True)["reason"], "self_chat_unverified")


if __name__ == "__main__":
    unittest.main()

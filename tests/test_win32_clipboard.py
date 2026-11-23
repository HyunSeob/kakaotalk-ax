from contextlib import nullcontext
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from kakaotalk_win32_api import Blocked, Win32
from kakaotalk_win32_clipboard import Clipboard, Foreground, foreground


class ClipboardProtectionTests(unittest.TestCase):
    def setUp(self):
        self.clip = clip = Clipboard.__new__(Clipboard)
        self.user = clip.user = Mock()
        clip.api = Mock()
        clip.api.pid.side_effect = lambda owner: owner
        self.owner = 10
        self.seq = 1
        clip.owner = lambda: self.owner
        clip.sequence = lambda: self.seq
        clip.open = Mock(side_effect=lambda *_: nullcontext())
        clip.formats = Mock(return_value=iter((13, 8, 15, 0xC117)))
        clip.duplicate = Mock(side_effect=lambda n, _: n + 500)
        self.restored = {}
        clip.free = Mock()
        self.user.SetClipboardData.side_effect = self.set_format
        self.user.GetClipboardData.return_value = 701
        clip.read_bytes = Mock(return_value="원문 🧪\0".encode("utf-16-le") + b"\0\0")
        self.guard = Mock()

    def set_format(self, fmt, handle):
        self.restored[fmt] = handle
        return True

    def app_copy(self):
        self.seq += 1
        self.owner = 42

    def unobserved(self, predicate, *args):
        return predicate()

    def test_original_unicode_image_file_and_registered_formats_restored(self):
        exact = self.clip.copy_chat(42, self.app_copy, self.guard, Mock())
        self.assertEqual(exact, "원문 🧪")
        self.assertEqual(self.restored, {13: 513, 8: 508, 15: 515, 0xC117: 0xC117 + 500})
        self.user.EmptyClipboard.assert_called_once()
        self.clip.free.assert_not_called()
        self.user.DestroyWindow.assert_called_once()

    def test_restore_runs_on_copy_and_decode_failures(self):
        self.clip.read_bytes.return_value = b"\x00\xd8\x00\x00"
        with self.assertRaisesRegex(Blocked, "clipboard_unicode_text_invalid"):
            self.clip.copy_chat(42, self.app_copy, self.guard, Mock())
        self.assertIn(8, self.restored)

    def test_unchanged_sequence_never_reads_prior_clipboard_as_messages(self):
        with patch("kakaotalk_win32_clipboard.until", side_effect=self.unobserved):
            with self.assertRaisesRegex(Blocked, "clipboard_copy_not_observed"):
                self.clip.copy_chat(42, lambda: None, self.guard, Mock())
        self.user.GetClipboardData.assert_not_called()
        self.user.EmptyClipboard.assert_not_called()

    def test_changed_external_clipboard_is_preserved(self):
        def external_copy():
            self.seq += 1
            self.owner = 55
        with patch("kakaotalk_win32_clipboard.until", side_effect=self.unobserved):
            with self.assertRaisesRegex(Blocked, "clipboard_changed"):
                self.clip.copy_chat(42, external_copy, self.guard, Mock())
        self.user.EmptyClipboard.assert_not_called()

    def test_opaque_format_stops_before_keys_are_sent(self):
        self.clip.duplicate.side_effect = Blocked("clipboard_format_cannot_be_preserved")
        perform = Mock()
        with self.assertRaisesRegex(Blocked, "clipboard_format_cannot_be_preserved"):
            self.clip.copy_chat(42, perform, self.guard, Mock())
        perform.assert_not_called()
        self.user.EmptyClipboard.assert_not_called()

    def test_newer_sequence_of_same_owner_is_not_erased(self):
        def changed_after_validation():
            self.seq += 1
        with self.assertRaisesRegex(Blocked, "clipboard_changed"):
            self.clip.copy_chat(42, self.app_copy, self.guard, changed_after_validation)
        self.user.EmptyClipboard.assert_not_called()

    def test_concurrent_user_input_prevents_attributing_a_same_app_copy(self):
        def copy_then_input():
            self.app_copy()
            self.guard.check.side_effect = Blocked("user_input_active")
        with self.assertRaisesRegex(Blocked, "user_input_active"):
            self.clip.copy_chat(42, copy_then_input, self.guard, Mock())
        self.user.EmptyClipboard.assert_not_called()


class ForegroundProtectionTests(unittest.TestCase):
    def setUp(self):
        self.api = api = Mock()
        self.user = u = api.user
        self.fg = 31
        self.focuses = {31: 310, 50: 500}
        self.last = 42
        u.GetForegroundWindow.side_effect = lambda: self.fg
        u.SetForegroundWindow.side_effect = self.activate
        api.focus_info.side_effect = lambda h: SimpleNamespace(focus=self.focuses[h])
        api.focus_child.side_effect = self.focus
        api.idle.return_value = self.last
        api.last_input.side_effect = lambda: self.last
        u.GetAsyncKeyState.return_value = 0

    def activate(self, hwnd):
        self.fg = hwnd
        return True

    def focus(self, child):
        self.focuses[self.fg] = child

    def test_restores_window_child_and_minimized_state_on_error(self):
        with self.assertRaisesRegex(RuntimeError, "fixture"):
            with foreground(self.api, 50) as guard:
                guard.focus(501)
                raise RuntimeError("fixture")
        self.assertEqual(self.fg, 31)
        self.assertEqual(self.focuses, {31: 310, 50: 500})
        self.user.ShowWindowAsync.assert_any_call(50, 9)
        self.user.ShowWindowAsync.assert_any_call(50, 7)

    def test_user_input_prevents_keyboard_injection_and_focus_stealing(self):
        guard = Foreground(self.api)
        guard.activate(50)
        self.last += 1
        self.fg = 77
        with self.assertRaisesRegex(Blocked, "user_input_active"):
            guard.keys(500, 65, 67, control=True)
        guard.restore()
        self.assertEqual(self.fg, 77)
        self.api.input_keys.assert_not_called()

    def _bound_win32(self, *, attach_succeeds=True, uipi_blocked=False):
        win = Win32.__new__(Win32)
        win.user = u = Mock()
        win.kernel = k = Mock()
        attached = set()
        unlocked = False
        u.IsWindow.return_value = True
        u.IsIconic.return_value = False
        u.IsWindowVisible.return_value = True
        u.GetForegroundWindow.side_effect = lambda: self.fg
        u.GetAsyncKeyState.return_value = 0
        k.GetCurrentThreadId.return_value = 1
        u.GetWindowThreadProcessId.side_effect = lambda hwnd, _: {31: 10, 50: 20}.get(hwnd, 0)

        def attach(cur, tid, flag):
            if not attach_succeeds:
                return False
            if flag:
                attached.add(tid)
            else:
                attached.discard(tid)
            return True

        def set_fg(hwnd):
            if attached or unlocked:
                self.fg = hwnd
                return True
            return False

        def send_input(events):
            nonlocal unlocked
            if uipi_blocked:
                raise Blocked("keyboard_input_rejected")
            unlocked = True
            self.last = events[0].value.keyboard.timestamp

        u.AttachThreadInput.side_effect = attach
        u.SetForegroundWindow.side_effect = set_fg
        win.pump_messages = Mock()
        win.idle = Mock(return_value=self.last)
        win.last_input = Mock(side_effect=lambda: self.last)
        win.tick = Mock(return_value=99)
        win.focus_info = Mock(side_effect=lambda h: SimpleNamespace(focus=self.focuses.get(h, 0)))
        win.focus_child = Mock(side_effect=self.focus)
        win.send_input = Mock(side_effect=send_input)
        return win, attached

    def test_background_process_unlocks_foreground_via_thread_input_attach(self):
        win, attached = self._bound_win32(attach_succeeds=True)
        with foreground(win, 50) as guard:
            self.assertEqual(self.fg, 50)
            guard.check()
        self.assertEqual(self.fg, 31)
        self.assertEqual(attached, set())
        win.send_input.assert_not_called()

    def test_null_foreground_fallback_tracks_unlock_stamp_without_false_user_input(self):
        self.fg = 0
        win, _ = self._bound_win32(attach_succeeds=False)
        with foreground(win, 50) as guard:
            self.assertEqual(self.fg, 50)
            self.assertIn(99, guard.allowed_ticks)
            guard.check()
        win.send_input.assert_called_once()

    def test_elevated_uipi_foreground_refusal_reports_foreground_unavailable(self):
        win, _ = self._bound_win32(attach_succeeds=False, uipi_blocked=True)
        with patch("kakaotalk_win32_api.until", side_effect=lambda fn, *_: fn()), \
                patch("kakaotalk_win32_clipboard.until", side_effect=lambda fn, *_: fn()):
            with self.assertRaisesRegex(Blocked, "foreground_unavailable"):
                with foreground(win, 50):
                    pass

    def test_inactive_thread_focus_is_captured_after_activation_and_restored(self):
        # Real GetGUIThreadInfo returns hwndFocus=0 while target thread is inactive.
        self.api.focus_info.side_effect = lambda h: SimpleNamespace(
            focus=self.focuses[h] if self.fg == h else 0
        )
        with foreground(self.api, 50) as guard:
            self.assertEqual(guard.target_focus, 500)
            self.user.BringWindowToTop.assert_called_with(50)
            guard.focus(501)
            self.assertEqual(self.focuses[50], 501)
        self.assertEqual(self.fg, 31)
        self.assertEqual(self.focuses[50], 500)

    def test_own_send_input_tick_does_not_self_block_idle_while_external_tick_blocks(self):
        from collections import deque
        from kakaotalk_win32_api import Input, InputData, KeyboardInput
        win = Win32.__new__(Win32)
        win._own_ticks = deque(maxlen=32)
        win.user = Mock()
        win.user.GetAsyncKeyState.return_value = 0
        win.user.SendInput.side_effect = lambda count, events, size: count
        stamp = 250
        win.last_input = Mock(return_value=stamp)
        win.tick = Mock(return_value=260)  # Only 10ms elapsed (< 400ms)
        win.send_input([Input(1, InputData(keyboard=KeyboardInput(0x41, 0, 0, stamp, 0)))])
        self.assertEqual(win.idle(400), stamp)
        win.last_input.return_value = stamp + 1  # External user input at 251
        with patch("kakaotalk_win32_api.until", side_effect=lambda fn, *_: fn()):
            with self.assertRaisesRegex(Blocked, "user_input_active"):
                win.idle(400)

    def test_arm_list_posts_background_gutter_click_when_tall_enough(self):
        win = Win32.__new__(Win32)
        win.rect = Mock(return_value=(0, 0, 380, 420))
        win.click = Mock()
        with patch("kakaotalk_win32_api.time.sleep"):
            win.arm_list(201)
        win.click.assert_called_once_with(201, 4, 412)
        win.click.reset_mock()
        win.rect.return_value = (0, 0, 380, 12)
        win.arm_list(201)
        win.click.assert_not_called()


if __name__ == "__main__":
    unittest.main()

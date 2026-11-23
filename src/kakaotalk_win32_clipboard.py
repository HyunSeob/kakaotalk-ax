"""Guard the interactive desktop and preserve Win32 clipboard formats."""

import ctypes as C
from ctypes import wintypes as W
from contextlib import contextmanager
import time

from kakaotalk_win32_api import Blocked, VK_CONTROL, bind, until


CF_UNICODETEXT = 13


class Foreground:
    def __init__(self, api):
        self.api = api
        self.user = api.user
        self.allowed_ticks = {api.idle()}
        self.initial = self.user.GetForegroundWindow()
        try:
            self.initial_focus = api.focus_info(self.initial).focus if self.initial else None
        except Blocked:
            self.initial_focus = None
        self.target = None
        self.target_focus = None
        self.was_minimized = False
        self.own_tick = None

    def check(self):
        if self.api.last_input() not in self.allowed_ticks:
            raise Blocked("user_input_active")

    def _switch(self, hwnd):
        if self.user.GetForegroundWindow() != hwnd:
            self.user.SetForegroundWindow(hwnd)
        if self.user.GetForegroundWindow() != hwnd:
            self.api.activate_foreground(hwnd, self.allowed_ticks)
        return bool(until(lambda: self.user.GetForegroundWindow() == hwnd, 0.4))

    def activate(self, hwnd):
        self.check()
        if self.user.GetForegroundWindow() != self.initial:
            raise Blocked("foreground_changed")
        self.target = hwnd
        self.was_minimized = bool(self.user.IsIconic(hwnd))
        self.target_focus = self.api.focus_info(hwnd).focus
        if self.was_minimized:
            self.user.ShowWindowAsync(hwnd, 9)  # SW_RESTORE
        if not self._switch(hwnd):
            raise Blocked("foreground_unavailable")
        if self.was_minimized and isinstance(self.user.IsIconic(hwnd), int):
            until(lambda: not self.user.IsIconic(hwnd), 0.4)
        self.user.BringWindowToTop(hwnd)
        if not self.target_focus:
            self.target_focus = self.api.focus_info(hwnd).focus
        self.check()

    def focus(self, child):
        self.check()
        if self.user.GetForegroundWindow() != self.target:
            raise Blocked("foreground_changed")
        if self.api.focus_info(self.target).focus != child:
            self.api.focus_child(child)

    def keys(self, child, *keys, control=False):
        self.check()
        if self.user.GetForegroundWindow() != self.target or self.api.focus_info(self.target).focus != child:
            raise Blocked("foreground_changed")
        sequence = []
        for key in keys:
            if control:
                sequence.append((VK_CONTROL, False))
            sequence.extend([(key, False), (key, True)])
            if control:
                sequence.append((VK_CONTROL, True))
        self.api.input_keys(sequence, self.input_stamp())

    def input_stamp(self):
        if any(self.user.GetAsyncKeyState(key) & 0x8000 for key in (1, 2, 4, 0x10, 0x11, 0x12, 0x5b, 0x5c)):
            raise Blocked("user_input_active")
        last = self.api.last_input()
        if last not in self.allowed_ticks:
            raise Blocked("user_input_active")
        stamp = self.api.tick()
        # GetLastInputInfo can report an older injected timestamp as queued
        # mouse/keyboard events arrive. Keep the stamps for this guard only.
        self.allowed_ticks.add(stamp)
        self.own_tick = stamp
        return stamp

    def visible_point(self, hwnd, x, y):
        self.check()
        if self.user.GetForegroundWindow() != self.target:
            raise Blocked("foreground_changed")
        screen = self.api.point(hwnd, x, y, to_screen=True)
        hit = self.user.WindowFromPoint(W.POINT(*screen))
        if not hit or self.user.GetAncestor(hit, 2) != self.target or hit != hwnd:
            raise Blocked("click_target_obscured")
        return screen

    def restore_cursor(self, position, at):
        if self.api.last_input() in self.allowed_ticks and self.api.cursor() == at and position != at:
            self.api.send_input([self.api.mouse_move(*position, self.own_tick)])

    def click(self, hwnd, x, y):
        screen = self.visible_point(hwnd, x, y)
        position = self.api.cursor()
        try:
            self.api.mouse_click(*screen, self.input_stamp())
            until(lambda: self.api.last_input() == self.own_tick, 0.15)
            # Kakao's owner-drawn buttons sample the live pointer on mouse-up.
            time.sleep(0.12)
        finally:
            self.restore_cursor(position, screen)

    def drag(self, hwnd, start, end, *, via=()):
        origin = self.visible_point(hwnd, *start)
        destinations = [self.visible_point(hwnd, *point) for point in (*via, end)]
        cursor = self.api.cursor()
        stamp = self.input_stamp()
        moved = origin
        try:
            self.api.send_input([self.api.mouse_move(*origin, stamp), self.api.mouse_button(2, stamp)])
            time.sleep(0.05)
            for destination in destinations:
                self.check()
                if self.user.GetForegroundWindow() != self.target:
                    raise Blocked("foreground_changed")
                moved = destination
                self.api.send_input([self.api.mouse_move(*destination, stamp)])
                time.sleep(0.05)
        finally:
            self.api.send_input([self.api.mouse_button(4, stamp)])
            time.sleep(0.06)
            self.restore_cursor(cursor, moved)
        self.check()

    def restore(self):
        # A user who activated another window during the operation keeps it.
        if not self.target or self.user.GetForegroundWindow() != self.target:
            return
        if self.api.last_input() not in self.allowed_ticks:
            return
        if self.target_focus and self.target_focus != self.api.focus_info(self.target).focus:
            if self.user.IsWindowVisible(self.target_focus):
                self.api.focus_child(self.target_focus)
        if self.initial and self.initial != self.target and self.user.IsWindow(self.initial):
            if not self._switch(self.initial):
                raise Blocked("foreground_restore_failed")
            if self.initial_focus and self.user.IsWindowVisible(self.initial_focus):
                try:
                    if self.api.focus_info(self.initial).focus != self.initial_focus:
                        self.api.focus_child(self.initial_focus)
                except Blocked:
                    pass
        if self.was_minimized and self.user.IsWindow(self.target):
            cmd = 7 if self.user.GetForegroundWindow() != self.target else 6
            self.user.ShowWindowAsync(self.target, cmd)  # SW_SHOWMINNOACTIVE or SW_MINIMIZE


@contextmanager
def foreground(api, hwnd):
    guard = Foreground(api)
    try:
        guard.activate(hwnd)
        yield guard
    finally:
        guard.restore()


class MetaFilePict(C.Structure):
    _fields_ = [("mode", W.LONG), ("x", W.LONG), ("y", W.LONG), ("metafile", W.HANDLE)]


class Clipboard:
    def __init__(self, api):
        self.api = api
        self.user = u = api.user
        self.kernel = k = api.kernel
        self.gdi = g = api.gdi
        ole = C.WinDLL("ole32", use_last_error=True)
        self.duplicate_data = bind(ole, "OleDuplicateData", W.HANDLE, W.HANDLE, W.WORD, W.UINT)
        bind(u, "CreateWindowExW", W.HWND, W.DWORD, W.LPCWSTR, W.LPCWSTR, W.DWORD,
             C.c_int, C.c_int, C.c_int, C.c_int, W.HWND, W.HMENU, W.HINSTANCE, C.c_void_p)
        for name in ("DestroyWindow", "OpenClipboard"):
            bind(u, name, W.BOOL, W.HWND)
        for name in ("CloseClipboard", "EmptyClipboard"):
            bind(u, name, W.BOOL)
        bind(u, "GetClipboardSequenceNumber", W.DWORD)
        bind(u, "GetClipboardOwner", W.HWND)
        bind(u, "GetClipboardData", W.HANDLE, W.UINT)
        bind(u, "SetClipboardData", W.HANDLE, W.UINT, W.HANDLE)
        bind(u, "EnumClipboardFormats", W.UINT, W.UINT)
        bind(u, "IsClipboardFormatAvailable", W.BOOL, W.UINT)
        bind(k, "GlobalLock", C.c_void_p, W.HANDLE)
        bind(k, "GlobalUnlock", W.BOOL, W.HANDLE)
        bind(k, "GlobalFree", W.HANDLE, W.HANDLE)
        bind(k, "GlobalSize", C.c_size_t, W.HANDLE)
        bind(g, "CopyEnhMetaFileW", W.HANDLE, W.HANDLE, W.LPCWSTR)
        bind(g, "DeleteEnhMetaFile", W.BOOL, W.HANDLE)
        bind(g, "DeleteMetaFile", W.BOOL, W.HANDLE)

    @contextmanager
    def open(self, hwnd=None):
        def try_open():
            self.api.pump_messages()
            return self.user.OpenClipboard(hwnd)

        if not until(try_open, 1.0):
            raise Blocked("clipboard_busy")
        try:
            yield
        finally:
            self.user.CloseClipboard()

    def sequence(self):
        number = self.user.GetClipboardSequenceNumber()
        if not number:
            raise Blocked("clipboard_sequence_unavailable")
        return number

    def owner(self):
        return self.user.GetClipboardOwner()

    def formats(self):
        number = 0
        while True:
            C.set_last_error(0)
            number = self.user.EnumClipboardFormats(number)
            if not number:
                if C.get_last_error():
                    raise Blocked("clipboard_backup_failed")
                return
            yield number

    def read_bytes(self, handle):
        size = self.kernel.GlobalSize(handle)
        if not size or size > 512 * 1024 * 1024:
            raise Blocked("clipboard_data_unavailable")
        address = self.kernel.GlobalLock(handle)
        if not address:
            raise Blocked("clipboard_data_unavailable")
        try:
            return C.string_at(address, size)
        finally:
            self.kernel.GlobalUnlock(handle)

    @staticmethod
    def data_type(number):
        return {0x82: 2, 0x83: 3, 0x8e: 14}.get(number, number)  # CF_DSP*

    def duplicate(self, number, saved=()):
        if number == 0x80 or 0x200 <= number <= 0x3ff:  # Owner-drawn/private handle types
            raise Blocked("clipboard_format_cannot_be_preserved")
        handle = self.user.GetClipboardData(number)
        if not handle:
            # EnumClipboardFormats also reports synthesized formats in order
            # after their source. Some DIBs cannot be synthesized as HBITMAPs.
            for equivalent in ({1, 7, 13}, {2, 8, 17}, {3, 14}):
                if number in equivalent and equivalent.intersection(saved):
                    return None
            raise Blocked("clipboard_backup_failed")
        kind = self.data_type(number)
        result = self.gdi.CopyEnhMetaFileW(handle, None) if kind == 14 else self.duplicate_data(handle, kind, 2)
        if not result:
            raise Blocked("clipboard_backup_failed")
        return result

    def free(self, number, handle):
        kind = self.data_type(number)
        if kind in (2, 9):
            self.gdi.DeleteObject(handle)
        elif kind == 14:
            self.gdi.DeleteEnhMetaFile(handle)
        else:
            if kind == 3:
                address = self.kernel.GlobalLock(handle)
                if address:
                    self.gdi.DeleteMetaFile(MetaFilePict.from_address(address).metafile)
                    self.kernel.GlobalUnlock(handle)
            self.kernel.GlobalFree(handle)

    def copy_chat(self, pid, perform, guard, check_target):
        """Verify a fresh KakaoTalk clipboard write and restore the original contents."""
        # EmptyClipboard must have a real HWND owner before SetClipboardData.
        hwnd = self.user.CreateWindowExW(0, "STATIC", None, 0, 0, 0, 0, 0, None, None, None, None)
        if not hwnd:
            raise Blocked("clipboard_window_unavailable")
        saved = {}
        old_sequence = copied_sequence = None
        attempted = False
        try:
            guard.check()
            with self.open(hwnd):
                for number in self.formats():
                    handle = self.duplicate(number, saved)
                    if handle:
                        saved[number] = handle
                old_sequence = self.sequence()  # Delayed formats may have rendered during backup.
            guard.check()
            if self.sequence() != old_sequence:
                raise Blocked("clipboard_changed")
            attempted = True
            perform()

            def new_owner():
                self.api.pump_messages()
                guard.check()
                owner = self.owner()
                changed = owner and self.api.pid(owner) == pid and self.sequence() != old_sequence
                guard.check()
                return changed

            if not until(new_owner, 1.25):
                raise Blocked("clipboard_copy_not_observed")
            with self.open(hwnd):
                if not new_owner():
                    raise Blocked("clipboard_changed")
                copied_sequence = self.sequence()
                handle = self.user.GetClipboardData(CF_UNICODETEXT)
                copied_sequence = self.sequence()
                if not handle:
                    raise Blocked("clipboard_unicode_text_unavailable")
                data = self.read_bytes(handle)
                # The first aligned UTF-16 NUL terminates the format; allocation may have padding.
                end = next((i for i in range(0, len(data)-1, 2) if data[i:i+2] == b"\0\0"), None)
                if end is None:
                    raise Blocked("clipboard_unicode_text_invalid")
                try:
                    text = data[:end].decode("utf-16-le")
                except UnicodeDecodeError as error:
                    raise Blocked("clipboard_unicode_text_invalid") from error
                guard.check()
                check_target()
                return text
        finally:
            try:
                if attempted:
                    if copied_sequence is None:
                        # With concurrent input and no sequence attributable
                        # to this copy, a newer Kakao clipboard may be the user's.
                        guard.check()
                    self.restore(hwnd, pid, old_sequence, copied_sequence, saved)
            finally:
                for number, handle in saved.items():
                    self.free(number, handle)
                self.user.DestroyWindow(hwnd)

    def restore(self, hwnd, pid, old_sequence, copied_sequence, saved):
        with self.open(hwnd):
            sequence = self.sequence()
            owner = self.owner()
            if sequence == old_sequence:
                return
            if not owner or self.api.pid(owner) != pid or (copied_sequence is not None and sequence != copied_sequence):
                raise Blocked("clipboard_changed")
            if not self.user.EmptyClipboard():
                raise Blocked("clipboard_restore_failed")
            missing = []
            for number, handle in list(saved.items()):
                if self.user.SetClipboardData(number, handle):
                    del saved[number]  # Ownership transferred to Windows.
                else:
                    missing.append(number)
            if missing:
                raise Blocked("clipboard_restore_failed")

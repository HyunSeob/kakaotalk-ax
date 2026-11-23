"""Typed Win32 bindings for the official KakaoTalk desktop UI."""

from collections import deque
from contextlib import contextmanager
import ctypes as C
from ctypes import wintypes as W
from dataclasses import dataclass
import ntpath
import time

from kakaotalk_win32_image import Frame


DWORD_PTR = C.c_size_t
LRESULT = C.c_ssize_t
WM_GETTEXT, WM_GETTEXTLENGTH, WM_SETTEXT = 0x000D, 0x000E, 0x000C
EM_SETSEL, EM_REPLACESEL = 0x00B1, 0x00C2
WM_CLOSE, WM_COMMAND, WM_NULL = 0x0010, 0x0111, 0
WM_KEYDOWN, WM_KEYUP = 0x0100, 0x0101
WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK = 0x0201, 0x0202, 0x0203
WM_RBUTTONDOWN, WM_RBUTTONUP, WM_MOUSEWHEEL = 0x0204, 0x0205, 0x020A
VK_CONTROL, VK_RETURN, VK_ESCAPE = 0x11, 0x0D, 0x1B
VK_HOME, VK_PRIOR, VK_END = 0x24, 0x21, 0x23
VK_UNASSIGNED = 0xE8
PW_RENDERFULLCONTENT = 2


class Blocked(RuntimeError):
    """An expected safety stop, with a stable public reason code."""


@dataclass(frozen=True)
class Window:
    hwnd: int
    cls: str
    name: str
    control_id: int


class LastInput(C.Structure):
    _fields_ = [("cbSize", W.UINT), ("dwTime", W.DWORD)]


class GuiThread(C.Structure):
    _fields_ = [("cbSize", W.DWORD), ("flags", W.DWORD)] + [
        (name, W.HWND) for name in ("active", "focus", "capture", "menu_owner", "move_size", "caret")
    ] + [("caret_rect", W.RECT)]


class KeyboardInput(C.Structure):
    _fields_ = [("vk", W.WORD), ("scan", W.WORD), ("flags", W.DWORD),
                ("timestamp", W.DWORD), ("extra", DWORD_PTR)]


class MouseInput(C.Structure):
    _fields_ = [("x", W.LONG), ("y", W.LONG), ("data", W.DWORD), ("flags", W.DWORD),
                ("timestamp", W.DWORD), ("extra", DWORD_PTR)]


class InputData(C.Union):
    _fields_ = [("keyboard", KeyboardInput), ("mouse", MouseInput)]


class Input(C.Structure):
    _fields_ = [("kind", W.DWORD), ("value", InputData)]


class BitmapInfo(C.Structure):
    _fields_ = [("size", W.DWORD), ("width", W.LONG), ("height", W.LONG),
                ("planes", W.WORD), ("bits", W.WORD), ("compression", W.DWORD),
                ("image_size", W.DWORD), ("x_resolution", W.LONG), ("y_resolution", W.LONG),
                ("colors", W.DWORD), ("important_colors", W.DWORD)]


def bind(dll, name, result, *args):
    function = getattr(dll, name)
    function.restype = result
    function.argtypes = args
    return function


def until(predicate, seconds=0.7, interval=0.025):
    deadline = time.monotonic() + seconds
    while True:
        result = predicate()
        if result or time.monotonic() >= deadline:
            return result
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def packed(x, y):
    return (x & 0xffff) | ((y & 0xffff) << 16)


class Win32:
    def __init__(self):
        self._own_ticks = deque(maxlen=32)
        self.user = U = C.WinDLL("user32", use_last_error=True)
        self.kernel = K = C.WinDLL("kernel32", use_last_error=True)
        self.gdi = G = C.WinDLL("gdi32", use_last_error=True)
        self.enum_callback = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
        bind(U, "EnumWindows", W.BOOL, self.enum_callback, W.LPARAM)
        bind(U, "EnumChildWindows", W.BOOL, W.HWND, self.enum_callback, W.LPARAM)
        for name in ("IsWindow", "IsWindowVisible", "IsWindowEnabled", "IsIconic",
                     "SetForegroundWindow", "BringWindowToTop"):
            bind(U, name, W.BOOL, W.HWND)
        for name in ("GetParent", "SetFocus", "GetDesktopWindow"):
            bind(U, name, W.HWND, *(() if name == "GetDesktopWindow" else (W.HWND,)))
        bind(U, "GetWindow", W.HWND, W.HWND, W.UINT)
        bind(U, "GetAncestor", W.HWND, W.HWND, W.UINT)
        bind(U, "GetWindowTextW", C.c_int, W.HWND, W.LPWSTR, C.c_int)
        bind(U, "GetWindowTextLengthW", C.c_int, W.HWND)
        bind(U, "GetClassNameW", C.c_int, W.HWND, W.LPWSTR, C.c_int)
        bind(U, "GetDlgCtrlID", C.c_int, W.HWND)
        bind(U, "GetWindowThreadProcessId", W.DWORD, W.HWND, C.POINTER(W.DWORD))
        bind(U, "GetForegroundWindow", W.HWND)
        bind(U, "GetCursorPos", W.BOOL, C.POINTER(W.POINT))
        bind(U, "SetCursorPos", W.BOOL, C.c_int, C.c_int)
        bind(U, "GetSystemMetrics", C.c_int, C.c_int)
        bind(U, "WindowFromPoint", W.HWND, W.POINT)
        bind(U, "GetGUIThreadInfo", W.BOOL, W.DWORD, C.POINTER(GuiThread))
        bind(U, "PeekMessageW", W.BOOL, C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT, W.UINT)
        bind(U, "GetLastInputInfo", W.BOOL, C.POINTER(LastInput))
        bind(U, "GetAsyncKeyState", W.SHORT, C.c_int)
        bind(U, "AttachThreadInput", W.BOOL, W.DWORD, W.DWORD, W.BOOL)
        bind(U, "ShowWindowAsync", W.BOOL, W.HWND, C.c_int)
        bind(U, "GetWindowRect", W.BOOL, W.HWND, C.POINTER(W.RECT))
        bind(U, "GetClientRect", W.BOOL, W.HWND, C.POINTER(W.RECT))
        bind(U, "ScreenToClient", W.BOOL, W.HWND, C.POINTER(W.POINT))
        bind(U, "ClientToScreen", W.BOOL, W.HWND, C.POINTER(W.POINT))
        bind(U, "GetDpiForWindow", W.UINT, W.HWND)
        bind(U, "SetThreadDpiAwarenessContext", W.HANDLE, W.HANDLE)
        bind(U, "SendMessageTimeoutW", LRESULT, W.HWND, W.UINT, W.WPARAM, W.LPARAM,
             W.UINT, W.UINT, C.POINTER(DWORD_PTR))
        bind(U, "PostMessageW", W.BOOL, W.HWND, W.UINT, W.WPARAM, W.LPARAM)
        bind(U, "SendInput", W.UINT, W.UINT, C.POINTER(Input), C.c_int)
        bind(U, "MapVirtualKeyW", W.UINT, W.UINT, W.UINT)
        bind(U, "GetDC", W.HDC, W.HWND)
        bind(U, "ReleaseDC", C.c_int, W.HWND, W.HDC)
        bind(U, "PrintWindow", W.BOOL, W.HWND, W.HDC, W.UINT)
        bind(G, "CreateCompatibleDC", W.HDC, W.HDC)
        bind(G, "CreateDIBSection", W.HBITMAP, W.HDC, C.POINTER(BitmapInfo), W.UINT,
             C.POINTER(C.c_void_p), W.HANDLE, W.DWORD)
        bind(G, "SelectObject", W.HGDIOBJ, W.HDC, W.HGDIOBJ)
        bind(G, "DeleteObject", W.BOOL, W.HGDIOBJ)
        bind(G, "DeleteDC", W.BOOL, W.HDC)
        bind(K, "OpenProcess", W.HANDLE, W.DWORD, W.BOOL, W.DWORD)
        bind(K, "CloseHandle", W.BOOL, W.HANDLE)
        bind(K, "GetTickCount", W.DWORD)
        bind(K, "GetCurrentThreadId", W.DWORD)
        bind(K, "GetProcessTimes", W.BOOL, W.HANDLE, *([C.POINTER(W.FILETIME)] * 4))
        bind(K, "QueryFullProcessImageNameW", W.BOOL, W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD))

    def handles(self, parent=None):
        found = []

        @self.enum_callback
        def visit(hwnd, _):
            found.append(hwnd)
            return True

        if parent is None:
            self.user.EnumWindows(visit, 0)
        else:
            self.user.EnumChildWindows(parent, visit, 0)
        return found

    def title(self, hwnd):
        length = self.user.GetWindowTextLengthW(hwnd)
        if length > 32767:
            raise Blocked("window_title_too_long")
        text = C.create_unicode_buffer(length + 2)
        n = self.user.GetWindowTextW(hwnd, text, len(text))
        if n == len(text) - 1:  # The title grew between calls.
            raise Blocked("window_title_changed")
        return text.value

    def class_name(self, hwnd):
        name = C.create_unicode_buffer(256)
        self.user.GetClassNameW(hwnd, name, len(name))
        return name.value

    def window(self, hwnd):
        return Window(hwnd, self.class_name(hwnd), self.title(hwnd), self.user.GetDlgCtrlID(hwnd))

    def children(self, hwnd):
        return [self.window(child) for child in self.handles(hwnd)]

    def pid(self, hwnd):
        pid = W.DWORD()
        self.user.GetWindowThreadProcessId(hwnd, C.byref(pid))
        return pid.value

    def process(self, pid):
        handle = self.kernel.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return "", 0
        try:
            name = C.create_unicode_buffer(32768)
            size = W.DWORD(len(name))
            times = [W.FILETIME() for _ in range(4)]
            if not self.kernel.QueryFullProcessImageNameW(handle, 0, name, C.byref(size)):
                return "", 0
            has_times = self.kernel.GetProcessTimes(handle, *(C.byref(t) for t in times))
            created = (times[0].dwHighDateTime << 32 | times[0].dwLowDateTime) if has_times else 0
            return ntpath.basename(name.value).casefold(), created
        finally:
            self.kernel.CloseHandle(handle)

    def message(self, hwnd, code, wparam=0, lparam=0):
        result = DWORD_PTR()
        # SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT
        if not self.user.SendMessageTimeoutW(hwnd, code, wparam, lparam, 0x23, 500, C.byref(result)):
            raise Blocked("win32_message_timeout")
        return result.value

    def post(self, hwnd, code, wparam=0, lparam=0):
        if not self.user.PostMessageW(hwnd, code, wparam, lparam):
            raise Blocked("win32_message_rejected")

    def control_text(self, hwnd, maximum=65536):
        length = self.message(hwnd, WM_GETTEXTLENGTH)
        if length > maximum:
            raise Blocked("control_text_too_long")
        value = C.create_unicode_buffer(length + 2)
        if self.message(hwnd, WM_GETTEXT, len(value), C.addressof(value)) == len(value) - 1:
            raise Blocked("control_text_changed")
        return value.value

    def set_text(self, hwnd, value):
        data = C.create_unicode_buffer(value)
        if not self.message(hwnd, WM_SETTEXT, 0, C.addressof(data)):
            raise Blocked("control_text_rejected")

    def replace_edit(self, hwnd, value):
        self.message(hwnd, EM_SETSEL, 0, -1)
        data = C.create_unicode_buffer(value)
        self.message(hwnd, EM_REPLACESEL, 1, C.addressof(data))

    def click(self, hwnd, x, y, *, double=False, right=False):
        position = packed(x, y)
        if right:
            self.post(hwnd, WM_RBUTTONDOWN, 2, position)
            self.post(hwnd, WM_RBUTTONUP, 0, position)
            return
        self.post(hwnd, WM_LBUTTONDOWN, 1, position)
        self.post(hwnd, WM_LBUTTONUP, 0, position)
        if double:
            self.post(hwnd, WM_LBUTTONDBLCLK, 1, position)
            self.post(hwnd, WM_LBUTTONUP, 0, position)

    def key_to(self, hwnd, key):
        scan = self.user.MapVirtualKeyW(key, 0) << 16
        self.post(hwnd, WM_KEYDOWN, key, 1 | scan)
        self.post(hwnd, WM_KEYUP, key, 0xc0000001 | scan)

    def tick(self):
        return self.kernel.GetTickCount()

    def last_input(self):
        state = LastInput(C.sizeof(LastInput))
        if not self.user.GetLastInputInfo(C.byref(state)):
            raise Blocked("user_input_status_unavailable")
        return state.dwTime

    def idle(self, milliseconds=400):
        own = getattr(self, "_own_ticks", ())

        def idle_tick():
            last = self.last_input()
            elapsed = (self.tick() - last) & 0xffffffff
            if milliseconds <= elapsed < 0x80000000 or last in own:
                if not any(self.user.GetAsyncKeyState(key) & 0x8000 for key in (1, 2, 4, 0x10, 0x11, 0x12, 0x5b, 0x5c)):
                    return (last,)
            return None

        result = until(idle_tick, 0.9)
        if result is None:
            raise Blocked("user_input_active")
        return result[0]

    def focus_info(self, hwnd=0):
        thread = self.user.GetWindowThreadProcessId(hwnd, None) if hwnd else 0
        info = GuiThread(C.sizeof(GuiThread))
        if not self.user.GetGUIThreadInfo(thread, C.byref(info)):
            raise Blocked("focus_unavailable")
        return info

    def focus_child(self, hwnd):
        target = self.user.GetWindowThreadProcessId(hwnd, None)
        current = self.kernel.GetCurrentThreadId()
        attached = current != target
        if attached:
            self.pump_messages()
            if not self.user.AttachThreadInput(current, target, True):
                raise Blocked("focus_unavailable")
        try:
            self.user.SetFocus(hwnd)
            if self.focus_info(hwnd).focus != hwnd:
                raise Blocked("focus_unavailable")
        finally:
            if attached:
                self.user.AttachThreadInput(current, target, False)

    def activate_foreground(self, hwnd, allowed_ticks=None, seconds=0.6):
        if not self.user.IsWindow(hwnd):
            return False
        if self.user.GetForegroundWindow() == hwnd and not self.user.IsIconic(hwnd):
            return True

        def try_attach():
            self.pump_messages()
            if self.user.IsIconic(hwnd):
                return False
            fg = self.user.GetForegroundWindow()
            if fg == hwnd:
                return True
            current = self.kernel.GetCurrentThreadId()
            fg_tid = self.user.GetWindowThreadProcessId(fg, None) if fg else 0
            tgt_tid = self.user.GetWindowThreadProcessId(hwnd, None)
            attached_fg = bool(fg_tid and fg_tid != current and self.user.AttachThreadInput(current, fg_tid, True))
            attached_tgt = bool(tgt_tid and tgt_tid not in (current, fg_tid)
                                and self.user.AttachThreadInput(current, tgt_tid, True))
            try:
                self.user.BringWindowToTop(hwnd)
                self.user.SetForegroundWindow(hwnd)
            finally:
                if attached_tgt:
                    self.user.AttachThreadInput(current, tgt_tid, False)
                if attached_fg:
                    self.user.AttachThreadInput(current, fg_tid, False)
            return self.user.GetForegroundWindow() == hwnd

        if until(try_attach, min(seconds, 0.35)):
            return True
        if any(self.user.GetAsyncKeyState(key) & 0x8000 for key in (1, 2, 4, 0x10, 0x11, 0x12, 0x5b, 0x5c)):
            return False
        if allowed_ticks is not None and self.last_input() not in allowed_ticks:
            return False
        stamp = self.tick()
        if allowed_ticks is not None:
            allowed_ticks.add(stamp)
        try:
            self.send_input([Input(1, InputData(keyboard=KeyboardInput(VK_UNASSIGNED, 0, 2, stamp, 0)))])
        except Blocked:
            return False
        return bool(until(try_attach, max(0.15, seconds - 0.35)))

    def pump_messages(self):
        # PeekMessage dispatches incoming sent messages, including
        # WM_DESTROYCLIPBOARD, and also creates this thread's message queue.
        self.user.PeekMessageW(C.byref(W.MSG()), None, 0, 0, 0)

    def input_keys(self, keys, stamp):
        values = [Input(1, InputData(keyboard=KeyboardInput(
            key, 0, (2 if up else 0) | (1 if key in (VK_HOME, VK_PRIOR, VK_END) else 0),
            stamp, 0))) for key, up in keys]
        self.send_input(values)

    def send_input(self, values):
        events = (Input * len(values))(*values)
        if self.user.SendInput(len(events), events, C.sizeof(Input)) != len(events):
            raise Blocked("keyboard_input_rejected")
        ticks = getattr(self, "_own_ticks", None)
        if ticks is not None:
            for event in values:
                stamp = event.value.keyboard.timestamp if event.kind == 1 else event.value.mouse.timestamp
                if stamp and stamp not in ticks:
                    ticks.append(stamp)

    def cursor(self):
        point = W.POINT()
        if not self.user.GetCursorPos(C.byref(point)):
            raise Blocked("cursor_unavailable")
        return point.x, point.y

    def mouse_click(self, x, y, stamp):
        self.send_input([self.mouse_move(x, y, stamp), self.mouse_button(2, stamp), self.mouse_button(4, stamp)])

    def mouse_move(self, x, y, stamp):
        left, top, width, height = (self.user.GetSystemMetrics(i) for i in (76, 77, 78, 79))
        if not (left <= x < left + width and top <= y < top + height):
            raise Blocked("click_target_off_screen")
        mx = ((x - left) * 65536 + width // 2) // width
        my = ((y - top) * 65536 + height // 2) // height
        return Input(0, InputData(mouse=MouseInput(mx, my, 0, 0xe001, stamp, 0)))

    @staticmethod
    def mouse_button(flags, stamp):
        return Input(0, InputData(mouse=MouseInput(0, 0, 0, flags, stamp, 0)))

    def arm_list(self, hwnd):
        _, _, _, height = self.rect(hwnd, client=True)
        if height < 16:
            return
        # EVA_VH_ListControl_Dblclk tracks internal widget focus separately from
        # Win32 SetFocus; a background click in the 4px left gutter arms shortcuts.
        self.click(hwnd, 4, height - 8)
        time.sleep(0.04)

    def clear_selection(self, hwnd):
        _, _, _, height = self.rect(hwnd, client=True)
        if height < 40:
            return
        # A short drag in the empty left gutter clears Kakao's selection.
        self.post(hwnd, WM_LBUTTONDOWN, 1, packed(5, 7))
        time.sleep(0.04)
        self.post(hwnd, WM_MOUSEMOVE, 1, packed(5, 33))
        time.sleep(0.04)
        self.post(hwnd, WM_LBUTTONUP, 0, packed(5, 33))
        self.click(hwnd, 5, height-8)
        time.sleep(0.03)

    def rect(self, hwnd, client=False):
        result = W.RECT()
        fn = self.user.GetClientRect if client else self.user.GetWindowRect
        if not fn(hwnd, C.byref(result)):
            raise Blocked("window_geometry_unavailable")
        return result.left, result.top, result.right, result.bottom

    def point(self, hwnd, x, y, *, to_screen=False):
        point = W.POINT(x, y)
        fn = self.user.ClientToScreen if to_screen else self.user.ScreenToClient
        if not fn(hwnd, C.byref(point)):
            raise Blocked("window_geometry_unavailable")
        return point.x, point.y

    def wheel(self, hwnd, delta):
        _, _, w, h = self.rect(hwnd, client=True)
        x, y = self.point(hwnd, w // 2, h // 2, to_screen=True)
        self.post(hwnd, WM_MOUSEWHEEL, packed(0, delta), packed(x, y))

    @contextmanager
    def dpi_aware(self):
        prior = self.user.SetThreadDpiAwarenessContext(C.c_void_p(-4))  # Per-monitor v2
        try:
            yield
        finally:
            if prior:
                self.user.SetThreadDpiAwarenessContext(prior)

    def capture(self, hwnd):
        # Probe the target with a bounded message before the synchronous PrintWindow.
        self.message(hwnd, WM_NULL)
        left, top, right, bottom = self.rect(hwnd)
        width, height = right - left, bottom - top
        if width <= 0 or height <= 0 or width * height > 20_000_000:
            raise Blocked("capture_geometry_invalid")
        dc = self.user.GetDC(None)
        if not dc:
            raise Blocked("window_capture_failed")
        memory = self.gdi.CreateCompatibleDC(dc)
        try:
            bits = C.c_void_p()
            info = BitmapInfo(C.sizeof(BitmapInfo), width, -height, 1, 32)
            bitmap = self.gdi.CreateDIBSection(dc, C.byref(info), 0, C.byref(bits), None, 0)
            if not memory or not bitmap or not bits:
                if bitmap:
                    self.gdi.DeleteObject(bitmap)
                raise Blocked("window_capture_failed")
            previous = self.gdi.SelectObject(memory, bitmap)
            try:
                C.memset(bits, 0, width * height * 4)
                if not self.user.PrintWindow(hwnd, memory, PW_RENDERFULLCONTENT):
                    raise Blocked("window_capture_failed")
                data = C.string_at(bits, width * height * 4)
                if not any(data[0::4]) and not any(data[1::4]) and not any(data[2::4]):
                    raise Blocked("window_capture_blank")
                return Frame(width, height, data)
            finally:
                if previous:
                    self.gdi.SelectObject(memory, previous)
                self.gdi.DeleteObject(bitmap)
        finally:
            if memory:
                self.gdi.DeleteDC(memory)
            self.user.ReleaseDC(None, dc)

"""KakaoTalk PC backend. All app access goes through the interactive desktop."""

from collections import Counter
from contextlib import contextmanager
from dataclasses import dataclass
from functools import wraps
import json
import os
from pathlib import Path
import re
import tempfile
import time

from kakaotalk_ax import AXOperations, AXSendEngine, boolean, integer, query_text, title, ui_session
from kakaotalk_state import digest
from kakaotalk_win32_api import Blocked, VK_HOME, VK_PRIOR, VK_RETURN, WM_CLOSE, Win32, until
from kakaotalk_win32_attachment import PhotoLocator, viewport_key
from kakaotalk_win32_clipboard import Clipboard, foreground
from kakaotalk_win32_ocr import OCR
from kakaotalk_win32_rows import blank_chat, room_items, transcript_rows


WIN32 = "windows_win32"
CLIPBOARD = "windows_clipboard"
OCR_SOURCE = "windows_ocr"
TOP_CLASSES = ("EVA_Window_Dblclk", "#32770")
LIST_CLASS = "EVA_VH_ListControl_Dblclk"
COMPOSER = "RICHEDIT50W"
TAB_POSITIONS = {1149: 26, 1150: 87, 1075: 143}
SNAPSHOT = re.compile(r"[0-9a-f]{24}")
SELF_ALIASES = frozenset({"나", "나와의채팅", "chatwithmyself", "me"})
PLACEHOLDERS = ("메시지 입력", "Enter a message", "Type a message")


def is_self_alias(value):
    compact = re.sub(r"\s+", "", value)
    return compact in SELF_ALIASES or compact.casefold() in SELF_ALIASES


def response(status="ok", source=WIN32, **values):
    return {"status": status, "source": source, "databaseCalls": 0, **values}


def desktop_operation(source=WIN32):
    def decorate(operation):
        @wraps(operation)
        def run(self, *args, **kwargs):
            try:
                with ui_session(), self.api.dpi_aware():
                    return operation(self, *args, **kwargs)
            except Blocked as error:
                return response("blocked", source, reason=str(error))
        return run
    return decorate


def win_text(value):
    if "\0" in value:
        raise ValueError("Win32 text fields do not accept NUL characters")
    return value


def snapshot_argument(value, required=False):
    if not isinstance(value, str) or ((required or value) and not SNAPSHOT.fullmatch(value)):
        raise ValueError("A current read_messages snapshot_id is required")


@dataclass(frozen=True)
class App:
    hwnd: int
    pid: int
    created: int
    online: int | None
    locked: bool


@dataclass(frozen=True)
class Chat:
    hwnd: int
    name: str
    pid: int
    created: int
    composer: int
    history: int
    search: int | None

    @property
    def identity(self):
        return [self.pid, self.created, self.hwnd, self.composer, self.history]


class WindowsAX(AXOperations):
    """Implement the same 13-tool surface with Win32 and the Windows clipboard."""

    source = CLIPBOARD

    def __init__(self, cache_root=None, *, api=None, clipboard=None, ocr=None, state_root=None):
        self.api = api if api is not None else Win32()
        self.user = self.api.user
        self._clipboard = clipboard
        self._ocr = ocr
        self._self_identity = None
        self._search_views = set()
        self._state_root = state_root

    @property
    def clipboard(self):
        if self._clipboard is None:
            self._clipboard = Clipboard(self.api)
        return self._clipboard

    @property
    def ocr(self):
        if self._ocr is None:
            self._ocr = OCR()
        return self._ocr

    def scale(self, hwnd, value):
        return round(value * (self.user.GetDpiForWindow(hwnd) or 96) / 96)

    def top(self, pid):
        return [self.api.window(hwnd) for hwnd in self.api.handles()
                if self.api.pid(hwnd) == pid and (self.user.IsWindowVisible(hwnd) or self.user.IsIconic(hwnd))]

    def app(self, *, ready=True):
        matches = []
        processes = {}
        for hwnd in self.api.handles():
            if self.api.class_name(hwnd) not in TOP_CLASSES or self.api.title(hwnd) not in ("카카오톡", "KakaoTalk"):
                continue
            pid = self.api.pid(hwnd)
            if pid not in processes:
                processes[pid] = self.api.process(pid)
            executable, created = processes[pid]
            if executable != "kakaotalk.exe":
                continue
            children = self.api.children(hwnd)
            active = [c.hwnd for c in children if c.name.startswith("OnlineMainView_") and c.control_id == 132]
            locked = any(c.name.startswith("LockModeView_") and c.control_id == 183 and self.user.IsWindowVisible(c.hwnd)
                         for c in children)
            matches.append(App(hwnd, pid, created, active[0] if len(active) == 1 else None, locked))
        if not matches:
            raise Blocked("app_not_running")
        # A chat titled 카카오톡 can also appear in EnumWindows.
        primary = [app for app in matches if app.online or app.locked]
        if len(primary or matches) != 1:
            raise Blocked("app_window_ambiguous")
        app = (primary or matches)[0]
        if ready:
            self.check_ready(app)
        return app

    def check_ready(self, app):
        if app.locked:
            raise Blocked("lock_mode_active")
        if not app.online:
            raise Blocked("not_logged_in")
        self.check_modal(app)

    def check_modal(self, app):
        if not self.user.IsWindowEnabled(app.hwnd):
            raise Blocked("modal_dialog_present")
        for window in self.top(app.pid):
            if window.hwnd == app.hwnd or not self.user.IsWindowVisible(window.hwnd):
                continue
            if not self.user.IsWindowEnabled(window.hwnd):
                raise Blocked("modal_dialog_present")
            owner = self.user.GetWindow(window.hwnd, 4)  # GW_OWNER
            if window.cls in ("tooltips_class32", "SysShadow"):
                continue
            if window.cls in TOP_CLASSES and self.chat(window, app):
                continue
            if owner == app.hwnd and window.cls == "EVA_Window_Dblclk" and not window.name:
                children = self.api.children(window.hwnd)
                if {5101, 5102, 5103, 5000}.issubset({c.control_id for c in children}) and any(
                        c.cls == "Chrome_WidgetWin_0" for c in children):
                    continue  # KakaoTalk's non-modal ad panel.
            if owner or window.cls in ("#32770", "EVA_Menu", "EVA_Window_Dblclk"):
                raise Blocked("modal_dialog_present")

    def chat(self, window, app):
        if window.hwnd == app.hwnd or window.cls not in TOP_CLASSES or not window.name:
            return None
        controls = self.api.children(window.hwnd)
        drafts = [c.hwnd for c in controls if c.cls == COMPOSER]
        histories = [c.hwnd for c in controls if c.cls == LIST_CLASS and c.control_id == 100
                     and self.user.GetParent(c.hwnd) == window.hwnd]
        searches = [c.hwnd for c in controls if c.cls == "Edit" and c.control_id == 100
                    and self.user.GetParent(c.hwnd) == window.hwnd]
        if len(drafts) == len(histories) == 1:
            return Chat(window.hwnd, window.name, app.pid, app.created, drafts[0], histories[0],
                        searches[0] if len(searches) == 1 else None)
        return None

    def chats(self, app):
        return [chat for window in self.top(app.pid) if (chat := self.chat(window, app))]

    def exact_chat(self, app, name, *, missing=False):
        matches = [chat for chat in self.chats(app) if chat.name == name]
        if len(matches) > 1:
            raise Blocked("target_room_window_ambiguous")
        if not matches and not missing:
            raise Blocked("target_room_window_not_open")
        return matches[0] if matches else None

    def check_chat(self, expected, *, modal=False):
        app = self.app(ready=modal)
        actual = self.exact_chat(app, expected.name)
        if actual.identity != expected.identity or self.api.title(expected.hwnd) != expected.name:
            raise Blocked("target_room_window_changed")
        return actual

    def windows(self):
        """Enumerate top-level chat HWNDs without activation or transcript reads."""
        try:
            with self.api.dpi_aware():
                app = self.app(ready=False)
                chats = self.chats(app)
                counts = Counter(chat.name for chat in chats)
                reason = None
                try:
                    self.check_ready(app)
                except Blocked as error:
                    reason = str(error)
                return response(processId=app.pid, items=[
                    {"windowTitle": chat.name, "minimized": bool(self.user.IsIconic(chat.hwnd)),
                     "readable": not reason and counts[chat.name] == 1 and bool(self.user.IsWindowEnabled(chat.hwnd)),
                     **({"reason": reason} if reason else {})}
                    for chat in chats])
        except Blocked as error:
            return response("blocked", reason=str(error), items=[])

    def build(self, kind="send"):
        """Check the official app and Korean WinRT OCR without a compiler."""
        if kind not in ("send", "read", "control"):
            raise ValueError("Unknown native bridge kind")
        try:
            with self.api.dpi_aware():
                self.app(ready=False)
                with self.ocr.session("ko"):
                    pass
        except Blocked as error:
            raise ValueError(str(error)) from error

    def tab(self, app, tab_id):
        if not self.user.IsWindowVisible(app.hwnd) or self.user.IsIconic(app.hwnd):
            raise Blocked("main_window_not_visible")
        tabs = [c.hwnd for c in self.api.children(app.online) if c.control_id == tab_id and
                self.user.GetParent(c.hwnd) == app.online]
        if len(tabs) != 1:
            raise Blocked("main_tab_unavailable")
        if not self.user.IsWindowVisible(tabs[0]):
            self.api.click(app.online, self.scale(app.hwnd, 31), self.scale(app.hwnd, TAB_POSITIONS[tab_id]))
            if not until(lambda: self.user.IsWindowVisible(tabs[0])):
                raise Blocked("main_tab_not_observed")
        return tabs[0]

    def room_controls(self, app):
        view = self.tab(app, 1150)
        children = self.api.children(view)
        edit = [c.hwnd for c in children if c.cls == "Edit" and c.control_id == 100]
        listing = [c.hwnd for c in children if c.name.startswith("ChatRoomListCtrl_") and c.control_id == 1003]
        search = [c.hwnd for c in children if c.name.startswith("SearchListCtrl_") and c.control_id == 1003]
        if len(edit) != 1 or len(listing) != 1 or len(search) != 1:
            raise Blocked("room_list_controls_unavailable")
        return edit[0], listing[0], search[0]

    def room_query(self, controls, query):
        edit, default, search = controls
        self.api.set_text(edit, query)
        desired = search if query else default
        if not until(lambda: self.api.control_text(edit) == query and self.user.IsWindowVisible(desired), 0.9):
            raise Blocked("room_search_not_observed")
        # Let the UI finish publishing filtered rows, including a legitimate empty result.
        time.sleep(0.12)
        return desired

    def list_rows(self, hwnd):
        with self.ocr.session("ko") as recognize:
            return room_items(self.api.capture(hwnd), recognize, self.user.GetDpiForWindow(hwnd) or 96)

    @staticmethod
    def room_output(items, query, search_query, total_exposed):
        return response(source=OCR_SOURCE, textExact=False, query=query, searchQuery=search_query,
                        scope="currently_visible_room_list", total=len(items), totalExposed=total_exposed,
                        reportedRowCount=None,
                        items=[{k: v for k, v in item.items() if k != "_point"} for item in items])

    @desktop_operation(OCR_SOURCE)
    def rooms(self, query=""):
        win_text(query_text(query, empty=True))
        app = self.app()
        controls = self.room_controls(app)
        previous = self.api.control_text(controls[0])
        listing = controls[2] if previous else controls[1]
        if not self.user.IsWindowVisible(listing):
            raise Blocked("room_list_not_visible")
        items = self.list_rows(listing)
        filtered = [item for item in items if query.casefold() in item["name"].casefold()]
        return self.room_output(filtered, query, previous, len(items))

    @desktop_operation(OCR_SOURCE)
    def search_rooms(self, query):
        win_text(query_text(query, empty=True))
        app = self.app()
        self.api.idle()
        listing = self.room_query(self.room_controls(app), query)
        items = self.list_rows(listing)
        return self.room_output(items, query, query, len(items))

    def switch_foreground(self, hwnd, allowed_ticks=None):
        if self.user.GetForegroundWindow() != hwnd:
            self.user.SetForegroundWindow(hwnd)
        if self.user.GetForegroundWindow() != hwnd:
            self.api.activate_foreground(hwnd, allowed_ticks)
        return bool(until(lambda: self.user.GetForegroundWindow() == hwnd, 0.4))

    @contextmanager
    def opening(self, app):
        original = self.user.GetForegroundWindow()
        allowed_ticks = {self.api.idle()}

        def no_input():
            if self.api.last_input() not in allowed_ticks:
                raise Blocked("user_input_active")

        no_input.allowed_ticks = allowed_ticks
        try:
            yield no_input
        finally:
            current = self.user.GetForegroundWindow()
            if (current and current != original and self.api.pid(current) == app.pid
                    and self.api.last_input() in allowed_ticks):
                if original and self.user.IsWindow(original):
                    if not self.switch_foreground(original, allowed_ticks):
                        raise Blocked("foreground_restore_failed")

    def verify_self(self, app, name, expected=None):
        if expected and self._self_identity == expected.identity:
            return expected, True
        if name is None and expected is None and self._self_identity:
            cached = [c for c in self.chats(app) if c.identity == self._self_identity]
            if len(cached) == 1:
                return self.check_chat(cached[0], modal=True), True
        initial_tab = next((c.control_id for c in self.api.children(app.online)
                            if c.control_id in TAB_POSITIONS and self.user.IsWindowVisible(c.hwnd)), None)
        with self.opening(app) as no_input:
            if not self.user.IsWindowVisible(app.hwnd) or self.user.IsIconic(app.hwnd):
                raise Blocked("main_window_not_visible")
            existing_chats = self.chats(app)
            before = {chat.hwnd for chat in existing_chats}
            before_iconic = {chat.hwnd for chat in existing_chats if self.user.IsIconic(chat.hwnd)}
            if not self.switch_foreground(app.hwnd, no_input.allowed_ticks):
                raise Blocked("foreground_unavailable")
            no_input()
            view = self.tab(app, 1149)
            controls = self.api.children(view)
            candidates = [c.hwnd for c in controls if c.name.startswith("ContactListCtrl_") and
                          self.user.IsWindowVisible(c.hwnd)]
            if len(candidates) != 1:
                raise Blocked("self_chat_unverified")
            menu = None
            created = None
            try:
                no_input()
                self.api.click(candidates[0], self.scale(app.hwnd, 104), self.scale(app.hwnd, 26), right=True)
                menu = until(lambda: next((w.hwnd for w in self.top(app.pid) if w.cls == "EVA_Menu" and
                                           self.user.GetWindow(w.hwnd, 4) == app.hwnd), None))
                if not menu:
                    raise Blocked("self_chat_unverified")
                def painted_menu():
                    try:
                        return self.api.capture(menu)
                    except Blocked as error:
                        if str(error) not in ("window_capture_blank", "window_capture_failed"):
                            raise
                        return None

                frame = until(painted_menu, 0.55)
                if not frame:
                    raise Blocked("self_chat_unverified")
                with self.ocr.session("ko") as recognize:
                    lines = recognize(frame.scale(2))
                labels = {re.sub(r"\s+", "", line.text) for line in lines}
                self_item = [line for line in lines if re.sub(r"\s+", "", line.text) in ("나와의", "나와의채팅", "Chatwithmyself")
                             and line.y / 2 < self.scale(menu, 30)]
                # The self-profile menu has a dedicated chat command AND Edit
                # Profile. Friend/group menus do not have this combination.
                if (len(self_item) != 1 or not {"프로필보기", "프로필편집"}.issubset(labels)
                        or not self.user.IsWindowVisible(menu)):
                    raise Blocked("self_chat_unverified")
                no_input()
                prior = self.user.GetForegroundWindow()
                self.api.click(menu, self.scale(menu, 30), self.scale(menu, 15))

                def self_opened():
                    if self.user.IsWindowVisible(menu):
                        return None
                    active = self.user.GetForegroundWindow()
                    chats = self.chats(app)
                    new = [chat for chat in chats if chat.hwnd not in before]
                    if len(new) == 1:
                        return new[0]
                    if not new:
                        if active != prior:
                            found = next((chat for chat in chats if chat.hwnd == active), None)
                            if found:
                                return found
                        unminimized = [chat for chat in chats
                                       if chat.hwnd in before_iconic and not self.user.IsIconic(chat.hwnd)]
                        if len(unminimized) == 1:
                            return unminimized[0]
                    return None

                observed = until(self_opened, 1.5)
                if not observed:
                    raise Blocked("self_chat_unverified")
                if observed.hwnd not in before:
                    created = observed
                no_input()
                if (name is not None and observed.name != name) or (expected and observed.identity != expected.identity):
                    raise Blocked("self_chat_unverified")
                chat = self.check_chat(observed, modal=True)
                self._self_identity = chat.identity
                created = None
                return chat, chat.hwnd in before
            finally:
                if menu and self.user.IsWindowVisible(menu):
                    self.api.post(menu, WM_CLOSE)
                    until(lambda: not self.user.IsWindowVisible(menu), 0.3)
                no_input()
                if created:
                    self.close_chat(created)
                if initial_tab and initial_tab != 1149:
                    self.tab(app, initial_tab)

    @desktop_operation()
    def open_room(self, window_title, require_self=False):
        win_text(title(window_title))
        boolean(require_self, "require_self")
        app = self.app()
        existing = self.exact_chat(app, window_title, missing=True)
        if require_self:
            target_name = None if existing is None and is_self_alias(window_title) else window_title
            chat, already = self.verify_self(app, target_name, existing)
            return response(windowTitle=chat.name, alreadyOpen=already, isSelf=True)
        if existing:
            return response(windowTitle=existing.name, alreadyOpen=True)

        with self.opening(app) as no_input:
            controls = self.room_controls(app)
            previous = self.api.control_text(controls[0])
            opened = None
            try:
                no_input()
                listing = self.room_query(controls, window_title)
                rows = self.list_rows(listing)
                # Spaces introduced/omitted by OCR are used only to find a
                # unique candidate. GetWindowTextW is the final authority.
                key = lambda text: re.sub(r"\s+", "", text)
                matches = [row for row in rows if key(row["name"]) == key(window_title)]
                if len(matches) != 1:
                    raise Blocked("target_room_ambiguous" if matches else "room_not_in_exposed_list")
                before = {chat.hwnd for chat in self.chats(app)}
                no_input()
                self.check_modal(app)
                verified = self.list_rows(listing)
                if [r for r in verified if key(r["name"]) == key(window_title)] != matches:
                    raise Blocked("room_list_changed")
                no_input()
                self.api.click(listing, *matches[0]["_point"], double=True)
                new = until(lambda: [chat for chat in self.chats(app) if chat.hwnd not in before], 1.5)
                if not new or len(new) != 1:
                    raise Blocked("room_open_not_observed")
                opened = new[0]
                if opened.name != window_title:
                    self.close_chat(opened)
                    raise Blocked("target_room_title_mismatch")
                self.check_chat(opened, modal=True)
                return response(windowTitle=opened.name, alreadyOpen=False, roomSearchUsed=True, roomSearchRestored=True)
            finally:
                if self.api.control_text(controls[0]) == window_title:
                    self.room_query(controls, previous)

    def _composer_frame_empty(self, chat):
        frame = self.api.capture(chat.hwnd)
        s = lambda n: self.scale(chat.hwnd, n)
        if frame.width < s(210) or frame.height < s(140):
            return False
        colors = [frame.pixel(frame.width - s(54), frame.height - s(y)) for y in (18, 22, 26)]
        if not all(color in ((240, 240, 240), (242, 242, 242)) for color in colors):
            return False
        left, top, _, _ = self.api.rect(chat.hwnd)
        x, y, _, _ = self.api.rect(chat.composer)
        x, y = x-left, y-top
        if not (0 <= x < frame.width - s(138) and 0 <= y < frame.height - s(23)):
            return False
        ink = [min(frame.pixel(col, line)) for line in range(y+s(3), y+s(22))
               for col in range(x+s(4), x+s(135))]
        return bool(ink) and 100 <= min(ink) < 220

    def composer_empty(self, chat):
        value = self.api.control_text(chat.composer)
        if not value:
            return True
        if value not in PLACEHOLDERS:
            return False
        # This RichEdit uses real grey text for its idle prompt. Require the
        # disabled send button and the grey prompt before treating it as empty.
        if self.user.IsIconic(chat.hwnd):
            with foreground(self.api, chat.hwnd):
                until(lambda: (r := self.api.rect(chat.hwnd))[2] - r[0] >= self.scale(chat.hwnd, 210), 0.4)
                if self.api.control_text(chat.composer) != value:
                    return False
                return self._composer_frame_empty(chat)
        return self._composer_frame_empty(chat)

    def send_button_ready(self, chat):
        frame = self.api.capture(chat.hwnd)
        s = lambda n: self.scale(chat.hwnd, n)
        if frame.width < s(70) or frame.height < s(32):
            return False
        # The enabled native send button has a yellow fill. Requiring the
        # armed state also catches programmatic edits Kakao didn't accept.
        colors = [frame.pixel(frame.width-s(x), frame.height-s(y)) for x, y in ((54, 18), (48, 22), (14, 26))]
        return all(r >= 200 and g >= 160 and r-b >= 110 and g-b >= 100 for b, g, r in colors)

    def close_chat(self, chat):
        self.check_chat(chat, modal=True)
        before_text = self.api.control_text(chat.composer)
        if not self.composer_empty(chat):
            raise Blocked("existing_draft_preserved")
        input_at = self.api.idle()
        self.check_chat(chat, modal=True)
        if self.user.IsIconic(chat.hwnd) and before_text in PLACEHOLDERS:
            if self.api.control_text(chat.composer) != before_text:
                raise Blocked("existing_draft_preserved")
        elif not self.composer_empty(chat):
            raise Blocked("existing_draft_preserved")
        if self.api.last_input() != input_at:
            raise Blocked("user_input_active")
        self.api.post(chat.hwnd, WM_CLOSE)
        if not until(lambda: not self.user.IsWindow(chat.hwnd) or not self.user.IsWindowVisible(chat.hwnd)):
            raise Blocked("room_close_not_observed")
        if self._self_identity == chat.identity:
            self._self_identity = None

    @desktop_operation()
    def close_room(self, window_title):
        win_text(title(window_title))
        chat = self.exact_chat(self.app(), window_title, missing=True)
        if chat:
            self.close_chat(chat)
        return response(windowTitle=window_title, alreadyClosed=chat is None)

    def arm_history(self, chat, guard):
        if hasattr(self.api, "arm_list"):
            guard.check()
            self.api.arm_list(chat.history)
        guard.focus(chat.history)

    def after_copy(self, chat, guard):
        if self.user.IsWindow(chat.history) and self.user.GetForegroundWindow() == chat.hwnd:
            guard.check()
            self.api.clear_selection(chat.history)

    def copied(self, chat, guard, *, select_all=True):
        if select_all:
            self.arm_history(chat, guard)
        else:
            guard.focus(chat.history)
        sent = False

        def perform():
            nonlocal sent
            if select_all:
                guard.keys(chat.history, 65, control=True)
                time.sleep(0.025)
                guard.keys(chat.history, 67, control=True)
            else:
                guard.keys(chat.history, 67, control=True)
            sent = True

        try:
            return self.clipboard.copy_chat(chat.pid, perform, guard, lambda: self.check_chat(chat))
        finally:
            if sent and select_all:
                self.after_copy(chat, guard)

    def snapshot(self, chat, guard=None):
        started = time.monotonic()

        def read(protection):
            try:
                return self.copied(chat, protection)
            except Blocked as error:
                if str(error) != "clipboard_copy_not_observed":
                    raise
                history_frame = self.api.capture(chat.history)
                if blank_chat(history_frame):
                    self.check_chat(chat)
                    protection.check()
                    return ""
                if history_frame.height >= 16:
                    protection.click(chat.history, 4, history_frame.height - 8)
                    return self.copied(chat, protection)
                raise

        if guard is None:
            with foreground(self.api, chat.hwnd) as protection:
                text = read(protection)
        else:
            text = read(guard)
        rows = transcript_rows(text)
        snapshot_id = digest(json.dumps([chat.name, chat.identity, text], ensure_ascii=False))[:24]
        return snapshot_id, rows, round((time.monotonic()-started)*1000, 1)

    @desktop_operation(CLIPBOARD)
    def read_messages(self, window_title, limit=50, offset=0, snapshot_id=""):
        win_text(title(window_title))
        integer(limit, "limit", 1, 500)
        integer(offset, "offset", 0, 1000)
        snapshot_argument(snapshot_id, required=bool(offset))
        chat = self.exact_chat(self.app(), window_title)
        actual, rows, elapsed = self.snapshot(chat)
        if snapshot_id and snapshot_id != actual:
            return response("snapshot_changed", CLIPBOARD, reason="Clipboard rows changed; restart at offset 0",
                            snapshotId=actual, items=[], uiActions=1)
        exposed = len(rows)
        rows = rows[-1000:]
        end = max(0, len(rows)-offset)
        start = max(0, end-limit)
        counts = Counter(row["kind"] for row in rows)
        return response(source=CLIPBOARD, windowTitle=chat.name, items=rows[start:end], total=len(rows),
                        limit=limit, offset=offset, hasMore=start > 0, nextOffset=offset+end-start if start else None,
                        snapshotId=actual, textExact=True, textMessageCount=counts["text"],
                        unclassifiedRowCount=counts["unclassified"], attachmentRowCount=counts["attachment"],
                        coverage={"scope": "currently_copied_chat_rows", "historyComplete": False,
                                  "consistency": "best_effort", "exposedRowCount": exposed,
                                  "rowsTruncated": exposed > len(rows), "rowStart": exposed-len(rows),
                                  "rowLimit": 1000, "scrollPerformed": False},
                        uiActions=1, elapsedMs=elapsed)

    @desktop_operation(CLIPBOARD)
    def read_many(self, window_titles, limit=50, open_if_needed=True, close_after=False, max_workers=3):
        if not isinstance(window_titles, list) or not 1 <= len(window_titles) <= 20:
            raise ValueError("window_titles must contain 1..20 exact titles")
        for name in window_titles:
            win_text(title(name))
        if len(set(window_titles)) != len(window_titles):
            raise ValueError("Duplicate window_titles are not allowed")
        integer(limit, "limit", 1, 500)
        integer(max_workers, "max_workers", 1, 4)
        boolean(open_if_needed, "open_if_needed")
        boolean(close_after, "close_after")
        started = time.monotonic()
        items, opened, cleanup = [], [], []
        try:
            for name in window_titles:
                begin = time.monotonic()
                try:
                    result = self.open_room(name) if open_if_needed else {"status": "ok", "alreadyOpen": True}
                    if result["status"] == "ok":
                        if not result["alreadyOpen"]:
                            opened.append(self.exact_chat(self.app(ready=False), name))
                        result = self.read_messages(name, limit=limit)
                except Blocked as error:
                    result = response("blocked", CLIPBOARD, reason=str(error))
                except Exception as error:
                    result = response("failed", CLIPBOARD, errorType=type(error).__name__)
                items.append({"windowTitle": name, **result,
                              "callElapsedMs": round((time.monotonic()-begin)*1000, 1)})
        finally:
            if close_after:
                for chat in reversed(opened):
                    try:
                        self.close_chat(chat)
                        cleanup.append(response(windowTitle=chat.name, alreadyClosed=False))
                    except Blocked as error:
                        cleanup.append(response("blocked", reason=str(error), windowTitle=chat.name))
                    except Exception as error:
                        cleanup.append(response("failed", errorType=type(error).__name__, windowTitle=chat.name))
        succeeded = sum(item["status"] == "ok" for item in items)
        status = "ok" if succeeded == len(items) and all(item["status"] == "ok" for item in cleanup) else "partial"
        return response(status, CLIPBOARD, items=items, total=len(items), succeeded=succeeded,
                        windowsOpened=[chat.name for chat in opened], cleanup=cleanup, parallel=False,
                        atomicAcrossRooms=False, elapsedMs=round((time.monotonic()-started)*1000, 1))

    def search_active(self, chat):
        if not chat.search or not self.user.IsWindowVisible(chat.search):
            return False
        x, y, right, bottom = self.api.rect(chat.search)
        list_top = self.api.rect(chat.history)[1]
        return right > x and bottom > y and list_top >= bottom

    def navigation_position(self, chat):
        return viewport_key(self.api.capture(chat.history))

    def search_bar(self, chat, guard):
        if self.search_active(chat):
            return
        self.arm_history(chat, guard)
        guard.keys(chat.history, 70, control=True)
        if not until(lambda: self.search_active(chat), 0.2):
            _, _, width, _ = self.api.rect(chat.hwnd, client=True)
            guard.check()
            self.api.click(chat.hwnd, width-self.scale(chat.hwnd, 81), self.scale(chat.hwnd, 58))
            if not until(lambda: self.search_active(chat), 0.5):
                raise Blocked("message_search_bar_unavailable")

    def search_command(self, chat, action, query):
        with foreground(self.api, chat.hwnd) as guard:
            self.check_chat(chat, modal=True)
            if action == "search_query":
                self._search_views.clear()
                self.search_bar(chat, guard)
                guard.focus(chat.search)
                before = self.navigation_position(chat)
                self.api.set_text(chat.search, query)
                if self.api.control_text(chat.search) != query:
                    raise Blocked("search_query_changed")
                guard.keys(chat.search, VK_RETURN)
                time.sleep(0.20)
                width = self.api.rect(chat.search)[2] - self.api.rect(chat.search)[0]
                after = self.navigation_position(chat)
                verified = self.search_active(chat) and (after != before or width <= self.scale(chat.hwnd, 105))
                self._search_views.add(after)
            else:
                if not self.search_active(chat) or self.api.control_text(chat.search) != query:
                    raise Blocked("search_query_changed")
                _, top, right, bottom = self.api.rect(chat.search)
                origin = self.api.point(chat.hwnd, right, (top+bottom)//2)
                x, y = origin[0]+self.scale(chat.hwnd, 39), origin[1]
                _, _, width, height = self.api.rect(chat.hwnd, client=True)
                if not (0 < x < width-self.scale(chat.hwnd, 55) and 0 < y < height):
                    raise Blocked("search_navigation_unavailable")
                prior_view = self.navigation_position(chat)
                guard.click(chat.hwnd, x, y)
                after = until(lambda: (view if (view := self.navigation_position(chat)) != prior_view else None),
                              0.65, 0.09)
                guard.check()
                if not after or after in self._search_views:
                    return response(windowTitle=chat.name, searchOutcome="navigation_exhausted",
                                    searchVerified=True, searchState={"previousEnabled": False})
                self._search_views.add(after)
                verified = self.search_active(chat) and self.api.control_text(chat.search) == query
            guard.check()
            return response("ok" if verified else "partial", windowTitle=chat.name, searchVerified=verified,
                            **({} if verified else {"reason": "search_result_not_observed"}))

    def _control(self, action, *, windowTitle, query=""):
        try:
            chat = self.exact_chat(self.app(), windowTitle)
            if action == "load_older":
                with foreground(self.api, chat.hwnd) as guard:
                    self.arm_history(chat, guard)
                    guard.keys(chat.history, VK_HOME, VK_PRIOR)
                    guard.check()
                    self.api.wheel(chat.history, 360)
                    time.sleep(0.23)
                    guard.check()
                return response(windowTitle=chat.name, scrollRequested=True)
            return self.search_command(chat, action, query)
        except Blocked as error:
            return response("blocked", reason=str(error))

    @desktop_operation(CLIPBOARD)
    def load_more(self, window_title, pages=1):
        return super().load_more(window_title, pages)

    def _searchable(self, row):
        return row["kind"] == "text"

    @desktop_operation(CLIPBOARD)
    def search_messages(self, window_title, query, limit=100, load_pages=0, case_sensitive=False,
                        use_app_search=True, max_steps=10, context_rows=2):
        win_text(query_text(query))
        result = super().search_messages(window_title, query, limit, load_pages, case_sensitive,
                                         use_app_search, max_steps, context_rows)
        if result.get("stopReason") == "user_input_active" and not result["items"]:
            return {**result, "status": "blocked", "reason": "user_input_active"}
        if use_app_search and result.get("searchVerified") and not result["items"]:
            return {**result, "status": "partial", "searchVerified": False,
                    "stopReason": "search_result_not_observed"}
        return result

    @desktop_operation()
    def close_search(self, window_title):
        win_text(title(window_title))
        chat = self.exact_chat(self.app(), window_title)
        if not self.search_active(chat):
            return response(windowTitle=chat.name, alreadyClosed=True)
        with foreground(self.api, chat.hwnd) as guard:
            draft = self.api.control_text(chat.composer)
            _, top, _, bottom = self.api.rect(chat.search)
            _, y = self.api.point(chat.hwnd, 0, (top+bottom)//2)
            _, _, width, _ = self.api.rect(chat.hwnd, client=True)
            guard.click(chat.hwnd, width-self.scale(chat.hwnd, 23), y)
            if not until(lambda: not self.search_active(chat)):
                raise Blocked("search_close_not_observed")
            if draft and draft not in ("메시지 입력", "Enter a message", "Type a message") and self.api.control_text(chat.composer) != draft:
                raise Blocked("composer_changed")
        return response(windowTitle=chat.name, alreadyClosed=False)

    @desktop_operation()
    def read_attachment(self, window_title, row_index, snapshot_id, output_path=None, ocr=False):
        win_text(title(window_title))
        integer(row_index, "row_index", 0, 1_000_000)
        snapshot_argument(snapshot_id, required=True)
        boolean(ocr, "ocr")
        if output_path is not None and (not isinstance(output_path, str) or not Path(output_path).is_absolute()):
            raise ValueError("output_path must be an absolute path")
        if output_path and Path(output_path).exists():
            raise ValueError("output_path already exists")
        chat = self.exact_chat(self.app(), window_title)
        with foreground(self.api, chat.hwnd) as guard:
            current, rows, _ = self.snapshot(chat, guard)
            if current != snapshot_id:
                return response("snapshot_changed", reason="Read the attachment row again", snapshotId=current)
            row = rows[row_index] if row_index < len(rows) else None
            if not row or not any(item["type"] == "image" for item in row["attachments"]):
                raise Blocked("attachment_row_required")
            image, restored = PhotoLocator(self, chat, guard, rows, row).locate()
            current, _, _ = self.snapshot(chat, guard)
            if current != snapshot_id:
                return response("snapshot_changed", reason="Read the attachment row again", snapshotId=current)
        extra = {}
        if ocr:
            with self.ocr.session("ko") as recognize:
                extra = {"ocr": "\n".join(line.text for line in recognize(image)), "ocrSource": OCR_SOURCE}
        png = image.png()
        if output_path:
            descriptor = os.open(output_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0), 0o600)
        else:
            descriptor, output_path = tempfile.mkstemp(prefix="kax-attachment-", suffix=".png")
        try:
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(png)
        except BaseException:
            Path(output_path).unlink(missing_ok=True)
            raise
        return response(windowTitle=chat.name, rowIndex=row_index, snapshotId=snapshot_id, imagePath=str(output_path),
                        width=image.width, height=image.height, scrollRestored=restored, ocrPerformed=ocr, **extra)

    def send_engine(self):
        return WindowsSendEngine(self) if self._state_root is None else WindowsSendEngine(self, self._state_root)

    @desktop_operation()
    def send_message(self, window_title, text, operation_id, wait_seconds=3, require_self=False):
        win_text(title(window_title))
        if isinstance(text, str):
            win_text(text)
        return self.send_engine().send(window_title, text, operation_id, wait_seconds, require_self)

    @desktop_operation()
    def send_result(self, operation_id, wait_seconds=0):
        return self.send_engine().result(operation_id, wait_seconds)


def composer_text(value):
    return value.replace("\r\n", "\n")


class WindowsSendEngine(AXSendEngine):
    source = WIN32
    confirmation_source = CLIPBOARD
    unknown_reason = "windows_confirmation_not_established"

    def text_hash(self, text):
        return self.hash(composer_text(text))

    def body_hashes(self, result):
        return [self.text_hash(row["text"]) for row in result.get("items", []) if row["kind"] == "text"]

    def prepare(self, window_title, require_self):
        try:
            app = self.native.app()
            chat = self.native.exact_chat(app, window_title)
            if not self.native.composer_empty(chat):
                raise Blocked("existing_draft_preserved")
            if require_self:
                self.native.verify_self(app, window_title, chat)
            return response(windowTitle=chat.name), {"windowIdentity": chat.identity}
        except Blocked as error:
            return response("blocked", reason=str(error)), {}

    def build_for_send(self):
        pass

    def bound_chat(self, record):
        chat = self.native.exact_chat(self.native.app(), record["windowTitle"])
        if chat.identity != record.get("windowIdentity"):
            raise Blocked("target_room_window_changed")
        return chat

    def read_for_confirmation(self, record):
        try:
            chat = self.bound_chat(record)
            result = self.native.read_messages(chat.name, limit=500)
            self.native.check_chat(chat, modal=True)
            return result
        except Blocked as error:
            return response("blocked", CLIPBOARD, reason=str(error))

    def inspect_for_confirmation(self, record):
        return response("ready" if self.native.composer_empty(self.bound_chat(record)) else "blocked")

    def dispatch(self, record, text):
        api = self.native.api
        written = attempted = False
        try:
            chat = self.bound_chat(record)
            with foreground(api, chat.hwnd) as guard:
                try:
                    guard.focus(chat.composer)
                    if not self.native.composer_empty(chat):
                        raise Blocked("existing_draft_preserved")
                    _, _, width, height = api.rect(chat.hwnd, client=True)
                    x = width-self.native.scale(chat.hwnd, 35)
                    y = height-self.native.scale(chat.hwnd, 22)
                    _, _, _, editor_bottom = api.rect(chat.composer)
                    if api.point(chat.hwnd, x, y, to_screen=True)[1] <= editor_bottom:
                        raise Blocked("send_button_geometry_invalid")
                    guard.visible_point(chat.hwnd, x, y)
                    written = True
                    api.replace_edit(chat.composer, text)
                    if composer_text(api.control_text(chat.composer)) != composer_text(text):
                        raise Blocked("composer_text_not_exact")
                    if not until(lambda: self.native.send_button_ready(chat), 0.45):
                        raise Blocked("send_button_not_enabled")
                    self.bound_chat(record)
                    if composer_text(api.control_text(chat.composer)) != composer_text(text):
                        raise Blocked("composer_text_not_exact")
                    guard.check()
                    attempted = True
                    guard.click(chat.hwnd, x, y)
                finally:
                    if written and not attempted:
                        # Revert only an untouched, exactly matching helper draft.
                        guard.check()
                        if (self.bound_chat(record).identity == chat.identity and
                                composer_text(api.control_text(chat.composer)) == composer_text(text)):
                            api.replace_edit(chat.composer, "")
            return response("dispatch_requested", dispatchAttempted=True)
        except Blocked as error:
            return response("dispatch_result_unknown" if attempted else "blocked", reason=str(error),
                            dispatchAttempted=True if attempted else False)

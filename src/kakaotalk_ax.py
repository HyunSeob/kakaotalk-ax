"""Native macOS Accessibility operations for KakaoTalk."""

import hashlib
import json
import os
import re
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from pathlib import Path

from kakaotalk_state import open_private, process_lock, state_directory

# Preserve the existing journals and shared lock to avoid duplicate dispatches
# across old/new command names. These directories contain helper state, not app data.
UI_ROOT = state_directory() / "ax-ui"
AX_STATE_ROOT = state_directory() / "ax-send-operations"
_mutex = threading.RLock()
_local = threading.local()


@contextmanager
def ui_session():
    """Serialize AX mutations across processes; nested calls in one thread share the lock."""
    with _mutex:
        if getattr(_local, "depth", 0):
            _local.depth += 1
            try:
                yield
            finally:
                _local.depth -= 1
            return
        with process_lock(UI_ROOT, "Another KakaoTalk AX operation is in progress"):
            _local.depth = 1
            try:
                yield
            finally:
                _local.depth = 0


def title(value):
    if not isinstance(value, str) or not value.strip() or len(value.encode("utf-8")) > 4096:
        raise ValueError("An exact window_title of 1..4096 UTF-8 bytes is required")
    return value


def integer(value, name, low, high):
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise ValueError(f"{name} must be an integer between {low} and {high}")
    return value


def boolean(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def query_text(value, empty=False):
    if not isinstance(value, str) or (not empty and not value.strip()) or len(value.encode("utf-8")) > 4096:
        raise ValueError("query must be a string of at most 4096 UTF-8 bytes")
    return value


class AXOperations:
    source = "macos_accessibility"

    def _searchable(self, row):
        return row.get("text") is not None

    def rooms(self, query=""):
        """Read the exposed main chat list; query is a literal local name filter."""
        query_text(query, empty=True)
        with ui_session():
            result = self._control("rooms")
        if result.get("status") != "ok":
            return result
        items = result["items"]
        filtered = [item for item in items if query.casefold() in item["name"].casefold()]
        return {**result, "items": filtered, "query": query, "total": len(filtered), "totalExposed": len(items)}

    def search_rooms(self, query):
        """Set the official app's room-search field and return its exposed results."""
        query_text(query, empty=True)
        with ui_session():
            return self._control("search_rooms", query=query)

    def open_room(self, window_title, require_self=False):
        title(window_title)
        boolean(require_self, "require_self")
        with ui_session():
            result = self._control("open", windowTitle=window_title, requireSelf=require_self)
            if result.get("reason") != "room_not_in_exposed_list":
                return result
            previous = self._control("rooms")
            if previous.get("status") != "ok":
                return previous
            search = self.search_rooms(window_title)
            try:
                result = self._control("open", windowTitle=window_title, requireSelf=require_self) if search.get("status") == "ok" else search
            finally:
                try:
                    restored = self.search_rooms(previous.get("searchQuery", ""))
                except Exception as error:
                    restored = {"status": "failed", "errorType": type(error).__name__}
            return {**result, "roomSearchUsed": True,
                    "roomSearchRestored": restored.get("status") == "ok"}

    def close_room(self, window_title):
        """Close a chat window, never leave a room; preserve nonempty message drafts."""
        title(window_title)
        with ui_session():
            return self._control("close", windowTitle=window_title)

    def close_search(self, window_title):
        title(window_title)
        with ui_session():
            return self._control("search_close", windowTitle=window_title)

    def read_many(self, window_titles, limit=50, open_if_needed=True, close_after=False, max_workers=3):
        if not isinstance(window_titles, list) or not 1 <= len(window_titles) <= 20:
            raise ValueError("window_titles must contain 1..20 exact titles")
        for name in window_titles:
            title(name)
        if len(set(window_titles)) != len(window_titles):
            raise ValueError("Duplicate window_titles are not allowed")
        integer(limit, "limit", 1, 500)
        integer(max_workers, "max_workers", 1, 4)
        boolean(open_if_needed, "open_if_needed")
        boolean(close_after, "close_after")
        started = time.monotonic()
        prepared, opened = {}, []
        with ui_session():
            if open_if_needed:
                for name in window_titles:
                    try:
                        result = self.open_room(name)
                    except Exception as error:
                        result = {"status": "failed", "errorType": type(error).__name__}
                    if result.get("status") != "ok":
                        prepared[name] = result
                    elif not result.get("alreadyOpen"):
                        opened.append(name)

        def read(name):
            if name in prepared:
                return {"windowTitle": name, **prepared[name]}
            begin = time.monotonic()
            try:
                value = self.read_messages(name, limit=limit)
            except Exception as error:
                value = {"status": "failed", "errorType": type(error).__name__}
            return {"windowTitle": name, **value, "callElapsedMs": round((time.monotonic()-begin)*1000, 1)}

        with ThreadPoolExecutor(max_workers=min(max_workers, len(window_titles))) as pool:
            items = list(pool.map(read, window_titles))
        cleanup = []
        if close_after:
            for name in reversed(opened):
                try:
                    cleanup.append(self.close_room(name))
                except Exception as error:
                    cleanup.append({"status": "failed", "windowTitle": name, "errorType": type(error).__name__})
        succeeded = sum(item.get("status") == "ok" for item in items)
        return {"status": "ok" if succeeded == len(items) and all(c.get("status") == "ok" for c in cleanup) else "partial",
                "source": "macos_accessibility", "items": items, "total": len(items), "succeeded": succeeded,
                "windowsOpened": opened, "cleanup": cleanup, "databaseCalls": 0,
                "parallel": max_workers > 1 and len(window_titles) > 1,
                "atomicAcrossRooms": False, "elapsedMs": round((time.monotonic()-started)*1000, 1)}

    def load_more(self, window_title, pages=1):
        """Request bounded upward history scrolling; never infer end-of-history from no change."""
        title(window_title)
        integer(pages, "pages", 1, 10)
        with ui_session():
            before = self.read_messages(window_title, limit=500)
            if before.get("status") != "ok":
                return before
            steps = []
            previous = before
            after = before
            for _ in range(pages):
                step = self._control("load_older", windowTitle=window_title)
                steps.append(step)
                if step.get("status") != "ok":
                    return {**step, "steps": steps}
                after = self.read_messages(window_title, limit=500)
                if after.get("status") != "ok":
                    return {**after, "steps": steps}
                if after["snapshotId"] == previous["snapshotId"]:
                    break
                previous = after
            return {"status": "ok", "source": self.source, "windowTitle": window_title,
                    "steps": steps, "pagesAttempted": len(steps),
                    "contentChanged": before["snapshotId"] != after["snapshotId"],
                    "exposedRowsBefore": before["coverage"]["exposedRowCount"],
                    "exposedRowsAfter": after["coverage"]["exposedRowCount"],
                    "endOfHistory": None, "historyComplete": False, "after": after, "databaseCalls": 0}

    def search_messages(self, window_title, query, limit=100, load_pages=0, case_sensitive=False,
                        use_app_search=True, max_steps=10, context_rows=2):
        """Visit bounded previous app results and preserve each snapshot's context.

        Overlapping snapshots can repeat a message. We deliberately do not merge
        equal bodies, since distinct messages can have identical text and no timestamp.
        """
        title(window_title)
        query_text(query)
        integer(limit, "limit", 1, 500)
        integer(load_pages, "load_pages", 0, 10)
        integer(max_steps, "max_steps", 1, 50)
        integer(context_rows, "context_rows", 0, 10)
        boolean(case_sensitive, "case_sensitive")
        boolean(use_app_search, "use_app_search")
        with ui_session():
            opened = self.open_room(window_title)
            if opened.get("status") != "ok":
                return opened
            history = self.load_more(window_title, load_pages) if load_pages else None
            if history and history.get("status") != "ok":
                return history
            visits, matches, seen_snapshots = [], [], set()
            searched_rows = 0
            stop_reason = "step_limit" if use_app_search else "exposed_rows_only"
            verified = True
            coverage = {"historyComplete": False}
            snapshot = None
            needle = query if case_sensitive else query.casefold()
            for step in range(max_steps if use_app_search else 1):
                native = self._control("search_query" if step == 0 else "search_previous",
                                       windowTitle=window_title, query=query) if use_app_search else None
                if native and native.get("searchOutcome") == "navigation_exhausted":
                    visits.append(native)
                    stop_reason = "navigation_exhausted"
                    break
                if native:
                    visits.append(native)
                    verified = verified and native.get("searchVerified") is True
                    if native.get("status") not in ("ok", "partial"):
                        stop_reason = native.get("reason", "search_navigation_failed")
                        verified = False
                        break
                page = self.read_messages(window_title, limit=500)
                if page.get("status") != "ok":
                    stop_reason = page.get("reason", page["status"])
                    verified = False
                    break
                rows = page["items"]
                snapshot = page["snapshotId"]
                coverage = page["coverage"]
                while page["hasMore"]:
                    page = self.read_messages(window_title, limit=500, offset=page["nextOffset"], snapshot_id=snapshot)
                    if page.get("status") != "ok":
                        break
                    rows = page["items"] + rows
                if page.get("status") != "ok":
                    stop_reason = page.get("reason", page["status"])
                    verified = False
                    break
                if snapshot not in seen_snapshots:
                    seen_snapshots.add(snapshot)
                    searched_rows += len(rows)
                    for index, row in enumerate(rows):
                        if not self._searchable(row) or needle not in (row["text"] if case_sensitive else row["text"].casefold()):
                            continue
                        matches.append({**row, "snapshotId": snapshot, "searchVisit": step,
                                        "contextBefore": rows[max(0, index-context_rows):index],
                                        "contextAfter": rows[index+1:index+1+context_rows]})
                if native and not verified:
                    stop_reason = native.get("reason", "search_result_not_observed")
                    break
                if len(matches) >= limit:
                    stop_reason = "result_limit"
                    break
                if native and native.get("searchState", {}).get("previousEnabled") is False:
                    stop_reason = "navigation_exhausted"
                    break
            return {"status": "ok" if verified else "partial", "source": self.source,
                    "windowTitle": window_title, "query": query, "items": matches[:limit],
                    "total": len(matches), "truncated": len(matches) > limit,
                    "coverage": {**coverage, "historyComplete": False, "snapshotsVisited": len(seen_snapshots)},
                    "searchedRows": searched_rows, "historyRequested": bool(load_pages),
                    "searchScope": "visited_app_search_snapshots" if use_app_search else "currently_exposed_bodies",
                    "snapshotId": snapshot, "databaseCalls": 0, "appSearchUsed": use_app_search,
                    "searchVerified": verified if use_app_search else None,
                    "stopReason": stop_reason, "searchVisits": visits,
                    "duplicatesPossible": len(seen_snapshots) > 1,
                    "nativeSearch": visits[0] if visits else None}

    def send_message(self, window_title, text, operation_id, wait_seconds=3, require_self=False):
        return AXSendEngine(self).send(window_title, text, operation_id, wait_seconds, require_self)

    def send_result(self, operation_id, wait_seconds=0):
        return AXSendEngine(self).result(operation_id, wait_seconds)


class AXSendEngine:
    source = confirmation_source = "macos_accessibility"

    def __init__(self, native, state_root=AX_STATE_ROOT):
        self.native = native
        self.root = Path(state_root)

    def path(self, operation_id):
        if not isinstance(operation_id, str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", operation_id):
            raise ValueError("operation_id must be 1..128 letters, digits, dots, underscores, colons or hyphens")
        return self.root / (self.hash(operation_id) + ".json")

    @staticmethod
    def hash(text):
        return hashlib.sha256(text.encode("utf-8")).hexdigest()

    @staticmethod
    def wait(value):
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not 0 <= value <= 20:
            raise ValueError("wait_seconds must be between 0 and 20")

    def load(self, path):
        try:
            fd = open_private(path)
        except FileNotFoundError:
            return None
        with os.fdopen(fd, encoding="utf-8") as stream:
            return json.load(stream)

    def output(self, record, **extra):
        return {"status": record["phase"], "operationId": record["operationId"], "windowTitle": record["windowTitle"],
                "source": self.source, "confirmationSource": self.confirmation_source,
                "databaseCalls": 0, "retrySafe": False, "reason": record.get("reason"),
                "native": record.get("native"), **extra}

    def confirm(self, path, record, wait_seconds):
        from kakaotalk_state import save_private
        until = time.monotonic() + wait_seconds
        while True:
            try:
                after = self.read_for_confirmation(record)
                hashes = self.body_hashes(after)
                expected = record["baselineTailHashes"] + [record["textHash"]]
                if (after.get("status") == "ok" and after["textMessageCount"] == record["baselineTextCount"] + 1 and
                        hashes[-len(expected):] == expected):
                    inspected = self.inspect_for_confirmation(record)
                    if inspected.get("status") == "ready":
                        record.update(phase="observed_in_ax", reason="new_text_occurrence_and_empty_composer")
                        save_private(path, record)
                        return self.output(record)
            except Exception:
                pass
            if time.monotonic() >= until:
                record.update(phase="outcome_unknown", reason=self.unknown_reason)
                save_private(path, record)
                return self.output(record)
            time.sleep(min(0.15, max(0, until-time.monotonic())))

    unknown_reason = "ax_confirmation_not_established"

    def text_hash(self, text):
        return self.hash(text)

    def body_hashes(self, result):
        return [self.text_hash(row["text"]) for row in result.get("items", []) if row.get("text") is not None]

    def prepare(self, window_title, require_self):
        return self.native.open_room(window_title, require_self=require_self), {}

    def build_for_send(self):
        self.native.build()

    def dispatch(self, record, text):
        return self.native.call("send", [record["windowTitle"]], text)

    def read_for_confirmation(self, record):
        return self.native.read_messages(record["windowTitle"], limit=500)

    def inspect_for_confirmation(self, record):
        return self.native.call("inspect", [record["windowTitle"]])

    def send(self, window_title, text, operation_id, wait_seconds=3, require_self=False):
        from kakaotalk_state import save_private, send_lock
        title(window_title)
        boolean(require_self, "require_self")
        self.wait(wait_seconds)
        if not isinstance(text, str) or not text.strip() or len(text.encode("utf-8")) > 16_000:
            raise ValueError("text must be non-empty and at most 16,000 UTF-8 bytes")
        path = self.path(operation_id)
        fingerprint = self.hash(json.dumps([window_title, text, require_self], ensure_ascii=False))
        with ui_session(), send_lock(self.root):
            record = self.load(path)
            if record:
                if record["fingerprint"] != fingerprint:
                    raise ValueError("operation_id is already bound to another target or message")
                if record["phase"] in ("observed_in_ax", "blocked"):
                    return self.output(record, deduplicated=True, nativeSendCalls=0)
                return {**self.confirm(path, record, wait_seconds), "deduplicated": True, "nativeSendCalls": 0}
            opened, target = self.prepare(window_title, require_self)
            if opened.get("status") != "ok":
                return {**opened, "operationId": operation_id, "retrySafe": False}
            before = self.read_for_confirmation({"windowTitle": window_title, **target})
            if before.get("status") != "ok":
                return {**before, "operationId": operation_id, "retrySafe": False}
            baseline = self.body_hashes(before)[-64:]
            self.build_for_send()  # Build before persisting a dispatch intent.
            record = {"operationId": operation_id, "windowTitle": window_title, "fingerprint": fingerprint,
                      "textHash": self.text_hash(text), "baselineTailHashes": baseline,
                      "baselineTextCount": before["textMessageCount"], "createdAt": time.time(), "phase": "dispatching",
                      **target}
            save_private(path, record)
            try:
                native = self.dispatch(record, text)
            except Exception as error:
                native = {"status": "bridge_result_unknown", "errorType": type(error).__name__, "dispatchAttempted": None}
            record["native"] = native
            if native.get("dispatchAttempted") is False:
                record.update(phase="blocked", reason=native.get("reason", native.get("status")))
                save_private(path, record)
                return self.output(record, nativeSendCalls=1)
            return {**self.confirm(path, record, wait_seconds), "nativeSendCalls": 1}

    def result(self, operation_id, wait_seconds=0):
        from kakaotalk_state import send_lock
        self.wait(wait_seconds)
        path = self.path(operation_id)
        with ui_session(), send_lock(self.root):
            record = self.load(path)
            if not record:
                raise ValueError("Unknown AX operation_id")
            if record["phase"] in ("observed_in_ax", "blocked"):
                return self.output(record)
            return self.confirm(path, record, wait_seconds)

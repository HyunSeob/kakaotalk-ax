import hashlib
import json
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from kakaotalk_ax import AXOperations, AXSendEngine, ui_session


class FixtureAX(AXOperations):
    def __init__(self):
        self.opened = {"Fixture"}
        self.known = {"Fixture", "Other"}
        self.rows = {"Fixture": ["old", "repeat"], "Other": ["other"]}
        self.controls = []
        self.sends = 0
        self.mode = "delivered"
        self.draft = ""
        self.barrier = None
        self.history = []

    def build(self):
        return Path("fixture-native")

    def _control(self, action, **args):
        self.controls.append((action, args))
        name = args.get("windowTitle")
        if action == "rooms":
            return {"status": "ok", "items": [{"name": n, "isSelf": n == "Fixture"} for n in sorted(self.known)],
                    "searchQuery": "", "scope": "currently_exposed_room_list"}
        if action == "search_rooms":
            return {"status": "ok", "items": []}
        if action == "open":
            if name not in self.known:
                return {"status": "blocked", "reason": "room_not_in_exposed_list"}
            existed = name in self.opened
            self.opened.add(name)
            return {"status": "ok", "alreadyOpen": existed}
        if action == "close":
            if self.draft and name == "Fixture":
                return {"status": "blocked", "reason": "existing_draft_preserved"}
            self.opened.discard(name)
            return {"status": "ok"}
        if action == "load_older" and self.history:
            self.rows[name].insert(0, self.history.pop(0))
        if action in ("search_query", "search_previous"):
            return {"status": "ok", "searchVerified": True, "searchState": {"previousEnabled": False}}
        return {"status": "ok"}

    def read_messages(self, window_title, limit=50, offset=0, snapshot_id=""):
        if self.barrier:
            self.barrier.wait(timeout=3)
        if window_title not in self.opened:
            return {"status": "blocked", "reason": "target_room_window_not_open"}
        bodies = self.rows[window_title]
        snapshot = hashlib.sha256(json.dumps(bodies).encode()).hexdigest()[:24]
        if snapshot_id and snapshot_id != snapshot:
            return {"status": "snapshot_changed", "items": []}
        end = max(0, len(bodies)-offset)
        start = max(0, end-limit)
        return {"status": "ok", "items": [{"text": text, "kind": "text", "rowIndex": i}
                                               for i, text in enumerate(bodies) if start <= i < end],
                "total": len(bodies), "textMessageCount": len(bodies), "hasMore": start > 0,
                "nextOffset": offset+end-start if start else None, "snapshotId": snapshot,
                "coverage": {"exposedRowCount": len(bodies), "historyComplete": False}, "databaseCalls": 0}

    def call(self, action, titles, text=None):
        if action == "inspect":
            return {"status": "blocked" if self.draft else "ready"}
        if action == "send":
            self.sends += 1
            if self.draft:
                return {"status": "blocked", "dispatchAttempted": False, "reason": "existing_draft_preserved"}
            if self.mode in ("delivered", "lost_reply", "extra_message"):
                if self.mode == "extra_message":
                    self.rows[titles[0]].append("unrelated incoming text")
                self.rows[titles[0]].append(text)
            if self.mode in ("lost_reply", "not_delivered"):
                raise TimeoutError()
            return {"status": "dispatch_result_unknown", "dispatchAttempted": True, "axResult": -25200}
        raise AssertionError("Unexpected native action")


class FixtureTestCase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.patch = patch("kakaotalk_ax.UI_ROOT", Path(self.directory.name) / "ui")
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.native = FixtureAX()


class AXOperationsTests(FixtureTestCase):
    def test_ui_lock_can_be_nested_by_operations(self):
        with ui_session(), ui_session():
            self.assertEqual(self.native.open_room("Other")["status"], "ok")

    def test_another_process_cannot_mutate_ui_while_a_session_holds_the_lock(self):
        code = ("from pathlib import Path\nimport sys\nimport kakaotalk_ax as ax\n"
                "ax.UI_ROOT=Path(sys.argv[1])\n"
                "try:\n with ax.ui_session(): print('unexpected')\n"
                "except ValueError:\n print('busy')\n")
        with ui_session():
            result = subprocess.run([sys.executable, "-c", code, str(Path(self.directory.name) / "ui")],
                                    capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "busy")

    def test_batch_opens_missing_rooms_and_reads_actually_in_parallel(self):
        self.native.barrier = threading.Barrier(2)
        result = self.native.read_many(["Fixture", "Other"], max_workers=2)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["windowsOpened"], ["Other"])
        self.assertEqual(result["succeeded"], 2)
        self.assertEqual([r["windowTitle"] for r in result["items"]], ["Fixture", "Other"])
        self.assertFalse(result["atomicAcrossRooms"])

    def test_batch_preserves_existing_windows_and_reports_missing_room_individually(self):
        result = self.native.read_many(["Fixture", "Other", "Missing"], close_after=True)
        self.assertEqual(result["status"], "partial")
        self.assertEqual(result["succeeded"], 2)
        self.assertEqual(self.native.opened, {"Fixture"})
        self.assertEqual(result["items"][2]["status"], "blocked")

    def test_batch_does_not_open_when_disabled(self):
        result = self.native.read_many(["Other"], open_if_needed=False)
        self.assertEqual(result["items"][0]["reason"], "target_room_window_not_open")
        self.assertEqual(self.native.controls, [])

    def test_batch_cleanup_does_not_discard_a_draft(self):
        self.native.opened.clear()
        self.native.draft = "keep this"
        result = self.native.read_many(["Fixture"], close_after=True)
        self.assertEqual(result["status"], "partial")
        self.assertIn("Fixture", self.native.opened)
        self.assertEqual(self.native.draft, "keep this")

    def test_cleanup_failure_keeps_the_already_read_results(self):
        with patch.object(self.native, "close_room", side_effect=TimeoutError()):
            result = self.native.read_many(["Other"], close_after=True)
        self.assertEqual(result["status"], "partial")
        self.assertEqual(result["succeeded"], 1)
        self.assertEqual(result["cleanup"][0]["errorType"], "TimeoutError")

    def test_literal_search_reads_all_exposed_pages_without_regex_or_wildcards(self):
        self.native.rows["Fixture"] = [f"row {i}" for i in range(501)] + ["A%_B", "a%_b", "AxxB"]
        result = self.native.search_messages("Fixture", "%_", use_app_search=False)
        self.assertEqual([x["text"] for x in result["items"]], ["A%_B", "a%_b"])
        self.assertEqual(result["searchedRows"], 504)
        self.assertFalse(result["coverage"]["historyComplete"])
        self.assertFalse(any(a == "search_query" for a, _ in self.native.controls))

    def test_app_search_is_inside_the_helper(self):
        result = self.native.search_messages("Fixture", "REPEAT")
        self.assertEqual(result["total"], 1)
        self.assertIn(("search_query", {"windowTitle": "Fixture", "query": "REPEAT"}), self.native.controls)

    def test_history_is_bounded_and_no_change_is_not_an_end_of_history_claim(self):
        result = self.native.load_more("Fixture", pages=5)
        self.assertEqual(result["pagesAttempted"], 1)
        self.assertFalse(result["contentChanged"])
        self.assertIsNone(result["endOfHistory"])
        self.assertFalse(result["historyComplete"])
        self.native.history = ["older one", "older two"]
        result = self.native.load_more("Fixture", pages=2)
        self.assertEqual(result["pagesAttempted"], 2)
        self.assertTrue(result["contentChanged"])

    def test_invalid_requests_do_not_reach_navigation(self):
        for call in [lambda: self.native.open_room(" "), lambda: self.native.open_room("Fixture", require_self=1),
                     lambda: self.native.read_many(["Fixture", "Fixture"]),
                     lambda: self.native.read_many(["Fixture"], max_workers=5),
                     lambda: self.native.load_more("Fixture", pages=11),
                     lambda: self.native.search_messages("Fixture", "")]:
            with self.assertRaises(ValueError):
                call()
        self.assertEqual(self.native.controls, [])


class AXSendTests(FixtureTestCase):
    def setUp(self):
        super().setUp()
        self.engine = AXSendEngine(self.native, Path(self.directory.name) / "operations")

    def test_ax_error_after_delivery_is_observed_without_retry_or_database(self):
        result = self.engine.send("Fixture", "new body", "intent", 0)
        self.assertEqual(result["status"], "observed_in_ax")
        self.assertEqual(result["native"]["axResult"], -25200)
        self.assertEqual(self.native.sends, 1)
        self.assertEqual(self.engine.send("Fixture", "new body", "intent", 0)["status"], "observed_in_ax")
        self.assertEqual(self.native.sends, 1)

    def test_lost_reply_after_delivery_can_be_observed(self):
        self.native.mode = "lost_reply"
        self.assertEqual(self.engine.send("Fixture", "new body", "intent", 0)["status"], "observed_in_ax")
        self.assertEqual(self.native.sends, 1)

    def test_uncertainty_is_persisted_and_late_observation_never_resends(self):
        self.native.mode = "not_delivered"
        self.assertEqual(self.engine.send("Fixture", "new body", "intent", 0)["status"], "outcome_unknown")
        second = AXSendEngine(self.native, self.engine.root).send("Fixture", "new body", "intent", 0)
        self.assertEqual(second["nativeSendCalls"], 0)
        self.native.rows["Fixture"].append("new body")
        self.assertEqual(self.engine.result("intent")["status"], "observed_in_ax")
        self.assertEqual(self.native.sends, 1)

    def test_preexisting_equal_body_does_not_confirm_an_unsent_message(self):
        self.native.rows["Fixture"].append("new body")
        self.native.mode = "not_delivered"
        self.assertEqual(self.engine.send("Fixture", "new body", "intent", 0)["status"], "outcome_unknown")

    def test_extra_incoming_message_makes_confirmation_ambiguous(self):
        self.native.mode = "extra_message"
        self.assertEqual(self.engine.send("Fixture", "new body", "intent", 0)["status"], "outcome_unknown")
        self.assertEqual(self.native.sends, 1)

    def test_existing_draft_is_preserved_and_blocked_intent_is_not_retried(self):
        self.native.draft = "keep this"
        result = self.engine.send("Fixture", "new body", "intent", 0)
        self.assertEqual(result["reason"], "existing_draft_preserved")
        self.assertEqual(self.native.draft, "keep this")
        self.engine.send("Fixture", "new body", "intent", 0)
        self.assertEqual(self.native.sends, 1)
        self.assertEqual(self.native.rows["Fixture"], ["old", "repeat"])

    def test_intent_cannot_be_reused_for_a_different_payload(self):
        self.engine.send("Fixture", "new body", "intent", 0)
        with self.assertRaises(ValueError):
            self.engine.send("Fixture", "different", "intent", 0)
        self.assertEqual(self.native.sends, 1)

    def test_journal_is_private_and_does_not_store_bodies(self):
        self.engine.send("Fixture", "fixture private payload", "intent", 0)
        path = self.engine.path("intent")
        if sys.platform != "win32":
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
        self.assertNotIn("fixture private payload", path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()

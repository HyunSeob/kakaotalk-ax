import io
import json
import os
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch


from kakaotalk_service import KakaoTalkService, encode_cursor
from kakaotalk_poll import PollingStateError


class SyntheticSource:
    def __init__(self, database=(100, 1), wal=(20, 2)):
        self.observation = {
            "available": True,
            "database_signature": database,
            "wal_signature": wal,
        }

    def __call__(self, _path):
        return dict(self.observation)


class SyntheticMessageQuery:
    def __init__(self, rows):
        self.rows = list(rows)
        self.calls = []
        self.update_calls = []
        self.high_water_calls = []
        self.message_columns = {
            "chatId",
            "logId",
            "authorId",
            "type",
            "sentAt",
            "message",
            "attachment",
            "msgId",
            "revision",
        }
        self.room_columns = {"chatId", "lastLogId", "chatName", "linkId"}
        self.user_columns = {"userId", "linkId", "nickName"}

    def __call__(self, path, key, sql, **kwargs):
        self.calls.append((path, key, sql, kwargs))
        if sql.startswith("PRAGMA table_info(NTChatMessage)"):
            return [{"name": name} for name in sorted(self.message_columns)]
        if sql.startswith("PRAGMA table_info(NTChatRoom)"):
            return [{"name": name} for name in sorted(self.room_columns)]
        if sql.startswith("PRAGMA table_info(NTUser)"):
            return [{"name": name} for name in sorted(self.user_columns)]

        params = kwargs["params"]
        rows = sorted(
            self.rows,
            key=lambda row: (
                row["chatId"],
                row["logId"],
                row["msgId"],
                row["revision"],
            ),
        )
        if params.get("room_id") is not None:
            rows = [row for row in rows if row["chatId"] == params["room_id"]]

        if 'ORDER BY m."chatId" DESC' in sql:
            self.high_water_calls.append((sql, kwargs))
            return [rows[-1]] if rows else []

        self.update_calls.append((sql, kwargs))
        cursor_key = (
            params.get("cursor_chat_id"),
            params.get("cursor_log_id"),
            params.get("cursor_msg_id"),
            params.get("cursor_revision"),
        )
        if all(value is not None for value in cursor_key):
            rows = [
                row
                for row in rows
                if (
                    row["chatId"],
                    row["logId"],
                    row["msgId"],
                    row["revision"],
                )
                > cursor_key
            ]
        return rows[: params["fetch_limit"]]


def synthetic_row(chat_id, log_id, msg_id, revision, text):
    return {
        "chatId": chat_id,
        "logId": log_id,
        "msgId": msg_id,
        "revision": revision,
        "authorId": 22,
        "type": 1,
        "sentAt": 1_786_852_800 + log_id,
        "message": text,
        "attachment": None,
        "roomName": "Synthetic room",
        "nickName": "Synthetic user",
    }


class PollingContractTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.state_file = self.root / "state" / "poll.json"
        self.source = SyntheticSource()
        self.query = SyntheticMessageQuery(
            [
                synthetic_row(11, 4, 40, 0, "old synthetic one"),
                synthetic_row(11, 5, 50, 0, "old synthetic two"),
            ]
        )
        self.service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=self.query,
            status_reader=self.source,
        )

    def tearDown(self):
        self.temp_dir.cleanup()

    def poller(self):
        from kakaotalk_poll import KakaoTalkPoller

        return KakaoTalkPoller(service=self.service, state_file=str(self.state_file))

    def read_state(self):
        return json.loads(self.state_file.read_text(encoding="utf-8"))

    def test_empty_state_bootstraps_at_high_water_without_replaying_history(self):
        poller = self.poller()

        result = poller.poll_once(limit=2)

        self.assertEqual(result["state"], "bootstrapped")
        self.assertEqual(result["messages"], [])
        self.assertFalse(result["ackRequired"])
        self.assertIsNone(result["ackToken"])
        self.assertEqual(
            result["nextCursor"],
            encode_cursor((11, 5, 50, 0)),
        )
        self.assertEqual(len(self.query.high_water_calls), 1)
        self.assertEqual(self.query.update_calls, [])
        self.assertNotIn('m."message"', self.query.high_water_calls[0][0])
        self.assertTrue(self.query.high_water_calls[0][1]["read_only"])
        self.assertEqual(self.query.high_water_calls[0][1]["max_rows"], 1)

        state = self.read_state()
        self.assertEqual(state["cursor"], result["nextCursor"])
        self.assertIsNone(state["pending"])
        self.assertEqual(state["signature"], {"database": [100, 1], "wal": [20, 2]})
        serialized = self.state_file.read_text(encoding="utf-8")
        for sensitive in (
            "old synthetic",
            "roomName",
            "nickName",
            "attachment",
            "db_path",
            "uuid",
            "key",
        ):
            self.assertNotIn(sensitive, serialized)

    def test_empty_database_bootstrap_keeps_a_null_cursor_until_first_new_row(self):
        self.query.rows.clear()
        poller = self.poller()

        bootstrap = poller.poll_once(limit=1)

        self.assertEqual(bootstrap["state"], "bootstrapped")
        self.assertEqual(bootstrap["messages"], [])
        self.assertIsNone(bootstrap["nextCursor"])

        self.query.rows.append(synthetic_row(11, 1, 10, 0, "first synthetic after bootstrap"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }
        updated = poller.poll_once(limit=1)
        self.assertEqual([row["message"] for row in updated["messages"]], ["first synthetic after bootstrap"])

    def test_unchanged_signatures_skip_the_updates_query(self):
        poller = self.poller()
        poller.poll_once(limit=2)
        self.query.update_calls.clear()
        poller = self.poller()

        result = poller.poll_once(limit=2)

        self.assertEqual(result["state"], "unchanged")
        self.assertEqual(result["messages"], [])
        self.assertEqual(self.query.update_calls, [])

    def test_changed_signatures_use_stored_cursor_and_bounded_limit(self):
        poller = self.poller()
        bootstrap = poller.poll_once(limit=2)
        self.query.update_calls.clear()
        self.query.rows.append(synthetic_row(11, 6, 60, 0, "new synthetic"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        with patch.object(self.service, "updates", wraps=self.service.updates) as updates:
            result = poller.poll_once(limit=1)

        updates.assert_called_once_with(
            cursor=bootstrap["nextCursor"],
            limit=1,
            room_id=None,
        )
        self.assertEqual(result["state"], "updated")
        self.assertEqual([row["message"] for row in result["messages"]], ["new synthetic"])
        self.assertTrue(result["ackRequired"])
        self.assertEqual(result["ackToken"], result["nextCursor"])
        self.assertEqual(self.query.update_calls[-1][1]["params"]["fetch_limit"], 2)

    def test_ack_advances_cursor_and_missing_or_bad_ack_retries_same_batch(self):
        poller = self.poller()
        bootstrap = poller.poll_once(limit=2)
        self.query.rows.append(synthetic_row(11, 6, 60, 0, "retry synthetic"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }
        first = poller.poll_once(limit=1)
        before_ack = self.read_state()
        self.assertEqual(before_ack["cursor"], bootstrap["nextCursor"])
        self.assertEqual(before_ack["pending"]["cursor"], first["ackToken"])

        with self.assertRaises(PollingStateError):
            poller.acknowledge("not-the-pending-token")
        self.assertEqual(self.read_state(), before_ack)

        retry = self.poller().poll_once(limit=1)
        self.assertEqual(retry["state"], "updated")
        self.assertTrue(retry["ackRequired"])
        self.assertEqual([row["message"] for row in retry["messages"]], ["retry synthetic"])
        self.assertEqual(
            self.query.update_calls[-1][1]["params"]["cursor_chat_id"],
            11,
        )

        acknowledged = poller.acknowledge(first["ackToken"])
        self.assertEqual(acknowledged["state"], "acknowledged")
        self.assertEqual(self.read_state()["cursor"], first["ackToken"])
        self.assertIsNone(self.read_state()["pending"])
        self.assertEqual(poller.poll_once(limit=1)["state"], "unchanged")

    def test_has_more_continuation_is_not_hidden_by_an_unchanged_signature(self):
        poller = self.poller()
        bootstrap = poller.poll_once(limit=2)
        self.query.rows.append(synthetic_row(11, 6, 60, 0, "first new synthetic"))
        self.query.rows.append(synthetic_row(11, 7, 70, 0, "second new synthetic"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        first = poller.poll_once(limit=1)
        self.assertTrue(first["hasMore"])
        poller.acknowledge(first["ackToken"])
        second = poller.poll_once(limit=1)

        self.assertEqual([row["message"] for row in second["messages"]], ["second new synthetic"])
        self.assertTrue(second["ackRequired"])

    def test_state_permissions_atomic_replacement_and_rejection(self):
        poller = self.poller()
        import kakaotalk_poll

        with patch.object(kakaotalk_poll.os, "replace", wraps=os.replace) as replace:
            poller.poll_once(limit=1)

        self.assertEqual(stat.S_IMODE(self.state_file.parent.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.state_file.stat().st_mode), 0o600)
        self.assertEqual(replace.call_count, 1)
        temporary_name, destination = replace.call_args.args
        self.assertNotEqual(temporary_name, destination)
        self.assertEqual(destination, str(self.state_file))
        self.assertFalse(list(self.state_file.parent.glob("*.tmp")))

        os.chmod(self.state_file, 0o644)
        self.assertEqual(poller.poll_once(limit=1)["state"], "unchanged")
        self.assertEqual(stat.S_IMODE(self.state_file.stat().st_mode), 0o600)

        self.state_file.write_text("{malformed", encoding="utf-8")
        with self.assertRaises(PollingStateError) as malformed:
            poller.poll_once(limit=1)
        self.assertNotIn("malformed", str(malformed.exception))

        self.state_file.write_text("x" * 10_000, encoding="utf-8")
        with self.assertRaises(PollingStateError):
            poller.poll_once(limit=1)

    def test_cli_emits_bounded_json_and_deterministic_errors(self):
        import kakaotalk_poll
        import kakaotalk_reader
        import kakaotalk_service

        class FakePoller:
            def __init__(self, *, service, state_file):
                self.service = service
                self.state_file = state_file

            def poll_once(self, *, limit, room_id):
                return {
                    "state": "unchanged",
                    "messages": [],
                    "hasMore": False,
                    "nextCursor": None,
                    "ackRequired": False,
                    "ackToken": None,
                }

            def acknowledge(self, token, *, room_id):
                return {
                    "state": "acknowledged",
                    "messages": [],
                    "hasMore": False,
                    "nextCursor": token,
                    "ackRequired": False,
                    "ackToken": None,
                }

        argv = [
            "kakaotalk-reader",
            "poll",
            "--state-file",
            str(self.state_file),
            "--limit",
            "2",
        ]
        stdout = io.StringIO()
        with (
            patch.object(kakaotalk_poll, "KakaoTalkPoller", FakePoller),
            patch.object(kakaotalk_service, "KakaoTalkService", return_value=object()),
            patch.object(sys, "argv", argv),
            redirect_stdout(stdout),
        ):
            kakaotalk_reader.main()
        payload = json.loads(stdout.getvalue())
        self.assertEqual(payload["state"], "unchanged")
        self.assertEqual(set(payload), {"state", "messages", "hasMore", "nextCursor", "ackRequired", "ackToken"})
        self.assertLess(len(stdout.getvalue().encode("utf-8")), 10_000)

        ack_argv = argv + ["--ack", "kt2.synthetic-token"]
        stdout = io.StringIO()
        with (
            patch.object(kakaotalk_poll, "KakaoTalkPoller", FakePoller),
            patch.object(kakaotalk_service, "KakaoTalkService", return_value=object()),
            patch.object(sys, "argv", ack_argv),
            redirect_stdout(stdout),
        ):
            kakaotalk_reader.main()
        self.assertEqual(json.loads(stdout.getvalue())["state"], "acknowledged")

        class FailingPoller(FakePoller):
            def poll_once(self, *, limit, room_id):
                raise RuntimeError("safe polling failure")

        stdout = io.StringIO()
        stderr = io.StringIO()
        with (
            patch.object(kakaotalk_poll, "KakaoTalkPoller", FailingPoller),
            patch.object(kakaotalk_service, "KakaoTalkService", return_value=object()),
            patch.object(sys, "argv", argv),
            redirect_stdout(stdout),
            redirect_stderr(stderr),
            self.assertRaises(SystemExit) as exited,
        ):
            kakaotalk_reader.main()
        self.assertEqual(exited.exception.code, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(stderr.getvalue(), "Error: safe polling failure\n")


if __name__ == "__main__":
    unittest.main()

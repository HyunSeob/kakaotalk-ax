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


from kakaotalk_service import KakaoTalkService, decode_cursor, encode_cursor
from kakaotalk_poll import PollingError, PollingStateError, _decode_global_cursor


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
        self.room_ids = set()
        for row in self.rows:
            self.room_ids.add(row["chatId"])

    def __call__(self, path, key, sql, **kwargs):
        self.calls.append((path, key, sql, kwargs))
        params = kwargs["params"]
        if sql.startswith("PRAGMA table_info(NTChatMessage)"):
            return [{"name": name} for name in sorted(self.message_columns)]
        if sql.startswith("PRAGMA table_info(NTChatRoom)"):
            return [{"name": name} for name in sorted(self.room_columns)]
        if sql.startswith("PRAGMA table_info(NTUser)"):
            return [{"name": name} for name in sorted(self.user_columns)]

        if 'SELECT DISTINCT m."chatId"' in sql:
            return [
                {"chatId": room_id}
                for room_id in sorted({row["chatId"] for row in self.rows})
            ][: params["fetch_limit"]]
        if 'SELECT DISTINCT r."chatId"' in sql:
            return [
                {"chatId": room_id}
                for room_id in sorted(self.room_ids)
            ][: params["fetch_limit"]]

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
            _decode_global_cursor(result["nextCursor"]),
            {11: encode_cursor((11, 5, 50, 0), scope=11)},
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

    def test_global_poll_returns_lower_room_message_after_higher_room_bootstrap(self):
        query = SyntheticMessageQuery(
            [
                synthetic_row(11, 40, 40, 0, "old lower room"),
                synthetic_row(22, 5, 5, 0, "old higher room"),
            ]
        )
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=query,
            status_reader=self.source,
        )
        poller = self.poller()
        poller.service = service

        bootstrap = poller.poll_once(limit=10)
        self.assertEqual(
            _decode_global_cursor(bootstrap["nextCursor"]),
            {
                11: encode_cursor((11, 40, 40, 0), scope=11),
                22: encode_cursor((22, 5, 5, 0), scope=22),
            },
        )

        query.rows.append(synthetic_row(11, 41, 41, 0, "new lower room"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        result = poller.poll_once(limit=10)

        self.assertEqual([row["message"] for row in result["messages"]], ["new lower room"])
        self.assertTrue(result["ackRequired"])
        self.assertTrue(query.update_calls)
        self.assertEqual(
            [call[1]["params"].get("room_id") for call in query.update_calls[-2:]],
            [11, 22],
        )
        self.assertTrue(all(call[3]["read_only"] for call in query.calls))

    def test_global_messages_are_ordered_by_room_and_row_key(self):
        query = SyntheticMessageQuery(
            [
                synthetic_row(11, 1, 1, 0, "old lower"),
                synthetic_row(22, 1, 1, 0, "old higher"),
            ]
        )
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=query,
            status_reader=self.source,
        )
        poller = self.poller()
        poller.service = service
        poller.poll_once(limit=10)

        query.rows.extend(
            [
                synthetic_row(22, 3, 3, 0, "new higher"),
                synthetic_row(11, 3, 3, 0, "new lower"),
            ]
        )
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        result = poller.poll_once(limit=10)

        self.assertEqual(
            [(row["chatId"], row["message"]) for row in result["messages"]],
            [(11, "new lower"), (22, "new higher")],
        )

    def test_global_pagination_continues_each_room_after_ack(self):
        query = SyntheticMessageQuery(
            [
                synthetic_row(11, 1, 1, 0, "old lower"),
                synthetic_row(22, 1, 1, 0, "old higher"),
            ]
        )
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=query,
            status_reader=self.source,
        )
        poller = self.poller()
        poller.service = service
        poller.poll_once(limit=10)
        query.rows.extend(
            [
                synthetic_row(11, 2, 2, 0, "lower one"),
                synthetic_row(11, 3, 3, 0, "lower two"),
                synthetic_row(22, 2, 2, 0, "higher one"),
            ]
        )
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        first = poller.poll_once(limit=2)
        self.assertEqual([row["message"] for row in first["messages"]], ["lower one", "lower two"])
        self.assertTrue(first["hasMore"])
        poller.acknowledge(first["ackToken"])

        second = poller.poll_once(limit=2)

        self.assertEqual([row["message"] for row in second["messages"]], ["higher one"])
        self.assertFalse(second["hasMore"])

    def test_global_room_discovery_tracks_empty_room_before_first_message(self):
        poller = self.poller()
        poller.poll_once(limit=10)
        self.query.room_ids.add(33)
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        discovered = poller.poll_once(limit=10)

        self.assertEqual(discovered["messages"], [])
        self.assertEqual(_decode_global_cursor(self.read_state()["cursor"])[33], None)

        self.query.rows.append(synthetic_row(33, 1, 1, 0, "first discovered room message"))
        self.source.observation = {
            "available": True,
            "database_signature": (102, 4),
            "wal_signature": (22, 5),
        }

        result = poller.poll_once(limit=10)

        self.assertEqual(
            [row["message"] for row in result["messages"]],
            ["first discovered room message"],
        )

    def test_new_nonempty_room_after_bootstrap_is_not_high_water_skipped(self):
        poller = self.poller()
        poller.poll_once(limit=10)
        self.query.rows.append(synthetic_row(33, 1, 1, 0, "first nonempty discovered room"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        result = poller.poll_once(limit=10)

        self.assertEqual(
            [row["message"] for row in result["messages"]],
            ["first nonempty discovered room"],
        )

    def test_pending_global_batch_carries_new_room_watermark_across_retries(self):
        poller = self.poller()
        poller.poll_once(limit=10)
        self.query.rows.append(synthetic_row(11, 6, 60, 0, "pending lower"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }
        first = poller.poll_once(limit=10)

        self.query.room_ids.add(33)
        self.source.observation = {
            "available": True,
            "database_signature": (102, 4),
            "wal_signature": (22, 5),
        }
        retry = poller.poll_once(limit=10)
        pending_cursor = json.loads(self.state_file.read_text())["pending"]["cursor"]
        self.assertIn(33, _decode_global_cursor(pending_cursor))

        self.query.rows.append(synthetic_row(33, 1, 1, 0, "new room after pending"))
        self.source.observation = {
            "available": True,
            "database_signature": (103, 5),
            "wal_signature": (23, 6),
        }
        final_retry = poller.poll_once(limit=10)

        self.assertIn("new room after pending", [row["message"] for row in final_retry["messages"]])
        self.assertTrue(final_retry["ackRequired"])
        self.assertNotEqual(first["ackToken"], retry["ackToken"])

    def test_global_room_discovery_fails_closed_above_the_bound(self):
        poller = self.poller()
        self.query.room_ids.update(range(1, 102))

        with self.assertRaises(PollingError):
            poller.poll_once(limit=1)

    def test_legacy_global_cursor_is_rejected_for_explicit_rebootstrap(self):
        legacy_cursor = encode_cursor((22, 5, 5, 0))
        self.state_file.parent.mkdir(mode=0o700)
        self.state_file.write_text(
            json.dumps(
                {
                    "v": 1,
                    "scope": None,
                    "cursor": legacy_cursor,
                    "signature": {"database": [100, 1], "wal": [20, 2]},
                    "pending": None,
                    "more": False,
                }
            ),
            encoding="utf-8",
        )

        with self.assertRaisesRegex(PollingStateError, "legacy cursor"):
            self.poller().poll_once(limit=1)

    def test_legacy_empty_global_state_is_upgraded_without_replaying_rows(self):
        self.state_file.parent.mkdir(mode=0o700)
        self.state_file.write_text(
            json.dumps(
                {
                    "v": 1,
                    "scope": None,
                    "cursor": None,
                    "signature": {"database": [100, 1], "wal": [20, 2]},
                    "pending": None,
                    "more": False,
                }
            ),
            encoding="utf-8",
        )

        result = self.poller().poll_once(limit=1)

        self.assertEqual(result["messages"], [])
        self.assertTrue(result["nextCursor"].startswith("ktg1."))
        cursor = json.loads(self.state_file.read_text())["cursor"]
        self.assertEqual(
            _decode_global_cursor(cursor)[11],
            encode_cursor((11, 5, 50, 0), scope=11),
        )

    def test_empty_database_bootstrap_keeps_a_null_cursor_until_first_new_row(self):
        self.query.rows.clear()
        poller = self.poller()

        bootstrap = poller.poll_once(limit=1)

        self.assertEqual(bootstrap["state"], "bootstrapped")
        self.assertEqual(bootstrap["messages"], [])
        self.assertEqual(_decode_global_cursor(bootstrap["nextCursor"]), {11: None})

        self.query.rows.append(synthetic_row(11, 1, 10, 0, "first synthetic after bootstrap"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }
        updated = poller.poll_once(limit=1)
        self.assertEqual(
            [row["message"] for row in updated["messages"]],
            ["first synthetic after bootstrap"],
        )

    def test_scoped_poll_keeps_the_existing_room_cursor_contract(self):
        poller = self.poller()

        bootstrap = poller.poll_once(limit=1, room_id=11)

        decoded = decode_cursor(bootstrap["nextCursor"])
        self.assertIsNotNone(decoded)
        self.assertEqual(decoded[1], 11)
        self.assertEqual(json.loads(self.state_file.read_text())["scope"], 11)

        self.query.rows.append(synthetic_row(11, 6, 60, 0, "scoped new synthetic"))
        self.source.observation = {
            "available": True,
            "database_signature": (101, 3),
            "wal_signature": (21, 4),
        }

        result = poller.poll_once(limit=1, room_id=11)

        self.assertEqual([row["message"] for row in result["messages"]], ["scoped new synthetic"])
        self.assertFalse(any('SELECT DISTINCT m."chatId"' in call[2] for call in self.query.calls))

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
            cursor=encode_cursor((11, 5, 50, 0), scope=11),
            limit=1,
            room_id=11,
        )
        self.assertEqual(result["state"], "updated")
        self.assertEqual([row["message"] for row in result["messages"]], ["new synthetic"])
        self.assertTrue(result["ackRequired"])
        self.assertEqual(result["ackToken"], result["nextCursor"])
        self.assertEqual(self.query.update_calls[-1][1]["params"]["fetch_limit"], 2)
        self.assertEqual(self.query.update_calls[-1][1]["params"]["room_id"], 11)

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

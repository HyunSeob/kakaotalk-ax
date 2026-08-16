import types
import unittest
from unittest.mock import patch


import kakaotalk_reader as reader
from kakaotalk_service import KakaoTalkService


class SqlSafetyTests(unittest.TestCase):
    def test_raw_cli_sql_is_wrapped_and_capped(self):
        safe = reader.validate_read_only_sql("SELECT chatId FROM NTChatRoom ORDER BY chatId")
        self.assertIn("SELECT * FROM (SELECT chatId FROM NTChatRoom ORDER BY chatId)", safe)
        self.assertTrue(safe.endswith("LIMIT 500;"))

    def test_raw_cli_sql_rejects_writes_comments_and_multiple_statements(self):
        for query in (
            "UPDATE NTUser SET nickName = 'x'",
            "SELECT 1; DELETE FROM NTUser",
            "SELECT 1 -- hidden write",
            "SELECT load_extension('x')",
        ):
            with self.subTest(query=query), self.assertRaises(ValueError):
                reader.validate_read_only_sql(query)

    def test_raw_cli_sql_rejects_quoted_dangerous_functions(self):
        for query in (
            "SELECT [readfile]('/tmp/synthetic')",
            'SELECT "writefile"("/tmp/synthetic", "x")',
            "SELECT `load_extension`('synthetic')",
        ):
            with self.subTest(query=query), self.assertRaises(ValueError):
                reader.validate_read_only_sql(query)

    def test_sqlcipher_uses_bound_parameter_commands_and_read_transaction(self):
        result = types.SimpleNamespace(returncode=0, stdout="[]", stderr="")
        with (
            patch.object(reader.os.path, "isfile", return_value=True),
            patch.object(reader.subprocess, "run", return_value=result) as run,
        ):
            reader.sqlcipher_query(
                "synthetic-db",
                "synthetic-key",
                "SELECT :keyword;",
                params={"keyword": "synthetic' OR 1=1"},
            )
        self.assertEqual(run.call_args.args[0][1], "-readonly")
        input_text = run.call_args.kwargs["input"]
        self.assertIn("PRAGMA query_only = ON;", input_text)
        self.assertIn("BEGIN;", input_text)
        self.assertIn("COMMIT;", input_text)
        self.assertIn(".parameter set :keyword X'73796e74686574696327204f5220313d31'", input_text)
        self.assertNotIn("synthetic' OR 1=1", input_text)

    def test_discovery_prefers_a_current_wal_database_over_stale_config(self):
        entries = [
            {"path": "/synthetic/stale.db", "has_wal": False, "size": 90000},
            {"path": "/synthetic/current.db", "has_wal": True, "size": 10000},
        ]
        with patch.object(reader, "find_db_files", return_value=entries):
            selected = reader._discovered_db_for_config("/synthetic/stale.db")
        self.assertEqual(selected, "/synthetic/current.db")

    def test_live_writer_lock_gets_a_fresh_read_retry(self):
        locked = types.SimpleNamespace(returncode=1, stdout="", stderr="database is locked")
        ready = types.SimpleNamespace(returncode=0, stdout="[]", stderr="")
        with (
            patch.object(reader.os.path, "isfile", return_value=True),
            patch.object(reader.subprocess, "run", side_effect=[locked, ready]) as run,
            patch.object(reader.time, "sleep") as sleep,
        ):
            self.assertEqual(reader.sqlcipher_query("synthetic-db", "synthetic-key", "SELECT 1;"), "[]")
        self.assertEqual(run.call_count, 2)
        sleep.assert_called_once()


class ServiceQueryTests(unittest.TestCase):
    def test_search_uses_a_bound_literal_and_normalizes_rows(self):
        calls = []

        def query(path, key, sql, **kwargs):
            calls.append((sql, kwargs))
            return [
                {
                    "chatId": 101,
                    "logId": 3,
                    "authorId": 202,
                    "type": 1,
                    "sentAt": 1786852800,
                    "message": "synthetic result",
                    "nickName": "Synthetic user",
                    "roomName": "Synthetic room",
                }
            ]

        service = KakaoTalkService(resolver=lambda: ("synthetic-db", "synthetic-key"), query=query)
        rows = service.search_messages(keyword="100%_", limit=4)
        self.assertEqual(rows[0]["typeLabel"], "text")
        self.assertIn("CAST(:keyword AS TEXT)", calls[0][0])
        self.assertEqual(calls[0][1]["params"]["keyword"], "100\\%\\_")
        self.assertEqual(calls[0][1]["params"]["limit"], 4)
        self.assertTrue(calls[0][1]["read_only"])

    def test_context_returns_target_and_chronological_neighbors(self):
        def row(log_id, text):
            return {
                "chatId": 101,
                "logId": log_id,
                "authorId": 202,
                "type": 1,
                "sentAt": 1786852800 + log_id,
                "message": text,
                "nickName": "Synthetic user",
                "roomName": "Synthetic room",
            }

        def query(path, key, sql, **kwargs):
            if "m.logId = :message_id" in sql:
                return [row(2, "target")]
            if "m.logId < :message_id" in sql:
                return [row(1, "before")]
            return [row(3, "after")]

        service = KakaoTalkService(resolver=lambda: ("synthetic-db", "synthetic-key"), query=query)
        context = service.message_context(room_id=101, message_id=2, before=1, after=1)
        self.assertEqual(context["message"]["message"], "target")
        self.assertEqual([item["message"] for item in context["before"]], ["before"])
        self.assertEqual([item["message"] for item in context["after"]], ["after"])


if __name__ == "__main__":
    unittest.main()

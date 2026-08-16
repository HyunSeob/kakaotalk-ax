import base64
import json
import unittest


from kakaotalk_service import (
    KakaoTalkService,
    ValidationError,
    decode_cursor,
    encode_cursor,
)


class SyntheticMessageStore:
    def __init__(self):
        self.calls = []
        self.rows = [
            self._row(11, 4, 40, 0, "first"),
            self._row(11, 5, 50, 0, "second"),
            self._row(12, 1, 10, 1, "third"),
        ]
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

    @staticmethod
    def _row(chat_id, log_id, msg_id, revision, text):
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

    def __call__(self, path, key, sql, **kwargs):
        self.calls.append((sql, kwargs))
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


class CursorContractTests(unittest.TestCase):
    def test_cursor_is_versioned_canonical_base64url_and_round_trips(self):
        cursor = encode_cursor((11, 5, 50, 0), scope=11)
        self.assertTrue(cursor.startswith("kt2."))
        self.assertLessEqual(len(cursor), 256)
        self.assertEqual(decode_cursor(cursor), ((11, 5, 50, 0), 11))
        self.assertEqual(decode_cursor(cursor + "="), ((11, 5, 50, 0), 11))

        encoded = cursor.removeprefix("kt2.")
        decoded = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
        self.assertEqual(
            json.loads(decoded),
            {"k": [11, 5, 50, 0], "scope": 11, "v": 1},
        )

    def test_cursor_rejects_malformed_oversized_and_non_cursor_payloads(self):
        invalid = [
            "not-a-cursor",
            "kt2.",
            "kt2.eyJ2IjoxLCJrIjpbMSwyXX0",
            "kt2." + "A" * 300,
        ]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValidationError):
                decode_cursor(value)

        with self.assertRaises(ValidationError):
            encode_cursor((1, 2, 3, 0), scope="not-an-id")

    def test_updates_page_then_cursor_continuation_has_no_duplicate(self):
        store = SyntheticMessageStore()
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=store,
        )

        first = service.updates(limit=2)
        self.assertEqual([row["message"] for row in first["messages"]], ["first", "second"])
        self.assertTrue(first["hasMore"])
        self.assertIsInstance(first["nextCursor"], str)
        first_data_call = next(call for call in store.calls if "FROM NTChatMessage" in call[0])
        self.assertIn(":fetch_limit", first_data_call[0])
        self.assertEqual(first_data_call[1]["params"]["fetch_limit"], 3)
        self.assertTrue(first_data_call[1]["read_only"])

        second = service.updates(cursor=first["nextCursor"], limit=2)
        self.assertEqual([row["message"] for row in second["messages"]], ["third"])
        self.assertFalse(second["hasMore"])
        self.assertEqual(second["nextCursor"], encode_cursor((12, 1, 10, 1)))
        second_data_call = [call for call in store.calls if "FROM NTChatMessage" in call[0]][-1]
        self.assertIn(":cursor_chat_id", second_data_call[0])
        self.assertEqual(second_data_call[1]["params"]["cursor_chat_id"], 11)

        empty = service.updates(cursor=second["nextCursor"], limit=2)
        self.assertEqual(empty["messages"], [])
        self.assertFalse(empty["hasMore"])

    def test_cursor_scope_must_match_the_requested_room(self):
        store = SyntheticMessageStore()
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=store,
        )
        cursor = service.updates(room_id=11, limit=1)["nextCursor"]
        with self.assertRaises(ValidationError):
            service.updates(cursor=cursor, room_id=12, limit=1)


class NormalizationContractTests(unittest.TestCase):
    def test_extended_message_metadata_is_structured_bounded_and_json_safe(self):
        row = {
            "chatId": 11,
            "logId": 6,
            "msgId": 60,
            "revision": "2",
            "authorId": 22,
            "type": 27,
            "sentAt": 1_786_852_800,
            "message": "synthetic system event",
            "attachment": json.dumps({"kind": "photo", "label": "synthetic", "file": "file://synthetic-file"}),
            "supplement": json.dumps({"items": ["synthetic"]}),
            "extra": "{" + ("x" * 20_000),
            "thread": {"root": 5, "reply": True},
            "scope": "room",
            "systemMessage": json.dumps({"code": "synthetic"}),
        }
        from kakaotalk_service import normalize_message

        normalized = normalize_message(row)
        encoded = json.dumps(normalized, ensure_ascii=False, sort_keys=True)
        self.assertEqual(normalized["msgId"], 60)
        self.assertEqual(normalized["revision"], 2)
        self.assertEqual(normalized["attachmentInfo"]["kind"], "json")
        self.assertEqual(normalized["supplement"]["kind"], "json")
        self.assertEqual(normalized["extra"]["kind"], "raw")
        self.assertEqual(normalized["thread"]["kind"], "json")
        self.assertEqual(normalized["scope"]["kind"], "raw")
        self.assertTrue(normalized["system"]["isSystem"])
        self.assertNotIn("file://", encoded)
        self.assertLess(len(encoded), 20_000)

    def test_updates_omit_missing_optional_columns_and_keep_fallback_ordering(self):
        calls = []

        def query(path, key, sql, **kwargs):
            calls.append(sql)
            if sql.startswith("PRAGMA table_info(NTChatMessage)"):
                return [
                    {"name": name}
                    for name in ("chatId", "logId", "authorId", "type", "sentAt", "message")
                ]
            if sql.startswith("PRAGMA table_info(NTChatRoom)"):
                return [{"name": "chatId"}, {"name": "lastLogId"}]
            if sql.startswith("PRAGMA table_info(NTUser)"):
                return []
            return [{
                "chatId": 11,
                "logId": 7,
                "authorId": 22,
                "type": 999,
                "sentAt": 1_786_852_800,
                "message": "synthetic unknown",
            }]

        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=query,
        )
        result = service.updates(limit=1)
        message = result["messages"][0]
        self.assertEqual(message["msgId"], 7)
        self.assertEqual(message["revision"], 0)
        self.assertEqual(message["typeLabel"], "type:999")
        self.assertNotIn("attachment", message)
        self.assertNotIn("attachmentInfo", message)
        self.assertTrue(any('lastLogId' in sql for sql in calls))


class StatusContractTests(unittest.TestCase):
    def test_status_reports_initial_unchanged_and_changed_wal_without_private_details(self):
        observations = iter(
            [
                {
                    "available": True,
                    "wal_present": True,
                    "database_signature": (10, 1),
                    "wal_signature": (20, 2),
                },
                {
                    "available": True,
                    "wal_present": True,
                    "database_signature": (10, 1),
                    "wal_signature": (20, 2),
                },
                {
                    "available": True,
                    "wal_present": True,
                    "database_signature": (10, 1),
                    "wal_signature": (21, 3),
                },
            ]
        )
        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            status_reader=lambda path: next(observations),
        )

        first = service.status()
        second = service.status()
        third = service.status()
        self.assertEqual(
            set(first),
            {"databaseAvailable", "walPresent", "walChanged", "databaseChanged", "changeKnown"},
        )
        self.assertFalse(first["changeKnown"])
        self.assertFalse(first["walChanged"])
        self.assertTrue(second["changeKnown"])
        self.assertFalse(second["walChanged"])
        self.assertTrue(third["walChanged"])
        self.assertNotIn("synthetic-db", json.dumps(third))


if __name__ == "__main__":
    unittest.main()

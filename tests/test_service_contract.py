import asyncio
import json
import unittest
from datetime import datetime
from unittest.mock import patch


from kakaotalk_service import (
    KakaoTalkService,
    ValidationError,
    KST,
    normalize_message,
    parse_date,
    validate_id,
    validate_limit,
)


class ValidationTests(unittest.TestCase):
    def test_limits_and_ids_are_bounded(self):
        self.assertEqual(validate_limit(3, default=10, maximum=20), 3)
        self.assertEqual(validate_limit(None, default=10, maximum=20), 10)
        with self.assertRaises(ValidationError):
            validate_limit(0, default=10, maximum=20)
        with self.assertRaises(ValidationError):
            validate_limit(21, default=10, maximum=20)
        with self.assertRaises(ValidationError):
            validate_id("101", "room_id")
        with self.assertRaises(ValidationError):
            validate_id(0, "room_id")

    def test_date_filters_use_kst_day_boundaries(self):
        now = datetime(2026, 8, 16, 13, 45, tzinfo=KST)
        self.assertEqual(parse_date("today", now=now), datetime(2026, 8, 16, tzinfo=KST))
        self.assertEqual(parse_date("yesterday", now=now), datetime(2026, 8, 15, tzinfo=KST))
        self.assertEqual(parse_date("-3d", now=now), datetime(2026, 8, 13, tzinfo=KST))
        with self.assertRaises(ValidationError):
            parse_date("not-a-date", now=now)

    def test_maximum_until_date_does_not_overflow(self):
        calls = []

        def query(path, key, sql, **kwargs):
            calls.append(kwargs)
            return []

        service = KakaoTalkService(
            resolver=lambda: ("synthetic-db", "synthetic-key"),
            query=query,
        )
        self.assertEqual(
            service.recent_messages(limit=1, until="9999-12-31"),
            [],
        )
        expected = int(datetime(9999, 12, 31, tzinfo=KST).timestamp()) + 86_400
        self.assertEqual(calls[0]["params"]["until_ts"], expected)

    def test_message_normalization_is_json_safe_and_human_readable(self):
        row = {
            "chatId": 101,
            "roomName": "Synthetic room",
            "logId": 7,
            "authorId": 202,
            "nickName": "Synthetic user",
            "type": 2,
            "sentAt": 1786852800,
            "message": "fixture text",
            "attachment": "x" * 20000,
        }
        normalized = normalize_message(row)
        encoded = json.dumps(normalized, ensure_ascii=False, sort_keys=True)
        self.assertEqual(normalized["roomName"], "Synthetic room")
        self.assertEqual(normalized["typeLabel"], "photo")
        self.assertTrue(normalized["time"].startswith("2026-08-16 "))
        self.assertLess(len(encoded), 20000)


class McpSurfaceTests(unittest.TestCase):
    def test_default_surface_has_named_read_tools_and_no_raw_sql(self):
        import kakaotalk_mcp

        names = {tool.name for tool in asyncio.run(kakaotalk_mcp.mcp.list_tools())}
        self.assertEqual(
            names,
            {
                "kakaotalk_list_rooms",
                "kakaotalk_read_messages",
                "kakaotalk_recent_messages",
                "kakaotalk_search_messages",
                "kakaotalk_list_users",
                "kakaotalk_message_context",
            },
        )

    def test_tool_returns_deterministic_json_without_opening_real_db(self):
        import kakaotalk_mcp

        fake = unittest.mock.Mock()
        fake.list_rooms.return_value = [{"chatId": 101, "displayName": "Synthetic room"}]
        with patch.object(kakaotalk_mcp, "SERVICE", fake):
            first = kakaotalk_mcp.kakaotalk_list_rooms(limit=1)
            second = kakaotalk_mcp.kakaotalk_list_rooms(limit=1)
        self.assertEqual(first, second)
        self.assertEqual(json.loads(first)[0]["chatId"], 101)

    def test_named_tools_delegate_to_the_matching_service_operations(self):
        import kakaotalk_mcp

        fake = unittest.mock.Mock()
        fake.list_rooms.return_value = []
        fake.read_messages.return_value = []
        fake.recent_messages.return_value = []
        fake.search_messages.return_value = []
        fake.list_users.return_value = []
        fake.message_context.return_value = {}
        with patch.object(kakaotalk_mcp, "SERVICE", fake):
            kakaotalk_mcp.kakaotalk_list_rooms(limit=2)
            kakaotalk_mcp.kakaotalk_read_messages(room_id=101, limit=3)
            kakaotalk_mcp.kakaotalk_recent_messages(
                limit=4,
                since="2026-08-15",
                until="2026-08-16",
            )
            kakaotalk_mcp.kakaotalk_search_messages(keyword="synthetic", limit=5)
            kakaotalk_mcp.kakaotalk_list_users(limit=6)
            kakaotalk_mcp.kakaotalk_message_context(
                room_id=101,
                message_id=7,
                before=1,
                after=2,
            )

        fake.list_rooms.assert_called_once_with(limit=2)
        fake.read_messages.assert_called_once_with(room_id=101, limit=3)
        fake.recent_messages.assert_called_once_with(
            limit=4,
            since="2026-08-15",
            until="2026-08-16",
        )
        fake.search_messages.assert_called_once_with(keyword="synthetic", limit=5)
        fake.list_users.assert_called_once_with(limit=6)
        fake.message_context.assert_called_once_with(
            room_id=101,
            message_id=7,
            before=1,
            after=2,
        )


if __name__ == "__main__":
    unittest.main()

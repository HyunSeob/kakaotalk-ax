import asyncio
import unittest
from unittest.mock import patch


import httpx

from kakaotalk_service import ServiceError, ValidationError
from kakaotalk_web.app import create_app
from kakaotalk_web.config import ConfigError, Settings


ALLOWED_HEADERS = {"Tailscale-User-Login": "viewer@example.invalid"}


class FakeService:
    def __init__(self):
        self.calls = []
        self.failure = None

    def _maybe_fail(self):
        if self.failure:
            raise self.failure

    def status(self):
        self.calls.append(("status",))
        self._maybe_fail()
        return {
            "databaseAvailable": True,
            "walPresent": True,
            "walChanged": False,
            "databaseChanged": True,
            "changeKnown": True,
            "databasePath": "/private/synthetic/database.db",
        }

    def list_rooms(self, *, limit):
        self.calls.append(("list_rooms", limit))
        self._maybe_fail()
        return [
            {
                "chatId": 101,
                "displayName": "Synthetic room",
                "roomType": "group",
                "lastLogId": 7,
                "activeMembersCount": 2,
                "linkId": 999,
                "path": "/private/synthetic/database.db",
                "unknown": "must not escape",
            }
        ]

    def read_messages(self, *, room_id, limit):
        self.calls.append(("read_messages", room_id, limit))
        self._maybe_fail()
        return [
            {
                "chatId": room_id,
                "roomName": "Synthetic room",
                "logId": 7,
                "nickName": "Synthetic user",
                "type": 1,
                "typeLabel": "text",
                "sentAt": 1_786_852_800,
                "time": "2026-08-16 13:00:00",
                "message": "Synthetic message",
                "attachment": "/private/synthetic/secret.jpg",
                "attachmentInfo": {"file": "/private/synthetic/secret.jpg"},
                "supplement": {"secret": "must not escape"},
                "extra": "must not escape",
                "thread": "must not escape",
                "scope": "must not escape",
                "systemMessage": "must not escape",
                "linkId": 999,
                "directChatMemberUserId": 888,
                "profileUrl": "https://synthetic.invalid/profile",
                "unknown": "must not escape",
            }
        ]

    def search_messages(self, *, keyword, limit):
        self.calls.append(("search_messages", keyword, limit))
        self._maybe_fail()
        return self.read_messages(room_id=101, limit=limit)


def call(app, method, path, *, json=None, headers=None):
    async def run():
        transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
        async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
            request_headers = ALLOWED_HEADERS if headers is None else headers
            return await client.request(method, path, json=json, headers=request_headers)

    return asyncio.run(run())


class WebApiTests(unittest.TestCase):
    def setUp(self):
        self.service = FakeService()
        settings = Settings(
            allowed_logins=("viewer@example.invalid",),
            port=8765,
            refresh_seconds=15,
        )
        self.app = create_app(service=self.service, settings=settings)

    def test_status_delegates_and_projects_only_boolean_status(self):
        response = call(self.app, "GET", "/api/v1/status")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "databaseAvailable": True,
                "walPresent": True,
                "walChanged": False,
                "databaseChanged": True,
                "changeKnown": True,
            },
        )
        self.assertEqual(self.service.calls, [("status",)])

    def test_app_construction_loads_secure_config_when_settings_are_not_injected(self):
        with patch("kakaotalk_web.app.load_settings", side_effect=ConfigError("missing")):
            with self.assertRaises(ConfigError):
                create_app(service=self.service)

    def test_rooms_to_messages_flow_uses_only_synthetic_service_data(self):
        rooms = call(self.app, "GET", "/api/v1/rooms?limit=1")
        self.assertEqual(rooms.status_code, 200)
        self.assertEqual(rooms.json()[0]["chatId"], 101)

        messages = call(self.app, "GET", "/api/v1/rooms/101/messages?limit=1")
        self.assertEqual(messages.status_code, 200)
        self.assertEqual(messages.json()[0]["chatId"], 101)
        self.assertEqual(
            self.service.calls[:2],
            [("list_rooms", 1), ("read_messages", 101, 1)],
        )

    def test_rooms_delegate_bounded_limit_and_strict_projection(self):
        response = call(self.app, "GET", "/api/v1/rooms?limit=100")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.service.calls, [("list_rooms", 100)])
        self.assertEqual(
            set(response.json()[0]),
            {"chatId", "displayName", "roomType", "lastLogId", "activeMembersCount"},
        )
        self.assertNotIn("database.db", response.text)
        self.assertNotIn("unknown", response.text)

    def test_room_and_message_limits_reject_values_outside_one_to_one_hundred(self):
        for query in ("0", "101", "not-an-int", "1.5"):
            with self.subTest(query=query):
                response = call(self.app, "GET", f"/api/v1/rooms?limit={query}")
                self.assertEqual(response.status_code, 400)
        for query in ("0", "101", "not-an-int"):
            with self.subTest(query=query):
                response = call(self.app, "GET", f"/api/v1/rooms/101/messages?limit={query}")
                self.assertEqual(response.status_code, 400)
        self.assertEqual(self.service.calls, [])

    def test_message_room_id_must_be_positive_integer_and_delegates(self):
        for room_id in ("0", "-1", "not-an-int", "1.5"):
            with self.subTest(room_id=room_id):
                response = call(self.app, "GET", f"/api/v1/rooms/{room_id}/messages")
                self.assertEqual(response.status_code, 400)
        response = call(self.app, "GET", "/api/v1/rooms/101/messages?limit=50")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.service.calls[-1], ("read_messages", 101, 50))

    def test_messages_and_search_project_only_browser_fields(self):
        response = call(self.app, "GET", "/api/v1/rooms/101/messages")
        self.assertEqual(response.status_code, 200)
        message = response.json()[0]
        self.assertEqual(
            set(message),
            {
                "chatId",
                "roomName",
                "logId",
                "nickName",
                "type",
                "typeLabel",
                "sentAt",
                "time",
                "message",
                "hasAttachment",
            },
        )
        self.assertTrue(message["hasAttachment"])
        for forbidden in ("attachment", "attachmentInfo", "supplement", "extra", "thread", "scope", "systemMessage", "profileUrl"):
            self.assertNotIn(forbidden, message)
        response = call(
            self.app,
            "POST",
            "/api/v1/search",
            json={"keyword": "needle", "limit": 3},
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.service.calls[-2:], [("search_messages", "needle", 3), ("read_messages", 101, 3)])

    def test_search_requires_post_json_and_bounds_keyword_and_limit(self):
        self.assertEqual(call(self.app, "GET", "/api/v1/search").status_code, 405)
        for body in ({}, {"keyword": "needle", "limit": 0}, {"keyword": "", "limit": 1}, {"keyword": "needle", "limit": 101}, {"keyword": "needle", "limit": "bad"}):
            with self.subTest(body=body):
                self.assertEqual(call(self.app, "POST", "/api/v1/search", json=body).status_code, 400)
        response = call(self.app, "POST", "/api/v1/search", json={"keyword": "needle", "limit": 100})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.service.calls[0], ("search_messages", "needle", 100))

        for body in (
            {"keyword": "needle", "limit": 1, "extra": "reject"},
            {"keyword": "needle", "limit": 1.5},
        ):
            with self.subTest(body=body):
                self.assertEqual(call(self.app, "POST", "/api/v1/search", json=body).status_code, 400)
        async def no_content_type():
            transport = httpx.ASGITransport(app=self.app, raise_app_exceptions=False)
            async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
                return await client.post("/api/v1/search", content='{"keyword":"needle"}', headers=ALLOWED_HEADERS)

        self.assertEqual(asyncio.run(no_content_type()).status_code, 400)

    def test_empty_and_unavailable_results_are_bounded(self):
        class EmptyService(FakeService):
            def list_rooms(self, *, limit):
                self.calls.append(("list_rooms", limit))
                return []

            def read_messages(self, *, room_id, limit):
                self.calls.append(("read_messages", room_id, limit))
                return []

            def status(self):
                self.calls.append(("status",))
                return {
                    "databaseAvailable": False,
                    "walPresent": False,
                    "walChanged": False,
                    "databaseChanged": False,
                    "changeKnown": False,
                }

        app = create_app(service=EmptyService(), settings=self.app.state.settings)
        self.assertEqual(call(app, "GET", "/api/v1/rooms").json(), [])
        self.assertEqual(call(app, "GET", "/api/v1/rooms/101/messages").json(), [])
        self.assertEqual(call(app, "GET", "/api/v1/status").json()["databaseAvailable"], False)

    def test_service_errors_map_to_generic_statuses_without_details(self):
        cases = ((ValidationError("synthetic validation"), 400), (ServiceError("synthetic source"), 503), (RuntimeError("synthetic secret"), 500))
        for error, expected_status in cases:
            with self.subTest(expected_status=expected_status):
                self.service.failure = error
                response = call(self.app, "GET", "/api/v1/status")
                self.assertEqual(response.status_code, expected_status)
                self.assertNotIn("synthetic", response.text)
                self.assertNotIn("database", response.text.lower())
                self.service.failure = None

    def test_forbidden_routes_and_api_docs_are_absent(self):
        for path in (
            "/docs",
            "/redoc",
            "/openapi.json",
            "/api/v1/users",
            "/api/v1/context",
            "/api/v1/updates",
            "/api/v1/export",
            "/api/v1/sql",
        ):
            with self.subTest(path=path):
                self.assertEqual(call(self.app, "GET", path).status_code, 404)

    def test_api_responses_have_no_store_security_headers_and_no_cors(self):
        response = call(self.app, "GET", "/api/v1/rooms")
        self.assertEqual(response.headers["cache-control"], "no-store")
        self.assertIn("default-src 'self'", response.headers["content-security-policy"])
        self.assertIn("object-src 'none'", response.headers["content-security-policy"])
        self.assertIn("frame-ancestors 'none'", response.headers["content-security-policy"])
        self.assertIn("base-uri 'none'", response.headers["content-security-policy"])
        self.assertEqual(response.headers["referrer-policy"], "no-referrer")
        self.assertEqual(response.headers["x-content-type-options"], "nosniff")
        self.assertNotIn("access-control-allow-origin", response.headers)

    def test_static_ui_is_bundled_and_protected(self):
        index = call(self.app, "GET", "/")
        self.assertEqual(index.status_code, 200)
        self.assertIn("text/html", index.headers["content-type"])
        self.assertIn("KakaoTalk", index.text)
        self.assertIn("loading", index.text)
        self.assertNotIn("cdn.", index.text.lower())

        script = call(self.app, "GET", "/assets/app.js")
        self.assertEqual(script.status_code, 200)
        self.assertIn("fetch", script.text)
        self.assertIn("setInterval", script.text)
        self.assertIn("unavailable", script.text)
        self.assertIn("error", script.text)
        self.assertEqual(script.headers["cache-control"], "no-store")

    def test_static_ui_requires_identity_in_identity_mode(self):
        response = call(self.app, "GET", "/", headers={})
        self.assertEqual(response.status_code, 401)


if __name__ == "__main__":
    unittest.main()

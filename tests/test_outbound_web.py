import asyncio
import tempfile
import unittest
from pathlib import Path

import httpx

from kakaotalk_outbound import AXAvailability, AXPreparation, AXSendOutcome, DraftStore
from kakaotalk_web.app import create_app
from kakaotalk_web.config import OutboundSettings, Settings


HEADERS = {"Tailscale-User-Login": "viewer@example.invalid"}


class FakeService:
    def __init__(self):
        self.rooms = [{"chatId": 101, "displayName": "Synthetic room"}]

    def list_rooms(self, *, limit):
        return list(self.rooms[:limit])

    def resolve_room_identity(self, *, room_id, display_identity):
        matches = [room for room in self.rooms if room["displayName"] == display_identity]
        if len(matches) != 1 or matches[0]["chatId"] != room_id:
            return None
        return dict(matches[0])


class FakeAdapter:
    def __init__(self):
        self.calls = []
        self.available = True
        self.preparation = AXPreparation.ready("synthetic-receipt")

    def check_availability(self):
        self.calls.append(("availability",))
        return AXAvailability(self.available)

    def prepare_text(self, *, expected_identity, text):
        self.calls.append(("prepare", expected_identity, text))
        return self.preparation

    def send_prepared(self, preparation):
        self.calls.append(("send", preparation.receipt))
        return AXSendOutcome.invoked()


def request(app, method, path, *, json=None, headers=None):
    async def run():
        transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
        async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
            return await client.request(method, path, json=json, headers=HEADERS if headers is None else headers)

    return asyncio.run(run())


class OutboundWebTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        helper = Path(self.directory.name) / "helper"
        helper.write_text("synthetic", encoding="utf-8")
        helper.chmod(0o700)
        self.settings = Settings(
            allowed_logins=("viewer@example.invalid",),
            outbound=OutboundSettings(enabled=True, helper_path=helper),
        )
        self.service = FakeService()
        self.adapter = FakeAdapter()
        self.app = create_app(
            service=self.service,
            settings=self.settings,
            outbound_adapter=self.adapter,
            draft_store=DraftStore(token_factory=lambda: "synthetic-web-draft-token"),
        )

    def tearDown(self):
        self.directory.cleanup()

    def test_preview_and_confirm_are_authenticated_two_step_posts(self):
        payload = {"roomId": 101, "targetIdentity": "Synthetic room", "text": "Synthetic private text"}
        self.assertEqual(request(self.app, "POST", "/api/v1/outbound/preview", json=payload, headers={}).status_code, 401)

        preview = request(self.app, "POST", "/api/v1/outbound/preview", json=payload)
        self.assertEqual(preview.status_code, 200)
        self.assertEqual(preview.json()["draftId"], "synthetic-web-draft-token")
        self.assertEqual(preview.json()["text"], "Synthetic private text")
        self.assertEqual(self.adapter.calls, [])

        confirm = request(
            self.app,
            "POST",
            "/api/v1/outbound/confirm",
            json={"draftId": "synthetic-web-draft-token", "confirmation": "CONFIRM SEND"},
        )
        self.assertEqual(confirm.status_code, 200)
        self.assertEqual(confirm.json()["status"], "uncertain")
        self.assertEqual(confirm.json()["verificationPath"], "/api/v1/rooms/101/messages?limit=50")
        self.assertEqual([call[0] for call in self.adapter.calls], ["availability", "prepare", "send"])

    def test_confirm_errors_are_generic_and_do_not_disclose_private_values(self):
        secret_identity = "Synthetic private room"
        secret_text = "Synthetic private message body"
        self.service.rooms = [{"chatId": 101, "displayName": secret_identity}]
        preview = request(
            self.app,
            "POST",
            "/api/v1/outbound/preview",
            json={"roomId": 101, "targetIdentity": secret_identity, "text": secret_text},
        )
        self.assertEqual(preview.status_code, 200)
        self.service.rooms = []
        response = request(
            self.app,
            "POST",
            "/api/v1/outbound/confirm",
            json={"draftId": "synthetic-web-draft-token", "confirmation": "CONFIRM SEND"},
        )
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["error"]["message"], "Outbound request refused.")
        self.assertNotIn(secret_identity, response.text)
        self.assertNotIn(secret_text, response.text)
        self.assertNotIn("synthetic-web-draft-token", response.text)

    def test_confirm_distinguishes_target_window_failure_without_private_values(self):
        self.adapter.preparation = AXPreparation.target_mismatch()
        request(
            self.app,
            "POST",
            "/api/v1/outbound/preview",
            json={"roomId": 101, "targetIdentity": "Synthetic room", "text": "Synthetic text"},
        )
        response = request(
            self.app,
            "POST",
            "/api/v1/outbound/confirm",
            json={"draftId": "synthetic-web-draft-token", "confirmation": "CONFIRM SEND"},
        )
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["state"], "target_mismatch")
        self.assertEqual(response.json()["error"]["message"], "Outbound request refused.")
        self.assertNotIn("Synthetic room", response.text)
        self.assertNotIn("Synthetic text", response.text)

    def test_disabled_status_and_confirmation_never_contact_adapter(self):
        disabled = Settings(allowed_logins=("viewer@example.invalid",))
        app = create_app(
            service=self.service,
            settings=disabled,
            outbound_adapter=self.adapter,
            draft_store=DraftStore(token_factory=lambda: "synthetic-disabled-draft"),
        )
        status = request(app, "GET", "/api/v1/outbound/status")
        self.assertEqual(status.json(), {"enabled": False, "available": False})
        preview = request(
            app,
            "POST",
            "/api/v1/outbound/preview",
            json={"roomId": 101, "targetIdentity": "Synthetic room", "text": "Synthetic text"},
        )
        self.assertEqual(preview.status_code, 200)
        confirm = request(
            app,
            "POST",
            "/api/v1/outbound/confirm",
            json={"draftId": "synthetic-disabled-draft", "confirmation": "CONFIRM SEND"},
        )
        self.assertEqual(confirm.status_code, 409)
        self.assertEqual(confirm.json()["state"], "disabled")
        self.assertEqual(self.adapter.calls, [])

    def test_unavailable_configured_helper_keeps_read_api_usable(self):
        missing_helper = Path(self.directory.name) / "missing-helper"
        settings = Settings(
            allowed_logins=("viewer@example.invalid",),
            outbound=OutboundSettings(enabled=True, helper_path=missing_helper),
        )
        app = create_app(service=self.service, settings=settings)
        rooms = request(app, "GET", "/api/v1/rooms")
        self.assertEqual(rooms.status_code, 200)
        self.assertEqual(rooms.json()[0]["chatId"], 101)
        self.assertEqual(
            request(app, "GET", "/api/v1/outbound/status").json(),
            {"enabled": True, "available": False},
        )

    def test_payloads_are_strict_and_outbound_responses_keep_security_headers(self):
        for path, body in (
            ("/api/v1/outbound/preview", {"roomId": 101, "targetIdentity": "Synthetic room", "text": "ok", "extra": True}),
            ("/api/v1/outbound/confirm", {"draftId": "synthetic-web-draft-token"}),
        ):
            with self.subTest(path=path):
                response = request(self.app, "POST", path, json=body)
                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.headers["cache-control"], "no-store")

    def test_static_ui_requires_preview_and_typed_confirmation_without_automatic_send(self):
        index = request(self.app, "GET", "/")
        script = request(self.app, "GET", "/assets/app.js")
        self.assertIn('id="outbound-preview-form"', index.text)
        self.assertIn('id="outbound-confirm-form"', index.text)
        self.assertIn('name="confirmation"', index.text)
        self.assertNotIn('id="send-button"', index.text)
        self.assertIn('addEventListener("submit"', script.text)
        self.assertIn("state.outboundUsable", script.text)
        self.assertIn("updateOutboundControls", script.text)
        self.assertEqual(script.text.count("confirmOutbound"), 2)
        self.assertIn('/api/v1/outbound/preview', script.text)
        self.assertIn('/api/v1/outbound/confirm', script.text)
        self.assertEqual(script.text.count('"/api/v1/outbound/confirm"'), 1)
        self.assertIn("Open exact target conversation window not found or focus changed", script.text)
        self.assertIn("Unique writable composer missing or prepared text changed", script.text)
        refresh_body = script.text.split("async function refresh()", 1)[1].split("function configureTimer()", 1)[0]
        self.assertNotIn("confirmOutbound", refresh_body)
        startup_body = script.text.rsplit("elements.outboundConfirmForm.addEventListener", 1)[1]
        self.assertNotIn('/api/v1/outbound/confirm', startup_body)
        self.assertNotIn('setInterval(confirm', script.text)
        self.assertNotIn('addEventListener("load", confirm', script.text)


if __name__ == "__main__":
    unittest.main()

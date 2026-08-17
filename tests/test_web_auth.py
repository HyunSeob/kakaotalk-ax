import asyncio
import unittest


import httpx

from kakaotalk_web.app import create_app
from kakaotalk_web.config import Settings


class SyntheticService:
    def status(self):
        return {
            "databaseAvailable": True,
            "walPresent": False,
            "walChanged": False,
            "databaseChanged": False,
            "changeKnown": False,
        }


def request(app, headers=None):
    async def run():
        transport = httpx.ASGITransport(app=app, raise_app_exceptions=False)
        async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
            return await client.get("/api/v1/status", headers=headers or {})

    return asyncio.run(run())


class WebAuthTests(unittest.TestCase):
    def setUp(self):
        self.settings = Settings(
            allowed_logins=("viewer@example.invalid",),
            port=8765,
            refresh_seconds=15,
        )
        self.app = create_app(service=SyntheticService(), settings=self.settings)

    def test_missing_identity_is_401(self):
        response = request(self.app)
        self.assertEqual(response.status_code, 401)
        self.assertNotIn("viewer@example.invalid", response.text)

    def test_unapproved_identity_is_403(self):
        response = request(self.app, {"Tailscale-User-Login": "other@example.invalid"})
        self.assertEqual(response.status_code, 403)
        self.assertNotIn("other@example.invalid", response.text)

    def test_normalized_exact_allowlisted_identity_is_allowed(self):
        response = request(self.app, {"Tailscale-User-Login": " Viewer@Example.INVALID "})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["databaseAvailable"], True)

    def test_forwarded_headers_do_not_substitute_for_identity(self):
        response = request(
            self.app,
            {
                "X-Forwarded-User": "viewer@example.invalid",
                "X-Forwarded-Email": "viewer@example.invalid",
                "X-Forwarded-For": "viewer@example.invalid",
            },
        )
        self.assertEqual(response.status_code, 401)

    def test_empty_tagged_device_style_identity_is_denied(self):
        response = request(self.app, {"Tailscale-User-Login": "   "})
        self.assertEqual(response.status_code, 401)

    def test_multiple_identity_headers_are_denied(self):
        async def run():
            transport = httpx.ASGITransport(app=self.app, raise_app_exceptions=False)
            async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
                return await client.get(
                    "/api/v1/status",
                    headers=[
                        ("Tailscale-User-Login", "viewer@example.invalid"),
                        ("Tailscale-User-Login", "other@example.invalid"),
                    ],
                )

        response = asyncio.run(run())
        self.assertEqual(response.status_code, 401)
        self.assertNotIn("other@example.invalid", response.text)

    def test_comma_joined_identity_header_is_not_treated_as_one_identity(self):
        response = request(
            self.app,
            {"Tailscale-User-Login": "viewer@example.invalid,other@example.invalid"},
        )
        self.assertEqual(response.status_code, 401)

    def test_explicit_network_mode_uses_tailnet_boundary_without_trusting_header(self):
        settings = Settings(allowed_logins=(), auth_mode="network")
        app = create_app(service=SyntheticService(), settings=settings)
        response = request(app)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["databaseAvailable"], True)

    def test_health_is_identity_free_but_reveals_no_database_state(self):
        async def run():
            transport = httpx.ASGITransport(app=self.app, raise_app_exceptions=False)
            async with httpx.AsyncClient(transport=transport, base_url="http://synthetic") as client:
                return await client.get("/healthz")

        response = asyncio.run(run())
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"ok": True})
        self.assertNotIn("databaseAvailable", response.text)


if __name__ == "__main__":
    unittest.main()

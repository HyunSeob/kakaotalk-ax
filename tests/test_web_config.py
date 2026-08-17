import json
import os
import stat
import tempfile
import unittest
from pathlib import Path


from kakaotalk_web.config import (
    LOOPBACK_HOST,
    ConfigError,
    Settings,
    load_settings,
    normalize_login,
    write_config,
)


class WebConfigTests(unittest.TestCase):
    def test_missing_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ConfigError):
                load_settings(Path(directory) / "missing.json")

    def test_empty_allowlist_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(json.dumps({"allowedLogins": []}), encoding="utf-8")
            os.chmod(path, 0o600)
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_group_or_world_readable_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(json.dumps({"allowedLogins": ["viewer@example.invalid"]}), encoding="utf-8")
            os.chmod(path, 0o640)
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_non_regular_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.mkdir()
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_invalid_port_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(
                json.dumps({"allowedLogins": ["viewer@example.invalid"], "port": 0}),
                encoding="utf-8",
            )
            os.chmod(path, 0o600)
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_defaults_and_fixed_loopback_host(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(json.dumps({"allowedLogins": [" Viewer@Example.INVALID "]}), encoding="utf-8")
            os.chmod(path, 0o600)
            settings = load_settings(path)
            self.assertEqual(settings.allowed_logins, ("viewer@example.invalid",))
            self.assertEqual(settings.port, 8765)
            self.assertEqual(settings.refresh_seconds, 15)
            self.assertEqual(settings.host, LOOPBACK_HOST)
            self.assertEqual(settings.host, "127.0.0.1")
            self.assertFalse(settings.outbound.enabled)
            self.assertIsNone(settings.outbound.helper_path)

    def test_outbound_requires_an_explicit_enabled_helper(self):
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / "kakaotalk-ax-helper"
            helper.write_text("synthetic", encoding="utf-8")
            helper.chmod(0o700)
            path = Path(directory) / "web.json"
            path.write_text(
                json.dumps(
                    {
                        "allowedLogins": ["viewer@example.invalid"],
                        "outbound": {"enabled": True, "helperPath": str(helper)},
                    }
                ),
                encoding="utf-8",
            )
            os.chmod(path, 0o600)
            settings = load_settings(path)
            self.assertTrue(settings.outbound.enabled)
            self.assertEqual(settings.outbound.helper_path, helper)

    def test_unavailable_outbound_helper_does_not_invalidate_read_config(self):
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / "missing-helper"
            path = Path(directory) / "web.json"
            path.write_text(
                json.dumps(
                    {
                        "allowedLogins": ["viewer@example.invalid"],
                        "outbound": {"enabled": True, "helperPath": str(helper)},
                    }
                ),
                encoding="utf-8",
            )
            os.chmod(path, 0o600)
            settings = load_settings(path)
            self.assertTrue(settings.outbound.enabled)
            self.assertEqual(settings.outbound.helper_path, helper)

    def test_outbound_config_fails_closed_for_unknown_or_incomplete_values(self):
        cases = (
            {"enabled": True},
            {"enabled": "yes", "helperPath": "/tmp/helper"},
            {"enabled": False, "helperPath": "/tmp/helper"},
            {"enabled": False, "extra": True},
        )
        for outbound in cases:
            with self.subTest(outbound=outbound), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "web.json"
                path.write_text(
                    json.dumps(
                        {
                            "allowedLogins": ["viewer@example.invalid"],
                            "outbound": outbound,
                        }
                    ),
                    encoding="utf-8",
                )
                os.chmod(path, 0o600)
                with self.assertRaises(ConfigError):
                    load_settings(path)

    def test_login_normalization_rejects_empty_and_control_values(self):
        self.assertEqual(normalize_login("  Viewer@Example.INVALID "), "viewer@example.invalid")
        for value in ("", "   ", "viewer\n@example.invalid", "viewer\x00@example.invalid"):
            with self.subTest(value=repr(value)), self.assertRaises(ConfigError):
                normalize_login(value)

    def test_write_config_creates_mode_0600_file_without_repo_dependency(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "nested" / "web.json"
            settings = write_config(
                path,
                allowed_logins=["Viewer@Example.INVALID"],
                port=8766,
                refresh_seconds=30,
            )
            self.assertIsInstance(settings, Settings)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(load_settings(path), settings)

    def test_identity_mode_requires_a_nonempty_allowlist(self):
        with self.assertRaises(ConfigError):
            Settings(allowed_logins=(), auth_mode="identity")

    def test_network_mode_is_explicit_and_has_no_identity_allowlist(self):
        settings = Settings(allowed_logins=(), auth_mode="network")
        self.assertEqual(settings.auth_mode, "network")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            write_config(path, allowed_logins=[], auth_mode="network")
            self.assertEqual(load_settings(path), settings)

    def test_unknown_auth_mode_fails_closed(self):
        with self.assertRaises(ConfigError):
            Settings(allowed_logins=("viewer@example.invalid",), auth_mode="header")

    def test_malformed_allowlist_type_fails_as_config_error(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(json.dumps({"allowedLogins": None}), encoding="utf-8")
            os.chmod(path, 0o600)
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_duplicate_config_keys_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            path.write_text(
                '{"allowedLogins":["viewer@example.invalid"],"allowedLogins":["other@example.invalid"]}',
                encoding="utf-8",
            )
            os.chmod(path, 0o600)
            with self.assertRaises(ConfigError):
                load_settings(path)

    def test_symlink_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target.json"
            target.write_text(json.dumps({"allowedLogins": ["viewer@example.invalid"]}), encoding="utf-8")
            os.chmod(target, 0o600)
            link = Path(directory) / "web.json"
            link.symlink_to(target)
            with self.assertRaises(ConfigError):
                load_settings(link)


if __name__ == "__main__":
    unittest.main()

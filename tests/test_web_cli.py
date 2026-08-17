import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import kakaotalk_reader


class WebCliTests(unittest.TestCase):
    def test_init_config_is_local_only_mode_0600_and_does_not_print_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            stdout = io.StringIO()
            with patch.object(
                sys,
                "argv",
                [
                    "kakaotalk-reader",
                    "web",
                    "--init-config",
                    "--config",
                    str(path),
                    "--login",
                    "viewer@example.invalid",
                ],
            ), contextlib.redirect_stdout(stdout):
                kakaotalk_reader.main()

            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)
            self.assertNotIn("viewer@example.invalid", stdout.getvalue())
            self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["allowedLogins"], ["viewer@example.invalid"])

    def test_network_mode_config_creation_does_not_require_a_login(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "web.json"
            with patch.object(
                sys,
                "argv",
                [
                    "kakaotalk-reader",
                    "web",
                    "--init-config",
                    "--config",
                    str(path),
                    "--auth-mode",
                    "network",
                ],
            ):
                kakaotalk_reader.main()

            self.assertEqual(json.loads(path.read_text(encoding="utf-8"))["authMode"], "network")

    def test_web_startup_uses_secure_config_loader_before_server(self):
        with patch.object(sys, "argv", ["kakaotalk-reader", "web", "--config", "/missing/web.json"]), self.assertRaises(SystemExit) as raised:
            with contextlib.redirect_stderr(io.StringIO()):
                kakaotalk_reader.main()
        self.assertEqual(raised.exception.code, 1)


if __name__ == "__main__":
    unittest.main()

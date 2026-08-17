import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from kakaotalk_outbound import AXPreparation
from kakaotalk_outbound.macos import HelperAXAdapter


class HelperAXAdapterTests(unittest.TestCase):
    def setUp(self):
        self.adapter = HelperAXAdapter(
            Path("/synthetic/ax-helper"),
            timeout_seconds=2,
            path_validator=lambda _path: True,
        )

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_availability_is_non_mutating_and_bounded(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, '{"status":"available"}\n', "")
        self.assertTrue(self.adapter.check_availability().is_available)
        self.assertEqual(run.call_args.args[0], ["/synthetic/ax-helper", "availability"])
        self.assertEqual(run.call_args.kwargs["input"], "")
        self.assertEqual(run.call_args.kwargs["timeout"], 2)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_private_payload_uses_stdin_not_process_arguments(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, '{"status":"ready"}\n', "")
        with self.assertLogs("kakaotalk_outbound.macos", level="INFO") as captured:
            preparation = self.adapter.prepare_text(
                expected_identity="Synthetic private room",
                text="Synthetic private body",
            )
        self.assertEqual(preparation.status, "ready")
        arguments = run.call_args.args[0]
        self.assertEqual(arguments, ["/synthetic/ax-helper", "prepare"])
        self.assertNotIn("Synthetic private room", " ".join(arguments))
        self.assertNotIn("Synthetic private body", " ".join(arguments))
        self.assertEqual(
            json.loads(run.call_args.kwargs["input"]),
            {"targetIdentity": "Synthetic private room", "text": "Synthetic private body"},
        )
        diagnostic = "\n".join(captured.output)
        self.assertIn("operation=prepare status=ready elapsed_ms=", diagnostic)
        self.assertNotIn("Synthetic private room", diagnostic)
        self.assertNotIn("Synthetic private body", diagnostic)
        self.assertNotIn("/synthetic/ax-helper", diagnostic)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_send_is_a_separate_one_time_helper_operation(self, run):
        run.side_effect = [
            subprocess.CompletedProcess([], 0, '{"status":"ready"}\n', ""),
            subprocess.CompletedProcess([], 0, '{"status":"invoked"}\n', ""),
        ]
        preparation = self.adapter.prepare_text(expected_identity="Synthetic room", text="Synthetic text")
        outcome = self.adapter.send_prepared(preparation)
        self.assertEqual(outcome.status, "invoked")
        self.assertEqual(run.call_args_list[1].args[0], ["/synthetic/ax-helper", "send"])
        with self.assertRaises(ValueError):
            self.adapter.send_prepared(preparation)
        self.assertEqual(run.call_count, 2)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_every_safe_prepare_failure_stage_remains_distinguishable(self, run):
        for helper_status in (
            "accessibility_permission_unavailable",
            "target_process_missing",
            "target_launch_unavailable",
            "target_window_unavailable",
            "target_row_missing",
            "target_row_ambiguous",
            "target_row_selection_unavailable",
            "composer_missing",
            "composer_ambiguous",
            "composer_write_unavailable",
            "composer_readback_mismatch",
        ):
            with self.subTest(helper_status=helper_status):
                run.return_value = subprocess.CompletedProcess(
                    [], 0, json.dumps({"status": helper_status}), ""
                )
                preparation = self.adapter.prepare_text(
                    expected_identity="Synthetic room",
                    text="Synthetic text",
                )
                self.assertEqual(preparation.status, helper_status)
                self.assertEqual(self.adapter.last_status, helper_status)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_previous_readback_status_is_normalized_at_the_helper_boundary(self, run):
        run.return_value = subprocess.CompletedProcess([], 0, '{"status":"readback_mismatch"}\n', "")
        preparation = self.adapter.prepare_text(
            expected_identity="Synthetic room",
            text="Synthetic text",
        )
        self.assertEqual(preparation.status, "composer_readback_mismatch")
        self.assertEqual(self.adapter.last_status, "composer_readback_mismatch")

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_every_safe_send_failure_stage_remains_distinguishable(self, run):
        for helper_status in (
            "accessibility_permission_unavailable",
            "target_process_missing",
            "target_launch_unavailable",
            "target_window_unavailable",
            "target_row_missing",
            "target_row_ambiguous",
            "target_row_selection_unavailable",
            "composer_missing",
            "composer_ambiguous",
            "composer_readback_mismatch",
            "send_mismatch",
            "send_unavailable",
        ):
            with self.subTest(helper_status=helper_status):
                adapter = HelperAXAdapter(
                    Path("/synthetic/ax-helper"),
                    timeout_seconds=2,
                    path_validator=lambda _path: True,
                )
                run.side_effect = [
                    subprocess.CompletedProcess([], 0, '{"status":"ready"}\n', ""),
                    subprocess.CompletedProcess(
                        [], 0, json.dumps({"status": helper_status}), ""
                    ),
                ]
                preparation = adapter.prepare_text(
                    expected_identity="Synthetic room",
                    text="Synthetic text",
                )
                self.assertEqual(adapter.send_prepared(preparation).status, helper_status)
                self.assertEqual(adapter.last_status, helper_status)
                run.reset_mock(side_effect=True)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_malformed_helper_output_fails_closed(self, run):
        for output in ("", "not-json", '{"status":"ready","private":"leak"}', "x" * 5000):
            with self.subTest(output=output[:20]):
                run.return_value = subprocess.CompletedProcess([], 0, output, "private stderr")
                self.assertFalse(self.adapter.check_availability().is_available)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_subprocess_failures_remain_distinguishable_and_private(self, run):
        cases = (
            ("helper_spawn_failed", OSError("private spawn detail")),
            (
                "helper_timeout",
                subprocess.TimeoutExpired(
                    cmd=["/private/helper/path", "prepare"],
                    timeout=2,
                    output="private output",
                    stderr="private stderr",
                ),
            ),
            ("helper_exit_failed", subprocess.CompletedProcess([], 7, "", "private stderr")),
            ("helper_output_invalid", subprocess.CompletedProcess([], 0, "not-json", "private stderr")),
            (
                "helper_status_unexpected",
                subprocess.CompletedProcess([], 0, '{"status":"private unexpected status"}', "private stderr"),
            ),
        )
        for expected, result in cases:
            with self.subTest(expected=expected):
                run.side_effect = result if isinstance(result, BaseException) else None
                run.return_value = None if isinstance(result, BaseException) else result
                with self.assertLogs("kakaotalk_outbound.macos", level="INFO") as captured:
                    preparation = self.adapter.prepare_text(
                        expected_identity="Synthetic private identity",
                        text="Synthetic private body",
                    )
                self.assertEqual(preparation.status, expected)
                self.assertEqual(self.adapter.last_status, expected)
                diagnostic = "\n".join(captured.output)
                self.assertIn("status=" + expected, diagnostic)
                for private_value in (
                    "Synthetic private identity",
                    "Synthetic private body",
                    "/private/helper/path",
                    "private output",
                    "private stderr",
                    "private unexpected status",
                ):
                    self.assertNotIn(private_value, diagnostic)
                run.reset_mock(side_effect=True, return_value=True)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_invalid_helper_path_has_a_distinct_status_without_spawning(self, run):
        adapter = HelperAXAdapter(
            Path("/synthetic/private-helper"),
            timeout_seconds=2,
            path_validator=lambda _path: False,
        )
        with self.assertLogs("kakaotalk_outbound.macos", level="INFO") as captured:
            preparation = adapter.prepare_text(
                expected_identity="Synthetic private identity",
                text="Synthetic private body",
            )
        self.assertEqual(preparation.status, "helper_path_invalid")
        self.assertEqual(adapter.last_status, "helper_path_invalid")
        self.assertNotIn("/synthetic/private-helper", "\n".join(captured.output))
        run.assert_not_called()

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_untrusted_helper_status_and_stderr_are_sanitized_before_logging(self, run):
        private_identity = "Synthetic private identity"
        private_text = "Synthetic private body"
        run.return_value = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": private_identity}), private_text
        )
        with self.assertLogs("kakaotalk_outbound.macos", level="INFO") as captured:
            preparation = self.adapter.prepare_text(
                expected_identity=private_identity,
                text=private_text,
            )
        self.assertEqual(preparation.status, "helper_status_unexpected")
        self.assertEqual(len(captured.output), 1)
        diagnostic = captured.output[0]
        self.assertIn("operation=prepare status=helper_status_unexpected elapsed_ms=", diagnostic)
        self.assertNotIn(private_identity, diagnostic)
        self.assertNotIn(private_text, diagnostic)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_helper_output_cannot_spoof_subprocess_failure_classification(self, run):
        for reported_status in (
            "helper_path_invalid",
            "helper_spawn_failed",
            "helper_timeout",
            "helper_exit_failed",
            "helper_output_invalid",
            "helper_status_unexpected",
        ):
            with self.subTest(reported_status=reported_status):
                run.return_value = subprocess.CompletedProcess(
                    [],
                    0,
                    json.dumps({"status": reported_status}),
                    "",
                )
                preparation = self.adapter.prepare_text(
                    expected_identity="Synthetic room",
                    text="Synthetic text",
                )
                self.assertEqual(preparation.status, "helper_status_unexpected")

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_helper_replacement_is_revalidated_before_every_invocation(self, run):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as directory:
            helper = Path(directory) / "helper"
            replacement = Path(directory) / "replacement"
            helper.write_text("synthetic", encoding="utf-8")
            replacement.write_text("replacement", encoding="utf-8")
            helper.chmod(0o700)
            replacement.chmod(0o700)
            adapter = HelperAXAdapter(helper)
            helper.unlink()
            helper.symlink_to(replacement)
            self.assertFalse(adapter.check_availability().is_available)
            run.assert_not_called()

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_helper_beneath_an_ancestor_symlink_is_rejected(self, run):
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / "helper"
            helper.write_text("synthetic", encoding="utf-8")
            helper.chmod(0o700)
            self.assertTrue(str(helper).startswith("/var/"))
            self.assertFalse(HelperAXAdapter(helper).check_availability().is_available)
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()

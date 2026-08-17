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
    def test_safe_target_and_composer_mismatches_remain_distinguishable(self, run):
        for helper_status, expected_status in (
            ("target_mismatch", "target_mismatch"),
            ("composer_mismatch", "composer_mismatch"),
        ):
            with self.subTest(helper_status=helper_status):
                run.return_value = subprocess.CompletedProcess(
                    [], 0, json.dumps({"status": helper_status}), ""
                )
                preparation = self.adapter.prepare_text(
                    expected_identity="Synthetic room",
                    text="Synthetic text",
                )
                self.assertEqual(preparation.status, expected_status)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_safe_send_mismatches_remain_distinguishable(self, run):
        for helper_status, expected_status in (
            ("target_mismatch", "target_mismatch"),
            ("composer_mismatch", "composer_mismatch"),
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
                self.assertEqual(adapter.send_prepared(preparation).status, expected_status)
                run.reset_mock(side_effect=True)

    @patch("kakaotalk_outbound.macos.subprocess.run")
    def test_malformed_helper_output_fails_closed(self, run):
        for output in ("", "not-json", '{"status":"ready","private":"leak"}', "x" * 5000):
            with self.subTest(output=output[:20]):
                run.return_value = subprocess.CompletedProcess([], 0, output, "private stderr")
                self.assertFalse(self.adapter.check_availability().is_available)

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
